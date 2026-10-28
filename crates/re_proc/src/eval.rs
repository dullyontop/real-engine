//! The [`Evaluator`]: evaluates a [`ProcGraph`] in parallel on the
//! [`TaskPool`], with per-node blake3 keys, a memory cache and an optional
//! postcard disk cache, and reports what it did in an [`EvalReport`]
//! (ARCHITECTURE §5, "Evaluation").
//!
//! **Scheduling.** A node starts as soon as every node it reads is done, so a
//! slow node holds up only the nodes downstream of it. The evaluation's
//! deadline and cancel flag are checked before each node starts, and long
//! nodes check them as they go ([`NodeCtx::check`](crate::NodeCtx::check)),
//! so a deadline or a cancel also stops a node while it runs. What a stopped
//! node returns is dropped, never cached.
//!
//! **Determinism.** A node's output depends only on its key's inputs: its
//! kind and the version it was written for (ADR-0043), its id, its
//! parameters (migrated to the kind's current shape), the evaluation's
//! [`Domain`] if it has one, its inputs' keys, its seed
//! (`Pcg32::from_seed(graph seed).fork(hash64(node id))`, for kinds that use
//! one) and the contents of the files it names. Nodes never share mutable
//! state, so the output is the same on 1 or N threads and in whatever order
//! the nodes run, and adding a node changes no other node's random numbers.
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
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
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
    /// Start no node after this instant, and stop the nodes that check while
    /// they run (`RE4016`).
    pub deadline: Option<Instant>,
    /// The window to evaluate, for streamed evaluation (ADR-0042). Part of
    /// every node's key when set.
    pub domain: Option<Domain>,
    /// Runs every node on the calling thread, one after another, with no
    /// fan-out on the pool: a streaming job is serial (ADR-0042). The output
    /// is the same either way.
    pub serial: bool,
    /// Once this flag is set, start no node, and stop the nodes that check
    /// while they run (`RE4310`); a streaming job's request was cancelled.
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

/// The reserved node name in the edges that carry a graph's inputs:
/// `["$in", name]` (ADR-0061).
const IN: &str = "$in";

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

/// What running a node gave.
enum Outcome {
    Output(Output),
    Failed(Diagnostic),
    /// The node saw its evaluation stop: whatever it returned is dropped.
    Stopped,
}

struct JobResult {
    outcome: Outcome,
    from_disk: bool,
    ms: f64,
    warnings: Vec<Diagnostic>,
    logs: Vec<String>,
}

/// Why an evaluation stopped early.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reason {
    /// Its deadline passed (`RE4016`).
    Deadline = 1,
    /// Its cancel flag was set (`RE4310`).
    Cancelled = 2,
}

/// An evaluation's deadline and cancel flag ([`EvalOptions`]), checked
/// before each node starts and inside nodes that check
/// ([`NodeCtx::check`](crate::NodeCtx::check)). The parts its nodes read and
/// the graphs they evaluate share it, so they stop with it.
pub(crate) struct Stop {
    deadline: Option<Instant>,
    cancel: Option<Arc<AtomicBool>>,
    /// The [`Reason`] once a check saw one, 0 before: a stopped evaluation
    /// stays stopped.
    reason: AtomicU8,
}

impl Stop {
    fn new(opts: &EvalOptions) -> Stop {
        Stop {
            deadline: opts.deadline,
            cancel: opts.cancel.clone(),
            reason: AtomicU8::new(0),
        }
    }

    /// Why the evaluation stops, if it does now.
    pub(crate) fn check(&self) -> Option<Reason> {
        if let Some(r) = self.reason() {
            return Some(r);
        }
        let r = if self.deadline.is_some_and(|d| Instant::now() >= d) {
            Reason::Deadline
        } else if self
            .cancel
            .as_ref()
            .is_some_and(|c| c.load(Ordering::Relaxed))
        {
            Reason::Cancelled
        } else {
            return None;
        };
        // The first reason seen wins.
        let _ = self
            .reason
            .compare_exchange(0, r as u8, Ordering::Relaxed, Ordering::Relaxed);
        self.reason()
    }

    /// The reason a check saw, without checking again.
    pub(crate) fn reason(&self) -> Option<Reason> {
        match self.reason.load(Ordering::Relaxed) {
            1 => Some(Reason::Deadline),
            2 => Some(Reason::Cancelled),
            _ => None,
        }
    }
}

