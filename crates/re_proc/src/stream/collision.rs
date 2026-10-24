//! Terrain collision chunks (ADR-0042): the level-0 chunks of a streamed
//! source's terrain within `physics_radius` of a focus get a generated entity
//! (`t/<x>_<z>`) with a static heightfield collider made from the same 65²
//! samples the view draws, stored at 16 bits per sample in blocks of 4 so
//! neighbouring chunks agree to 0.1 mm.
//!
//! Unlike the view, this is simulation state: a chunk is requested at the
//! tick a focus comes within `physics_radius`, generated on the pool, and
//! applied at `tick + delay_ticks` in key order within `apply_chunks` per
//! tick, waiting for a late result under [`Lateness::Wait`]. It is removed
//! once every focus is farther than `physics_radius + unload_margin`. So the
//! same chunks collide at the same ticks on any machine.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use glam::{Vec2, Vec3};
use re_core::{Diagnostic, TaskPool};
use re_geom::Heightfield;
use re_geom::terrain::{CHUNK_CELLS, CHUNK_SAMPLES};
use re_scene::{World, chunk_key};

use super::manager::{Lateness, SHAPE_BYTES, StepReport};
use super::source::{Source, TerrainSource};
use crate::value::DerivedAsset;
use crate::{Domain, EvalOptions, GraphStore, Output, codes};

/// The name prefix of collision chunk heightfields.
const PREFIX: &str = "derived:terrain-collider/";

/// The physics precision of the derived heightfield named `name`: 16 bits
/// per sample in blocks of 4 for terrain collision chunks, `None` (the
/// backend's defaults) for every other heightfield.
pub fn collider_precision(name: &str) -> Option<re_types::physics::HeightfieldPrecision> {
    name.starts_with(PREFIX)
        .then_some(re_types::physics::HeightfieldPrecision::FINE)
}

type ChunkResult = Result<Arc<Heightfield>, Diagnostic>;
type Slot = Arc<(Mutex<Option<ChunkResult>>, Condvar)>;

struct Job {
    cancel: Arc<AtomicBool>,
    slot: Slot,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job").finish_non_exhaustive()
    }
}

/// One source's collision chunks.
#[derive(Debug, Default)]
struct SourceChunks {
    /// The inputs the applied chunks were generated with.
    inputs: Option<blake3::Hash>,
    /// Applied chunks and the heightfield each names.
    applied: BTreeMap<[i32; 2], String>,
    /// Requested chunks and the tick they apply at.
    pending: BTreeMap<[i32; 2], u64>,
    jobs: BTreeMap<[i32; 2], Job>,
}

impl Clone for SourceChunks {
    /// The same chunks and requests, without the jobs (a fork generates its
    /// pending chunks again, with the same results).
    fn clone(&self) -> Self {
        SourceChunks {
            inputs: self.inputs,
            applied: self.applied.clone(),
            pending: self.pending.clone(),
            jobs: BTreeMap::new(),
        }
    }
}

/// The collision chunks of every streamed source with terrain, by source id.
#[derive(Clone, Debug, Default)]
pub(crate) struct CollisionState {
    sources: BTreeMap<String, SourceChunks>,
}

impl CollisionState {
    /// Generation jobs in flight.
    pub fn in_flight(&self) -> usize {
        self.sources.values().map(|s| s.jobs.len()).sum()
    }

    /// Applied chunks.
    pub fn len(&self) -> usize {
        self.sources.values().map(|s| s.applied.len()).sum()
    }

    /// Requested chunks not applied yet.
    pub fn pending(&self) -> usize {
        self.sources.values().map(|s| s.pending.len()).sum()
    }

    /// Accounted bytes of the applied chunks' shapes.
    pub fn bytes(&self) -> u64 {
        self.len() as u64 * SHAPE_BYTES
    }

