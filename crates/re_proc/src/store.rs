//! [`GraphStore`]: the world resource the `graph` generator reads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use re_core::files::{ProjectFiles, Writable};
use re_geom::Heightfield;
use re_scene::ecs as bevy_ecs;
use re_types::graph::GraphDoc;
use re_types::mesh::MeshData;

use crate::NodeRegistry;
use crate::eval::{EvalReport, Evaluator};
use crate::value::DerivedAsset;

/// Limits on one evaluation (`graph.generate`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    /// Stop before a level of nodes that would start after this many milliseconds.
    pub max_ms: Option<u64>,
    /// Fail when the output has more entities than this.
    pub max_entities: Option<usize>,
}

/// The graph documents of a world's procedural sources, by source id, plus
/// the shared [`Evaluator`], the derived assets the graphs made and each
/// source's last [`EvalReport`].
///
/// `re_app` fills it when a scene loads (from each `GraphSource`'s `graph`
/// file or `builder` script) and inserts it as a world resource; the `graph`
/// generator reads it through `GenContext::resource`.
///
/// **Cloning** (for a world copy, `World::fork`): the documents, the derived
/// assets (cheap: each is an `Arc`) and the last reports are copied, so
/// generating or releasing in the copy never changes the original; the
/// evaluator (its caches, keyed by content hash) is shared.
#[derive(bevy_ecs::resource::Resource)]
pub struct GraphStore {
    graphs: IndexMap<String, GraphDoc>,
    evaluator: Arc<Evaluator>,
    root: Option<PathBuf>,
    files: Option<ProjectFiles>,
    derived: Arc<Mutex<HashMap<String, DerivedAsset>>>,
    reports: Mutex<HashMap<String, EvalReport>>,
    budget: Budget,
}

impl Default for GraphStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for GraphStore {
    fn clone(&self) -> Self {
        GraphStore {
            graphs: self.graphs.clone(),
            evaluator: self.evaluator.clone(),
            root: self.root.clone(),
            files: self.files.clone(),
            derived: Arc::new(Mutex::new(self.lock_derived().clone())),
            reports: Mutex::new(self.lock_reports().clone()),
            budget: self.budget,
        }
    }
}

impl std::fmt::Debug for GraphStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphStore")
            .field("graphs", &self.graphs.keys().collect::<Vec<_>>())
            .field("root", &self.root)
            .field("budget", &self.budget)
            .finish_non_exhaustive()
    }
}

impl GraphStore {
    /// An empty store with the M1 nodes, no disk cache, and file paths
    /// relative to the current directory.
    pub fn new() -> Self {
        Self::with_evaluator(Arc::new(Evaluator::default()), None)
    }

    /// A store for the project at `root`: prefab paths are relative to it and
    /// node outputs are cached in `<root>/.real/cache/proc`.
    pub fn for_project(root: impl Into<PathBuf>) -> Self {
        Self::for_files(ProjectFiles::project(root))
    }

    /// A store reading file parameters through `files` (ADR-0037) and caching
    /// node outputs in `proc/` of its cache folder.
    pub fn for_files(files: ProjectFiles) -> Self {
        let evaluator =
            Evaluator::default().with_disk_cache_in(files.writable(Writable::Cache).sub("proc"));
        let mut store = Self::with_evaluator(
            Arc::new(evaluator),
            files.project_dir().map(Path::to_path_buf),
        );
        store.files = Some(files);
        store
    }

    /// A store using `evaluator`, with file paths relative to `root`.
    pub fn with_evaluator(evaluator: Arc<Evaluator>, root: Option<PathBuf>) -> Self {
        GraphStore {
            graphs: IndexMap::new(),
            evaluator,
            root,
            files: None,
            derived: Arc::default(),
            reports: Mutex::default(),
            budget: Budget::default(),
        }
    }

