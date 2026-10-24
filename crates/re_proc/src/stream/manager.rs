//! The streaming manager: which cells are wanted, when they load and unload,
//! within which budgets, and what it costs in memory (ADR-0042).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use glam::{Affine3A, Vec2, Vec3};
use re_core::time::Instant;
use re_core::{Diagnostic, Location, TaskPool};
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::entity::Entity;
use re_scene::ecs::resource::Resource;
use re_scene::{
    Camera, Generated, Procedural, StreamCell, StreamFocus, Streamed, Transform, World, cell_key,
    hierarchy, parse_cell_key,
};

use super::StreamConfig;
use super::source::{self, CellJob, CellOutput, RawCell, Source};
use crate::value::DerivedAsset;
use crate::{GraphStore, codes};

/// The accounted size of one entity, in bytes (its components and indices).
pub const ENTITY_BYTES: u64 = 512;

/// The accounted size of one heightfield physics shape at 16 bits per sample
/// and block size 4, in bytes (measured: 10.8 KiB per 65² chunk).
pub const SHAPE_BYTES: u64 = 11_059;

/// The angle each side of a focus's forward within which cells load out to
/// `radius_ahead`: ±60°, as its cosine.
const AHEAD_COS: f32 = 0.5;

/// How far from the simulation origin, per axis, physics simulates (Jolt's
/// single-precision limit, `re_physics::WORLD_LIMIT`), in metres.
const PHYSICS_REACH: f32 = 5000.0;

/// How far from the world origin, per axis, f32 scene positions stay exact
/// to about 2 mm (`RE4309`), in metres.
const PRECISION_LIMIT: f32 = 16_000.0;

/// A point the world streams around, at one tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Focus {
    /// World X and Z.
    pub position: Vec2,
    /// The horizontal forward direction (normalised), or zero.
    pub forward: Vec2,
}

/// One loaded cell, as the manager tracks it.
#[derive(Clone, Debug)]
struct Loaded {
    cell: StreamCell,
    /// Accounted bytes.
    bytes: u64,
    /// Streamed entities it holds (generated and stashed-authored).
    entities: usize,
    /// The inputs of each source it was generated with.
    inputs: BTreeMap<String, blake3::Hash>,
    /// The sources it holds content of (unloaded even if they stop being
    /// valid streamed sources).
    sources: Vec<Entity>,
}

/// One source's content for a cell, or why it could not be generated.
/// (source id, the source's inputs it was generated with, the content).
type SourceOutput = (String, blake3::Hash, Result<CellOutput, Diagnostic>);

/// What one [`step`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepReport {
    /// The tick.
    pub tick: u64,
    /// Cells loaded at this tick, in key order.
    pub loaded: Vec<String>,
    /// Cells unloaded at this tick, in key order.
    pub unloaded: Vec<String>,
    /// Loaded cells generated again because a source's inputs changed.
    pub reloaded: Vec<String>,
    /// Requests whose tick came but that moved to a later tick (budget).
    pub deferred: usize,
    /// Streamed entities spawned or despawned at this tick.
    pub entities: usize,
    /// Cells loaded after this tick.
    pub resident: usize,
    /// Accounted streamed bytes after this tick.
    pub memory_bytes: u64,
    /// Cells requested and not yet applied.
    pub pending: usize,
    /// Generation jobs in flight after this tick.
    pub in_flight: usize,
    /// Results the tick waited for ([`Lateness::Wait`]); zero is the goal.
    pub waits: usize,
    /// Cells applied after their tick ([`Lateness::Late`]).
    pub late: usize,
    /// Problems found at this tick (each reported once per subject).
    pub diagnostics: Vec<Diagnostic>,
    /// Wall time spent applying unloads and loads (including any waits).
    pub apply_time: core::time::Duration,
    /// The simulation origin after this tick ([`Streamer::origin`]).
    pub origin: Vec3,
}

/// The cells loaded and unloaded at the last tick, for scripts
/// (`stream.on_loaded`, `stream.on_unloaded`): unloads first, each list in
/// key order. Replaced at every [`step`].
#[derive(Resource, Clone, Debug, Default, PartialEq)]
pub struct StreamEvents {
    /// The tick.
    pub tick: u64,
    /// Cells unloaded at this tick.
    pub unloaded: Vec<StreamCell>,
    /// Cells loaded at this tick.
    pub loaded: Vec<StreamCell>,
}

/// What a job hands back: for each source, its id, the inputs it ran with
/// and the raw result.
type JobOutput = Vec<(String, blake3::Hash, RawCell)>;

/// Where a job leaves its result, and how the main thread waits for it.
type Slot = Arc<(Mutex<Option<JobOutput>>, Condvar)>;

/// A cell being generated on the pool.
struct InFlight {
    cancel: Arc<AtomicBool>,
    slot: Slot,
    /// The covering sources and their inputs it was issued with.
    inputs: Vec<(String, blake3::Hash)>,
}

impl std::fmt::Debug for InFlight {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InFlight").finish_non_exhaustive()
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        // A job nobody will read stops at its next level of nodes.
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// How the manager treats a result that is not ready at its apply tick.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lateness {
    /// Wait for it and count the wait ([`StepReport::waits`]): `real
    /// verify`, the D2 harness and replays, where the loaded set must not
    /// depend on the machine.
    #[default]
    Wait,
    /// Apply it when it arrives (`RE4305`, once per cell): play, where no
    /// frame waits.
    Late,
}