    /// One fixed step: requests chunks near `focuses`, removes far ones and
    /// applies due ones within `config.apply_chunks`.
    #[allow(clippy::too_many_arguments)]
    pub fn step(
        &mut self,
        world: &mut World,
        sources: &[Source],
        focuses: &[Vec2],
        tick: u64,
        config: &super::StreamConfig,
        lateness: Lateness,
        pool: &TaskPool,
        report: &mut StepReport,
    ) {
        let terrains: Vec<(&Source, &Arc<TerrainSource>)> = sources
            .iter()
            .filter_map(|s| s.terrain.as_ref().map(|t| (s, t)))
            .collect();
        // Sources that stopped having terrain lose their chunks.
        let gone: Vec<String> = self
            .sources
            .keys()
            .filter(|id| !terrains.iter().any(|(s, _)| s.id == **id))
            .cloned()
            .collect();
        for id in gone {
            let Some(st) = self.sources.remove(&id) else {
                continue;
            };
            let entity = world.entity(&id).ok();
            for (c, name) in st.applied {
                if let Some(e) = entity {
                    world.remove_terrain_chunk(e, &chunk_key(c[0], c[1]));
                }
                release(world, &name);
            }
        }
        let mut budget = config.apply_chunks as usize;
        for (source, terrain) in terrains {
            let st = self.sources.entry(source.id.clone()).or_default();
            let size = source.spacing * CHUNK_CELLS as f32;
            let near = |radius: f32| -> Vec<[i32; 2]> {
                let mut out = Vec::new();
                for f in focuses {
                    let lo = ((f - Vec2::splat(radius)) / size).floor();
                    let hi = ((f + Vec2::splat(radius)) / size).floor();
                    for x in lo.x as i32..=hi.x as i32 {
                        for z in lo.y as i32..=hi.y as i32 {
                            let c = [x, z];
                            if distance(*f, c, size) <= radius && covers(source, c, size) {
                                out.push(c);
                            }
                        }
                    }
                }
                out.sort();
                out.dedup();
                out
            };
            let desired = near(config.physics_radius);
            let keep = near(config.physics_radius + config.unload_margin);
            // New inputs: every applied chunk is generated again, and keeps
            // its old collider until then.
            if st.inputs != Some(source.inputs) {
                st.inputs = Some(source.inputs);
                st.jobs.clear();
                let due = tick + u64::from(config.delay_ticks);
                for c in st.applied.keys() {
                    st.pending.insert(*c, due);
                }
            }
            for c in &desired {
                if !st.applied.contains_key(c) && !st.pending.contains_key(c) {
                    st.pending.insert(*c, tick + u64::from(config.delay_ticks));
                }
            }
            st.pending.retain(|c, _| keep.contains(c));
            let pending = &st.pending;
            st.jobs.retain(|c, _| pending.contains_key(c));
            // Jobs for every request (chunks are small and few).
            if let Some(store) = world.ecs.get_resource::<GraphStore>() {
                for c in st.pending.keys() {
                    if !st.jobs.contains_key(c) {
                        let opts = EvalOptions {
                            seed: Some(source.seed),
                            root: store.root().map(Into::into),
                            files: store.files().cloned(),
                            serial: true,
                            global_spacing: Some(source.spacing),
                            ..Default::default()
                        };
                        st.jobs
                            .insert(*c, issue(pool, store, terrain.clone(), opts, source, *c));
                    }
                }
            }
            // Removals first, then due chunks, in key order.
            let far: Vec<[i32; 2]> = st
                .applied
                .keys()
                .filter(|c| !keep.contains(c))
                .copied()
                .collect();
            for c in far {
                if budget == 0 {
                    report.deferred += 1;
                    continue;
                }
                budget -= 1;
                if let Some(name) = st.applied.remove(&c) {
                    world.remove_terrain_chunk(source.entity, &chunk_key(c[0], c[1]));
                    release(world, &name);
                }
            }
            let due: Vec<[i32; 2]> = st
                .pending
                .iter()
                .filter(|(_, t)| **t <= tick)
                .map(|(c, _)| *c)
                .collect();
            for c in due {
                if budget == 0 {
                    report.deferred += 1;
                    continue;
                }
                let Some(job) = st.jobs.get(&c) else {
                    // No store: nothing can be generated.
                    continue;
                };
                let finished = job
                    .slot
                    .0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some();
                if !finished {
                    match lateness {
                        Lateness::Late => {
                            report.deferred += 1;
                            continue;
                        }
                        Lateness::Wait => {
                            report.waits += 1;
                            let (lock, cv) = &*job.slot;
                            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                            while guard.is_none() {
                                guard = cv.wait(guard).unwrap_or_else(|e| e.into_inner());
                            }
                        }
                    }
                }
                budget -= 1;
                st.pending.remove(&c);
                let Some(job) = st.jobs.remove(&c) else {
                    continue;
                };
                let result = job.slot.0.lock().unwrap_or_else(|e| e.into_inner()).take();
                let key = chunk_key(c[0], c[1]);
                match result {
                    Some(Ok(h)) => {
                        let name = heightfield_name(source, c);
                        if let Some(store) = world.ecs.get_resource::<GraphStore>() {
                            store.add_derived(vec![(name.clone(), DerivedAsset::Heightfield(h))]);
                        }
                        match world.set_terrain_chunk(source.entity, &key, corner(source, c), &name)
                        {
                            Ok(old) => {
                                if let Some(old) = old {
                                    release(world, &old);
                                }
                                if let Some(old) = st.applied.insert(c, name.clone())
                                    && old != name
                                {
                                    release(world, &old);
                                }
                            }
                            Err(e) => {
                                release(world, &name);
                                report.diagnostics.push(e.to_diagnostic());
                            }
                        }
                    }
                    Some(Err(d)) => report.diagnostics.push(
                        Diagnostic::error(
                            codes::CELL_FAILED,
                            format!("{}: terrain collision chunk {key} failed", source.id),
                        )
                        .with_related(d),
                    ),
                    None => {}
                }
            }
        }
    }
}

