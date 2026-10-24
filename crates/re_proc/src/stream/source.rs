//! A streamed procedural source, checked and ready to be evaluated cell by
//! cell, and the evaluation of one cell.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use glam::{Quat, Vec3};
use re_core::{Diagnostic, Location};
use re_scene::ecs::entity::Entity;
use re_scene::{GenNode, GraphSource, Procedural, SceneFragment, Streamed, Transform, World};
use re_types::graph::GraphDoc;
use serde_json::Value as Json;

use super::StreamConfig;
use crate::canon::{doc_hash, version_stamp};
use crate::generator::{GENERATOR, apply_overrides, parse_overrides};
use crate::value::{DerivedAsset, Fragment};
use crate::{Domain, EvalOptions, Evaluator, GraphStore, PortType, ProcGraph, Streaming, codes};

/// A streamed procedural source of the `graph` generator, as the manager sees
/// it at one tick.
#[derive(Clone, Debug)]
pub(crate) struct Source {
    pub entity: Entity,
    pub id: String,
    /// Half side of the square it covers around the origin; 0 is unbounded.
    pub bounds: f32,
    pub graph: Arc<ProcGraph>,
    pub seed: u64,
    /// Metres between samples of a cell's window.
    pub spacing: f32,
    /// Sample intervals per cell side (`cell_size / spacing`).
    pub steps: u32,
    /// The version stamp (hex) of its graph.
    pub stamp: String,
    /// Everything that decides its cells' content; when it changes, its
    /// loaded cells are generated again.
    pub inputs: blake3::Hash,
    /// Its `terrain.chunks` terrain, if its graph has one.
    pub terrain: Option<Arc<TerrainSource>>,
    /// Its nodes written for an older version (`RE4019`), which still
    /// evaluate as that version.
    pub outdated: Vec<Diagnostic>,
}

/// The terrain of a streamed source: the field reaching its `terrain.chunks`
/// node, which world streaming evaluates per chunk (ADR-0042).
#[derive(Debug)]
pub(crate) struct TerrainSource {
    /// The nodes that compute the field, and nothing else.
    pub graph: Arc<ProcGraph>,
    /// The id of the node whose output is the field.
    pub field: String,
    /// The material file chunks are drawn with, or empty.
    pub material: String,
    /// The coarsest level of detail.
    pub max_level: u8,
}

impl Source {
    /// The window of cell `[x, z]`: its samples, at global sample indices.
    pub fn domain(&self, cell: [i32; 2]) -> Domain {
        let steps = i64::from(self.steps);
        Domain {
            origin: [i64::from(cell[0]) * steps, i64::from(cell[1]) * steps],
            n: self.steps + 1,
            spacing: self.spacing,
        }
    }

    /// True if cell `[x, z]` of side `size` touches the source's square.
    pub fn covers(&self, cell: [i32; 2], size: f32) -> bool {
        if self.bounds <= 0.0 {
            return true;
        }
        let b = self.bounds;
        let (x0, z0) = (cell[0] as f32 * size, cell[1] as f32 * size);
        x0 < b && x0 + size > -b && z0 < b && z0 + size > -b
    }
}

fn located(d: Diagnostic, id: &str) -> Diagnostic {
    if d.location.is_some() {
        d
    } else {
        d.at(Location::entity(id.to_string()))
    }
}