    /// Evaluates with `registry` from now on (the M1 kinds plus a project's
    /// Luau kinds, ADR-0043 §4), keeping the disk cache. A source whose
    /// graph uses a kind that changed gets a new version stamp, so it
    /// regenerates at the next `regenerate_changed`.
    pub fn set_registry(&mut self, registry: Arc<NodeRegistry>) {
        self.evaluator = Arc::new(self.evaluator.with_registry(registry));
    }

    /// Stores (or replaces) the graph of procedural source `source_id`.
    pub fn insert(&mut self, source_id: impl Into<String>, doc: GraphDoc) {
        self.graphs.insert(source_id.into(), doc);
    }

    /// Removes a source's graph.
    pub fn remove(&mut self, source_id: &str) -> Option<GraphDoc> {
        self.lock_reports().remove(source_id);
        self.graphs.shift_remove(source_id)
    }

    /// The graph of `source_id`.
    pub fn graph(&self, source_id: &str) -> Option<&GraphDoc> {
        self.graphs.get(source_id)
    }

    /// Every `(source id, graph)`, in insertion order.
    pub fn graphs(&self) -> impl Iterator<Item = (&str, &GraphDoc)> {
        self.graphs.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// The evaluator.
    pub fn evaluator(&self) -> &Arc<Evaluator> {
        &self.evaluator
    }

    /// The project root file parameters are relative to.
    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }

    /// The project files file parameters are read through, when the store was
    /// made with [`for_files`](Self::for_files) or
    /// [`for_project`](Self::for_project).
    pub fn files(&self) -> Option<&ProjectFiles> {
        self.files.as_ref()
    }

    /// The budget applied to evaluations through this store.
    pub fn budget(&self) -> Budget {
        self.budget
    }

    /// Sets the budget (`graph.generate` sets one on the copy it evaluates).
    pub fn set_budget(&mut self, budget: Budget) {
        self.budget = budget;
    }

    fn lock_reports(&self) -> std::sync::MutexGuard<'_, HashMap<String, EvalReport>> {
        self.reports.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The last evaluation of `source_id`'s graph, if any.
    pub fn report(&self, source_id: &str) -> Option<EvalReport> {
        self.lock_reports().get(source_id).cloned()
    }

    pub(crate) fn set_report(&self, source_id: &str, report: EvalReport) {
        self.lock_reports().insert(source_id.to_string(), report);
    }

    fn lock_derived(&self) -> std::sync::MutexGuard<'_, HashMap<String, DerivedAsset>> {
        self.derived.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn add_derived(&self, assets: Vec<(String, DerivedAsset)>) {
        let mut d = self.lock_derived();
        for (name, asset) in assets {
            d.entry(name).or_insert(asset);
        }
    }

    /// The derived asset named `name` (`derived:<64 hex digits>`), as
    /// written in `MeshRef.asset` or `Collider.source`.
    pub fn derived(&self, name: &str) -> Option<DerivedAsset> {
        self.lock_derived().get(name).cloned()
    }

    /// The derived mesh named `name`, for `Renderer::upload_mesh`.
    pub fn mesh(&self, name: &str) -> Option<Arc<MeshData>> {
        match self.derived(name)? {
            DerivedAsset::Mesh(m) => Some(m),
            DerivedAsset::Heightfield(_) => None,
        }
    }

    /// The derived heightfield named `name`, for a `Collider` with
    /// `shape = Heightfield` (`Heightfield::to_shape` gives the physics shape).
    pub fn heightfield(&self, name: &str) -> Option<Arc<Heightfield>> {
        match self.derived(name)? {
            DerivedAsset::Heightfield(h) => Some(h),
            DerivedAsset::Mesh(_) => None,
        }
    }

    /// Forgets a derived asset (after `RegenReport::released_assets` named
    /// it), and the heightfield collider of a released terrain mesh. A later
    /// generation that uses it again stores it again.
    pub fn release(&self, name: &str) -> bool {
        let mut d = self.lock_derived();
        if let Some(collider) = crate::value::collider_name(name) {
            d.remove(&collider);
        }
        d.remove(name).is_some()
    }

    /// The number of derived assets held.
    pub fn derived_count(&self) -> usize {
        self.lock_derived().len()
    }
}