/// The world-streaming state of one world (ADR-0042): the loaded cells, the
/// pending requests and their apply ticks, the generation jobs in flight,
/// and the accounting. A world resource; [`step`] advances it once per
/// fixed tick.
#[derive(Resource, Debug)]
pub struct Streamer {
    config: StreamConfig,
    loaded: BTreeMap<String, Loaded>,
    /// Requested cells and the tick they apply at.
    pending: BTreeMap<String, u64>,
    /// Generated content waiting for its apply tick or its turn in the budget.
    ready: BTreeMap<String, Vec<SourceOutput>>,
    /// Cells being generated on the pool.
    jobs: BTreeMap<String, InFlight>,
    /// The pool jobs run on (the global pool when `None`).
    pool: Option<Arc<TaskPool>>,
    lateness: Lateness,
    /// The load radius after memory pressure (starts at `radius`).
    radius: f32,
    /// Subjects already reported, by code.
    reported: BTreeSet<(String, String)>,
    /// Cells loaded by hand (`stream.load`), kept until released.
    pinned: BTreeSet<String>,
    /// The sources as last prepared.
    described: source::DescribeCache,
    /// True while the terrain view is kept up to date.
    view: bool,
    terrain: super::terrain::TerrainState,
    /// Terrain collision chunks (simulation state).
    collision: super::collision::CollisionState,
    /// The floating origin (simulation state).
    origin: Vec3,
}

impl Streamer {
    /// A manager with `config` (checked: `RE4300`).
    pub fn new(config: StreamConfig) -> Result<Streamer, Diagnostic> {
        config.validate()?;
        Ok(Streamer {
            radius: config.radius,
            config,
            loaded: BTreeMap::new(),
            pending: BTreeMap::new(),
            ready: BTreeMap::new(),
            jobs: BTreeMap::new(),
            pool: None,
            lateness: Lateness::default(),
            reported: BTreeSet::new(),
            pinned: BTreeSet::new(),
            described: source::DescribeCache::new(),
            view: false,
            terrain: Default::default(),
            collision: Default::default(),
            origin: Vec3::ZERO,
        })
    }