/// Checks the streamed procedural source `entity` and prepares it: `RE4302`
/// unless it and its parents have identity transforms, `RE4301` for a node
/// that does not stream or a spacing that does not divide `cell_size`, and
/// the graph's own errors.
///
/// The prepared source is kept in `cache` and reused while its graph, seed,
/// overrides, bounds, evaluator and `cell_size` stay the same, so a tick
/// with nothing changed builds no graph.
pub(crate) fn describe(
    world: &World,
    entity: Entity,
    config: &StreamConfig,
    cache: &mut DescribeCache,
) -> Result<Source, Diagnostic> {
    let id = world.id_of(entity).unwrap_or_default().to_string();
    let procedural = world
        .ecs
        .get::<Procedural>(entity)
        .ok_or_else(|| not_streamable(&id, "it is not a procedural source".into()))?;
    if procedural.generator != GENERATOR {
        return Err(not_streamable(
            &id,
            format!(
                "only `graph` sources stream; its generator is `{}`",
                procedural.generator
            ),
        ));
    }
    check_identity(world, entity, &id)?;
    let bounds = world.ecs.get::<Streamed>(entity).map_or(0.0, |s| s.bounds);
    let store = world.ecs.get_resource::<GraphStore>().ok_or_else(|| {
        Diagnostic::error(
            codes::NO_GRAPH,
            format!("no graph is stored for streamed source `{id}` (the world has no GraphStore)"),
        )
        .at(Location::entity(id.clone()))
    })?;
    let doc = store.current(world, &id).ok_or_else(|| {
        Diagnostic::error(
            codes::NO_GRAPH,
            format!("no graph is stored for streamed source `{id}`"),
        )
        .at(Location::entity(id.clone()))
    })??;
    let doc: &GraphDoc = &doc;
    let params = world
        .ecs
        .get::<GraphSource>(entity)
        .map_or("", |g| g.params.as_str());
    if let Some(c) = cache.get(&entity)
        && c.id == id
        && c.seed == procedural.seed
        && c.params == params
        && c.bounds.to_bits() == bounds.to_bits()
        && c.cell_size.to_bits() == config.cell_size.to_bits()
        && Arc::ptr_eq(&c.evaluator, store.evaluator())
        && c.doc == *doc
    {
        return c.result.clone();
    }
    let result = prepare(
        store,
        doc,
        &id,
        params,
        procedural.seed,
        bounds,
        entity,
        config,
    );
    cache.insert(
        entity,
        Described {
            id,
            seed: procedural.seed,
            params: params.to_string(),
            bounds,
            cell_size: config.cell_size,
            evaluator: store.evaluator().clone(),
            doc: doc.clone(),
            result: result.clone(),
        },
    );
    result
}

/// Prepared sources by entity, with what they were prepared from.
pub(crate) type DescribeCache = BTreeMap<Entity, Described>;

#[derive(Clone, Debug)]
pub(crate) struct Described {
    id: String,
    seed: i64,
    params: String,
    bounds: f32,
    cell_size: f32,
    evaluator: Arc<Evaluator>,
    doc: GraphDoc,
    result: Result<Source, Diagnostic>,
}

/// [`describe`] past its cache: the graph with its overrides, checked and
/// built.
#[allow(clippy::too_many_arguments)]
fn prepare(
    store: &GraphStore,
    doc: &GraphDoc,
    id: &str,
    params: &str,
    seed: i64,
    bounds: f32,
    entity: Entity,
    config: &StreamConfig,
) -> Result<Source, Diagnostic> {
    let id = id.to_string();
    let registry = store.evaluator().registry();
    let doc = match parse_overrides(params) {
        Ok(o) => apply_overrides(doc, &o, registry, &id).0,
        Err(_) => doc.clone(),
    };
    for node in &doc.nodes {
        if let Some(def) = registry.get(&node.kind)
            && def.streaming == Streaming::Whole
        {
            return Err(not_streamable(
                &id,
                format!("node `{}` ({}) does not stream", node.id, node.kind),
            ));
        }
    }
    let spacing = graph_spacing(&doc).map_err(|e| not_streamable(&id, e))?;
    let (cells, terrain) = split_terrain(&doc, registry).map_err(|e| not_streamable(&id, e))?;
    let steps = config.cell_size / spacing;
    let whole = steps.round();
    if (steps - whole).abs() > 1.0e-4 || !(1.0..=16_384.0).contains(&whole) {
        return Err(not_streamable(
            &id,
            format!(
                "[stream] cell_size {} is not a whole multiple (1 to 16384) of the graph's spacing {spacing}",
                config.cell_size
            ),
        ));
    }
    let build = |doc: &GraphDoc| {
        ProcGraph::from_doc(doc, registry).map_err(|mut problems| {
            let mut first = problems.remove(0);
            for d in problems {
                first = first.with_related(d);
            }
            located(first, &id)
        })
    };
    // The graph cells evaluate: without the terrain branch, which streaming
    // evaluates per chunk instead.
    let graph = build(&cells)?;
    let terrain = match terrain {
        Some((doc, field, material, max_level)) => Some(Arc::new(TerrainSource {
            graph: Arc::new(build(&doc)?),
            field,
            material,
            max_level,
        })),
        None => None,
    };
    let outdated = crate::graph::outdated_nodes(
        &graph.id,
        doc.nodes
            .iter()
            .map(|n| (n.id.as_str(), n.kind.as_str(), n.version)),
        registry,
    )
    .into_iter()
    .map(|d| d.at(Location::entity(id.clone())))
    .collect();
    let stamp = version_stamp(&doc, registry).to_hex().to_string();
    let seed = seed as u64;
    let mut h = blake3::Hasher::new();
    h.update(doc_hash(&doc).as_bytes());
    h.update(stamp.as_bytes());
    h.update(&seed.to_le_bytes());
    h.update(&spacing.to_bits().to_le_bytes());
    Ok(Source {
        entity,
        id,
        bounds,
        graph: Arc::new(graph),
        seed,
        spacing,
        steps: whole as u32,
        stamp,
        inputs: h.finalize(),
        terrain,
        outdated,
    })
}

