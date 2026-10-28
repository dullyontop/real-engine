//! The [`Evaluator`]: evaluates a [`ProcGraph`] in topological order, in
//! parallel on the [`TaskPool`], with per-node blake3 keys, a memory cache
//! and an optional postcard disk cache, and reports what it did in an
//! [`EvalReport`] (ARCHITECTURE §5, "Evaluation").
//!
//! **Determinism.** A node's output depends only on its key's inputs: its
//! kind and the version it was written for (ADR-0043), its id, its
//! parameters (migrated to the kind's current shape), the evaluation's
//! [`Domain`] if it has one, its inputs' keys, its seed
//! (`Pcg32::from_seed(graph seed).fork(hash64(node id))`, for kinds that use
//! one) and the contents of the files it names. Nodes never share mutable
//! state and the order of their results is fixed, so the output is the same
//! on 1 or N threads, and adding a node changes no other node's random
//! numbers.
//!
//! **Global nodes.** In a streamed evaluation a [`Streaming::Global`] node
//! is keyed and run without the window: its key hashes the global keys of
//! its inputs (their keys without a window, at
//! [`EvalOptions::global_spacing`]), so every cell and chunk of a source
//! shares its output. It samples its fields through
//! [`NodeCtx::sample`](crate::NodeCtx::sample) instead of taking them over
//! the window, so a node that only feeds a global node's fields does not
//! run over the window. Windows evaluated at the same moment on a cold
//! cache may each run it; they get the same output. What a cell node makes
//! from global inputs for every cell, such as a joined road network, is
//! kept in the memory cache too ([`NodeCtx::shared`](crate::NodeCtx::shared)).

use std::any::Any;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use re_core::files::{ProjectFiles, ProjectPath, WritableDir};
use re_core::time::Instant;
use re_core::{Diagnostic, Location, TaskPool};
use re_noise::Pcg32;
use re_reflect::Registry;
use re_scene::{GenNode, SceneFragment};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::cache::DiskCache;
use crate::canon::{canonical_json, fragment_hash, hash64};
use crate::codes;
use crate::graph::ProcGraph;
use crate::nodes::{
    Domain, InputValue, NodeCtx, NodeDef, NodeError, NodeRegistry, SharedValue, Streaming,
    natural_cmp, sink_fragment_in,
};
use crate::value::{DerivedAsset, Field, Fragment, Output, PortType};

/// Where a node's output came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheState {
    /// The memory cache.
    Memory,
    /// The disk cache.
    Disk,
    /// Computed.
    Miss,
    /// Not run, because an input failed or the budget ran out, or, in a
    /// streamed evaluation, because only global nodes' fields need it
    /// (they sample it themselves).
    Skipped,
    /// Ran and failed.
    Failed,
}

/// The size of a node's output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputInfo {
    /// The output's type.
    #[serde(rename = "type")]
    pub ty: PortType,
    /// Samples, vertices, points or entities.
    pub items: u64,
    /// An estimate of the memory it holds.
    pub bytes: u64,
}

/// What happened to one node.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NodeReport {
    /// The node's id.
    pub id: String,
    /// The node's kind.
    pub kind: String,
    /// The node's key, in hex (empty when it was skipped before its key was known).
    pub key: String,
    /// Where the output came from.
    pub cache: CacheState,
    /// Time spent on the node (loading or computing), in milliseconds.
    pub ms: f64,
    /// The output's size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<OutputInfo>,
    /// The node's problems.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
    /// What the node logged while it computed (a Luau node's `ctx:log`);
    /// empty when its output came from a cache.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
}

/// What an evaluation did. `real graph eval --json` and `graph.report`
/// print it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EvalReport {
    /// The graph's name.
    pub graph: String,
    /// The seed used.
    pub seed: u64,
    /// The worker threads available.
    pub threads: usize,
    /// One entry per node, in document order.
    pub nodes: Vec<NodeReport>,
    /// Nodes served from the memory cache.
    pub memory_hits: usize,
    /// Nodes served from the disk cache.
    pub disk_hits: usize,
    /// Nodes computed.
    pub misses: usize,
    /// Wall time of the evaluation, in milliseconds.
    pub ms: f64,
    /// The graph's [version stamp](crate::version_stamp), in hex.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stamp: String,
    /// The hash of the output fragment (hex), once it is assembled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment_hash: Option<String>,
    /// The number of entities in the output fragment, once it is assembled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entities: Option<usize>,
    /// Problems that belong to no single node (validation, budget, parameter
    /// overrides, assembly).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<Diagnostic>,
}

impl EvalReport {
    /// Every diagnostic: the report's own, then each node's.
    pub fn all_diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        self.diagnostics
            .iter()
            .chain(self.nodes.iter().flat_map(|n| n.diagnostics.iter()))
    }

    /// The first error, if any.
    pub fn first_error(&self) -> Option<&Diagnostic> {
        self.all_diagnostics().find(|d| d.is_error())
    }

    /// True if nothing failed.
    pub fn ok(&self) -> bool {
        self.first_error().is_none()
    }

    /// The report of node `id`.
    pub fn node(&self, id: &str) -> Option<&NodeReport> {
        self.nodes.iter().find(|n| n.id == id)
    }
}

/// Options of one evaluation.
#[derive(Clone, Debug, Default)]
pub struct EvalOptions {
    /// The seed; the graph's own seed when `None`.
    pub seed: Option<u64>,
    /// The project root that file parameters (prefab paths) are relative
    /// to; the current directory when `None`. Used only without
    /// [`files`](Self::files).
    pub root: Option<PathBuf>,
    /// The project's files: file parameters are read through them when set
    /// (ADR-0037).
    pub files: Option<ProjectFiles>,
    /// Stop before starting a level of nodes after this instant (`RE4016`).
    pub deadline: Option<Instant>,
    /// The window to evaluate, for streamed evaluation (ADR-0042). Part of
    /// every node's key when set.
    pub domain: Option<Domain>,
    /// Runs every node on the calling thread, one after another, with no
    /// fan-out on the pool: a streaming job is serial (ADR-0042). The output
    /// is the same either way.
    pub serial: bool,
    /// Stops before the next level of nodes once this flag is set (`RE4310`);
    /// a streaming job's request was cancelled.
    pub cancel: Option<Arc<AtomicBool>>,
    /// In a streamed evaluation, the spacing (metres) at which
    /// [`Streaming::Global`] nodes sample their fields, part of their keys:
    /// the streamed source's spacing, so a level-of-detail chunk at a
    /// coarser spacing sees the same roads as a cell. The domain's spacing
    /// when `None`.
    pub global_spacing: Option<f32>,
}

