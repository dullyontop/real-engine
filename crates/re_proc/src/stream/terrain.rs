//! The level-of-detail terrain of streamed sources (ADR-0042): the field
//! reaching a source's `terrain.chunks` node, evaluated per chunk of a
//! quadtree around the focuses on the task pool and meshed with stitched
//! seams. The chunks are view data, not simulation state: they live in the
//! [`TerrainView`] resource, are swapped in as soon as they are ready, and
//! are not part of `state_hash`. Before a screenshot,
//! [`Streamer::complete_view`](super::Streamer::complete_view) waits for every
//! chunk instead, so the image does not depend on how fast they were made.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use glam::Vec3;
use re_core::time::Instant;
use re_core::{Diagnostic, Location, TaskPool};
use re_geom::terrain::{CHUNK_SAMPLES, ChunkKey, Edges, TerrainGrid, chunk_mesh, select};
use re_scene::World;
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::entity::Entity;
use re_scene::ecs::resource::Resource;
use re_types::mesh::MeshData;

use super::source::{Source, TerrainSource};
use crate::{Domain, EvalOptions, Evaluator, GraphStore, Output};

/// A chunk splits while a focus is nearer than this many times its size.
const SPLIT: f32 = 1.5;

/// Wall time per tick for meshing shown chunks again whose stitched edges
/// changed (view data, so a time budget is allowed).
const RESTITCH_BUDGET_MS: f64 = 2.0;

const FIRST: ChunkKey = ChunkKey {
    level: 0,
    x: i32::MIN,
    z: i32::MIN,
};
const LAST: ChunkKey = ChunkKey {
    level: u8::MAX,
    x: i32::MAX,
    z: i32::MAX,
};

/// One chunk being drawn.
#[derive(Clone, Debug)]
pub struct ViewChunk {
    /// The streamed source whose terrain it is (draw it with the source's
    /// id, so picking selects the source).
    pub source: Entity,
    /// Its level, and chunk coordinates on that level's grid.
    pub key: ChunkKey,
    /// The edges stitched to a coarser neighbour.
    pub edges: Edges,
    /// The mesh's `derived:` name, from the source, the chunk, its stitching
    /// and the source's inputs: another name whenever the mesh differs, and
    /// the same name for the same mesh in every world (a play copy draws
    /// with the live app's GPU side).
    pub mesh: String,
    /// The material file, or empty.
    pub material: String,
    /// Where the chunk's corner (its sample `(0, 0)`) lies in the world:
    /// draw the mesh there.
    pub corner: Vec3,
    data: Arc<MeshData>,
}

impl ViewChunk {
    /// The chunk's mesh, relative to [`corner`](Self::corner).
    pub fn data(&self) -> &Arc<MeshData> {
        &self.data
    }
}

/// The terrain chunks to draw, by source id and chunk. Kept up to date by
/// [`step`](super::step) while the streamer's view is on
/// ([`Streamer::set_view`](super::Streamer::set_view)).
#[derive(Resource, Clone, Debug, Default)]
pub struct TerrainView {
    chunks: BTreeMap<(String, ChunkKey), ViewChunk>,
}

impl TerrainView {
    /// The chunks to draw, by source id then chunk.
    pub fn chunks(&self) -> impl Iterator<Item = &ViewChunk> {
        self.chunks.values()
    }

    /// The number of chunks drawn.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// True when nothing is drawn.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// The mesh named `name`, if a drawn chunk uses it.
    pub fn mesh(&self, name: &str) -> Option<&Arc<MeshData>> {
        self.chunks
            .values()
            .find(|c| c.mesh == name)
            .map(|c| &c.data)
    }

    /// The bytes of mesh data drawn.
    pub fn bytes(&self) -> u64 {
        self.chunks.values().map(|c| mesh_bytes(&c.data)).sum()
    }