/// The terrain branch of `doc`, split off: the graph cells evaluate (without
/// the `terrain.chunks` node, and without the nodes that only fed it), and,
/// when there is a `terrain.chunks` node, the graph of its field with the
/// field node's id, the material and `max_level`.
#[allow(clippy::type_complexity)]
fn split_terrain(
    doc: &GraphDoc,
    registry: &crate::NodeRegistry,
) -> Result<(GraphDoc, Option<(GraphDoc, String, String, u8)>), String> {
    let chunks: Vec<&re_types::graph::NodeDoc> = doc
        .nodes
        .iter()
        .filter(|n| n.kind == "terrain.chunks")
        .collect();
    let chunk = match chunks.as_slice() {
        [] => return Ok((doc.clone(), None)),
        [one] => *one,
        _ => {
            return Err(format!(
                "a streamed graph has at most one `terrain.chunks` node; it has {}",
                chunks.len()
            ));
        }
    };
    let Some(field) = doc
        .edges
        .iter()
        .find(|e| e.to.0 == chunk.id && e.to.1 == "field")
        .map(|e| e.from.0.clone())
    else {
        return Err(format!("node `{}` (terrain.chunks) has no field", chunk.id));
    };
    // The field's graph: the field node and everything upstream of it.
    let mut keep: BTreeSet<&str> = BTreeSet::from([field.as_str()]);
    loop {
        let before = keep.len();
        for e in &doc.edges {
            if keep.contains(e.to.0.as_str()) {
                keep.insert(e.from.0.as_str());
            }
        }
        if keep.len() == before {
            break;
        }
    }
    let field_doc = GraphDoc {
        nodes: doc
            .nodes
            .iter()
            .filter(|n| keep.contains(n.id.as_str()))
            .cloned()
            .collect(),
        edges: doc
            .edges
            .iter()
            .filter(|e| keep.contains(e.to.0.as_str()))
            .cloned()
            .collect(),
        ..doc.clone()
    };
    // The cells' graph: without the chunks node, then without every field
    // or points node whose output nothing uses any more (a mesh or fragment
    // left unused is output, so it stays).
    let mut cells = doc.clone();
    cells.nodes.retain(|n| n.id != chunk.id);
    cells.edges.retain(|e| e.to.0 != chunk.id);
    loop {
        let used: BTreeSet<&str> = cells.edges.iter().map(|e| e.from.0.as_str()).collect();
        let dead: BTreeSet<String> = cells
            .nodes
            .iter()
            .filter(|n| {
                !used.contains(n.id.as_str())
                    && registry
                        .get(&n.kind)
                        .is_some_and(|d| matches!(d.output, PortType::Field | PortType::Points))
            })
            .map(|n| n.id.clone())
            .collect();
        if dead.is_empty() {
            break;
        }
        cells.nodes.retain(|n| !dead.contains(&n.id));
        cells.edges.retain(|e| !dead.contains(&e.to.0));
    }
    let param = |k: &str| chunk.params.get(k);
    let material = param("material")
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string();
    let max_level = param("max_level")
        .and_then(Json::as_u64)
        .unwrap_or(5)
        .min(8) as u8;
    Ok((cells, Some((field_doc, field, material, max_level))))
}

fn not_streamable(id: &str, why: String) -> Diagnostic {
    Diagnostic::error(codes::NOT_STREAMABLE, format!("{id}: {why}"))
        .at(Location::entity(id.to_string()))
        .with_hint("see `real explain RE4301`; or remove `Streamed` from the source")
}

/// The graph's sample spacing: the `cell` of its `terrain.heightfield`
/// nodes (all equal), or 1 m with none.
fn graph_spacing(doc: &GraphDoc) -> Result<f32, String> {
    let mut spacing: Option<f32> = None;
    for node in doc.nodes.iter().filter(|n| n.kind == "terrain.heightfield") {
        let cell = node
            .params
            .get("cell")
            .and_then(Json::as_f64)
            .unwrap_or(1.0) as f32;
        if !(cell.is_finite() && cell > 0.0) {
            return Err(format!("node `{}` has spacing {cell}", node.id));
        }
        match spacing {
            Some(s) if s != cell => {
                return Err(format!(
                    "its heightfields use different spacings ({s} and {cell} at node `{}`)",
                    node.id
                ));
            }
            _ => spacing = Some(cell),
        }
    }
    Ok(spacing.unwrap_or(1.0))
}