struct MemEntry {
    output: Output,
    bytes: u64,
    used: u64,
}

/// A value nodes share ([`NodeCtx::shared`](crate::NodeCtx::shared)):
/// made once, by whichever node asks first.
type SharedSlot = Arc<OnceLock<Result<SharedValue, NodeError>>>;

struct SharedEntry {
    slot: SharedSlot,
    /// Counted in the cache's bytes once made.
    bytes: u64,
    used: u64,
}

#[derive(Default)]
struct MemCache {
    map: HashMap<[u8; 32], MemEntry>,
    /// Values made from nodes' inputs and shared, by key.
    shared: HashMap<[u8; 32], SharedEntry>,
    bytes: u64,
    tick: u64,
    /// The ticks of the evaluations running now: nothing they used is
    /// evicted until they finish.
    active: BTreeSet<u64>,
}

/// The default memory-cache budget: 256 MiB.
pub const DEFAULT_MEMORY_BUDGET: u64 = 256 << 20;

/// Evaluates graphs. Holds the node registry, the memory cache and the
/// optional disk cache; safe to share between threads (and between a world
/// and its copies) behind an `Arc`.
pub struct Evaluator {
    registry: Arc<NodeRegistry>,
    pool: Option<Arc<TaskPool>>,
    disk: Option<DiskCache>,
    memory: Mutex<MemCache>,
    budget: u64,
}

impl std::fmt::Debug for Evaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Evaluator")
            .field("kinds", &self.registry.len())
            .field("disk", &self.disk.as_ref().map(DiskCache::dir))
            .finish_non_exhaustive()
    }
}

impl Default for Evaluator {
    fn default() -> Self {
        Self::new(Arc::new(NodeRegistry::m1()))
    }
}

/// A node ready to run: its keys, inputs and files.
struct Job<'a> {
    index: usize,
    def: &'a NodeDef,
    params: &'a Json,
    /// The node's key ([`NodeCtx::key`](crate::NodeCtx::key)).
    key: [u8; 32],
    /// The key its output is cached under ([`lookup_key`]).
    cache: [u8; 32],
    seed: u64,
    inputs: Vec<(String, Output)>,
    files: HashMap<String, Result<Arc<Vec<u8>>, String>>,
}

struct JobResult {
    output: Result<Output, Diagnostic>,
    from_disk: bool,
    ms: f64,
    warnings: Vec<Diagnostic>,
    logs: Vec<String>,
}

/// An evaluation running on an [`Evaluator`] ([`Evaluator::begin`]); it
/// finishes when dropped, even if a node panicked.
struct Running<'e> {
    evaluator: &'e Evaluator,
    tick: u64,
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.evaluator.finish(self.tick);
    }
}

impl Evaluator {
    /// An evaluator for `registry` on the global pool, with no disk cache.
    pub fn new(registry: Arc<NodeRegistry>) -> Self {
        Evaluator {
            registry,
            pool: None,
            disk: None,
            memory: Mutex::new(MemCache::default()),
            budget: DEFAULT_MEMORY_BUDGET,
        }
    }