    fn of<'a>(&'a self, id: &'a str) -> impl Iterator<Item = (&'a ChunkKey, &'a ViewChunk)> + 'a {
        self.chunks
            .range((id.to_string(), FIRST)..=(id.to_string(), LAST))
            .map(|((_, k), c)| (k, c))
    }
}

fn mesh_bytes(m: &MeshData) -> u64 {
    (m.vertices.len() * std::mem::size_of::<re_types::mesh::Vertex>()
        + m.indices.len() * std::mem::size_of::<u32>()) as u64
}

/// A finished chunk: its samples (with the one-sample apron) and its mesh
/// with the edges it was built for.
type ChunkResult = Result<(Arc<Vec<f32>>, Edges, Arc<MeshData>), Diagnostic>;

/// Where a job leaves its chunk, and how the main thread waits for it.
type Slot = Arc<(Mutex<Option<ChunkResult>>, Condvar)>;

struct ChunkJob {
    cancel: Arc<AtomicBool>,
    slot: Slot,
}

impl ChunkJob {
    /// The job's chunk once it is done (waiting for it if `wait`), else
    /// `None`.
    fn take(&self, wait: bool) -> Option<ChunkResult> {
        let (lock, done) = &*self.slot;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        while wait && guard.is_none() {
            guard = done.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
        guard.take()
    }
}

impl Drop for ChunkJob {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for ChunkJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkJob").finish_non_exhaustive()
    }
}

/// A generated chunk not drawn yet, or drawn.
#[derive(Debug)]
struct Ready {
    heights: Arc<Vec<f32>>,
    edges: Edges,
    mesh: Arc<MeshData>,
}

/// One source's chunks in the making.
#[derive(Debug, Default)]
struct SourceTerrain {
    inputs: Option<blake3::Hash>,
    ready: BTreeMap<ChunkKey, Ready>,
    jobs: BTreeMap<ChunkKey, ChunkJob>,
    /// Drawn chunks built from earlier inputs, replaced as their new
    /// versions come in.
    stale: BTreeSet<ChunkKey>,
    /// Selected chunks whose generation failed with these inputs: left bare,
    /// and not tried again while they stay selected with these inputs (they
    /// would fail the same way).
    failed: BTreeSet<ChunkKey>,
}

/// How much one [`TerrainState::step`] does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pace {
    /// Starts at most this many jobs, swaps in the chunks that are ready, and
    /// meshes drawn chunks whose stitching changed within
    /// [`RESTITCH_BUDGET_MS`].
    Jobs(usize),
    /// Generates every selected chunk, waiting for each, and meshes every
    /// drawn chunk whose stitching changed: afterwards the view is the
    /// selection, with the ground of any chunk that failed left bare.
    Complete,
}

/// The terrain state of a [`Streamer`](super::Streamer): chunks generated
/// and in flight, by source id.
#[derive(Debug, Default)]
pub(crate) struct TerrainState {
    sources: BTreeMap<String, SourceTerrain>,
    /// Selected chunks not drawn as selected yet, after the last step.
    missing: usize,
}

impl TerrainState {
    /// Generation jobs in flight.
    pub fn in_flight(&self) -> usize {
        self.sources.values().map(|s| s.jobs.len()).sum()
    }

    /// True when no source had terrain at the last step: nothing is drawn or
    /// in the making.
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Selected chunks not drawn as selected yet: not drawn, drawn from
    /// earlier inputs, or drawn with an old stitching. Zero once the terrain
    /// within the horizon is all shown as selected.
    pub fn missing(&self) -> usize {
        self.missing
    }