/// A running node's view of its evaluation's [`Stop`]: it remembers whether
/// the node saw the evaluation stop, in which case what the node returns is
/// dropped.
pub(crate) struct NodeStop<'a> {
    stop: &'a Stop,
    seen: AtomicBool,
}

impl<'a> NodeStop<'a> {
    fn new(stop: &'a Stop) -> NodeStop<'a> {
        NodeStop {
            stop,
            seen: AtomicBool::new(false),
        }
    }

    /// True once the evaluation stops, which the node has then seen.
    pub(crate) fn check(&self) -> bool {
        let stopped = self.stop.check().is_some();
        if stopped {
            self.seen.store(true, Ordering::Relaxed);
        }
        stopped
    }

    /// A part the node read, or a graph it evaluated, came back incomplete:
    /// the node saw the stop if the evaluation stopped.
    pub(crate) fn incomplete(&self) {
        if self.stop.reason().is_some() {
            self.seen.store(true, Ordering::Relaxed);
        }
    }

    fn seen(&self) -> bool {
        self.seen.load(Ordering::Relaxed)
    }
}

/// A value given to a graph that a node evaluates
/// ([`NodeCtx::evaluate`](crate::NodeCtx::evaluate)): what the graph's edges
/// from `["$in", name]` carry, with the keys that the keys of the nodes it
/// feeds hash.
#[derive(Clone, Debug)]
pub struct SubInput {
    /// The input's name, as in the edges `["$in", name]`.
    pub name: String,
    /// The value.
    pub output: Output,
    /// Its key, as a node's output has one ([`NodeCtx::key`](crate::NodeCtx::key)).
    pub key: [u8; 32],
    /// Its cache key, which spellings of the same parameters share.
    pub cache_key: [u8; 32],
}

/// What [`NodeCtx::evaluate`](crate::NodeCtx::evaluate) gives: the report of
/// the graph's evaluation, and each node's output and keys.
#[derive(Clone, Debug)]
pub struct SubEvaluation {
    /// What happened: a row per node, with its key, where its output came
    /// from, its diagnostics and its logs.
    pub report: EvalReport,
    /// The graph's node ids, in document order; the lists below follow it.
    pub ids: Vec<String>,
    /// Per node: true if no edge to another node leaves it.
    pub sinks: Vec<bool>,
    /// Per node: its output, if it ran.
    pub outputs: Vec<Option<Output>>,
    /// Per node: its key, once known.
    pub keys: Vec<Option<[u8; 32]>>,
    /// Per node: its cache key, once known.
    pub cache_keys: Vec<Option<[u8; 32]>>,
    /// True when the evaluation stopped (its deadline passed, or it was
    /// cancelled) before every node ran.
    pub stopped: bool,
}

impl SubEvaluation {
    /// True if nothing failed.
    pub fn ok(&self) -> bool {
        self.report.ok()
    }

    fn position(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|n| n == id)
    }

    /// Node `id`'s output, if it ran.
    pub fn output(&self, id: &str) -> Option<&Output> {
        self.outputs[self.position(id)?].as_ref()
    }

    /// Node `id`'s key, once known.
    pub fn key(&self, id: &str) -> Option<[u8; 32]> {
        self.keys[self.position(id)?]
    }

    /// Node `id`'s cache key, once known.
    pub fn cache_key(&self, id: &str) -> Option<[u8; 32]> {
        self.cache_keys[self.position(id)?]
    }
}