    /// Runs generation jobs on `pool` instead of the global pool.
    #[must_use]
    pub fn with_pool(mut self, pool: Arc<TaskPool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Sets how a result that is late for its apply tick is treated.
    pub fn set_lateness(&mut self, lateness: Lateness) {
        self.lateness = lateness;
    }

    /// How a result that is late for its apply tick is treated.
    pub fn lateness(&self) -> Lateness {
        self.lateness
    }

    /// Keeps the [`TerrainView`](super::TerrainView) of the world's terrain
    /// sources up to date while `on` (off by default: only something that
    /// draws needs it). The view is not simulation state.
    pub fn set_view(&mut self, on: bool) {
        self.view = on;
    }

    /// True while the terrain view is kept up to date.
    pub fn view(&self) -> bool {
        self.view
    }

    fn pool(&self) -> &TaskPool {
        self.pool.as_deref().unwrap_or_else(|| TaskPool::global())
    }

    /// Generation jobs in flight.
    pub fn in_flight(&self) -> usize {
        self.jobs.len() + self.terrain.in_flight() + self.collision.in_flight()
    }

    /// Terrain chunks the view has selected but not drawn yet, after the
    /// last step (zero with the view off).
    pub fn view_missing(&self) -> usize {
        self.terrain.missing()
    }

    /// The settings.
    pub fn config(&self) -> &StreamConfig {
        &self.config
    }

    /// True when nothing is loaded, requested or pinned: a world without
    /// streamed sources can skip [`step`] while this holds.
    pub fn is_idle(&self) -> bool {
        self.loaded.is_empty()
            && self.pending.is_empty()
            && self.pinned.is_empty()
            && self.collision.len() == 0
            && self.collision.pending() == 0
    }

    /// The loaded cells, in key order.
    pub fn cells(&self) -> impl Iterator<Item = &StreamCell> {
        self.loaded.values().map(|l| &l.cell)
    }

    /// True if cell `key` is loaded.
    pub fn is_loaded(&self, key: &str) -> bool {
        self.loaded.contains_key(key)
    }

    /// The tick a pending cell applies at, if it is pending.
    pub fn pending_at(&self, key: &str) -> Option<u64> {
        self.pending.get(key).copied()
    }

    /// Accounted streamed bytes.
    pub fn memory_bytes(&self) -> u64 {
        self.loaded.values().map(|l| l.bytes).sum::<u64>() + self.collision.bytes()
    }

    /// The floating origin (ADR-0042): a point of the `origin_grid` that
    /// follows the focuses, where physics should put its simulation origin
    /// before the tick's physics commands. It depends only on the focuses'
    /// history, so a replay moves it at the same ticks.
    pub fn origin(&self) -> Vec3 {
        self.origin
    }

    /// Moves the origin when a focus is more than `origin_shift` from it on
    /// X or Z, to the grid point nearest the focuses' mean; reports `RE4308`
    /// for focuses then beyond physics' reach.
    fn follow(&mut self, focuses: &[(String, Focus)], out: &mut Vec<Diagnostic>) {
        if focuses.is_empty() {
            return;
        }
        let at = Vec2::new(self.origin.x, self.origin.z);
        let shift = self.config.origin_shift;
        let off = |f: &Focus| (f.position - at).abs().max_element();
        if focuses.iter().any(|(_, f)| off(f) > shift) {
            let mean = focuses.iter().map(|(_, f)| f.position).sum::<Vec2>() / focuses.len() as f32;
            let grid = self.config.origin_grid;
            let o = (mean / grid).round() * grid;
            self.origin = Vec3::new(o.x, 0.0, o.y);
        }
        let at = Vec2::new(self.origin.x, self.origin.z);
        for (id, f) in focuses {
            let d = (f.position - at).abs().max_element();
            if d > PHYSICS_REACH {
                let o = self.origin;
                let diag = Diagnostic::warning(
                    codes::ORIGIN_RANGE,
                    format!(
                        "focus `{id}` is {:.1} km from the origin [{}, {}, {}]; physics range is {} km",
                        d / 1000.0,
                        o.x,
                        o.y,
                        o.z,
                        PHYSICS_REACH / 1000.0
                    ),
                )
                .with_hint("keep the focuses closer together, or remove StreamFocus from the far one");
                self.report_once(out, &format!("origin {id}"), diag);
            }
        }
    }

    /// The current load radius (smaller than `radius` under memory pressure).
    pub fn radius(&self) -> f32 {
        self.radius
    }

    /// This manager for `to`, a fork of `from` ([`World::fork`]): the same
    /// state, with the source entities it holds content of found again in
    /// `to` by id (a fork gives every entity a new handle).
    ///
    /// Jobs in flight are not shared: the fork generates its pending cells
    /// again, with the same results. It waits for late results
    /// ([`Lateness::Wait`]) until told otherwise.
    pub fn forked(&self, from: &World, to: &World) -> Streamer {
        let mut loaded = self.loaded.clone();
        for l in loaded.values_mut() {
            l.sources = l
                .sources
                .iter()
                .filter_map(|e| to.entity(from.id_of(*e)?).ok())
                .collect();
        }
        Streamer {
            config: self.config.clone(),
            loaded,
            pending: self.pending.clone(),
            ready: self.ready.clone(),
            jobs: BTreeMap::new(),
            pool: self.pool.clone(),
            lateness: Lateness::Wait,
            radius: self.radius,
            reported: self.reported.clone(),
            pinned: self.pinned.clone(),
            described: source::DescribeCache::new(),
            view: self.view,
            terrain: Default::default(),
            collision: self.collision.clone(),
            origin: self.origin,
        }
    }

    /// Keeps cell `key` wanted whatever the focuses (the editor and tools'
    /// `stream.load`); it loads at the next step's request tick.
    pub fn pin(&mut self, key: &str) -> bool {
        parse_cell_key(key).is_some() && self.pinned.insert(key.to_string())
    }

    /// Releases a pinned cell; it unloads if no focus wants it.
    pub fn unpin(&mut self, key: &str) -> bool {
        self.pinned.remove(key)
    }

    /// `RE4304` if the entity with id `id` lies in a cell that is not
    /// loaded: generated content of a streamed source, or a stashed authored
    /// entity.
    pub fn check_loaded(&self, world: &World, id: &str) -> Result<(), Diagnostic> {
        let stashed = world
            .stashed()
            .and_then(|s| s.cell_of(id))
            .map(str::to_string);
        let generated = id
            .split_once('/')
            .and_then(|(_, key)| re_scene::stream::owning_cell(key))
            .filter(|_| world.ids().get(id).is_none())
            .map(str::to_string);
        match stashed.or(generated) {
            Some(cell) if !self.is_loaded(&cell) => Err(Diagnostic::error(
                codes::IN_UNLOADED_CELL,
                format!("`{id}` is in cell {cell}, which is not loaded"),
            )
            .at(Location::entity(id.to_string()))
            .with_hint("wait for `stream.on_loaded`, or load it with `stream.load`")),
            _ => Ok(()),
        }
    }

    fn report_once(&mut self, out: &mut Vec<Diagnostic>, subject: &str, d: Diagnostic) {
        if self
            .reported
            .insert((d.code.to_string(), subject.to_string()))
        {
            out.push(d);
        }
    }
}

/// The focus points: entities with [`StreamFocus`], in id order; with none,
/// the active camera.
pub fn focuses(world: &mut World) -> Vec<Focus> {
    named_focuses(world).into_iter().map(|(_, f)| f).collect()
}

/// [`focuses`] with their entity ids.
fn named_focuses(world: &mut World) -> Vec<(String, Focus)> {
    let mut out: Vec<(String, Focus)> = Vec::new();
    let mut q = world.ecs.query::<(Entity, &StreamFocus)>();
    for (e, _) in q.iter(&world.ecs) {
        if let Some(id) = world.id_of(e) {
            out.push((id.to_string(), focus_of(world, e)));
        }
    }
    if out.is_empty() {
        let mut q = world.ecs.query::<(Entity, &Camera)>();
        for (e, c) in q.iter(&world.ecs) {
            if c.active
                && let Some(id) = world.id_of(e)
            {
                out.push((id.to_string(), focus_of(world, e)));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out.truncate(1);
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// The world transform of `e`, from the `Transform`s of it and its parents
/// (not `GlobalTransform`, which may lag a tick).
fn world_affine(world: &World, e: Entity) -> Affine3A {
    let mut m = Affine3A::IDENTITY;
    let mut cur = Some(e);
    while let Some(c) = cur {
        if let Some(t) = world.ecs.get::<Transform>(c) {
            m = t.affine() * m;
        }
        cur = hierarchy::parent(&world.ecs, c);
    }
    m
}

fn focus_of(world: &World, e: Entity) -> Focus {
    let m = world_affine(world, e);
    let p = Vec3::from(m.translation);
    let f = m.transform_vector3(Vec3::NEG_Z);
    let forward = Vec2::new(f.x, f.z);
    Focus {
        position: Vec2::new(p.x, p.z),
        forward: if forward.length_squared() > 1.0e-12 {
            forward.normalize()
        } else {
            Vec2::ZERO
        },
    }
}

/// The cell holding world position `(x, z)`.
pub fn cell_at(x: f32, z: f32, size: f32) -> [i32; 2] {
    // `floor` is exact (IEEE 754), unlike the transcendental functions.
    [(x / size).floor() as i32, (z / size).floor() as i32]
}

/// The distance from `p` to the square of cell `c`, and the direction from
/// `p` to the nearest point of it.
fn to_cell(p: Vec2, c: [i32; 2], size: f32) -> (f32, Vec2) {
    let lo = Vec2::new(c[0] as f32 * size, c[1] as f32 * size);
    let hi = lo + Vec2::splat(size);
    let q = p.clamp(lo, hi);
    let d = q - p;
    (d.length(), d)
}

impl Streamer {
    /// Cells within `reach` of any focus, intersected with `wanted`.
    fn near(
        &self,
        focuses: &[Focus],
        reach: f32,
        wanted: impl Fn([i32; 2]) -> bool,
    ) -> BTreeSet<String> {
        let size = self.config.cell_size;
        let mut out = BTreeSet::new();
        for f in focuses {
            let r = (reach / size) as i32 + 1;
            let [cx, cz] = cell_at(f.position.x, f.position.y, size);
            for z in cz - r..=cz + r {
                for x in cx - r..=cx + r {
                    let (d, _) = to_cell(f.position, [x, z], size);
                    if d <= reach && wanted([x, z]) {
                        out.insert(cell_key(x, z));
                    }
                }
            }
        }
        out
    }

    /// The cells the focuses want now: within the load radius, or within
    /// `radius_ahead` and ±60° of a focus's forward.
    fn desired(
        &self,
        focuses: &[Focus],
        has_content: &impl Fn([i32; 2]) -> bool,
    ) -> BTreeSet<String> {
        let size = self.config.cell_size;
        let mut out = self.near(focuses, self.radius, has_content);
        // Memory pressure shrinks the ahead reach by as much as the radius.
        let reach = self.config.radius_ahead - (self.config.radius - self.radius);
        let ahead = self.near(focuses, reach, |c| {
            has_content(c)
                && focuses.iter().any(|f| {
                    let (d, dir) = to_cell(f.position, c, size);
                    d <= reach
                        && f.forward != Vec2::ZERO
                        && (d == 0.0 || dir.dot(f.forward) >= AHEAD_COS * d)
                })
        });
        out.extend(ahead);
        out.extend(self.pinned.iter().cloned());
        out
    }

    /// The distance from cell `key` to the nearest focus.
    fn nearest(&self, focuses: &[Focus], key: &str) -> f32 {
        let c = parse_cell_key(key).unwrap_or_default();
        focuses
            .iter()
            .map(|f| to_cell(f.position, c, self.config.cell_size).0)
            .fold(f32::INFINITY, f32::min)
    }
}

/// The authored [`Streamed`] roots: no parent, not generated, not a
/// procedural source; with the cell holding each one's translation.
fn authored_roots(world: &mut World, size: f32) -> Vec<(String, Entity, String)> {
    let mut out = Vec::new();
    let mut q = world.ecs.query::<(Entity, &Streamed)>();
    for (e, _) in q.iter(&world.ecs) {
        if world.ecs.get::<Procedural>(e).is_some()
            || world.ecs.get::<Generated>(e).is_some()
            || hierarchy::parent(&world.ecs, e).is_some()
        {
            continue;
        }
        let Some(id) = world.id_of(e) else { continue };
        let t = world
            .ecs
            .get::<Transform>(e)
            .map_or(Vec3::ZERO, |t| t.translation);
        let [x, z] = cell_at(t.x, t.z, size);
        out.push((id.to_string(), e, cell_key(x, z)));
    }
    out.sort();
    out
}

/// The streamed procedural sources, in id order.
fn source_entities(world: &mut World) -> Vec<(String, Entity)> {
    let mut out = Vec::new();
    let mut q = world.ecs.query::<(Entity, &Streamed, &Procedural)>();
    for (e, _, _) in q.iter(&world.ecs) {
        if let Some(id) = world.id_of(e) {
            out.push((id.to_string(), e));
        }
    }
    out.sort();
    out
}

fn asset_bytes(asset: &DerivedAsset) -> u64 {
    match asset {
        DerivedAsset::Mesh(m) => {
            let v = (m.vertices.len() * std::mem::size_of::<re_types::mesh::Vertex>()) as u64;
            let i = (m.indices.len() * 4) as u64;
            // The CPU copy and the GPU copy.
            2 * (v + i)
        }
        DerivedAsset::Heightfield(h) => (h.samples.len() * 4) as u64 + SHAPE_BYTES,
    }
}

/// Advances `world`'s streaming by one fixed tick (ADR-0042), with the
/// [`Streamer`] resource (inserted with `config` if missing):
///
/// 1. the focuses give the desired cells; new ones are requested for tick
///    `tick + delay_ticks`, requests no longer desired are cancelled, and
///    loaded cells farther than `radius_ahead + unload_margin` from every
///    focus are unloaded (their authored [`Streamed`] roots stashed);
/// 2. requests whose tick has come are generated and applied in key order,
///    unloads first, within `apply_entities` (at least one cell per tick);
/// 3. over `memory_mb`, the loaded cells farthest from every focus and
///    outside `radius` unload first, then the radius shrinks (`RE4306`);
/// 4. [`StreamEvents`] holds the tick's unloaded and loaded cells.
///
/// Everything depends only on the world and `tick`, so a replay loads the
/// same cells at the same ticks on any machine.
pub fn step(world: &mut World, tick: u64, config: &StreamConfig) -> StepReport {
    let mut streamer = match world.ecs.remove_resource::<Streamer>() {
        Some(s) => s,
        None => match Streamer::new(config.clone()) {
            Ok(s) => s,
            Err(d) => {
                return StepReport {
                    tick,
                    diagnostics: vec![d],
                    ..Default::default()
                };
            }
        },
    };
    let report = streamer.step(world, tick);
    world.ecs.insert_resource(streamer);
    report
}

impl Streamer {
    /// [`step`] on this manager.
    pub fn step(&mut self, world: &mut World, tick: u64) -> StepReport {
        let mut report = StepReport {
            tick,
            ..Default::default()
        };
        let size = self.config.cell_size;

        // The sources, checked.
        let mut sources: Vec<Source> = Vec::new();
        let entities = source_entities(world);
        self.described
            .retain(|e, _| entities.iter().any(|(_, s)| s == e));
        for (id, e) in entities {
            match source::describe(world, e, &self.config, &mut self.described) {
                Ok(s) => sources.push(s),
                Err(d) => self.report_once(&mut report.diagnostics, &id, d),
            }
        }
        let roots = authored_roots(world, size);
        let mut authored_cells: BTreeSet<String> = roots.iter().map(|r| r.2.clone()).collect();
        if let Some(stash) = world.stashed() {
            authored_cells.extend(stash.iter().map(|(k, _)| k.to_string()));
        }
        let has_content = |c: [i32; 2]| {
            sources.iter().any(|s| s.covers(c, size))
                || authored_cells.contains(&cell_key(c[0], c[1]))
        };

        // 0. The floating origin follows the focuses.
        let named = named_focuses(world);
        self.follow(&named, &mut report.diagnostics);
        let focuses: Vec<Focus> = named.into_iter().map(|(_, f)| f).collect();

        // 1. Requests.
        let desired = self.desired(&focuses, &has_content);
        let keep = self.near(
            &focuses,
            self.config.radius_ahead + self.config.unload_margin,
            has_content,
        );
        // Loaded cells a source's changed inputs make stale; regenerated while
        // they are kept, wanted or not.
        let stale: BTreeSet<String> = self
            .loaded
            .iter()
            .filter(|(k, l)| {
                let covers = |s: &Source| parse_cell_key(k).is_some_and(|c| s.covers(c, size));
                // Kept: near a focus, pinned, or with no focus at all.
                (keep.contains(*k) || self.pinned.contains(*k) || focuses.is_empty())
                    && (sources
                        .iter()
                        .any(|s| covers(s) && l.inputs.get(&s.id) != Some(&s.inputs))
                        // A source that held content here no longer covers
                        // the cell or is no longer valid.
                        || l.sources.iter().any(|e| {
                            !sources.iter().any(|s| s.entity == *e && covers(s))
                        }))
            })
            .map(|(k, _)| k.clone())
            .collect();
        self.pending
            .retain(|k, _| desired.contains(k) || stale.contains(k));
        self.ready
            .retain(|k, _| desired.contains(k) || stale.contains(k));
        for k in &desired {
            if let Some(c) = parse_cell_key(k) {
                let far = ((Vec2::new(c[0] as f32, c[1] as f32) + 0.5) * size)
                    .abs()
                    .max_element();
                if far > PRECISION_LIMIT && !self.loaded.contains_key(k) {
                    let d = Diagnostic::warning(
                        codes::PRECISION_LIMIT,
                        format!(
                            "cell {k} lies {:.1} km from the world origin, beyond the {} km precision limit",
                            far / 1000.0,
                            PRECISION_LIMIT / 1000.0
                        ),
                    )
                    .with_hint("keep the world within 16 km of its origin, e.g. with Streamed.bounds");
                    self.report_once(&mut report.diagnostics, k, d);
                }
            }
        }
        for k in desired.iter().chain(&stale) {
            if (!self.loaded.contains_key(k) || stale.contains(k)) && !self.pending.contains_key(k)
            {
                self.pending
                    .insert(k.clone(), tick + u64::from(self.config.delay_ticks));
            }
        }
        let mut unloads: Vec<String> = self
            .loaded
            .keys()
            .filter(|k| !keep.contains(*k) && !self.pinned.contains(*k) && !focuses.is_empty())
            .cloned()
            .collect();

        // Jobs for cells no longer requested stop; finished ones are taken in,
        // and results whose inputs changed meanwhile are dropped.
        let pending = &self.pending;
        self.jobs.retain(|k, _| pending.contains_key(k));
        self.collect(world, &sources);

        // Authored roots outside every loaded or pending cell are stashed.
        for (id, e, cell) in &roots {
            if !self.loaded.contains_key(cell) && !self.pending.contains_key(cell) {
                self.stash_root(world, *e, id, cell, &mut report);
            }
        }

        // 3 (before applying): memory. Evict the farthest cells no focus
        // wants (kept only by the margin) until the loaded data fits; if
        // that is not enough, shrink the radius.
        let budget = self.config.memory_bytes();
        let mut total: u64 = self
            .loaded
            .iter()
            .filter(|(k, _)| !unloads.contains(*k))
            .map(|(_, l)| l.bytes)
            .sum();
        if total > budget {
            let mut far: Vec<(f32, String)> = self
                .loaded
                .keys()
                .filter(|k| {
                    !unloads.contains(*k) && !self.pinned.contains(*k) && !desired.contains(*k)
                })
                .map(|k| (self.nearest(&focuses, k), k.clone()))
                .collect();
            far.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            for (_, k) in far {
                if total <= budget {
                    break;
                }
                total -= self.loaded[&k].bytes;
                // An evicted stale cell is not generated again.
                self.pending.remove(&k);
                self.ready.remove(&k);
                unloads.push(k);
            }
            if total > budget {
                let shrunk = (self.radius - size).max(0.0);
                let d = Diagnostic::warning(
                    codes::STREAM_MEMORY,
                    format!(
                        "streamed data needs {} MiB within radius {} m, over memory_mb {}; the load radius shrinks to {shrunk} m",
                        total >> 20,
                        self.radius,
                        self.config.memory_mb
                    ),
                )
                .with_hint("raise [stream] memory_mb, lower radius, or generate less per cell");
                self.report_once(&mut report.diagnostics, "memory", d);
                self.radius = shrunk;
            }
        } else if self.radius < self.config.radius && total <= budget / 4 * 3 {
            // Room again: the radius grows back a cell at a time.
            self.radius = (self.radius + size).min(self.config.radius);
        }
        unloads.sort();
        unloads.dedup();

        // 2. Apply: unloads, then due loads, in key order, within the budget.
        let limit = self.config.apply_entities as usize;
        let mut events = StreamEvents {
            tick,
            ..Default::default()
        };
        let mut done = 0usize;
        let apply_start = Instant::now();
        for k in unloads {
            if done > 0 && done + self.loaded[&k].entities > limit {
                report.deferred += 1;
                continue;
            }
            done += self.unload(world, &k, &mut report, &mut events);
        }
        let due: Vec<String> = self
            .pending
            .iter()
            .filter(|(_, t)| **t <= tick)
            .map(|(k, _)| k.clone())
            .collect();
        for k in due {
            if done >= limit && done > 0 {
                report.deferred += 1;
                continue;
            }
            let Some(c) = parse_cell_key(&k) else {
                continue;
            };
            if !self.ready.contains_key(&k) {
                match self.lateness {
                    Lateness::Late => {
                        report.deferred += 1;
                        continue;
                    }
                    Lateness::Wait => {
                        // Not issued yet (all workers busy), or still running.
                        if !self.jobs.contains_key(&k) {
                            self.issue(world, &sources, &k);
                        }
                        report.waits += 1;
                        self.wait_for(world, &sources, &k);
                    }
                }
            }
            let Some(outputs) = self.ready.get(&k) else {
                continue;
            };
            let count: usize = outputs
                .iter()
                .map(|(_, _, r)| r.as_ref().map_or(0, |o| o.fragment.len()))
                .sum::<usize>()
                + world.stashed().map_or(0, |s| {
                    s.iter()
                        .filter(|(cell, _)| *cell == k)
                        .map(|(_, r)| r.snapshot.entities.len())
                        .sum()
                });
            if done > 0 && done + count > limit {
                report.deferred += 1;
                continue;
            }
            let outputs = self.ready.remove(&k).unwrap_or_default();
            let due_at = self.pending.remove(&k).unwrap_or(tick);
            if tick > due_at + 1 && self.lateness == Lateness::Late {
                report.late += 1;
                let d = Diagnostic::warning(
                    codes::LATE_CELL,
                    format!(
                        "cell {k} was applied {} ticks late (generation took longer than delay_ticks)",
                        tick - due_at
                    ),
                )
                .with_hint("raise [stream] delay_ticks or workers, or make the cell cheaper");
                self.report_once(&mut report.diagnostics, &k, d);
            }
            done += self.load(world, &sources, &k, c, outputs, &mut report, &mut events);
        }
        // Terrain collision chunks near the focuses.
        {
            let at: Vec<Vec2> = focuses.iter().map(|f| f.position).collect();
            let pool = self.pool.clone();
            let pool = pool.as_deref().unwrap_or_else(|| TaskPool::global());
            self.collision.step(
                world,
                &sources,
                &at,
                tick,
                &self.config,
                self.lateness,
                pool,
                &mut report,
            );
        }
        report.apply_time = apply_start.elapsed();
        // Start jobs for the next requests, nearest apply tick first.
        self.issue_next(world, &sources, &focuses);
        // Then terrain chunks, with the workers cells leave.
        if self.view {
            let cap = self.config.jobs(self.pool().threads());
            let free = cap.saturating_sub(self.jobs.len() + self.terrain.in_flight());
            let at: Vec<Vec3> = focuses
                .iter()
                .map(|f| Vec3::new(f.position.x, 0.0, f.position.y))
                .collect();
            let pool = self.pool.clone();
            let pool = pool.as_deref().unwrap_or_else(|| TaskPool::global());
            self.terrain
                .step(world, &sources, &at, self.config.terrain_view, pool, free);
        }
        report.entities = done;
        report.resident = self.loaded.len();
        report.memory_bytes = self.memory_bytes();
        report.origin = self.origin;
        report.pending = self.pending.len();
        report.in_flight = self.jobs.len() + self.collision.in_flight();
        world.ecs.insert_resource(events);
        report
    }

    /// The inputs each source covering cell `c` has now.
    fn covering(sources: &[Source], c: [i32; 2], size: f32) -> Vec<(String, blake3::Hash)> {
        sources
            .iter()
            .filter(|s| s.covers(c, size))
            .map(|s| (s.id.clone(), s.inputs))
            .collect()
    }

    /// Starts generating cell `k` on the pool.
    fn issue(&mut self, world: &World, sources: &[Source], k: &str) {
        let Some(c) = parse_cell_key(k) else { return };
        let size = self.config.cell_size;
        let cancel = Arc::new(AtomicBool::new(false));
        let inputs = Self::covering(sources, c, size);
        let mut work = Vec::new();
        for s in sources.iter().filter(|s| s.covers(c, size)) {
            work.push((
                s.id.clone(),
                s.inputs,
                CellJob::new(world, s, c, Some(cancel.clone())),
            ));
        }
        let slot: Slot = Arc::new((Mutex::new(None), Condvar::new()));
        let out = slot.clone();
        self.pool().spawn(move || {
            let result: JobOutput = work
                .into_iter()
                .map(|(id, inputs, job)| {
                    // A panicking node fails its cell instead of the pool
                    // thread, and the result still arrives for `wait_for`.
                    let raw = match job {
                        Ok(job) => {
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job.run()))
                                .unwrap_or_else(|p| {
                                    let why = p
                                        .downcast_ref::<&str>()
                                        .map(|s| (*s).to_string())
                                        .or_else(|| p.downcast_ref::<String>().cloned())
                                        .unwrap_or_default();
                                    Err(Diagnostic::error(
                                        codes::CELL_FAILED,
                                        format!("generation panicked: {why}"),
                                    ))
                                })
                        }
                        Err(d) => Err(d),
                    };
                    (id, inputs, raw)
                })
                .collect();
            let (lock, ready) = &*out;
            *lock.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            ready.notify_all();
        });
        self.jobs.insert(
            k.to_string(),
            InFlight {
                cancel,
                slot,
                inputs,
            },
        );
    }

    /// Starts jobs for pending cells with no result and no job, up to the
    /// workers allowed: earliest apply tick first, then nearest, then key.
    fn issue_next(&mut self, world: &World, sources: &[Source], focuses: &[Focus]) {
        let cap = self.config.jobs(self.pool().threads());
        if self.jobs.len() >= cap {
            return;
        }
        let mut next: Vec<(u64, f32, String)> = self
            .pending
            .iter()
            .filter(|(k, _)| !self.ready.contains_key(*k) && !self.jobs.contains_key(*k))
            .map(|(k, t)| (*t, self.nearest(focuses, k), k.clone()))
            .collect();
        next.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)).then(a.2.cmp(&b.2)));
        for (_, _, k) in next.into_iter().take(cap - self.jobs.len()) {
            self.issue(world, sources, &k);
        }
    }