    /// Runs nodes on `pool` instead of the global pool.
    #[must_use]
    pub fn with_pool(mut self, pool: Arc<TaskPool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Caches fields, meshes and points in the directory `dir`
    /// (`<project>/.real/cache/proc`).
    #[must_use]
    pub fn with_disk_cache(self, dir: impl Into<PathBuf>) -> Self {
        self.with_disk_cache_in(WritableDir::dir(dir))
    }

    /// Caches fields, meshes and points in `dir` (the run's `proc` cache folder).
    #[must_use]
    pub fn with_disk_cache_in(mut self, dir: WritableDir) -> Self {
        self.disk = Some(DiskCache::new(dir));
        self
    }

    /// Sets the memory-cache budget in bytes (least recently used outputs
    /// beyond it are dropped after each evaluation).
    #[must_use]
    pub fn with_memory_budget(mut self, bytes: u64) -> Self {
        self.budget = bytes;
        self
    }

    /// The node registry.
    pub fn registry(&self) -> &NodeRegistry {
        &self.registry
    }

    /// This evaluator with `registry` instead of its own: the same pool, disk
    /// cache and memory budget, with an empty memory cache. Keys include
    /// each kind's version and implementation hash, so disk entries of
    /// kinds that did not change stay valid.
    pub fn with_registry(&self, registry: Arc<NodeRegistry>) -> Evaluator {
        Evaluator {
            registry,
            pool: self.pool.clone(),
            disk: self.disk.clone(),
            memory: Mutex::new(MemCache::default()),
            budget: self.budget,
        }
    }

    /// The disk cache directory, if there is a disk cache and it is on disk.
    pub fn disk_cache_dir(&self) -> Option<&Path> {
        self.disk.as_ref().and_then(|d| d.dir().path())
    }

    /// Empties the memory cache.
    pub fn clear_memory(&self) {
        let mut m = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        *m = MemCache::default();
    }

    fn pool(&self) -> &TaskPool {
        self.pool.as_deref().unwrap_or_else(|| TaskPool::global())
    }

    /// Evaluates `graph` (which must have been validated against this
    /// evaluator's registry, as [`ProcGraph::from_doc`] does). Never panics
    /// on bad input: failures are diagnostics in the report, and nodes after
    /// a failed one are skipped.
    pub fn evaluate(&self, graph: &ProcGraph, opts: &EvalOptions) -> Evaluation {
        let start = Instant::now();
        let stamp = graph.version_stamp(&self.registry).to_hex().to_string();
        let ids = graph.nodes.keys().cloned().collect();
        let problems = graph.validate(&self.registry);
        if !problems.is_empty() {
            let report = EvalReport {
                graph: graph.id.clone(),
                seed: opts.seed.unwrap_or(graph.seed),
                threads: self.pool().threads(),
                stamp,
                nodes: graph
                    .nodes
                    .iter()
                    .map(|(id, node)| skipped(id, &node.kind))
                    .collect(),
                diagnostics: problems,
                ms: ms_since(start),
                ..Default::default()
            };
            return Evaluation {
                report,
                ids,
                output_nodes: Vec::new(),
                outputs: vec![None; graph.nodes.len()],
                domain: opts.domain,
            };
        }
        let plan = Plan::new(graph, &self.registry);
        let running = self.begin();
        let (mut report, outputs) = self.run(&plan, opts, running.tick);
        drop(running);
        report.stamp = stamp;
        report.ms = ms_since(start);
        Evaluation {
            report,
            ids,
            output_nodes: graph.output_nodes(),
            outputs,
            domain: opts.domain,
        }
    }

    /// Evaluates `plan`'s nodes, marking what they use in the memory cache
    /// with `tick`: the evaluation's own, or, for a part a node reads, the
    /// tick of the evaluation that node belongs to.
    fn run(&self, plan: &Plan, opts: &EvalOptions, tick: u64) -> (EvalReport, Vec<Option<Output>>) {
        let start = Instant::now();
        let graph = plan.graph;
        let seed = opts.seed.unwrap_or(graph.seed);
        let pool = self.pool();
        let n = graph.nodes.len();
        let mut report = EvalReport {
            graph: graph.id.clone(),
            seed,
            threads: pool.threads(),
            nodes: plan
                .members
                .iter()
                .map(|&i| {
                    let (id, node) = graph.nodes.get_index(i).expect("members are valid");
                    skipped(id, &node.kind)
                })
                .collect(),
            ..Default::default()
        };
        let mut outputs: Vec<Option<Output>> = vec![None; n];
        // Each node's key and cache key (see `lookup_key`).
        let mut keys: Vec<Option<[u8; 32]>> = vec![None; n];
        let mut caches: Vec<Option<[u8; 32]>> = vec![None; n];
        // In a streamed evaluation: the spacing global nodes sample at, each
        // node's global keys (its keys without the window, when its output
        // is the same for every window), and the nodes the output needs.
        let global = opts
            .domain
            .map(|d| opts.global_spacing.unwrap_or(d.spacing));
        let mut gkeys: Vec<Option<[u8; 32]>> = vec![None; n];
        let mut gcaches: Vec<Option<[u8; 32]>> = vec![None; n];
        let needed = match global {
            Some(_) => plan.needed(),
            None => vec![true; n],
        };
        let base = Pcg32::from_seed(seed);

        'levels: for level in &plan.levels {
            if let Some(deadline) = opts.deadline
                && Instant::now() >= deadline
            {
                let first = &graph
                    .nodes
                    .get_index(level[0])
                    .map(|(k, _)| k.clone())
                    .unwrap_or_default();
                report.diagnostics.push(
                    Diagnostic::error(
                        codes::BUDGET_EXCEEDED,
                        format!(
                            "evaluation of `{}` stopped after {:.0} ms, over its time budget, before node `{first}`",
                            graph.id,
                            ms_since(start)
                        ),
                    )
                    .with_hint("raise `max_ms`, or make the graph cheaper (smaller fields, larger scatter radius)"),
                );
                break 'levels;
            }
            if opts
                .cancel
                .as_ref()
                .is_some_and(|c| c.load(Ordering::Relaxed))
            {
                report.diagnostics.push(Diagnostic::error(
                    codes::CANCELLED,
                    format!("evaluation of `{}` was cancelled", graph.id),
                ));
                break 'levels;
            }
            let mut jobs: Vec<Job> = Vec::new();
            for &index in level {
                let Some(node) = &plan.nodes[index] else {
                    continue;
                };
                let def = node.def;
                // A global node in a streamed evaluation samples its fields.
                let as_global = global.is_some() && def.streaming == Streaming::Global;
                let sampled =
                    |from: usize| as_global && plan.output_type(from) == Some(PortType::Field);
                // An input failed or was skipped, or the output does not need it.
                let runs = needed[index]
                    && node
                        .inputs
                        .iter()
                        .all(|&(_, from)| outputs[from].is_some() || sampled(from));
                let global_key = global.filter(|_| {
                    matches!(
                        def.streaming,
                        Streaming::Global | Streaming::Pointwise | Streaming::Region
                    ) && node.inputs.iter().all(|&(_, from)| gkeys[from].is_some())
                });
                if !runs && global_key.is_none() {
                    continue;
                }
                let node_seed = base.fork(node.salt).next_u64();
                let files = load_files(def, &node.params, opts);
                let key_inputs = KeyInputs {
                    id: node.id,
                    def,
                    version: node.version,
                    params: &node.params,
                    inputs: &node.inputs,
                    keys: &keys,
                    seed: node_seed,
                    files: &files,
                    domain: opts.domain,
                    global: None,
                    cache: false,
                };
                let cache_inputs = KeyInputs {
                    params: &node.canonical,
                    keys: &caches,
                    cache: true,
                    ..key_inputs
                };
                if let Some(spacing) = global_key {
                    gkeys[index] = Some(node_key(&KeyInputs {
                        keys: &gkeys,
                        domain: None,
                        global: Some(spacing),
                        ..key_inputs
                    }));
                    gcaches[index] = Some(node_key(&KeyInputs {
                        keys: &gcaches,
                        domain: None,
                        global: Some(spacing),
                        ..cache_inputs
                    }));
                }
                if !runs {
                    continue;
                }
                let slot = plan.slot[index];
                let (key, cache) = if as_global {
                    let (Some(key), Some(cache)) = (gkeys[index], gcaches[index]) else {
                        let nr = &mut report.nodes[slot];
                        nr.cache = CacheState::Failed;
                        nr.diagnostics.push(Diagnostic::error(
                            codes::NOT_STREAMABLE,
                            format!(
                                "node `{}` ({}) is global, but not all its inputs are global, pointwise or region nodes",
                                node.id, def.kind
                            ),
                        ));
                        continue;
                    };
                    (key, cache)
                } else {
                    (node_key(&key_inputs), node_key(&cache_inputs))
                };
                keys[index] = Some(key);
                caches[index] = Some(cache);
                let cache = lookup_key(def, key, cache);
                let nr = &mut report.nodes[slot];
                nr.key = blake3::Hash::from_bytes(key).to_hex().to_string();
                let gather = || -> Vec<(String, Output)> {
                    node.inputs
                        .iter()
                        .filter(|(_, from)| !sampled(*from))
                        .map(|&(port, from)| {
                            (
                                port.to_string(),
                                outputs[from].clone().expect("checked above"),
                            )
                        })
                        .collect()
                };
                // Input checks run whether or not the output is cached, so a
                // warning does not come and go with the cache.
                if def.checks_inputs() {
                    let values: Vec<InputValue> = node
                        .inputs
                        .iter()
                        .filter_map(|&(port, from)| {
                            Some(InputValue {
                                port,
                                value: outputs[from].as_ref()?,
                                from_kind: plan.nodes[from].as_ref()?.def.kind,
                            })
                        })
                        .collect();
                    nr.diagnostics.extend(
                        def.check_inputs(&values)
                            .into_iter()
                            .map(|d| in_node(d, node.id, def.kind)),
                    );
                }
                let hit = {
                    let mut m = self.lock_memory();
                    m.map.get_mut(&cache).map(|e| {
                        e.used = e.used.max(tick);
                        e.output.clone()
                    })
                };
                if let Some(output) = hit {
                    nr.cache = CacheState::Memory;
                    nr.output = Some(info(&output));
                    report.memory_hits += 1;
                    outputs[index] = Some(output);
                    continue;
                }
                jobs.push(Job {
                    index,
                    def,
                    params: &node.params,
                    key,
                    cache,
                    seed: node_seed,
                    inputs: gather(),
                    files,
                });
            }
            let results: Vec<Mutex<Option<JobResult>>> =
                jobs.iter().map(|_| Mutex::new(None)).collect();
            if jobs.len() == 1 || opts.serial {
                for (job, slot) in jobs.iter().zip(&results) {
                    *slot.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(self.run_job(plan, job, pool, opts, tick));
                }
            } else if !jobs.is_empty() {
                pool.scope(|s| {
                    for (job, slot) in jobs.iter().zip(&results) {
                        s.spawn(move |_| {
                            let r = self.run_job(plan, job, pool, opts, tick);
                            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                        });
                    }
                });
            }
            let mut m = self.lock_memory();
            for (job, slot) in jobs.iter().zip(results) {
                let Some(r) = slot.into_inner().unwrap_or_else(|e| e.into_inner()) else {
                    continue;
                };
                let nr = &mut report.nodes[plan.slot[job.index]];
                nr.ms = r.ms;
                nr.diagnostics.extend(r.warnings);
                nr.logs = r.logs;
                match r.output {
                    Ok(output) => {
                        nr.output = Some(info(&output));
                        if r.from_disk {
                            nr.cache = CacheState::Disk;
                            report.disk_hits += 1;
                        } else {
                            nr.cache = CacheState::Miss;
                            report.misses += 1;
                        }
                        let bytes = output.size().1;
                        if let Some(old) = m.map.insert(
                            job.cache,
                            MemEntry {
                                output: output.clone(),
                                bytes,
                                used: tick,
                            },
                        ) {
                            m.bytes -= old.bytes;
                        }
                        m.bytes += bytes;
                        outputs[job.index] = Some(output);
                    }
                    Err(d) => {
                        nr.cache = CacheState::Failed;
                        nr.diagnostics.insert(0, d);
                    }
                }
            }
        }
        report.ms = ms_since(start);
        (report, outputs)
    }