/// What [`Evaluator::run`] gives: the report, and per node of the plan's
/// graph its output and keys.
struct RunResult {
    report: EvalReport,
    outputs: Vec<Option<Output>>,
    keys: Vec<Option<[u8; 32]>>,
    caches: Vec<Option<[u8; 32]>>,
    /// The evaluation stopped before every node ran.
    stopped: bool,
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
                sinks: vec![false; graph.nodes.len()],
                outputs: vec![None; graph.nodes.len()],
                domain: opts.domain,
            };
        }
        let plan = Plan::new(graph, &self.registry, &[]);
        let running = self.begin();
        let run = self.run(&plan, opts, running.tick, &Stop::new(opts));
        drop(running);
        let mut report = run.report;
        report.stamp = stamp;
        report.ms = ms_since(start);
        Evaluation {
            report,
            ids,
            sinks: plan.sinks[..graph.nodes.len()].to_vec(),
            outputs: run.outputs,
            domain: opts.domain,
        }
    }

    /// Evaluates `graph` inside the evaluation of `tick` (a node's
    /// [`NodeCtx::evaluate`](crate::NodeCtx::evaluate)): with `seed`,
    /// `inputs` on its `["$in", name]` edges, no window, and the evaluation's
    /// files, deadline and cancel flag. It is not validated here: a graph
    /// kind is validated when it is declared.
    fn evaluate_in(
        &self,
        graph: &ProcGraph,
        seed: u64,
        inputs: &[SubInput],
        opts: &EvalOptions,
        tick: u64,
        stop: &Stop,
    ) -> SubEvaluation {
        let start = Instant::now();
        let opts = EvalOptions {
            seed: Some(seed),
            domain: None,
            global_spacing: None,
            ..opts.clone()
        };
        let plan = Plan::new(graph, &self.registry, inputs);
        let run = self.run(&plan, &opts, tick, stop);
        let mut report = run.report;
        report.ms = ms_since(start);
        SubEvaluation {
            report,
            ids: graph.nodes.keys().cloned().collect(),
            sinks: plan.sinks[..graph.nodes.len()].to_vec(),
            outputs: run.outputs,
            keys: run.keys,
            cache_keys: run.caches,
            stopped: run.stopped,
        }
    }

    /// Evaluates `plan`'s nodes, marking what they use in the memory cache
    /// with `tick`: the evaluation's own, or, for a part a node reads or a
    /// graph it evaluates, the tick of the evaluation that node belongs to.
    /// Each node starts once every node it reads is done: on the pool, or in
    /// topological order on this thread when the evaluation is serial or the
    /// pool has one worker. A worker that waits here for the nodes it started
    /// runs other pool jobs meanwhile, so nested evaluations cannot deadlock.
    fn run(&self, plan: &Plan, opts: &EvalOptions, tick: u64, stop: &Stop) -> RunResult {
        let pool = self.pool();
        let run = Run::new(self, plan, opts, tick, stop, pool);
        if opts.serial || pool.threads() <= 1 {
            for &index in plan.levels.iter().flatten() {
                if !run.node(index) {
                    break;
                }
            }
        } else {
            let run = &run;
            pool.scope(|s| {
                for &index in &plan.members {
                    if run.waiting[index].load(Ordering::Relaxed) == 0 {
                        s.spawn(move |s| run.visit(s, index));
                    }
                }
            });
        }
        run.finish()
    }

    fn run_job(
        &self,
        plan: &Plan,
        job: &Job,
        pool: &TaskPool,
        opts: &EvalOptions,
        tick: u64,
        stop: &Stop,
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
                        outcome: Outcome::Output(output),
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
        let node_stop = NodeStop::new(stop);
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
            let field = self.read_field(plan, from, port, &opts, tick, stop);
            if field.is_err() {
                node_stop.incomplete();
            }
            field
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
                let field = self.read_field(plan, from, port, &opts, tick, stop);
                if field.is_err() {
                    node_stop.incomplete();
                }
                field
            })
            .map(Some)
        };
        let share = |key: [u8; 32], make: &mut dyn FnMut() -> Result<SharedValue, NodeError>| {
            self.shared(key, make)
        };
        let nested = |graph: &ProcGraph, seed: u64, inputs: &[SubInput]| {
            let sub = self.evaluate_in(graph, seed, inputs, opts, tick, stop);
            if sub.stopped {
                node_stop.incomplete();
            }
            sub
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
            nested: Some(&nested as &crate::nodes::Nested),
            stop: Some(&node_stop),
        };
        let output = job.def.eval(&ctx, job.params);
        let logs = ctx.logs.into_inner().unwrap_or_else(|e| e.into_inner());
        if node_stop.seen() {
            // What it returned may be partial: it is neither used nor cached.
            return JobResult {
                outcome: Outcome::Stopped,
                from_disk: false,
                ms: ms_since(start),
                warnings,
                logs,
            };
        }
        let outcome = match output {
            Ok(o) => {
                if let Some(disk) = &self.disk
                    && DiskCache::stores(&o)
                    && let Err(e) = disk.store(&job.cache, &o)
                {
                    warnings.push(cache_warning(id, e));
                }
                Outcome::Output(o)
            }
            Err(e) => Outcome::Failed(Diagnostic::error(
                e.code,
                format!("node `{id}` ({}): {}", job.def.kind, e.message),
            )),
        };
        JobResult {
            outcome,
            from_disk: false,
            ms: ms_since(start),
            warnings,
            logs,
        }
    }

    /// Node `from`'s field, from the part of `plan` it comes from evaluated
    /// with `opts` (a window): what a region or cell node reads and a global
    /// node samples on input `port`. The part runs with the reading node's
    /// `tick`, so it never evicts what that evaluation uses, and stops with
    /// it.
    fn read_field(
        &self,
        plan: &Plan,
        from: usize,
        port: &str,
        opts: &EvalOptions,
        tick: u64,
        stop: &Stop,
    ) -> Result<Arc<Field>, String> {
        let part = plan.part(from);
        let run = self.run(&part, opts, tick, stop);
        match &run.outputs[from] {
            Some(Output::Field(f)) => Ok(f.clone()),
            _ => Err(run
                .report
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

/// One [`Evaluator::run`] in progress. Each node is visited once, when every
/// node it reads is done, and writes its output and keys once, before the
/// nodes that read it start.
struct Run<'r, 'a> {
    evaluator: &'r Evaluator,
    plan: &'r Plan<'a>,
    opts: &'r EvalOptions,
    tick: u64,
    stop: &'r Stop,
    pool: &'r TaskPool,
    start: Instant,
    base: Pcg32,
    /// In a streamed evaluation: the spacing global nodes sample their fields at.
    global: Option<f32>,
    /// Per node: false when the output does not need it ([`Plan::needed`]).
    needed: Vec<bool>,
    /// Per node: its output, its key and cache key (see [`lookup_key`]),
    /// and in a streamed evaluation its global keys (its keys without the
    /// window, when its output is the same for every window).
    outputs: Vec<OnceLock<Output>>,
    keys: Vec<OnceLock<[u8; 32]>>,
    caches: Vec<OnceLock<[u8; 32]>>,
    gkeys: Vec<OnceLock<[u8; 32]>>,
    gcaches: Vec<OnceLock<[u8; 32]>>,
    /// Per node: the nodes that read it, and how many of the nodes it reads
    /// are not done.
    readers: Vec<Vec<usize>>,
    waiting: Vec<AtomicUsize>,
    report: Mutex<EvalReport>,
    /// The first node the stop held back (`false`) or stopped while it ran
    /// (`true`), and when (milliseconds into the run).
    stopped: Mutex<Option<(usize, bool, f64)>>,
}

/// `len` empty slots.
fn unset<T>(len: usize) -> Vec<OnceLock<T>> {
    (0..len).map(|_| OnceLock::new()).collect()
}

/// The values of the first `n` slots.
fn taken<T>(slots: Vec<OnceLock<T>>, n: usize) -> Vec<Option<T>> {
    slots
        .into_iter()
        .take(n)
        .map(OnceLock::into_inner)
        .collect()
}

impl<'r, 'a> Run<'r, 'a> {
    fn new(
        evaluator: &'r Evaluator,
        plan: &'r Plan<'a>,
        opts: &'r EvalOptions,
        tick: u64,
        stop: &'r Stop,
        pool: &'r TaskPool,
    ) -> Run<'r, 'a> {
        let graph = plan.graph;
        let seed = opts.seed.unwrap_or(graph.seed);
        let n = graph.nodes.len();
        let len = plan.nodes.len();
        let report = EvalReport {
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
        let global = opts
            .domain
            .map(|d| opts.global_spacing.unwrap_or(d.spacing));
        let needed = match global {
            Some(_) => plan.needed(),
            None => vec![true; len],
        };
        let (outputs, keys, caches) = (unset(len), unset(len), unset(len));
        // The values bound to `$in` edges are done from the start.
        for (j, b) in plan.bound.iter().enumerate() {
            let _ = outputs[n + j].set(b.output.clone());
            let _ = keys[n + j].set(b.key);
            let _ = caches[n + j].set(b.cache_key);
        }
        let mut readers: Vec<Vec<usize>> = vec![Vec::new(); len];
        let mut waiting = vec![0usize; len];
        for &i in &plan.members {
            for &(_, from) in plan.nodes[i].iter().flat_map(|node| &node.inputs) {
                if from < n {
                    readers[from].push(i);
                    waiting[i] += 1;
                }
            }
        }
        Run {
            evaluator,
            plan,
            opts,
            tick,
            stop,
            pool,
            start: Instant::now(),
            base: Pcg32::from_seed(seed),
            global,
            needed,
            outputs,
            keys,
            caches,
            gkeys: unset(len),
            gcaches: unset(len),
            readers,
            waiting: waiting.into_iter().map(AtomicUsize::new).collect(),
            report: Mutex::new(report),
            stopped: Mutex::new(None),
        }
    }

    /// Visits node `index` on the pool, then starts each node reading it
    /// whose inputs are now all done.
    fn visit<'s>(&'s self, s: &rayon::Scope<'s>, index: usize) {
        if !self.node(index) {
            return;
        }
        for &reader in &self.readers[index] {
            if self.waiting[reader].fetch_sub(1, Ordering::AcqRel) == 1 {
                s.spawn(move |s| self.visit(s, reader));
            }
        }
    }

    fn report(&self) -> std::sync::MutexGuard<'_, EvalReport> {
        self.report.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Node `index`, whose inputs are done: its keys, then its output from
    /// the memory cache or by running it, and its report row. False when the
    /// evaluation stopped before or while it ran, so the nodes reading it do
    /// not start.
    fn node(&self, index: usize) -> bool {
        if self.stop.check().is_some() {
            self.stopped_at(index, false);
            return false;
        }
        let plan = self.plan;
        let Some(node) = &plan.nodes[index] else {
            return true;
        };
        let def = node.def;
        // A global node in a streamed evaluation samples its fields.
        let as_global = self.global.is_some() && def.streaming == Streaming::Global;
        let sampled = |from: usize| as_global && plan.output_type(from) == Some(PortType::Field);
        // An input failed or was skipped, or the output does not need it.
        let runs = self.needed[index]
            && node
                .inputs
                .iter()
                .all(|&(_, from)| self.outputs[from].get().is_some() || sampled(from));
        let global_key = self.global.filter(|_| {
            matches!(
                def.streaming,
                Streaming::Global | Streaming::Pointwise | Streaming::Region
            ) && node
                .inputs
                .iter()
                .all(|&(_, from)| self.gkeys[from].get().is_some())
        });
        if !runs && global_key.is_none() {
            return true;
        }
        let node_seed = self.base.fork(node.salt).next_u64();
        let files = load_files(def, &node.params, self.opts);
        let key_inputs = KeyInputs {
            id: node.id,
            def,
            version: node.version,
            params: &node.params,
            inputs: &node.inputs,
            keys: &self.keys,
            seed: node_seed,
            files: &files,
            domain: self.opts.domain,
            global: None,
            cache: false,
        };
        let cache_inputs = KeyInputs {
            params: &node.canonical,
            keys: &self.caches,
            cache: true,
            ..key_inputs
        };
        if let Some(spacing) = global_key {
            let _ = self.gkeys[index].set(node_key(&KeyInputs {
                keys: &self.gkeys,
                domain: None,
                global: Some(spacing),
                ..key_inputs
            }));
            let _ = self.gcaches[index].set(node_key(&KeyInputs {
                keys: &self.gcaches,
                domain: None,
                global: Some(spacing),
                ..cache_inputs
            }));
        }
        if !runs {
            return true;
        }
        let slot = plan.slot[index];
        let (key, cache) = if as_global {
            let (Some(&key), Some(&cache)) = (self.gkeys[index].get(), self.gcaches[index].get())
            else {
                let mut report = self.report();
                let nr = &mut report.nodes[slot];
                nr.cache = CacheState::Failed;
                nr.diagnostics.push(Diagnostic::error(
                    codes::NOT_STREAMABLE,
                    format!(
                        "node `{}` ({}) is global, but not all its inputs are global, pointwise or region nodes",
                        node.id, def.kind
                    ),
                ));
                return true;
            };
            (key, cache)
        } else {
            (node_key(&key_inputs), node_key(&cache_inputs))
        };
        let _ = self.keys[index].set(key);
        let _ = self.caches[index].set(cache);
        let cache = lookup_key(def, key, cache);
        let hex = blake3::Hash::from_bytes(key).to_hex().to_string();
        // Input checks run whether or not the output is cached, so a
        // warning does not come and go with the cache.
        let mut diagnostics = Vec::new();
        if def.checks_inputs() {
            let values: Vec<InputValue> = node
                .inputs
                .iter()
                .filter_map(|&(port, from)| {
                    Some(InputValue {
                        port,
                        value: self.outputs[from].get()?,
                        from_kind: plan.kind_of(from)?,
                    })
                })
                .collect();
            diagnostics.extend(
                def.check_inputs(&values)
                    .into_iter()
                    .map(|d| in_node(d, node.id, def.kind)),
            );
        }
        let hit = {
            let mut m = self.evaluator.lock_memory();
            m.map.get_mut(&cache).map(|e| {
                e.used = e.used.max(self.tick);
                e.output.clone()
            })
        };
        if let Some(output) = hit {
            let mut report = self.report();
            report.memory_hits += 1;
            let nr = &mut report.nodes[slot];
            nr.key = hex;
            nr.diagnostics = diagnostics;
            nr.cache = CacheState::Memory;
            nr.output = Some(info(&output));
            drop(report);
            let _ = self.outputs[index].set(output);
            return true;
        }
        let job = Job {
            index,
            def,
            params: &node.params,
            key,
            cache,
            seed: node_seed,
            inputs: node
                .inputs
                .iter()
                .filter(|(_, from)| !sampled(*from))
                .map(|&(port, from)| {
                    let output = self.outputs[from].get().expect("checked above");
                    (port.to_string(), output.clone())
                })
                .collect(),
            files,
        };
        let r = self
            .evaluator
            .run_job(plan, &job, self.pool, self.opts, self.tick, self.stop);
        if let Outcome::Output(output) = &r.outcome {
            let bytes = output.size().1;
            let mut m = self.evaluator.lock_memory();
            let entry = MemEntry {
                output: output.clone(),
                bytes,
                used: self.tick,
            };
            if let Some(old) = m.map.insert(cache, entry) {
                m.bytes -= old.bytes;
            }
            m.bytes += bytes;
        }
        let mut report = self.report();
        match &r.outcome {
            Outcome::Output(_) if r.from_disk => report.disk_hits += 1,
            Outcome::Output(_) => report.misses += 1,
            Outcome::Failed(_) | Outcome::Stopped => {}
        }
        let nr = &mut report.nodes[slot];
        nr.key = hex;
        nr.ms = r.ms;
        match r.outcome {
            Outcome::Output(output) => {
                nr.cache = if r.from_disk {
                    CacheState::Disk
                } else {
                    CacheState::Miss
                };
                nr.output = Some(info(&output));
                nr.diagnostics = diagnostics;
                nr.diagnostics.extend(r.warnings);
                nr.logs = r.logs;
                drop(report);
                let _ = self.outputs[index].set(output);
                true
            }
            Outcome::Failed(d) => {
                nr.cache = CacheState::Failed;
                nr.diagnostics = diagnostics;
                nr.diagnostics.insert(0, d);
                nr.diagnostics.extend(r.warnings);
                nr.logs = r.logs;
                true
            }
            Outcome::Stopped => {
                // Skipped, with what it returned, its warnings and its logs.
                drop(report);
                self.stopped_at(index, true);
                false
            }
        }
    }

    /// Records the first node the stop held back or stopped.
    fn stopped_at(&self, index: usize, running: bool) {
        let mut s = self.stopped.lock().unwrap_or_else(|e| e.into_inner());
        if s.is_none() {
            *s = Some((index, running, ms_since(self.start)));
        }
    }

    /// The report (with `RE4016` or `RE4310` if the stop held back or stopped
    /// a node) and each node's output and keys.
    fn finish(self) -> RunResult {
        let graph = self.plan.graph;
        let n = graph.nodes.len();
        let mut report = self.report.into_inner().unwrap_or_else(|e| e.into_inner());
        let stopped = self.stopped.into_inner().unwrap_or_else(|e| e.into_inner());
        if let Some((index, running, ms)) = stopped {
            let id = graph.nodes.get_index(index).map_or("", |(k, _)| k.as_str());
            report.diagnostics.push(match self.stop.reason() {
                Some(Reason::Cancelled) => Diagnostic::error(
                    codes::CANCELLED,
                    format!("evaluation of `{}` was cancelled", graph.id),
                ),
                _ => Diagnostic::error(
                    codes::BUDGET_EXCEEDED,
                    format!(
                        "evaluation of `{}` stopped after {ms:.0} ms, over its time budget, {} node `{id}`",
                        graph.id,
                        if running { "in" } else { "before" }
                    ),
                )
                .with_hint("raise `max_ms`, or make the graph cheaper (smaller fields, larger scatter radius)"),
            });
        }
        report.ms = ms_since(self.start);
        RunResult {
            report,
            outputs: taken(self.outputs, n),
            keys: taken(self.keys, n),
            caches: taken(self.caches, n),
            stopped: stopped.is_some(),
        }
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
    /// Per node: its key (or cache key, or global key), once known.
    keys: &'a [OnceLock<[u8; 32]>],
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
        h.update(&keys[*from].get().copied().unwrap_or([0; 32]));
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
    sinks: Vec<bool>,
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

    /// The graph's output: the fragments of every node no edge leaves (a mesh
    /// becomes one entity `<node>/mesh`; in a streamed window it sits at the
    /// window's corner, and an empty one adds nothing), in document order,
    /// with the derived assets they reference. Fails with the first error of
    /// the evaluation, or `RE4013` when two entities share a key.
    pub fn fragment(&self) -> Result<Fragment, Diagnostic> {
        if let Some(d) = self.report.first_error() {
            return Err(d.clone());
        }
        let mut out = Fragment::default();
        let mut keys: HashSet<String> = HashSet::new();
        let mut assets: HashSet<String> = HashSet::new();
        for (i, id) in self.ids.iter().enumerate() {
            if !self.sinks[i] {
                continue;
            }
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
    /// Per node of `graph` (`None` only for a node validation refuses),
    /// then a `None` per value in `bound`.
    nodes: Arc<Vec<Option<PlanNode<'a>>>>,
    /// The values bound to the graph's `["$in", name]` edges (a graph a node
    /// evaluates): index `graph.nodes.len() + j` of `nodes` is `bound[j]`.
    bound: Arc<Vec<SubInput>>,
    /// The nodes evaluated, in document order.
    members: Vec<usize>,
    /// Per node of `graph`: its position in `members` (and the report).
    slot: Vec<usize>,
    /// `members` in topological levels.
    levels: Vec<Vec<usize>>,
    /// Per index of `nodes`: true if its output is the plan's output.
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
    /// Its inputs `(port, index in the plan's nodes)`, in natural port order
    /// (edge order within a port).
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
    /// The plan of `graph`, which validates against `registry`, with
    /// `bound` on its `["$in", name]` edges.
    fn new(graph: &'a ProcGraph, registry: &'a NodeRegistry, bound: &[SubInput]) -> Plan<'a> {
        let n = graph.nodes.len();
        let mut inputs: Vec<Vec<(&str, usize)>> = vec![Vec::new(); n];
        let mut sinks = vec![true; n];
        sinks.resize(n + bound.len(), false);
        for e in &graph.edges {
            let Some(to) = graph.nodes.get_index_of(&e.to.0) else {
                continue;
            };
            let from = match graph.nodes.get_index_of(&e.from.0) {
                Some(from) => from,
                // The value bound to the graph's input of that name; an
                // input with none is not connected.
                None if e.from.0 == IN => match bound.iter().position(|b| b.name == e.from.1) {
                    Some(j) => n + j,
                    None => continue,
                },
                None => continue,
            };
            inputs[to].push((e.to.1.as_str(), from));
            sinks[from] = false;
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
            .chain(bound.iter().map(|_| None))
            .collect();
        Plan {
            graph,
            nodes: Arc::new(nodes),
            bound: Arc::new(bound.to_vec()),
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
                let mut keep = vec![false; self.nodes.len()];
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
                let mut sinks = vec![false; self.nodes.len()];
                sinks[root] = true;
                Arc::new(Plan {
                    graph: self.graph,
                    nodes: self.nodes.clone(),
                    bound: self.bound.clone(),
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

    /// The bound value at index `index` of `nodes`, if it is one.
    fn bound_at(&self, index: usize) -> Option<&SubInput> {
        self.bound.get(index.checked_sub(self.graph.nodes.len())?)
    }

    /// The output type of node `index`'s kind, or of the value bound there.
    fn output_type(&self, index: usize) -> Option<PortType> {
        match &self.nodes[index] {
            Some(node) => Some(node.def.output),
            None => self.bound_at(index).map(|b| b.output.port_type()),
        }
    }

    /// Node `index`'s kind, or [`IN`] for a bound value.
    fn kind_of(&self, index: usize) -> Option<&'a str> {
        match &self.nodes[index] {
            Some(node) => Some(node.def.kind),
            None => self.bound_at(index).map(|_| IN),
        }
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