    /// Takes in finished jobs; drops results whose inputs no longer hold.
    fn collect(&mut self, world: &World, sources: &[Source]) {
        // A job whose sources or inputs changed since it was issued is
        // stopped, whether or not it has finished, so what loads never
        // depends on how fast generation ran.
        let size = self.config.cell_size;
        self.jobs.retain(|k, j| {
            parse_cell_key(k).is_some_and(|c| Self::covering(sources, c, size) == j.inputs)
        });
        let done: Vec<String> = self
            .jobs
            .iter()
            .filter(|(_, j)| j.slot.0.lock().unwrap_or_else(|e| e.into_inner()).is_some())
            .map(|(k, _)| k.clone())
            .collect();
        for k in done {
            self.take(world, sources, &k);
        }
        self.ready.retain(|k, outs| {
            let now = parse_cell_key(k).map(|c| Self::covering(sources, c, size));
            now.is_some_and(|now| {
                now.len() == outs.len()
                    && now
                        .iter()
                        .zip(outs.iter())
                        .all(|((id, inputs), (oid, oinputs, _))| id == oid && inputs == oinputs)
            })
        });
    }

    /// Moves a finished job's result into `ready`, finishing it on this
    /// thread (prefabs, keys under the cell).
    fn take(&mut self, world: &World, sources: &[Source], k: &str) {
        let Some(job) = self.jobs.remove(k) else {
            return;
        };
        let raw = job.slot.0.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(raw) = raw else { return };
        let Some(c) = parse_cell_key(k) else { return };
        let mut outputs = Vec::new();
        for (id, inputs, r) in raw {
            let Some(s) = sources.iter().find(|s| s.id == id) else {
                continue;
            };
            outputs.push((id, inputs, source::finish(world, s, c, r)));
        }
        self.ready.insert(k.to_string(), outputs);
    }

