//! [`GraphStore`]: the world resource the `graph` generator reads.

use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;
use re_core::files::{ProjectFiles, Writable};
use re_core::{Diagnostic, Location};
use re_geom::Heightfield;
use re_reflect::Value;
use re_scene::World;
use re_scene::ecs as bevy_ecs;
use re_types::graph::GraphDoc;
use re_types::mesh::MeshData;
use serde::Deserialize;

use crate::codes;
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
/// A source whose graph file is open in the World (ADR-0044) is generated
/// from the open document instead, so regeneration sees unsaved edits
/// ([`GraphStore::current`]). `re_app` reloads the stored graph from the file
/// when it saves an open document, so after a save and a close the stored
/// graph is the saved one.
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
    /// Open graph documents as parsed (or why they are not graphs), by path,
    /// with the revision parsed.
    open: Mutex<HashMap<String, (u64, ParsedOpen)>>,
}

/// An open document parsed as a graph, or why it is not one.
type ParsedOpen = Result<Arc<GraphDoc>, String>;

/// A procedural source's graph as generation sees it ([`GraphStore::current`]).
#[derive(Clone, Debug)]
pub enum CurrentGraph<'a> {
    /// The document stored for the source.
    Stored(&'a GraphDoc),
    /// The source's graph file, open in the World (possibly with unsaved edits).
    Open(Arc<GraphDoc>),
}

impl Deref for CurrentGraph<'_> {
    type Target = GraphDoc;

    fn deref(&self) -> &GraphDoc {
        match self {
            CurrentGraph::Stored(doc) => doc,
            CurrentGraph::Open(doc) => doc,
        }
    }
}

/// `value` as a graph: it parses as a [`GraphDoc`], passes
/// [`GraphDoc::validate`], and every layout position is finite (a number too
/// large for `f32` would be written back as `null`).
fn parse_graph(value: &serde_json::Value) -> Result<GraphDoc, String> {
    let doc = GraphDoc::deserialize(value).map_err(|e| format!("invalid graph file: {e}"))?;
    doc.validate().map_err(|e| e.to_string())?;
    if let Some((node, _)) = doc
        .layout
        .iter()
        .find(|(_, p)| !p.iter().all(|x| x.is_finite()))
    {
        return Err(format!(
            "the layout position of node `{node}` is not a finite pair of numbers"
        ));
    }
    Ok(doc)
}

/// The validator of open graph documents: the document parses as a
/// [`GraphDoc`], passes [`GraphDoc::validate`], and its layout positions are
/// finite.
pub fn validate_graph_json(value: &serde_json::Value) -> Result<(), String> {
    parse_graph(value).map(|_| ())
}

/// Registers the validator of open graph documents ([`validate_graph_json`])
/// with `world` (ADR-0044) for the suffix `.graph.json` and for the `graph`
/// file of every procedural source in `world` whatever its name, so a
/// transaction that leaves a graph file invalid is rejected. Call it again
/// when sources are added (`re_app` does on every graph load).
pub fn register_doc_validators(world: &mut World) {
    world.register_doc_validator(".graph.json", validate_graph_json);
    let paths: Vec<String> = world
        .ecs
        .query::<&re_scene::GraphSource>()
        .iter(&world.ecs)
        .filter_map(|g| re_scene::doc_path(&g.graph).ok())
        .collect();
    for path in paths {
        world.register_doc_validator(path, validate_graph_json);
    }
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
            open: Mutex::new(self.lock_open().clone()),
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
            open: Mutex::default(),
        }
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

    /// The graph of `source_id` as generation sees it: when its
    /// `GraphSource.graph` file is open in `world` (ADR-0044), the open
    /// document, unsaved edits included; otherwise the stored one. `None`
    /// when there is neither. An open document that is not a valid graph is
    /// an `RE4000` error, not a silent fall back to the stored graph. The
    /// parsed open document is kept until its revision changes.
    pub fn current(
        &self,
        world: &World,
        source_id: &str,
    ) -> Option<Result<CurrentGraph<'_>, Diagnostic>> {
        match self.open_graph(world, source_id) {
            Some((_, Ok(doc))) => Some(Ok(CurrentGraph::Open(doc))),
            Some((path, Err(e))) => Some(Err(Diagnostic::error(
                codes::INVALID_GRAPH,
                format!("the open document {path} is not a valid graph: {e}"),
            )
            .at(Location::file(path))
            .with_hint("fix or undo the edit; generation does not fall back to the file"))),
            None => self.graph(source_id).map(|d| Ok(CurrentGraph::Stored(d))),
        }
    }

    fn open_graph(&self, world: &World, source_id: &str) -> Option<(String, ParsedOpen)> {
        let Ok(Value::Str(path)) = world.get(source_id, "GraphSource", "graph") else {
            return None;
        };
        let path = re_scene::doc_path(&path).ok()?;
        let open = world.doc(&path)?;
        let mut parsed = self.lock_open();
        if let Some((revision, doc)) = parsed.get(&path)
            && *revision == open.revision
        {
            return Some((path, doc.clone()));
        }
        let doc = parse_graph(&open.value).map(Arc::new);
        parsed.insert(path.clone(), (open.revision, doc.clone()));
        Some((path, doc))
    }

    fn lock_open(&self) -> std::sync::MutexGuard<'_, HashMap<String, (u64, ParsedOpen)>> {
        self.open.lock().unwrap_or_else(|e| e.into_inner())
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