    fn run_job(
        &self,
        plan: &Plan,
        job: &Job,
        pool: &TaskPool,
        opts: &EvalOptions,
        tick: u64,
    ) -> JobResult {
        let start = Instant::now();
        let node = plan.nodes[job.index]
            .as_ref()
            .expect("jobs are planned nodes");
        let id = node.id;
        let mut warnings = Vec::new();
        if let Some(disk) = &self.disk
            && job.def.output != PortType::Fragment
        {
            match disk.load(&job.cache) {
                Ok(Some(output)) if output.port_type() == job.def.output => {
                    return JobResult {
                        output: Ok(output),
                        from_disk: true,
                        ms: ms_since(start),
                        warnings,
                        logs: Vec::new(),
                    };
                }
                Ok(_) => {}
                Err(e) => warnings.push(cache_warning(id, e)),
            }
        }
        let global = opts
            .domain
            .map(|d| opts.global_spacing.unwrap_or(d.spacing));
        // A region node reads its inputs over its tiles' windows, and a cell
        // node over its window with an apron: the part of the plan an input
        // comes from, evaluated again with that window as the domain.
        let reads = opts.domain.is_some()
            && matches!(job.def.streaming, Streaming::Region | Streaming::Cell);
        let read = |port: &str, domain: Domain| -> Result<Arc<Field>, String> {
            if !reads {
                return Err("not a streamed region or cell node".into());
            }
            let from = node
                .input(port)
                .ok_or_else(|| format!("input `{port}` is not connected"))?;
            let opts = EvalOptions {
                domain: Some(domain),
                global_spacing: global,
                ..opts.clone()
            };
            self.read_field(plan, from, port, &opts, tick)
        };
        // A global node samples its fields on the source's grid: the part of
        // the plan the field comes from, evaluated over small windows around
        // the points at the global spacing.
        let sampling = global.filter(|_| job.def.streaming == Streaming::Global);
        let sample = |port: &str, points: &[[f64; 2]]| -> Result<Option<Vec<f64>>, String> {
            let Some(spacing) = sampling else {
                return Err("not a streamed global node".into());
            };
            let Some(from) = node.input(port) else {
                return Ok(None);
            };
            sample_field(points, spacing, |domain| {
                let opts = EvalOptions {
                    domain: Some(domain),
                    global_spacing: Some(spacing),
                    ..opts.clone()
                };
                self.read_field(plan, from, port, &opts, tick)
            })
            .map(Some)
        };
        let share = |key: [u8; 32], make: &mut dyn FnMut() -> Result<SharedValue, NodeError>| {
            self.shared(key, make)
        };
        let ctx = NodeCtx {
            node_id: id,
            key: job.key,
            version: node.version,
            domain: if sampling.is_some() {
                None
            } else {
                opts.domain
            },
            serial: opts.serial,
            seed: job.seed,
            inputs: job.inputs.clone(),
            files: job.files.clone(),
            pool,
            logs: Default::default(),
            upstream: reads.then_some(&read as &crate::nodes::Upstream),
            sample: sampling.map(|_| &sample as &crate::nodes::Sampler),
            share: Some(&share as &crate::nodes::Share),
        };
        let output = job.def.eval(&ctx, job.params).map_err(|e| {
            Diagnostic::error(
                e.code,
                format!("node `{id}` ({}): {}", job.def.kind, e.message),
            )
        });
        if let (Ok(o), Some(disk)) = (&output, &self.disk)
            && DiskCache::stores(o)
            && let Err(e) = disk.store(&job.cache, o)
        {
            warnings.push(cache_warning(id, e));
        }
        JobResult {
            output,
            from_disk: false,
            ms: ms_since(start),
            warnings,
            logs: ctx.logs.into_inner().unwrap_or_else(|e| e.into_inner()),
        }
    }