    /// Blocks until cell `k`'s job has finished, then takes its result.
    fn wait_for(&mut self, world: &World, sources: &[Source], k: &str) {
        let Some(job) = self.jobs.get(k) else { return };
        let slot = job.slot.clone();
        {
            let (lock, cv) = &*slot;
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            while guard.is_none() {
                guard = cv.wait(guard).unwrap_or_else(|e| e.into_inner());
            }
        }
        self.take(world, sources, k);
    }

    fn stash_root(
        &mut self,
        world: &mut World,
        e: Entity,
        id: &str,
        cell: &str,
        report: &mut StepReport,
    ) {
        match world.stash(e, cell) {
            Ok(0) => {}
            Ok(n) => {
                let d = Diagnostic::warning(
                    codes::REFERENCE_INTO_UNLOADED,
                    format!(
                        "{n} entit{} reference{} into `{id}`, unloaded with cell {cell}; the references read as empty until it loads",
                        if n == 1 { "y" } else { "ies" },
                        if n == 1 { "s" } else { "" },
                    ),
                )
                .at(Location::entity(id.to_string()));
                self.report_once(&mut report.diagnostics, id, d);
            }
            Err(e) => report.diagnostics.push(e.to_diagnostic()),
        }
    }

    /// Unloads cell `k`; returns the entities despawned.
    fn unload(
        &mut self,
        world: &mut World,
        k: &str,
        report: &mut StepReport,
        events: &mut StreamEvents,
    ) -> usize {
        let Some(l) = self.loaded.remove(k) else {
            return 0;
        };
        let released = remove_content(world, &l.sources, k);
        release(world, &released);
        let size = self.config.cell_size;
        for (id, e, cell) in authored_roots(world, size) {
            if cell == k {
                self.stash_root(world, e, &id, &cell, report);
            }
        }
        report.unloaded.push(k.to_string());
        events.unloaded.push(l.cell);
        l.entities
    }