    /// Brings `world`'s [`TerrainView`] up to date for `focuses` (world
    /// positions) at `pace`: takes finished jobs in, starts jobs for missing
    /// chunks and swaps ready ones in. Returns the chunks that failed, as
    /// problems to report once per subject.
    pub fn step(
        &mut self,
        world: &mut World,
        sources: &[Source],
        focuses: &[Vec3],
        view_radius: f32,
        pool: &TaskPool,
        pace: Pace,
    ) -> Vec<(String, Diagnostic)> {
        let mut view = world
            .ecs
            .remove_resource::<TerrainView>()
            .unwrap_or_default();
        let terrains: Vec<(&Source, &Arc<TerrainSource>)> = sources
            .iter()
            .filter_map(|s| s.terrain.as_ref().map(|t| (s, t)))
            .collect();
        // Sources that stopped having terrain are dropped with their chunks.
        self.sources
            .retain(|id, _| terrains.iter().any(|(s, _)| s.id == *id));
        view.chunks
            .retain(|(id, _), _| terrains.iter().any(|(s, _)| s.id == *id));
        let (mut free, wait) = match pace {
            Pace::Jobs(n) => (n, false),
            Pace::Complete => (usize::MAX, true),
        };
        self.missing = 0;
        let mut failures = Vec::new();
        let started = Instant::now();
        let store = world.ecs.get_resource::<GraphStore>();
        // Every source's jobs start before any is waited for.
        let mut selected = Vec::new();
        for (source, terrain) in terrains {
            let st = self.sources.entry(source.id.clone()).or_default();
            if st.inputs != Some(source.inputs) {
                // New inputs: everything is generated again; what is drawn
                // stays until its replacement is ready.
                st.inputs = Some(source.inputs);
                st.ready.clear();
                st.jobs.clear();
                st.failed.clear();
                st.stale = view.of(&source.id).map(|(k, _)| *k).collect();
            }
            let grid = TerrainGrid {
                base_spacing: source.spacing,
                max_level: terrain.max_level,
            };
            let desired: BTreeMap<ChunkKey, Edges> = if focuses.is_empty() || view_radius <= 0.0 {
                BTreeMap::new()
            } else {
                select(&grid, focuses, view_radius, SPLIT)
                    .into_iter()
                    .filter(|(k, _)| covers(source, &grid, *k))
                    .collect()
            };
            let shown: Vec<ChunkKey> = view.of(&source.id).map(|(k, _)| *k).collect();
            st.jobs.retain(|k, _| desired.contains_key(k));
            st.ready
                .retain(|k, _| desired.contains_key(k) || shown.contains(k));
            st.failed.retain(|k| desired.contains_key(k));
            take_finished(st, source, false, &mut failures);
            // New jobs: nearest chunks first.
            if let Some(store) = store {
                let mut missing: Vec<(f32, ChunkKey)> = desired
                    .keys()
                    .filter(|k| {
                        !st.ready.contains_key(*k)
                            && !st.jobs.contains_key(*k)
                            && !st.failed.contains(*k)
                    })
                    .map(|k| {
                        let d = focuses
                            .iter()
                            .map(|p| grid.distance(*k, *p))
                            .fold(f32::MAX, f32::min);
                        (d, *k)
                    })
                    .collect();
                missing.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
                for (_, k) in missing.into_iter().take(free) {
                    let job = issue(
                        pool,
                        store.evaluator().clone(),
                        terrain.clone(),
                        EvalOptions {
                            seed: Some(source.seed),
                            root: store.root().map(Into::into),
                            files: store.files().cloned(),
                            serial: true,
                            // Roads drape and flatten at the cells' spacing
                            // at every level of detail.
                            global_spacing: Some(source.spacing),
                            ..Default::default()
                        },
                        grid,
                        k,
                        desired[&k],
                    );
                    st.jobs.insert(k, job);
                    free -= 1;
                }
            }
            selected.push((source, terrain, grid, desired));
        }
        for (source, terrain, grid, desired) in selected {
            let st = self
                .sources
                .get_mut(&source.id)
                .expect("every selected source has its state");
            if wait {
                take_finished(st, source, true, &mut failures);
            }
            let budget = (!wait).then_some(started);
            swap(&mut view, st, source, terrain, &grid, &desired, budget);
            self.missing += missing(&view, st, &source.id, &desired);
        }
        world.ecs.insert_resource(view);
        failures
    }
}