    /// Node `from`'s field, from the part of `plan` it comes from evaluated
    /// with `opts` (a window): what a region or cell node reads and a global
    /// node samples on input `port`. The part runs with the reading node's
    /// `tick`, so it never evicts what that evaluation uses.
    fn read_field(
        &self,
        plan: &Plan,
        from: usize,
        port: &str,
        opts: &EvalOptions,
        tick: u64,
    ) -> Result<Arc<Field>, String> {
        let part = plan.part(from);
        let (report, outputs) = self.run(&part, opts, tick);
        match &outputs[from] {
            Some(Output::Field(f)) => Ok(f.clone()),
            _ => Err(report
                .first_error()
                .map(|d| d.message.clone())
                .unwrap_or_else(|| format!("input `{port}` has no field"))),
        }
    }

    /// The value shared under `key`, made by `make` if no node has made it
    /// (see [`NodeCtx::shared`](crate::NodeCtx::shared)). A node asking
    /// while another makes it waits for that one.
    fn shared(
        &self,
        key: [u8; 32],
        make: &mut dyn FnMut() -> Result<SharedValue, NodeError>,
    ) -> Result<Arc<dyn Any + Send + Sync>, NodeError> {
        let slot = {
            let mut m = self.memory.lock().unwrap_or_else(|e| e.into_inner());
            let tick = m.tick;
            let e = m.shared.entry(key).or_insert_with(|| SharedEntry {
                slot: SharedSlot::default(),
                bytes: 0,
                used: tick,
            });
            e.used = tick;
            e.slot.clone()
        };
        let mut made = false;
        let r = slot.get_or_init(|| {
            made = true;
            make()
        });
        if made {
            // Count what was made, unless it was dropped meanwhile; a
            // failure is not kept.
            let mut m = self.memory.lock().unwrap_or_else(|e| e.into_inner());
            let ours = m
                .shared
                .get(&key)
                .is_some_and(|e| Arc::ptr_eq(&e.slot, &slot));
            match r {
                Ok((_, bytes)) if ours => {
                    if let Some(e) = m.shared.get_mut(&key) {
                        e.bytes = *bytes;
                    }
                    m.bytes += bytes;
                }
                Err(_) if ours => {
                    m.shared.remove(&key);
                }
                _ => {}
            }
        }
        r.as_ref().map(|(v, _)| v.clone()).map_err(Clone::clone)
    }

    fn lock_memory(&self) -> std::sync::MutexGuard<'_, MemCache> {
        self.memory.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Starts an evaluation: its tick in the memory cache. Nothing it uses
    /// is evicted until the guard is dropped, which evicts
    /// ([`Evaluator::finish`]).
    fn begin(&self) -> Running<'_> {
        let mut m = self.lock_memory();
        m.tick += 1;
        let tick = m.tick;
        m.active.insert(tick);
        Running {
            evaluator: self,
            tick,
        }
    }

    /// Ends the evaluation of `tick`, then drops least recently used entries
    /// (outputs and shared values) until the cache fits its budget. Entries
    /// that a running evaluation used are kept (its tick or a later one),
    /// and so are shared values still being made, which hold nothing yet.
    fn finish(&self, tick: u64) {
        let mut m = self.lock_memory();
        m.active.remove(&tick);
        if m.bytes <= self.budget {
            return;
        }
        let keep = m.active.first().map_or(tick, |&t| t.min(tick));
        let mut old: Vec<(u64, bool, [u8; 32], u64)> = m
            .map
            .iter()
            .filter(|(_, e)| e.used < keep)
            .map(|(k, e)| (e.used, false, *k, e.bytes))
            .collect();
        old.extend(
            m.shared
                .iter()
                .filter(|(_, e)| e.used < keep && e.slot.get().is_some())
                .map(|(k, e)| (e.used, true, *k, e.bytes)),
        );
        old.sort_unstable();
        for (_, shared, k, bytes) in old {
            if m.bytes <= self.budget {
                break;
            }
            if shared {
                m.shared.remove(&k);
            } else {
                m.map.remove(&k);
            }
            m.bytes -= bytes;
        }
    }

    /// The number of outputs in the memory cache.
    pub fn memory_entries(&self) -> usize {
        self.memory
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .map
            .len()
    }
}

fn ms_since(start: Instant) -> f64 {
    let d: Duration = start.elapsed();
    d.as_secs_f64() * 1000.0
}

fn info(output: &Output) -> OutputInfo {
    let (items, bytes) = output.size();
    OutputInfo {
        ty: output.port_type(),
        items,
        bytes,
    }
}

fn cache_warning(node: &str, message: String) -> Diagnostic {
    Diagnostic::warning(codes::CACHE_IO, format!("node `{node}`: {message}"))
        .with_hint("the proc cache (.real/cache/proc) is safe to delete")
}