fn check_identity(world: &World, entity: Entity, id: &str) -> Result<(), Diagnostic> {
    let mut e = Some(entity);
    while let Some(cur) = e {
        if let Some(t) = world.ecs.get::<Transform>(cur)
            && (t.translation != Vec3::ZERO || t.rotation != Quat::IDENTITY || t.scale != Vec3::ONE)
        {
            let who = world.id_of(cur).unwrap_or_default();
            return Err(Diagnostic::error(
                codes::STREAMED_SOURCE_MOVED,
                format!(
                    "streamed source `{id}` is not at the origin (`{who}` has translation {:?}, rotation {:?}, scale {:?})",
                    t.translation.to_array(),
                    t.rotation.to_array(),
                    t.scale.to_array()
                ),
            )
            .at(Location::entity(id.to_string()))
            .with_hint("reset the Transform of the source and its parents; move the content in the graph instead"));
        }
        e = re_scene::hierarchy::parent(&world.ecs, cur);
    }
    Ok(())
}

/// One cell's content from one source.
#[derive(Clone, Debug)]
pub(crate) struct CellOutput {
    /// The scene fragment, keys under the cell (`c/<x>_<z>/…`).
    pub fragment: SceneFragment,
    /// The derived assets it references.
    pub assets: Vec<(String, DerivedAsset)>,
}

/// One cell's evaluation for one source, owned, so it can run on a worker
/// (ADR-0042): serial, cancellable, with the shared evaluator and its caches.
pub(crate) struct CellJob {
    evaluator: Arc<Evaluator>,
    graph: Arc<ProcGraph>,
    opts: EvalOptions,
}

/// The raw result of a [`CellJob`]: the graph's output fragment, before
/// prefabs are instantiated and keys are put under the cell.
pub(crate) type RawCell = Result<Fragment, Diagnostic>;

impl CellJob {
    /// The job for `source`'s cell `cell`, stopping when `cancel` is set.
    pub fn new(
        world: &World,
        source: &Source,
        cell: [i32; 2],
        cancel: Option<Arc<AtomicBool>>,
    ) -> Result<CellJob, Diagnostic> {
        let store = world
            .ecs
            .get_resource::<GraphStore>()
            .ok_or_else(|| Diagnostic::error(codes::NO_GRAPH, "the world has no GraphStore"))?;
        Ok(CellJob {
            evaluator: store.evaluator().clone(),
            graph: source.graph.clone(),
            opts: EvalOptions {
                seed: Some(source.seed),
                root: store.root().map(Into::into),
                files: store.files().cloned(),
                domain: Some(source.domain(cell)),
                serial: true,
                cancel,
                ..Default::default()
            },
        })
    }

    /// Evaluates the graph over the cell's window.
    pub fn run(self) -> RawCell {
        self.evaluator.evaluate(&self.graph, &self.opts).fragment()
    }
}

/// Turns a job's result into the cell's content, on the main thread:
/// prefabs instantiated against the world's registry and keys put under the
/// cell (`RE4307` on failure, with the cause as a related diagnostic).
pub(crate) fn finish(
    world: &World,
    source: &Source,
    cell: [i32; 2],
    raw: RawCell,
) -> Result<CellOutput, Diagnostic> {
    let key = re_scene::cell_key(cell[0], cell[1]);
    let failed = |cause: Diagnostic| {
        Diagnostic::error(
            codes::CELL_FAILED,
            format!("{}: cell {key} could not be generated", source.id),
        )
        .at(Location::entity(source.id.clone()))
        .with_related(cause)
    };
    let raw = raw.map_err(failed)?;
    let fragment =
        crate::to_scene_fragment(&source.graph.id, &raw, &world.registry).map_err(failed)?;
    let fragment = fragment
        .into_iter()
        .map(|n| GenNode {
            key: format!("{key}/{}", n.key),
            parent_key: n.parent_key.map(|p| format!("{key}/{p}")),
            ..n
        })
        .collect();
    Ok(CellOutput {
        fragment,
        assets: raw.assets,
    })
}