/// The chunks of `desired` that source `id` does not draw as selected: not
/// drawn, drawn from earlier inputs, or drawn with another stitching.
fn missing(
    view: &TerrainView,
    st: &SourceTerrain,
    id: &str,
    desired: &BTreeMap<ChunkKey, Edges>,
) -> usize {
    desired
        .iter()
        .filter(|(k, edges)| {
            st.stale.contains(*k)
                || view
                    .chunks
                    .get(&(id.to_string(), **k))
                    .is_none_or(|c| c.edges != **edges)
        })
        .count()
}

/// Takes the finished jobs of `source` in (every job, waiting for it, if
/// `wait`): ready chunks, and failed ones with the problem to report, once
/// per code of its cause and the source's inputs (chunks over a field that
/// is not finite fail at other samples, and one report stands for them all).
fn take_finished(
    st: &mut SourceTerrain,
    source: &Source,
    wait: bool,
    failures: &mut Vec<(String, Diagnostic)>,
) {
    let done: Vec<(ChunkKey, ChunkResult)> = st
        .jobs
        .iter()
        .filter_map(|(k, j)| j.take(wait).map(|r| (*k, r)))
        .collect();
    for (k, result) in done {
        st.jobs.remove(&k);
        match result {
            Ok((heights, edges, mesh)) => {
                st.ready.insert(
                    k,
                    Ready {
                        heights,
                        edges,
                        mesh,
                    },
                );
            }
            Err(d) => {
                st.failed.insert(k);
                failures.push((
                    format!("{}/view/{}/{}", source.id, source.inputs, d.code),
                    Diagnostic::error(
                        crate::codes::CELL_FAILED,
                        format!(
                            "{}: terrain view chunk {}/{}_{} failed; its ground is not drawn",
                            source.id, k.level, k.x, k.z
                        ),
                    )
                    .at(Location::entity(source.id.clone()))
                    .with_related(d),
                ));
            }
        }
    }
}

/// True if chunk `k` touches the source's square (all chunks when unbounded).
fn covers(source: &Source, grid: &TerrainGrid, k: ChunkKey) -> bool {
    if source.bounds <= 0.0 {
        return true;
    }
    let b = source.bounds;
    let (lo, hi) = grid.bounds(k);
    lo[0] < b && hi[0] > -b && lo[1] < b && hi[1] > -b
}

/// True if `a` and `b` cover some of the same ground: one is the other or
/// one of its ancestors.
fn overlaps(a: ChunkKey, b: ChunkKey) -> bool {
    let (fine, coarse) = if a.level <= b.level { (a, b) } else { (b, a) };
    let d = u32::from(coarse.level - fine.level);
    (fine.x >> d) == coarse.x && (fine.z >> d) == coarse.z
}

#[allow(clippy::too_many_arguments)]
fn issue(
    pool: &TaskPool,
    evaluator: Arc<Evaluator>,
    terrain: Arc<TerrainSource>,
    mut opts: EvalOptions,
    grid: TerrainGrid,
    key: ChunkKey,
    edges: Edges,
) -> ChunkJob {
    let cancel = Arc::new(AtomicBool::new(false));
    let slot: Slot = Arc::new((Mutex::new(None), Condvar::new()));
    let out = slot.clone();
    let origin = grid.origin_index(key);
    let spacing = grid.spacing(key.level);
    opts.domain = Some(Domain {
        // One sample of apron on every side, for normals.
        origin: [origin[0] - 1, origin[1] - 1],
        n: CHUNK_SAMPLES + 2,
        spacing,
    });
    opts.cancel = Some(cancel.clone());
    pool.spawn(move || {
        let result: ChunkResult = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let evaluation = evaluator.evaluate(&terrain.graph, &opts);
            let heights = match evaluation.output(&terrain.field) {
                Some(Output::Field(f)) => Arc::new(f.0.samples.clone()),
                _ => {
                    let why = evaluation.report.first_error().cloned().unwrap_or_else(|| {
                        Diagnostic::error(
                            crate::codes::CELL_FAILED,
                            "the terrain field has no output",
                        )
                    });
                    return Err(why);
                }
            };
            let mesh = chunk_mesh(&heights, spacing, edges)
                .map_err(|e| Diagnostic::error(crate::codes::CELL_FAILED, e.to_string()))?;
            Ok((heights, edges, Arc::new(mesh)))
        }))
        .unwrap_or_else(|_| {
            Err(Diagnostic::error(
                crate::codes::CELL_FAILED,
                "terrain chunk generation panicked",
            ))
        });
        let (lock, done) = &*out;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        done.notify_all();
    });
    ChunkJob { cancel, slot }
}