/// The file paths a node's file parameters name (a string or an object of strings).
fn file_paths(def: &NodeDef, params: &Json) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in def.file_params {
        match params.get(name) {
            Some(Json::String(s)) if !s.is_empty() => out.push(s.clone()),
            Some(Json::Object(m)) => out.extend(
                m.values()
                    .filter_map(Json::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            ),
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

fn load_files(
    def: &NodeDef,
    params: &Json,
    opts: &EvalOptions,
) -> HashMap<String, Result<Arc<Vec<u8>>, String>> {
    const LEAVES: &str = "paths are relative to the project root and may not leave it";
    file_paths(def, params)
        .into_iter()
        .map(|p| {
            if let Some(files) = &opts.files {
                let content = match ProjectPath::new(&p) {
                    Ok(_) => files
                        .read_source(&p)
                        .map(|b| Arc::new(b.to_vec()))
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(format!("{LEAVES} (`{p}`: {e})")),
                };
                return (p, content);
            }
            let rel = Path::new(&p);
            let content = if rel.is_absolute()
                || rel.components().any(|c| {
                    matches!(
                        c,
                        Component::ParentDir | Component::Prefix(_) | Component::RootDir
                    )
                }) {
                Err(LEAVES.to_string())
            } else {
                let full = opts
                    .root
                    .as_deref()
                    .map_or_else(|| rel.to_path_buf(), |r| r.join(rel));
                std::fs::read(&full)
                    .map(Arc::new)
                    .map_err(|e| e.to_string())
            };
            (p, content)
        })
        .collect()
}

/// The key a node's output is cached under, in memory and on disk: its
/// cache key, so `16`, `16.0` and a left-out default of 16 share one entry.
/// A mesh or a fragment names derived assets after node keys, which spellings
/// do not share, so those are cached under the node's key.
fn lookup_key(def: &NodeDef, key: [u8; 32], cache: [u8; 32]) -> [u8; 32] {
    match def.output {
        PortType::Mesh | PortType::Fragment => key,
        PortType::Field | PortType::Points | PortType::Shapes | PortType::Path => cache,
    }
}

/// A node's report before it runs.
fn skipped(id: &str, kind: &str) -> NodeReport {
    NodeReport {
        id: id.to_string(),
        kind: kind.to_string(),
        key: String::new(),
        cache: CacheState::Skipped,
        ms: 0.0,
        output: None,
        diagnostics: Vec::new(),
        logs: Vec::new(),
    }
}

/// `d`, a problem of node `id`, saying so.
fn in_node(mut d: Diagnostic, id: &str, kind: &str) -> Diagnostic {
    d.message = format!("node `{id}` ({kind}): {}", d.message);
    d
}

/// Everything a node's key hashes.
struct KeyInputs<'a> {
    id: &'a str,
    def: &'a NodeDef,
    /// The version the node was written for.
    version: u32,
    /// Its parameters, migrated to the kind's current shape.
    params: &'a Json,
    inputs: &'a [(&'a str, usize)],
    keys: &'a [Option<[u8; 32]>],
    seed: u64,
    files: &'a HashMap<String, Result<Arc<Vec<u8>>, String>>,
    domain: Option<Domain>,
    /// For a global key: the global spacing (and `keys` are global keys).
    global: Option<f32>,
    /// A cache key: `params` are canonical and `keys` are cache keys.
    cache: bool,
}

/// A node's key, or with [`KeyInputs::cache`] its cache key.
///
/// The key hashes the parameters as written (migrated to the kind's current
/// shape), so `16` and `16.0` give two keys. It names the derived assets the
/// node makes (`derived:<key>`), which the scene's entities reference. For a
/// node at its kind's current version, outside a domain, and with a Rust
/// kind, the bytes hashed are exactly M1's, so caches written before
/// versioning stay valid.
///
/// The cache key hashes the [canonical](NodeDef::canonical_params)
/// parameters and the inputs' cache keys instead, so spellings of the same
/// parameters share it ([`lookup_key`]).
fn node_key(k: &KeyInputs) -> [u8; 32] {
    let KeyInputs {
        id,
        def,
        version,
        params,
        inputs,
        keys,
        seed,
        files,
        domain,
        global,
        cache,
    } = *k;
    let mut h = blake3::Hasher::new();
    let s = |h: &mut blake3::Hasher, t: &str| {
        h.update(&(t.len() as u64).to_le_bytes());
        h.update(t.as_bytes());
    };
    h.update(if cache {
        b"re_proc cache key v1".as_slice()
    } else {
        b"re_proc node key v1".as_slice()
    });
    s(&mut h, def.kind);
    h.update(&version.to_le_bytes());
    s(&mut h, id);
    s(&mut h, &canonical_json(params));
    h.update(&(inputs.len() as u64).to_le_bytes());
    for (port, from) in inputs {
        s(&mut h, port);
        h.update(&keys[*from].unwrap_or([0; 32]));
    }
    if def.uses_seed {
        h.update(&[1]);
        h.update(&seed.to_le_bytes());
    } else {
        h.update(&[0]);
    }
    let mut paths: Vec<&String> = files.keys().collect();
    paths.sort();
    for p in paths {
        s(&mut h, p);
        match &files[p] {
            Ok(bytes) => {
                h.update(&[1]);
                h.update(blake3::hash(bytes).as_bytes());
            }
            Err(_) => {
                h.update(&[0]);
            }
        }
    }
    if def.implementation != [0; 32] {
        h.update(b"implementation");
        h.update(&def.implementation);
    }
    if let Some(d) = domain {
        h.update(b"domain");
        h.update(&d.origin[0].to_le_bytes());
        h.update(&d.origin[1].to_le_bytes());
        h.update(&d.n.to_le_bytes());
        h.update(&d.spacing.to_bits().to_le_bytes());
    }
    if let Some(spacing) = global {
        h.update(b"global");
        h.update(&spacing.to_bits().to_le_bytes());
    }
    *h.finalize().as_bytes()
}

