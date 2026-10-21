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

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
use crate::nodes::{Domain, NodeCtx, NodeDef, NodeRegistry, natural_cmp, sink_fragment};
use crate::value::{DerivedAsset, Fragment, Output, PortType};

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
    /// Not run, because an input failed or the budget ran out.
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
    /// to; the current directory when `None`.
    pub root: Option<PathBuf>,
    /// Stop before starting a level of nodes after this instant (`RE4016`).
    pub deadline: Option<Instant>,
    /// The window to evaluate, for streamed evaluation (ADR-0042). Part of
    /// every node's key when set.
    pub domain: Option<Domain>,
}

struct MemEntry {
    output: Output,
    bytes: u64,
    used: u64,
}

#[derive(Default)]
struct MemCache {
    map: HashMap<[u8; 32], MemEntry>,
    bytes: u64,
    tick: u64,
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

/// The output of a node and its key, ready to run.
struct Job<'a> {
    index: usize,
    def: &'a NodeDef,
    params: Json,
    key: [u8; 32],
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

    /// Caches fields, meshes and points in `dir` (`<project>/.real/cache/proc`).
    #[must_use]
    pub fn with_disk_cache(mut self, dir: impl Into<PathBuf>) -> Self {
        self.disk = Some(DiskCache::new(dir.into()));
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

    /// The disk cache directory, if any.
    pub fn disk_cache_dir(&self) -> Option<&Path> {
        self.disk.as_ref().map(DiskCache::dir)
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
        let seed = opts.seed.unwrap_or(graph.seed);
        let pool = self.pool();
        let n = graph.nodes.len();
        let mut report = EvalReport {
            graph: graph.id.clone(),
            seed,
            threads: pool.threads(),
            stamp: graph.version_stamp(&self.registry).to_hex().to_string(),
            nodes: graph
                .nodes
                .iter()
                .map(|(id, node)| NodeReport {
                    id: id.clone(),
                    kind: node.kind.clone(),
                    key: String::new(),
                    cache: CacheState::Skipped,
                    ms: 0.0,
                    output: None,
                    diagnostics: Vec::new(),
                    logs: Vec::new(),
                })
                .collect(),
            ..Default::default()
        };
        let mut outputs: Vec<Option<Output>> = vec![None; n];
        let mut keys: Vec<Option<[u8; 32]>> = vec![None; n];

        let problems = graph.validate(&self.registry);
        if !problems.is_empty() {
            report.diagnostics = problems;
            report.ms = ms_since(start);
            return Evaluation::new(graph, report, outputs);
        }
        let levels = graph.levels().unwrap_or_default();
        let base = Pcg32::from_seed(seed);
        let tick = {
            let mut m = self.memory.lock().unwrap_or_else(|e| e.into_inner());
            m.tick += 1;
            m.tick
        };

        'levels: for level in levels {
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
            let mut jobs: Vec<Job> = Vec::new();
            for &index in &level {
                let (id, node) = graph
                    .nodes
                    .get_index(index)
                    .expect("level indices are valid");
                let Some(def) = self.registry.get(&node.kind) else {
                    continue;
                };
                let mut inputs = graph.inputs_of(index);
                inputs.sort_by(|a, b| natural_cmp(a.0, b.0));
                if inputs.iter().any(|(_, from)| outputs[*from].is_none()) {
                    continue; // An input failed or was skipped.
                }
                let node_seed = base.fork(hash64(id)).next_u64();
                // Validation above guarantees the migration succeeds.
                let Ok(params) = def.migrate(node.version, &node.params) else {
                    continue;
                };
                let params = params.into_owned();
                let files = load_files(def, &params, opts.root.as_deref());
                let key = node_key(&KeyInputs {
                    id,
                    def,
                    version: node.version,
                    params: &params,
                    inputs: &inputs,
                    keys: &keys,
                    seed: node_seed,
                    files: &files,
                    domain: opts.domain,
                });
                keys[index] = Some(key);
                report.nodes[index].key = blake3::Hash::from_bytes(key).to_hex().to_string();
                let hit = {
                    let mut m = self.memory.lock().unwrap_or_else(|e| e.into_inner());
                    m.map.get_mut(&key).map(|e| {
                        e.used = tick;
                        e.output.clone()
                    })
                };
                if let Some(output) = hit {
                    report.nodes[index].cache = CacheState::Memory;
                    report.nodes[index].output = Some(info(&output));
                    report.memory_hits += 1;
                    outputs[index] = Some(output);
                    continue;
                }
                let inputs = inputs
                    .into_iter()
                    .map(|(port, from)| {
                        (
                            port.to_string(),
                            outputs[from].clone().expect("checked above"),
                        )
                    })
                    .collect();
                jobs.push(Job {
                    index,
                    def,
                    params,
                    key,
                    seed: node_seed,
                    inputs,
                    files,
                });
            }
            let results: Vec<Mutex<Option<JobResult>>> =
                jobs.iter().map(|_| Mutex::new(None)).collect();
            if jobs.len() == 1 {
                *results[0].lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(self.run_job(graph, &jobs[0], pool, opts.domain));
            } else if !jobs.is_empty() {
                pool.scope(|s| {
                    for (job, slot) in jobs.iter().zip(&results) {
                        s.spawn(move |_| {
                            let r = self.run_job(graph, job, pool, opts.domain);
                            *slot.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                        });
                    }
                });
            }
            let mut m = self.memory.lock().unwrap_or_else(|e| e.into_inner());
            for (job, slot) in jobs.iter().zip(results) {
                let Some(r) = slot.into_inner().unwrap_or_else(|e| e.into_inner()) else {
                    continue;
                };
                let nr = &mut report.nodes[job.index];
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
                            job.key,
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
        self.evict(tick);
        report.ms = ms_since(start);
        Evaluation::new(graph, report, outputs)
    }

    fn run_job(
        &self,
        graph: &ProcGraph,
        job: &Job,
        pool: &TaskPool,
        domain: Option<Domain>,
    ) -> JobResult {
        let start = Instant::now();
        let (id, node) = graph
            .nodes
            .get_index(job.index)
            .expect("job indices are valid");
        let mut warnings = Vec::new();
        if let Some(disk) = &self.disk
            && job.def.output != PortType::Fragment
        {
            match disk.load(&job.key) {
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
        let ctx = NodeCtx {
            node_id: id,
            key: job.key,
            version: node.version,
            domain,
            seed: job.seed,
            inputs: job.inputs.clone(),
            files: job.files.clone(),
            pool,
            logs: Default::default(),
        };
        let output = job.def.eval(&ctx, &job.params).map_err(|e| {
            Diagnostic::error(
                e.code,
                format!("node `{id}` ({}): {}", node.kind, e.message),
            )
        });
        if let (Ok(o), Some(disk)) = (&output, &self.disk)
            && DiskCache::stores(o)
            && let Err(e) = disk.store(&job.key, o)
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

    /// Drops least recently used entries until the cache fits its budget;
    /// entries used by the evaluation that just ran (`tick`) are kept.
    fn evict(&self, tick: u64) {
        let mut m = self.memory.lock().unwrap_or_else(|e| e.into_inner());
        if m.bytes <= self.budget {
            return;
        }
        let mut old: Vec<([u8; 32], u64, u64)> = m
            .map
            .iter()
            .filter(|(_, e)| e.used < tick)
            .map(|(k, e)| (*k, e.used, e.bytes))
            .collect();
        old.sort_by_key(|(k, used, _)| (*used, *k));
        for (k, _, bytes) in old {
            if m.bytes <= self.budget {
                break;
            }
            m.map.remove(&k);
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
    root: Option<&Path>,
) -> HashMap<String, Result<Arc<Vec<u8>>, String>> {
    file_paths(def, params)
        .into_iter()
        .map(|p| {
            let rel = Path::new(&p);
            let content = if rel.is_absolute()
                || rel.components().any(|c| {
                    matches!(
                        c,
                        Component::ParentDir | Component::Prefix(_) | Component::RootDir
                    )
                }) {
                Err("paths are relative to the project root and may not leave it".to_string())
            } else {
                let full = root.map_or_else(|| rel.to_path_buf(), |r| r.join(rel));
                std::fs::read(&full)
                    .map(Arc::new)
                    .map_err(|e| e.to_string())
            };
            (p, content)
        })
        .collect()
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
}

/// A node's key. For a node at its kind's current version, outside a
/// domain, and with a Rust kind, the bytes hashed are exactly M1's, so
/// caches written before versioning stay valid.
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
    } = *k;
    let mut h = blake3::Hasher::new();
    let s = |h: &mut blake3::Hasher, t: &str| {
        h.update(&(t.len() as u64).to_le_bytes());
        h.update(t.as_bytes());
    };
    h.update(b"re_proc node key v1");
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
    *h.finalize().as_bytes()
}

/// The result of [`Evaluator::evaluate`]: the report and every node's output.
#[derive(Debug)]
pub struct Evaluation {
    /// What happened.
    pub report: EvalReport,
    ids: Vec<String>,
    sinks: Vec<bool>,
    outputs: Vec<Option<Output>>,
}

impl Evaluation {
    fn new(graph: &ProcGraph, report: EvalReport, outputs: Vec<Option<Output>>) -> Self {
        Evaluation {
            report,
            ids: graph.nodes.keys().cloned().collect(),
            sinks: (0..graph.nodes.len()).map(|i| graph.is_sink(i)).collect(),
            outputs,
        }
    }

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
    /// becomes one entity `<node>/mesh`), in document order, with the derived
    /// assets they reference. Fails with the first error of the evaluation,
    /// or `RE4013` when two entities share a key.
    pub fn fragment(&self) -> Result<Fragment, Diagnostic> {
        if let Some(d) = self.report.first_error() {
            return Err(d.clone());
        }
        let mut out = Fragment::default();
        let mut seen: HashMap<String, ()> = HashMap::new();
        for (i, id) in self.ids.iter().enumerate() {
            if !self.sinks[i] {
                continue;
            }
            let Some(f) = self.outputs[i].as_ref().and_then(|o| sink_fragment(id, o)) else {
                continue;
            };
            for item in &f.items {
                if seen.insert(item.key.clone(), ()).is_some() {
                    return Err(duplicate(&self.report.graph, &item.key));
                }
                out.items.push(item.clone());
            }
            for asset in &f.assets {
                if !out.assets.iter().any(|(n, _)| n == &asset.0) {
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
    let mut seen: HashMap<String, ()> = HashMap::new();
    let mut push = |out: &mut SceneFragment, node: GenNode| {
        if seen.insert(node.key.clone(), ()).is_some() {
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