/// Swaps ready chunks in for `source`: a drawn chunk the selection no longer
/// has goes once every selected chunk covering its ground is ready, and those
/// come in at the same time, so no ground is ever drawn twice or left bare.
/// A failed chunk counts as ready and is left out: its ground is bare, not
/// kept under whatever happened to be drawn before. Drawn chunks whose
/// stitched edges changed are meshed again, within [`RESTITCH_BUDGET_MS`] of
/// `budget` (the step's start), or all of them without one.
#[allow(clippy::too_many_arguments)]
fn swap(
    view: &mut TerrainView,
    st: &mut SourceTerrain,
    source: &Source,
    terrain: &TerrainSource,
    grid: &TerrainGrid,
    desired: &BTreeMap<ChunkKey, Edges>,
    budget: Option<Instant>,
) {
    let id = source.id.clone();
    let mut shown: BTreeMap<ChunkKey, Edges> = view.of(&id).map(|(k, c)| (*k, c.edges)).collect();
    let mut add: Vec<ChunkKey> = Vec::new();
    let mut remove: Vec<ChunkKey> = Vec::new();
    let ready = |k: &ChunkKey, shown: &BTreeMap<ChunkKey, Edges>| {
        shown.contains_key(k) || st.ready.contains_key(k) || st.failed.contains(k)
    };
    for old in shown.keys().copied().collect::<Vec<_>>() {
        if desired.contains_key(&old) {
            continue;
        }
        let cover: Vec<ChunkKey> = desired
            .keys()
            .copied()
            .filter(|d| overlaps(*d, old))
            .collect();
        if cover.iter().all(|d| ready(d, &shown)) {
            remove.push(old);
            shown.remove(&old);
            for d in cover {
                if let std::collections::btree_map::Entry::Vacant(e) = shown.entry(d) {
                    e.insert(Edges::default());
                    add.push(d);
                }
            }
        }
    }
    for k in desired.keys() {
        if shown.contains_key(k) || !st.ready.contains_key(k) {
            continue;
        }
        if !shown.keys().any(|s| overlaps(*s, *k)) {
            shown.insert(*k, Edges::default());
            add.push(*k);
        }
    }
    for k in remove {
        view.chunks.remove(&(id.clone(), k));
        st.stale.remove(&k);
    }
    // Drawn chunks from earlier inputs: replaced by their new version once
    // it is ready (the inputs may have moved the chunk's corner or changed
    // its material too), or gone if it failed.
    for k in st.stale.iter().copied().collect::<Vec<_>>() {
        if !shown.contains_key(&k) {
            st.stale.remove(&k);
            continue;
        }
        if st.failed.contains(&k) {
            view.chunks.remove(&(id.clone(), k));
            st.stale.remove(&k);
            continue;
        }
        if let Some(r) = st.ready.get(&k) {
            if let Some(c) = view.chunks.get_mut(&(id.clone(), k)) {
                c.source = source.entity;
                c.edges = r.edges;
                c.mesh = mesh_name(source, k, r.edges);
                c.material = terrain.material.clone();
                c.corner = corner(grid, k);
                c.data = r.mesh.clone();
            }
            st.stale.remove(&k);
        }
    }
    for k in add {
        let Some(r) = st.ready.get(&k) else { continue };
        view.chunks.insert(
            (id.clone(), k),
            ViewChunk {
                source: source.entity,
                key: k,
                edges: r.edges,
                mesh: mesh_name(source, k, r.edges),
                material: terrain.material.clone(),
                corner: corner(grid, k),
                data: r.mesh.clone(),
            },
        );
    }
    // Drawn chunks whose stitching changed.
    for (k, want) in desired {
        let Some(c) = view.chunks.get_mut(&(id.clone(), *k)) else {
            continue;
        };
        if c.edges == *want {
            continue;
        }
        if budget
            .is_some_and(|started| started.elapsed().as_secs_f64() * 1000.0 > RESTITCH_BUDGET_MS)
        {
            break;
        }
        let Some(r) = st.ready.get_mut(k) else {
            continue;
        };
        if let Ok(mesh) = chunk_mesh(&r.heights, grid.spacing(k.level), *want) {
            r.edges = *want;
            r.mesh = Arc::new(mesh);
            c.edges = *want;
            c.data = r.mesh.clone();
            c.mesh = mesh_name(source, *k, *want);
        }
    }
}