    /// Applies cell `k`'s generated content and unstashes its authored
    /// roots; returns the entities spawned.
    #[allow(clippy::too_many_arguments)]
    fn load(
        &mut self,
        world: &mut World,
        sources: &[Source],
        k: &str,
        c: [i32; 2],
        outputs: Vec<SourceOutput>,
        report: &mut StepReport,
        events: &mut StreamEvents,
    ) -> usize {
        let size = self.config.cell_size;
        let reload = self.loaded.contains_key(k);
        // A source that held content here and no longer does (it stopped
        // covering the cell or being a valid streamed source) loses it.
        if let Some(old) = self.loaded.get(k) {
            let keep: Vec<Entity> = outputs
                .iter()
                .filter_map(|(sid, _, _)| sources.iter().find(|s| s.id == *sid))
                .map(|s| s.entity)
                .collect();
            let gone: Vec<Entity> = old
                .sources
                .iter()
                .copied()
                .filter(|e| !keep.contains(e))
                .collect();
            let released = remove_content(world, &gone, k);
            release(world, &released);
        }
        let mut bytes = 0u64;
        let mut entities = 0usize;
        let mut ymin = f32::INFINITY;
        let mut ymax = f32::NEG_INFINITY;
        let mut ids = Vec::new();
        let mut inputs = BTreeMap::new();
        let mut stamps = Vec::new();
        let mut held = Vec::new();
        for (sid, used, out) in outputs {
            let Some(s) = sources.iter().find(|s| s.id == sid) else {
                continue;
            };
            inputs.insert(sid.clone(), used);
            held.push(s.entity);
            let out = match out {
                Ok(o) => o,
                Err(d) => {
                    // Its earlier content goes too: the cell holds what
                    // these inputs make, which is nothing.
                    let released = remove_content(world, &[s.entity], k);
                    release(world, &released);
                    self.report_once(
                        &mut report.diagnostics,
                        &format!("{sid}/{k}/{}", s.inputs),
                        d,
                    );
                    continue;
                }
            };
            let applied = world.apply_cell(s.entity, k, &out.fragment);
            // A deleted or suppressed cell root keeps the cell empty: its
            // content, and its assets, are not used.
            if applied.is_ok() && world.cell_root(s.entity, k).is_none() {
                ids.push(sid.clone());
                stamps.push(s.stamp.clone());
                continue;
            }
            // Only applied content is counted.
            for (_, a) in out.assets.iter().filter(|_| applied.is_ok()) {
                bytes += asset_bytes(a);
                if let DerivedAsset::Heightfield(h) = a {
                    let (lo, hi) = h.height_range();
                    ymin = ymin.min(lo);
                    ymax = ymax.max(hi);
                }
            }
            for n in out.fragment.iter().filter(|_| applied.is_ok()) {
                for (cname, fields) in &n.components {
                    if cname == "Transform" {
                        for (f, v) in fields {
                            if let (true, re_reflect::Value::Vec3(p)) = (f == "translation", v) {
                                ymin = ymin.min(p.y);
                                ymax = ymax.max(p.y);
                            }
                        }
                    }
                }
            }
            match applied {
                Ok(r) => {
                    entities += out.fragment.len();
                    if let Some(store) = world.ecs.get_resource::<GraphStore>() {
                        store.add_derived(out.assets);
                    }
                    release(world, &r.released_assets);
                    for d in r.diagnostics {
                        report.diagnostics.push(d);
                    }
                    for o in r.orphans {
                        report.diagnostics.push(
                            Diagnostic::warning(
                                codes::ORPHANED_OVERRIDE,
                                format!(
                                    "{}: override of `{}` has no target in cell {k}",
                                    o.source, o.target
                                ),
                            )
                            .at(Location::entity(o.source.clone())),
                        );
                    }
                    ids.push(sid.clone());
                    stamps.push(s.stamp.clone());
                }
                Err(e) => {
                    // Nothing was applied; its earlier content goes, as for
                    // a cell that fails to generate.
                    let released = remove_content(world, &[s.entity], k);
                    release(world, &released);
                    let d = re_core::Diagnostic::error(
                        codes::CELL_FAILED,
                        format!("{sid}: cell {k} could not be applied"),
                    )
                    .at(Location::entity(sid.clone()))
                    .with_related(e.to_diagnostic());
                    self.report_once(
                        &mut report.diagnostics,
                        &format!("{sid}/{k}/{}", s.inputs),
                        d,
                    );
                }
            }
        }
        if let Err(e) = world.unstash_cell(k) {
            report.diagnostics.push(e.to_diagnostic());
        }
        // The authored roots now in the cell, whether they came back from the
        // stash or were never stashed.
        for (_, e, cell) in authored_roots(world, size) {
            if cell == k {
                entities += hierarchy::subtree(&world.ecs, e).len();
            }
        }
        bytes += entities as u64 * ENTITY_BYTES;
        if !ymin.is_finite() {
            (ymin, ymax) = (0.0, 0.0);
        }
        let version = match stamps.as_slice() {
            [one] => one.clone(),
            many => {
                let mut h = blake3::Hasher::new();
                for s in many {
                    h.update(s.as_bytes());
                }
                h.finalize().to_hex().to_string()
            }
        };
        let cell = StreamCell {
            key: k.to_string(),
            coord: c,
            min: Vec3::new(c[0] as f32 * size, ymin, c[1] as f32 * size),
            max: Vec3::new((c[0] + 1) as f32 * size, ymax, (c[1] + 1) as f32 * size),
            sources: ids,
            version,
        };
        if reload {
            report.reloaded.push(k.to_string());
        } else {
            report.loaded.push(k.to_string());
            events.loaded.push(cell.clone());
        }
        self.loaded.insert(
            k.to_string(),
            Loaded {
                cell,
                bytes,
                entities,
                inputs,
                sources: held,
            },
        );
        entities
    }
}

/// Removes the content of `sources` in cell `k`; returns the derived assets
/// it held.
fn remove_content(world: &mut World, sources: &[Entity], k: &str) -> Vec<String> {
    let mut released = Vec::new();
    for &e in sources {
        if world.ecs.get_entity(e).is_err() {
            continue;
        }
        match world.unload_cell(e, k) {
            Ok(r) => released.extend(r.released_assets),
            // No longer a streamed source: remove its cell all the same.
            Err(_) => {
                if let Some(root) = world.cell_root(e, k) {
                    hierarchy::despawn_recursive(&mut world.ecs, root);
                }
            }
        }
    }
    released
}

fn release(world: &World, assets: &[String]) {
    if let Some(store) = world.ecs.get_resource::<GraphStore>() {
        for a in assets {
            store.release(a);
        }
    }
}
