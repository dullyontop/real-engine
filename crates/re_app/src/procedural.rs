//! Procedural sources in the runtime (ADR-0015): each `GraphSource`'s graph
//! (a `graph` file, or a `builder` script run in the `tools` context) goes into
//! the World's [`GraphStore`]; `World::regenerate_changed` evaluates it; the
//! derived meshes and heightfields it makes are handed to [`Media`] and the
//! physics [`ShapeLibrary`], and released when nothing uses them any more.

use re_core::files::ProjectFiles;
use std::collections::{BTreeMap, BTreeSet};

use re_core::{Diagnostic, Location};
use re_proc::GraphStore;
use re_scene::ecs as bevy_ecs;
use re_scene::{Collider, ColliderShape, GraphSource, MeshRef, World};
use re_script::{NoServices, RuntimeConfig, ScriptRuntime, WorldHost};
use re_types::ShapeDesc;
use re_types::graph::GraphDoc;

use crate::codes;
use crate::media::{DERIVED, Media};
use crate::sync::physics::{HeightfieldData, ShapeLibrary};

/// Registers the `graph` generator and the validator of open graph documents,
/// and gives the World an empty store for the project read through `files`.
pub fn install(world: &mut World, files: &ProjectFiles) {
    re_proc::register_generator(&mut world.generators);
    re_proc::register_doc_validators(world);
    world
        .registry
        .register::<re_proc::bake::Baked>(&mut world.ecs);
    world
        .ecs
        .insert_resource(GraphStore::for_files(files.clone()));
}

/// Declares the project's Luau node kinds (`scripts/generators/`, ADR-0043
/// §4) into the World's store: graphs use the M1 kinds plus these from now
/// on. Called when the project loads and when a generator script changes; a
/// source whose graph uses a kind whose scripts changed regenerates at the
/// next `regenerate_changed`. Returns the declaration problems (`RE4100`).
pub fn declare_generators(
    world: &mut World,
    project: &ProjectFiles,
    limits: re_script::generator::Limits,
) -> Vec<Diagnostic> {
    let base = re_proc::NodeRegistry::m1();
    let declared = re_script::generator::declare(project, &base, limits);
    if let Some(mut store) = world.ecs.get_resource_mut::<GraphStore>() {
        let registry = declared.registry_keeping(&base, store.evaluator().registry());
        store.set_registry(std::sync::Arc::new(registry));
    }
    declared.diagnostics
}