/// The values of a field at points `[x, z]` (metres): its samples on the
/// global grid of `spacing`, from windows that `window` evaluates,
/// interpolated on the grid's triangles (split as
/// [`Heightfield::height_at`](re_geom::Heightfield::height_at) splits them).
/// Every window agrees on shared samples (ADR-0042), so the values do not
/// depend on which window a point is read from; the points of each block of
/// 16 × 16 grid cells share one.
fn sample_field(
    points: &[[f64; 2]],
    spacing: f32,
    mut window: impl FnMut(Domain) -> Result<Arc<Field>, String>,
) -> Result<Vec<f64>, String> {
    const BLOCK: i64 = 16;
    let s = f64::from(spacing);
    if !(s.is_finite() && s > 0.0) {
        return Err(format!(
            "fields are sampled on a grid of {spacing} m; its spacing must be positive"
        ));
    }
    let mut cells = Vec::with_capacity(points.len());
    for p in points {
        if !p
            .iter()
            .all(|v| v.is_finite() && v.abs() <= re_geom::polygon::MAX_COORD)
        {
            return Err(format!(
                "a field cannot be sampled at {p:?}: positions must be finite and within ±{} m",
                re_geom::polygon::MAX_COORD
            ));
        }
        cells.push([(p[0] / s).floor() as i64, (p[1] / s).floor() as i64]);
    }
    let block = |c: [i64; 2]| [c[0].div_euclid(BLOCK), c[1].div_euclid(BLOCK)];
    // Each block's window: the box of its points' cells, one sample wider.
    let mut boxes: BTreeMap<[i64; 2], [i64; 4]> = BTreeMap::new();
    for &c in &cells {
        let b = boxes.entry(block(c)).or_insert([c[0], c[1], c[0], c[1]]);
        *b = [
            b[0].min(c[0]),
            b[1].min(c[1]),
            b[2].max(c[0]),
            b[3].max(c[1]),
        ];
    }
    let mut fields: BTreeMap<[i64; 2], (Domain, Arc<Field>)> = BTreeMap::new();
    for (k, b) in boxes {
        let domain = Domain {
            origin: [b[0], b[1]],
            n: ((b[2] - b[0]).max(b[3] - b[1]) + 2) as u32,
            spacing,
        };
        let field = window(domain)?;
        if field.n() != domain.n {
            return Err(format!(
                "a field over a {0} × {0} window has {1} × {1} samples",
                domain.n,
                field.n()
            ));
        }
        fields.insert(k, (domain, field));
    }
    Ok(points
        .iter()
        .zip(&cells)
        .map(|(p, &c)| {
            let (d, f) = &fields[&block(c)];
            let (x, z) = ((c[0] - d.origin[0]) as u32, (c[1] - d.origin[1]) as u32);
            let h = |dx: u32, dz: u32| f64::from(f.0.sample(x + dx, z + dz));
            let tx = (p[0] / s - c[0] as f64).clamp(0.0, 1.0);
            let tz = (p[1] / s - c[1] as f64).clamp(0.0, 1.0);
            let (h00, h10, h01, h11) = (h(0, 0), h(1, 0), h(0, 1), h(1, 1));
            let v = if tz >= tx {
                h00 + tz * (h01 - h00) + tx * (h11 - h01)
            } else {
                h00 + tx * (h10 - h00) + tz * (h11 - h10)
            };
            v + f64::from(f.0.offset.y)
        })
        .collect())
}

/// The result of [`Evaluator::evaluate`]: the report and every node's output.
#[derive(Debug)]
pub struct Evaluation {
    /// What happened.
    pub report: EvalReport,
    ids: Vec<String>,
    /// The nodes whose outputs make the graph's output, in order
    /// ([`ProcGraph::output_nodes`]).
    output_nodes: Vec<usize>,
    outputs: Vec<Option<Output>>,
    domain: Option<Domain>,
}

impl Evaluation {
    /// True if nothing failed.
    pub fn ok(&self) -> bool {
        self.report.ok()
    }

    /// Node `id`'s output, if it ran.
    pub fn output(&self, id: &str) -> Option<&Output> {
        let i = self.ids.iter().position(|n| n == id)?;
        self.outputs[i].as_ref()
    }

    /// The graph's output: the fragments of every node no edge leaves, in
    /// document order, or of the nodes that feed its declared outputs, in
    /// declaration order ([`ProcGraph::output_nodes`]); a mesh becomes one
    /// entity `<node>/mesh` (in a streamed window it sits at the window's
    /// corner, and an empty one adds nothing). With the derived assets they
    /// reference. Fails with the first error of the evaluation, or `RE4013`
    /// when two entities share a key.
    pub fn fragment(&self) -> Result<Fragment, Diagnostic> {
        if let Some(d) = self.report.first_error() {
            return Err(d.clone());
        }
        let mut out = Fragment::default();
        let mut keys: HashSet<String> = HashSet::new();
        let mut assets: HashSet<String> = HashSet::new();
        for &i in &self.output_nodes {
            let id = &self.ids[i];
            let Some(f) = self.outputs[i]
                .as_ref()
                .and_then(|o| sink_fragment_in(id, o, self.domain.as_ref()))
            else {
                continue;
            };
            for item in &f.items {
                if !keys.insert(item.key.clone()) {
                    return Err(duplicate(&self.report.graph, &item.key));
                }
                out.items.push(item.clone());
            }
            for asset in &f.assets {
                if assets.insert(asset.0.clone()) {
                    out.assets.push(asset.clone());
                }
            }
        }
        Ok(out)
    }

    /// [`Evaluation::fragment`] as a scene fragment: prefabs instantiated
    /// against `registry` (their entities keyed `<item key>/<prefab entity
    /// id>` under the item), plus the derived assets. Prefab problems are
    /// `RE4014`.
    pub fn scene_fragment(
        &self,
        registry: &Registry,
    ) -> Result<(SceneFragment, Vec<(String, DerivedAsset)>), Diagnostic> {
        let fragment = self.fragment()?;
        let nodes = to_scene_fragment(&self.report.graph, &fragment, registry)?;
        Ok((nodes, fragment.assets))
    }
}

fn duplicate(graph: &str, key: &str) -> Diagnostic {
    Diagnostic::error(
        codes::DUPLICATE_KEY,
        format!("graph `{graph}`: key `{key}` is produced twice"),
    )
    .with_hint("connect each fragment to one `fragment.merge`, or leave it unconnected")
}

/// Turns a graph fragment into the scene fragment a generator returns.
pub fn to_scene_fragment(
    graph: &str,
    fragment: &Fragment,
    registry: &Registry,
) -> Result<SceneFragment, Diagnostic> {
    let mut out: SceneFragment = Vec::with_capacity(fragment.items.len());
    let mut seen: HashSet<String> = HashSet::new();
    let mut push = |out: &mut SceneFragment, node: GenNode| {
        if !seen.insert(node.key.clone()) {
            return Err(duplicate(graph, &node.key));
        }
        out.push(node);
        Ok(())
    };
    for item in &fragment.items {
        push(
            &mut out,
            GenNode {
                key: item.key.clone(),
                name: item.name.clone(),
                parent_key: item.parent_key.clone(),
                components: item.components.clone(),
            },
        )?;
        if let Some(prefab) = &item.prefab {
            let nodes = prefab.file.instantiate(registry, &item.key).map_err(|e| {
                Diagnostic::error(
                    codes::PREFAB_FAILED,
                    format!(
                        "graph `{graph}`: prefab `{}` could not be instantiated: {e}",
                        prefab.path
                    ),
                )
                .at(Location::file(prefab.path.clone()))
                .with_related(e.to_diagnostic())
            })?;
            for mut node in nodes {
                if node.parent_key.is_none() {
                    node.parent_key = Some(item.key.clone());
                }
                push(&mut out, node)?;
            }
        }
    }
    Ok(out)
}

