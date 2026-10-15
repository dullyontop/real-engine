//! Procedural sources in the runtime (ADR-0015): each `GraphSource`'s graph
//! (a `graph` file, or a `builder` script run in the `tools` context) goes into
//! the World's [`GraphStore`]; `World::regenerate_changed` evaluates it; the
//! derived meshes and heightfields it makes are handed to [`Media`] and the
//! physics [`ShapeLibrary`], and released when nothing uses them any more.

use std::collections::BTreeSet;
use std::path::Path;

use re_core::{Diagnostic, Location};
use re_proc::GraphStore;
use re_scene::{Collider, ColliderShape, GraphSource, MeshRef, World};
use re_script::{NoServices, RuntimeConfig, ScriptRuntime, WorldHost};
use re_types::ShapeDesc;
use re_types::graph::GraphDoc;

use crate::codes;
use crate::media::{DERIVED, Media};
use crate::sync::physics::{HeightfieldData, ShapeLibrary};

/// Registers the `graph` generator and gives the World an empty store for the
/// project at `project`.
pub fn install(world: &mut World, project: &Path) {
    re_proc::register_generator(&mut world.generators);
    world
        .ecs
        .insert_resource(GraphStore::for_project(project.to_path_buf()));
}

/// Every procedural source with a `GraphSource`: `(stable id, graph, builder)`.
fn sources(world: &mut World) -> Vec<(String, String, String)> {
    let mut found: Vec<_> = world
        .ecs
        .query::<(re_scene::Entity, &GraphSource)>()
        .iter(&world.ecs)
        .map(|(e, g)| (e, g.graph.clone(), g.builder.clone()))
        .collect::<Vec<_>>()
        .into_iter()
        .filter_map(|(e, graph, builder)| world.id_of(e).map(|id| (id.to_string(), graph, builder)))
        .collect();
    found.sort();
    found
}

fn failed(source: &str, file: &str, message: String) -> Diagnostic {
    Diagnostic::error(codes::GRAPH_SOURCE_FAILED, message)
        .at(Location::file(file))
        .with_hint(format!("procedural source {source:?}"))
}

/// The graph of one source: its `graph` file, else what its `builder` returns.
fn graph_of(
    world: &mut World,
    project: &Path,
    builders: &mut Option<ScriptRuntime>,
    (id, graph, builder): &(String, String, String),
) -> Result<GraphDoc, Diagnostic> {
    if !graph.is_empty() {
        let path =
            crate::commands::project_path(graph, "the graph file", codes::GRAPH_SOURCE_FAILED)?;
        let text = std::fs::read_to_string(project.join(&path))
            .map_err(|e| failed(id, &path, format!("cannot read the graph {path}: {e}")))?;
        return GraphDoc::from_json(&text)
            .map_err(|e| failed(id, &path, format!("{path} is not a valid graph: {e}")));
    }
    if builder.is_empty() {
        return Err(Diagnostic::error(
            codes::GRAPH_SOURCE_FAILED,
            format!("the procedural source {id:?} has neither a graph file nor a builder script"),
        )
        .at(Location::entity(id.clone())));
    }
    if builders.is_none() {
        let runtime = ScriptRuntime::new(RuntimeConfig::new(project)).map_err(|e| {
            failed(
                id,
                builder,
                "cannot start a script context for the builder".into(),
            )
            .with_related(e.to_diagnostic())
        })?;
        *builders = Some(runtime);
    }
    let runtime = builders.as_mut().expect("created above");
    let mut services = NoServices::default();
    let mut host = WorldHost {
        world,
        services: &mut services,
    };
    runtime.run_builder(&mut host, builder).map_err(|ds| {
        let mut d = failed(
            id,
            builder,
            format!("the builder {builder} did not return a graph"),
        );
        for r in ds {
            d = d.with_related(r);
        }
        d
    })
}

/// Fills the World's store with the graph of every procedural source (only the
/// sources whose `graph` or `builder` is in `only`, when given). A source whose
/// graph cannot be made keeps its previous graph, if any, and is reported.
pub fn load_graphs(world: &mut World, project: &Path, only: Option<&[String]>) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut builders = None;
    for source in sources(world) {
        if let Some(only) = only
            && !only.iter().any(|p| *p == source.1 || *p == source.2)
        {
            continue;
        }
        match graph_of(world, project, &mut builders, &source) {
            Ok(doc) => {
                if let Some(mut store) = world.ecs.get_resource_mut::<GraphStore>() {
                    store.insert(source.0.clone(), doc);
                }
            }
            Err(d) => diags.push(d),
        }
    }
    diags
}