/// True if `path` (a project path) is a generator script or module.
pub fn is_generator_script(path: &str) -> bool {
    path.strip_prefix(re_script::generator::GENERATORS_DIR)
        .is_some_and(|rest| rest.starts_with('/'))
        && path.ends_with(".luau")
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
    project: &ProjectFiles,
    builders: &mut Option<ScriptRuntime>,
    (id, graph, builder): &(String, String, String),
) -> Result<GraphDoc, Diagnostic> {
    if !graph.is_empty() {
        let path =
            crate::commands::project_path(graph, "the graph file", codes::GRAPH_SOURCE_FAILED)?;
        let text = project
            .read_str(&path)
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
        let runtime =
            ScriptRuntime::new(RuntimeConfig::with_files(project.clone())).map_err(|e| {
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

/// A project path as the file watcher reports it (`./a.json` and `a\\b`
/// spellings normalised); `None` for a path outside the project.
fn norm(path: &str) -> Option<String> {
    crate::commands::project_path(path, "", codes::GRAPH_SOURCE_FAILED)
        .ok()
        .map(|p| p.replace("/./", "/"))
}

/// True when the changed project file `path` is an input of `source`: its
/// graph file, its builder, or a Luau module beside or below the builder (what
/// it can `require`).
fn reads(source: &(String, String, String), path: &str) -> bool {
    let (_, graph, builder) = source;
    let Some(path) = norm(path) else {
        return false;
    };
    if norm(graph).is_some_and(|g| g == path) {
        return true;
    }
    let Some(builder) = norm(builder) else {
        return false;
    };
    if builder == path {
        return true;
    }
    let dir = builder.rsplit_once('/').map_or("", |(d, _)| d);
    path.ends_with(".luau") && (dir.is_empty() || path.starts_with(&format!("{dir}/")))
}

/// What each procedural source's graph is made from: `id -> (graph, builder)`.
pub type GraphInputs = BTreeMap<String, (String, String)>;

/// The current [`GraphInputs`] of the World's procedural sources.
pub fn inputs(world: &mut World) -> GraphInputs {
    sources(world)
        .into_iter()
        .map(|(id, graph, builder)| (id, (graph, builder)))
        .collect()
}

fn load_where(
    world: &mut World,
    project: &ProjectFiles,
    wanted: impl Fn(&(String, String, String)) -> bool,
) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut builders = None;
    for source in sources(world) {
        if !wanted(&source) {
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
    // Every source's graph file is checked as a graph when open, whatever
    // its name (ADR-0044).
    re_proc::register_doc_validators(world);
    diags
}

/// Fills the World's store with the graph of every procedural source (only the
/// sources that read one of the changed project files in `only`, when given). A
/// source whose graph cannot be made keeps its previous graph, if any, and is
/// reported.
pub fn load_graphs(
    world: &mut World,
    project: &ProjectFiles,
    only: Option<&[String]>,
) -> Vec<Diagnostic> {
    match only {
        None => load_where(world, project, |_| true),
        Some(paths) => load_where(world, project, |s| paths.iter().any(|p| reads(s, p))),
    }
}

/// Loads the graphs of the sources that appeared, or whose `graph` or `builder`
/// changed, since `known` was taken (by a command, an undo, a prefab spawn),
/// and updates `known`. A source that fails is reported once, then left until
/// its inputs or files change again.
pub fn refresh_graphs(
    world: &mut World,
    project: &ProjectFiles,
    known: &mut GraphInputs,
) -> Vec<Diagnostic> {
    let now = inputs(world);
    if now == *known {
        return Vec::new();
    }
    // A source still being filled in (no graph and no builder yet, as the
    // inspector adds it) waits quietly.
    let stale = |(id, graph, builder): &(String, String, String)| {
        !(graph.is_empty() && builder.is_empty())
            && known
                .get(id)
                .is_none_or(|(g, b)| g != graph || b != builder)
    };
    let diags = load_where(world, project, stale);
    *known = now;
    diags
}

/// True when a changed project file is an input of a procedural source.
pub fn is_graph_input(world: &mut World, path: &str) -> bool {
    sources(world).iter().any(|s| reads(s, path))
}

/// Baked `.heightfield` files that failed to load: read again only after
/// they change ([`heightfield_changed`]) or disappear.
#[derive(Default, bevy_ecs::resource::Resource)]
struct FailedHeightfields(BTreeSet<String>);

/// Forgets that the `.heightfield` file at `path` failed to load, so the
/// next [`sync_derived`] reads it again (the project watcher saw it change).
pub fn heightfield_changed(world: &mut World, path: &str) {
    if let Some(mut failed) = world.ecs.get_resource_mut::<FailedHeightfields>() {
        failed.0.remove(path);
    }
}

/// Gives `media` the derived meshes that `MeshRef`s name, and `shapes` the
/// derived meshes and heightfields that colliders name, from the World's store,
/// and forgets the derived ones nothing names any more (whatever released them:
/// a regeneration, a `graph.regenerate` command, a play copy).
pub fn sync_derived(world: &mut World, media: &mut Media, shapes: &mut ShapeLibrary) {
    let mut meshes: BTreeSet<String> = world
        .ecs
        .query::<&MeshRef>()
        .iter(&world.ecs)
        .filter(|m| m.asset.starts_with(DERIVED))
        .map(|m| m.asset.clone())
        .collect();
    // Streamed terrain chunks (ADR-0042) carry their meshes.
    if let Some(view) = world.ecs.get_resource::<re_proc::stream::TerrainView>() {
        for c in view.chunks() {
            if !media.has_derived_mesh(&c.mesh) {
                media.insert_derived_mesh(c.mesh.clone(), c.data().clone());
            }
            meshes.insert(c.mesh.clone());
        }
    }
    let colliders: Vec<(ColliderShape, String)> = world
        .ecs
        .query::<&Collider>()
        .iter(&world.ecs)
        .filter(|c| c.source.starts_with(DERIVED))
        .map(|c| (c.shape, c.source.clone()))
        .collect();
    media.retain_derived_meshes(|name| meshes.contains(name));
    shapes.retain(|name| !name.starts_with(DERIVED) || colliders.iter().any(|(_, c)| c == name));
    let baked: Vec<String> = world
        .ecs
        .query::<&Collider>()
        .iter(&world.ecs)
        .filter(|c| c.shape == ColliderShape::Heightfield && c.source.ends_with(".heightfield"))
        .map(|c| c.source.clone())
        .collect();
    let mut failed = world
        .ecs
        .remove_resource::<FailedHeightfields>()
        .unwrap_or_default()
        .0;
    failed.retain(|name| baked.contains(name));
    let Some(store) = world.ecs.get_resource::<GraphStore>() else {
        world.ecs.insert_resource(FailedHeightfields(failed));
        return;
    };
    // Baked heightfields (ADR-0043): `.heightfield` files in the project.
    for name in baked {
        if shapes.heightfield(&name).is_some() {
            continue;
        }
        let Some(files) = store.files() else {
            continue;
        };
        // A file that cannot be read stays missing; physics reports it. A
        // file that failed is read again once it changes, or is removed and
        // comes back.
        if !files.is_file(&name) {
            failed.remove(&name);
            continue;
        }
        if failed.contains(&name) {
            continue;
        }
        let shape = files
            .read(&name)
            .ok()
            .and_then(|data| re_proc::bake::read_heightfield(&data.bytes).ok())
            .map(|h| h.to_shape());
        let Some(ShapeDesc::Heightfield {
            n,
            samples,
            spacing,
            offset,
            precision,
        }) = shape
        else {
            failed.insert(name);
            continue;
        };
        failed.remove(&name);
        shapes.insert_heightfield(
            name,
            std::sync::Arc::new(HeightfieldData {
                n,
                spacing,
                offset,
                samples,
                precision,
            }),
        );
    }
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
                        ..
                    } = h.to_shape()
                {
                    let precision = re_proc::stream::collider_precision(&name);
                    shapes.insert_heightfield(
                        name,
                        std::sync::Arc::new(HeightfieldData {
                            n,
                            spacing,
                            offset,
                            samples,
                            precision,
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
    world.ecs.insert_resource(FailedHeightfields(failed));
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
        // A terrain's heightfield collider goes with its mesh.
        if let Some(collider) = re_proc::value::collider_name(name) {
            shapes.remove(&collider);
        }
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

/// The lowest and highest world-space height of the derived heightfields
/// colliders use (their samples placed by each terrain's `GlobalTransform`): a
/// ray from above the top to below the bottom crosses all of them.
pub fn ground_span(world: &mut World) -> Option<(f32, f32)> {
    let terrains: Vec<(String, glam::Affine3A)> = world
        .ecs
        .query::<(&Collider, Option<&re_scene::GlobalTransform>)>()
        .iter(&world.ecs)
        .filter(|(c, _)| c.shape == ColliderShape::Heightfield && c.source.starts_with(DERIVED))
        .map(|(c, gt)| {
            (
                c.source.clone(),
                gt.map_or(glam::Affine3A::IDENTITY, |g| g.0),
            )
        })
        .collect();
    let store = world.ecs.get_resource::<GraphStore>()?;
    let mut span: Option<(f32, f32)> = None;
    for (name, pose) in terrains {
        let Some(h) = store.heightfield(&name) else {
            continue;
        };
        let ShapeDesc::Heightfield {
            n,
            samples,
            spacing,
            offset,
            ..
        } = h.to_shape()
        else {
            continue;
        };
        let (lo, hi) = samples
            .iter()
            .fold((f32::MAX, f32::MIN), |(lo, hi), &s| (lo.min(s), hi.max(s)));
        let width = n.saturating_sub(1) as f32 * spacing;
        for x in [0.0, width] {
            for y in [lo, hi] {
                for z in [0.0, width] {
                    let p = pose.transform_point3(offset + glam::Vec3::new(x, y, z));
                    span = Some(span.map_or((p.y, p.y), |(a, b)| (a.min(p.y), b.max(p.y))));
                }
            }
        }
    }
    span
}