/// Chunk `k`'s corner in the world.
fn corner(grid: &TerrainGrid, k: ChunkKey) -> Vec3 {
    let [x, z] = grid.origin_index(k);
    let s = grid.spacing(k.level);
    Vec3::new(x as f32 * s, 0.0, z as f32 * s)
}

/// The derived name of `source`'s chunk `k` mesh with `edges` stitched, for
/// its current inputs: the chunk's samples depend on nothing else, so one
/// name is one mesh in every world that draws it.
fn mesh_name(source: &Source, k: ChunkKey, edges: Edges) -> String {
    format!(
        "derived:terrain/{}/{}_{}_{}/{}/{}",
        source.id,
        k.level,
        k.x,
        k.z,
        edges.0,
        &source.inputs.to_hex()[..16]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_terrain_overlap_is_ancestry() {
        let k = |level, x, z| ChunkKey { level, x, z };
        assert!(overlaps(k(0, 5, -3), k(1, 2, -2)));
        assert!(overlaps(k(2, 0, -1), k(0, 3, -1)));
        assert!(!overlaps(k(0, 4, 0), k(1, 1, 0)));
        assert!(overlaps(k(1, 1, 1), k(1, 1, 1)));
        assert!(!overlaps(k(1, 1, 1), k(1, 1, 2)));
    }

    #[test]
    fn stream_terrain_missing_is_what_is_not_drawn_as_selected() {
        let k = |x| ChunkKey { level: 0, x, z: 0 };
        let chunk = |x, edges| ViewChunk {
            source: Entity::PLACEHOLDER,
            key: k(x),
            edges,
            mesh: String::new(),
            material: String::new(),
            corner: Vec3::ZERO,
            data: Arc::new(MeshData::default()),
        };
        let stitched = Edges(Edges::NEG_X);
        let mut view = TerrainView::default();
        // Another source's chunk draws none of this one's ground.
        view.chunks
            .insert(("other".into(), k(0)), chunk(0, Edges::default()));
        for x in 1..4 {
            view.chunks
                .insert(("land".into(), k(x)), chunk(x, Edges::default()));
        }
        let mut st = SourceTerrain::default();
        st.stale.insert(k(2));
        let desired: BTreeMap<ChunkKey, Edges> = [
            (k(0), Edges::default()),
            (k(1), Edges::default()),
            (k(2), Edges::default()),
            (k(3), stitched),
        ]
        .into();
        // Chunk 0 is not drawn, 2 is from earlier inputs and 3 has another
        // stitching; 1 is drawn as selected.
        assert_eq!(missing(&view, &st, "land", &desired), 3);
        st.stale.clear();
        view.chunks
            .insert(("land".into(), k(0)), chunk(0, Edges::default()));
        view.chunks.get_mut(&("land".into(), k(3))).unwrap().edges = stitched;
        assert_eq!(missing(&view, &st, "land", &desired), 0);
    }
}