/// True when a changed project file is the graph or builder of a source.
pub fn is_graph_input(world: &mut World, path: &str) -> bool {
    sources(world)
        .iter()
        .any(|(_, graph, builder)| graph == path || builder == path)
}

/// Gives `media` the derived meshes that `MeshRef`s name, and `shapes` the
/// derived meshes and heightfields that colliders name, from the World's store.
pub fn sync_derived(world: &mut World, media: &mut Media, shapes: &mut ShapeLibrary) {
    let meshes: BTreeSet<String> = world
        .ecs
        .query::<&MeshRef>()
        .iter(&world.ecs)
        .filter(|m| m.asset.starts_with(DERIVED))
        .map(|m| m.asset.clone())
        .collect();
    let colliders: Vec<(ColliderShape, String)> = world
        .ecs
        .query::<&Collider>()
        .iter(&world.ecs)
        .filter(|c| c.source.starts_with(DERIVED))
        .map(|c| (c.shape, c.source.clone()))
        .collect();
    let Some(store) = world.ecs.get_resource::<GraphStore>() else {
        return;
    };
    for name in meshes {
        if !media.has_derived_mesh(&name)
            && let Some(mesh) = store.mesh(&name)
        {
            media.insert_derived_mesh(name, mesh);
        }
    }
    for (shape, name) in colliders {
        match shape {
            ColliderShape::Heightfield => {
                if shapes.heightfield(&name).is_none()
                    && let Some(h) = store.heightfield(&name)
                    && let ShapeDesc::Heightfield {
                        n,
                        samples,
                        spacing,
                        offset,
                    } = h.to_shape()
                {
                    shapes.insert_heightfield(
                        name,
                        std::sync::Arc::new(HeightfieldData {
                            n,
                            spacing,
                            offset,
                            samples,
                        }),
                    );
                }
            }
            ColliderShape::Mesh | ColliderShape::Convex => {
                if shapes.mesh(&name).is_none()
                    && let Some(mesh) = store.mesh(&name)
                {
                    shapes.insert_mesh(name, mesh);
                }
            }
            _ => {}
        }
    }
}

/// Forgets derived assets a regeneration released: from the store (which also
/// drops a terrain's heightfield), from `media` and from `shapes`. Returns the
/// `derived:` names, for the caller to release on the GPU.
pub fn release(
    world: &mut World,
    media: &mut Media,
    shapes: &mut ShapeLibrary,
    released: &[String],
) -> Vec<String> {
    let derived: Vec<String> = released
        .iter()
        .filter(|a| a.starts_with(DERIVED))
        .cloned()
        .collect();
    let store = world.ecs.get_resource::<GraphStore>();
    for name in &derived {
        if let Some(store) = store {
            store.release(name);
        }
        media.remove_derived_mesh(name);
        shapes.remove(name);
    }
    derived
}

/// The `derived:` heightfields colliders use: a change means the ground moved.
pub fn ground(world: &mut World) -> BTreeSet<String> {
    world
        .ecs
        .query::<&Collider>()
        .iter(&world.ecs)
        .filter(|c| c.shape == ColliderShape::Heightfield && c.source.starts_with(DERIVED))
        .map(|c| c.source.clone())
        .collect()
}

/// The highest point of any derived heightfield in the store, in its local
/// space (a ray from above it finds the ground).
pub fn ground_top(world: &World) -> Option<f32> {
    let store = world.ecs.get_resource::<GraphStore>()?;
    let mut top: Option<f32> = None;
    for name in world
        .ecs
        .iter_entities()
        .filter_map(|e| e.get::<Collider>())
        .filter(|c| c.shape == ColliderShape::Heightfield)
        .map(|c| c.source.clone())
    {
        if let Some(h) = store.heightfield(&name)
            && let ShapeDesc::Heightfield {
                samples, offset, ..
            } = h.to_shape()
        {
            let max = samples.iter().copied().fold(f32::MIN, f32::max) + offset.y;
            top = Some(top.map_or(max, |t| t.max(max)));
        }
    }
    top
}