/// The hash (hex) of a scene fragment, as in [`EvalReport::fragment_hash`].
pub fn fragment_hash_hex(fragment: &SceneFragment) -> String {
    fragment_hash(fragment).to_hex().to_string()
}

/// A validated graph prepared for evaluation: each node's kind, its
/// parameters migrated to the kind's current shape and in canonical form,
/// its inputs in natural port order, and the levels. A region or cell
/// node's reads and a global node's samples evaluate parts of it
/// ([`Plan::part`]), each prepared once per evaluation however many windows
/// they read.
struct Plan<'a> {
    graph: &'a ProcGraph,
    /// Per node of `graph` (`None` only for a node validation refuses).
    nodes: Arc<Vec<Option<PlanNode<'a>>>>,
    /// The nodes evaluated, in document order.
    members: Vec<usize>,
    /// Per node of `graph`: its position in `members` (and the report).
    slot: Vec<usize>,
    /// `members` in topological levels.
    levels: Vec<Vec<usize>>,
    /// Per node of `graph`: true if its output is the plan's output.
    sinks: Vec<bool>,
    /// The parts prepared so far, by the node each one outputs.
    parts: Mutex<HashMap<usize, Arc<Plan<'a>>>>,
}

struct PlanNode<'a> {
    id: &'a str,
    def: &'a NodeDef,
    /// The version the node was written for.
    version: u32,
    /// Its parameters, migrated to the kind's current shape.
    params: Json,
    /// [`NodeDef::canonical_params`] of `params`.
    canonical: Json,
    /// Its inputs `(port, node)`, in natural port order (edge order within
    /// a port).
    inputs: Vec<(&'a str, usize)>,
    /// `hash64(id)`, which forks its seed.
    salt: u64,
}

impl PlanNode<'_> {
    /// The node input `port` comes from.
    fn input(&self, port: &str) -> Option<usize> {
        self.inputs
            .iter()
            .find(|(p, _)| *p == port)
            .map(|&(_, from)| from)
    }
}

impl<'a> Plan<'a> {
    /// The plan of `graph`, which validates against `registry`.
    fn new(graph: &'a ProcGraph, registry: &'a NodeRegistry) -> Plan<'a> {
        let n = graph.nodes.len();
        let mut inputs: Vec<Vec<(&str, usize)>> = vec![Vec::new(); n];
        for e in &graph.edges {
            if let (Some(from), Some(to)) = (
                graph.nodes.get_index_of(&e.from.0),
                graph.nodes.get_index_of(&e.to.0),
            ) {
                inputs[to].push((e.to.1.as_str(), from));
            }
        }
        let mut sinks = vec![false; n];
        for i in graph.output_nodes() {
            sinks[i] = true;
        }
        let nodes = graph
            .nodes
            .iter()
            .zip(inputs)
            .map(|((id, node), mut inputs)| {
                let def = registry.get(&node.kind)?;
                // Validation guarantees the migration succeeds.
                let params = def.migrate(node.version, &node.params).ok()?.into_owned();
                inputs.sort_by(|a, b| natural_cmp(a.0, b.0));
                Some(PlanNode {
                    id,
                    def,
                    version: node.version,
                    canonical: def.canonical_params(&params),
                    params,
                    inputs,
                    salt: hash64(id),
                })
            })
            .collect();
        Plan {
            graph,
            nodes: Arc::new(nodes),
            members: (0..n).collect(),
            slot: (0..n).collect(),
            levels: graph.levels().unwrap_or_default(),
            sinks,
            parts: Mutex::default(),
        }
    }

    /// The part of this plan whose output is node `root`'s: `root` and its
    /// ancestors.
    fn part(&self, root: usize) -> Arc<Plan<'a>> {
        let mut parts = self.parts.lock().unwrap_or_else(|e| e.into_inner());
        parts
            .entry(root)
            .or_insert_with(|| {
                let n = self.graph.nodes.len();
                let mut keep = vec![false; n];
                keep[root] = true;
                let mut todo = vec![root];
                while let Some(i) = todo.pop() {
                    for &(_, from) in self.nodes[i].iter().flat_map(|node| &node.inputs) {
                        if !keep[from] {
                            keep[from] = true;
                            todo.push(from);
                        }
                    }
                }
                let members: Vec<usize> = (0..n).filter(|&i| keep[i]).collect();
                let mut slot = vec![usize::MAX; n];
                for (s, &i) in members.iter().enumerate() {
                    slot[i] = s;
                }
                let mut sinks = vec![false; n];
                sinks[root] = true;
                Arc::new(Plan {
                    graph: self.graph,
                    nodes: self.nodes.clone(),
                    members,
                    slot,
                    levels: self
                        .levels
                        .iter()
                        .map(|level| level.iter().copied().filter(|&i| keep[i]).collect())
                        .filter(|level: &Vec<usize>| !level.is_empty())
                        .collect(),
                    sinks,
                    parts: Mutex::default(),
                })
            })
            .clone()
    }

    /// The output type of node `index`'s kind.
    fn output_type(&self, index: usize) -> Option<PortType> {
        self.nodes[index].as_ref().map(|node| node.def.output)
    }

    /// The nodes a streamed evaluation runs: all but those whose output
    /// reaches the plan's output only as fields of global nodes, which
    /// sample their fields themselves.
    fn needed(&self) -> Vec<bool> {
        let mut needed = self.sinks.clone();
        for &index in self.levels.iter().rev().flatten() {
            let Some(node) = self.nodes[index].as_ref().filter(|_| needed[index]) else {
                continue;
            };
            let global = node.def.streaming == Streaming::Global;
            for &(_, from) in &node.inputs {
                if !(global && self.output_type(from) == Some(PortType::Field)) {
                    needed[from] = true;
                }
            }
        }
        needed
    }
}