/// The derived name of source `s`'s chunk `c` heightfield, for its current
/// inputs.
fn heightfield_name(s: &Source, c: [i32; 2]) -> String {
    format!(
        "{PREFIX}{}/{}_{}/{}",
        s.id,
        c[0],
        c[1],
        &s.inputs.to_hex()[..16]
    )
}

/// Distance in the XZ plane from `p` to chunk `c`'s square.
fn distance(p: Vec2, c: [i32; 2], size: f32) -> f32 {
    let lo = Vec2::new(c[0] as f32, c[1] as f32) * size;
    let hi = lo + Vec2::splat(size);
    let d = (lo - p).max(Vec2::ZERO).max(p - hi);
    d.length()
}

/// True if chunk `c` touches the source's square (all chunks when unbounded).
fn covers(source: &Source, c: [i32; 2], size: f32) -> bool {
    source.covers(c, size)
}

fn release(world: &World, name: &str) {
    if let Some(store) = world.ecs.get_resource::<GraphStore>() {
        store.release(name);
    }
}

/// Chunk `c`'s corner (its sample `(0, 0)`) from the source's origin: where
/// its entity sits, so its body is where its ground is.
fn corner(source: &Source, c: [i32; 2]) -> Vec3 {
    let cells = i64::from(CHUNK_CELLS);
    Vec3::new(
        (i64::from(c[0]) * cells) as f32 * source.spacing,
        0.0,
        (i64::from(c[1]) * cells) as f32 * source.spacing,
    )
}

/// Starts generating chunk `c`'s heights on the pool.
fn issue(
    pool: &TaskPool,
    store: &GraphStore,
    terrain: Arc<TerrainSource>,
    mut opts: EvalOptions,
    source: &Source,
    c: [i32; 2],
) -> Job {
    let evaluator = store.evaluator().clone();
    let cancel = Arc::new(AtomicBool::new(false));
    let slot: Slot = Arc::new((Mutex::new(None), Condvar::new()));
    let out = slot.clone();
    let spacing = source.spacing;
    let cells = i64::from(CHUNK_CELLS);
    opts.domain = Some(Domain {
        origin: [i64::from(c[0]) * cells, i64::from(c[1]) * cells],
        n: CHUNK_SAMPLES,
        spacing,
    });
    opts.cancel = Some(cancel.clone());
    pool.spawn(move || {
        let result: ChunkResult = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let evaluation = evaluator.evaluate(&terrain.graph, &opts);
            match evaluation.output(&terrain.field) {
                Some(Output::Field(f)) => {
                    Heightfield::new(CHUNK_SAMPLES, spacing, Vec3::ZERO, f.0.samples.clone())
                        .map(Arc::new)
                        .map_err(|e| Diagnostic::error(codes::CELL_FAILED, e.to_string()))
                }
                _ => Err(evaluation.report.first_error().cloned().unwrap_or_else(|| {
                    Diagnostic::error(codes::CELL_FAILED, "the terrain field has no output")
                })),
            }
        }))
        .unwrap_or_else(|_| {
            Err(Diagnostic::error(
                codes::CELL_FAILED,
                "terrain collision chunk generation panicked",
            ))
        });
        let (lock, ready) = &*out;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
        ready.notify_all();
    });
    Job { cancel, slot }
}
