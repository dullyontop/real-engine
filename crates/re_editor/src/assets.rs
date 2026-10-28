//! The asset index (ADR-0064 §7): the project's files by kind, for the
//! Assets panel, the asset pickers and the Scenes menu.
//!
//! It is built on a worker thread when the editor first draws a project and
//! kept current from the file changes the App applies
//! ([`App::take_file_changes`]: the watcher's, the save's and those commands
//! make), so there is no second watcher. The worker walks the project,
//! leaving out the engine's `.real/`, other hidden folders and linked
//! folders, and reads each file of a known kind: its size and, for textures
//! and models, its sidecar's asset id and its import state
//! ([`re_assets::import_key`]). A model's animations and skins are assets of
//! their own, `clip` and `skin`, named `<model>#<name>`. Sidecars are not
//! listed: a change to one is a change to its file.
//!
//! *Loaded* and *failed* are the App's: [`AssetIndex::state`] reads them from
//! its asset server and media when asked, over what the worker found.
//!
//! The worker also makes the Assets panel's thumbnails ([`crate::thumbs`]),
//! after the index's own jobs: a change is looked at before the next
//! thumbnail is made.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use re_app::App;
use re_assets::{AssetChange, AssetMeta, AssetsConfig, ProcessedCache};
use re_core::{Diagnostic, ProjectFiles};
use re_render::Image;

use crate::thumbs::{Made, MaterialJob, Thumbs};

/// A kind of asset, as `x-real-asset` and the field hints name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AssetKind {
    /// `*.material.json`.
    Material,
    /// `*.prefab.json`.
    Prefab,
    /// `*.scene.json`.
    Scene,
    /// `*.graph.json`, a procedural graph.
    Graph,
    /// `*.animgraph.json`, an animation graph.
    AnimGraph,
    /// `*.luau`.
    Script,
    /// PNG and JPEG images.
    Texture,
    /// glTF and GLB files.
    Model,
    /// A model's animation, `<model>#<name>`.
    Clip,
    /// A model's skin, `<model>#<name>`.
    Skin,
    /// What `re_audio` plays: WAV and Ogg Vorbis files.
    Audio,
}

impl AssetKind {
    /// Every kind, in the order the Assets panel's filter lists them.
    pub const ALL: [AssetKind; 11] = [
        AssetKind::Scene,
        AssetKind::Prefab,
        AssetKind::Material,
        AssetKind::Texture,
        AssetKind::Model,
        AssetKind::Clip,
        AssetKind::Skin,
        AssetKind::Graph,
        AssetKind::AnimGraph,
        AssetKind::Script,
        AssetKind::Audio,
    ];

    /// Its name, as `x-real-asset` and the field hints write it.
    pub fn name(self) -> &'static str {
        match self {
            AssetKind::Material => "material",
            AssetKind::Prefab => "prefab",
            AssetKind::Scene => "scene",
            AssetKind::Graph => "graph",
            AssetKind::AnimGraph => "animgraph",
            AssetKind::Script => "script",
            AssetKind::Texture => "texture",
            AssetKind::Model => "model",
            AssetKind::Clip => "clip",
            AssetKind::Skin => "skin",
            AssetKind::Audio => "audio",
        }
    }

    /// The kind named `name`.
    pub fn from_name(name: &str) -> Option<AssetKind> {
        AssetKind::ALL.into_iter().find(|k| k.name() == name)
    }

    /// The suffixes of its files; none for `clip` and `skin`, which are
    /// parts of model files.
    pub fn suffixes(self) -> &'static [&'static str] {
        match self {
            AssetKind::Material => &[".material.json"],
            AssetKind::Prefab => &[".prefab.json"],
            AssetKind::Scene => &[".scene.json"],
            AssetKind::Graph => &[".graph.json"],
            AssetKind::AnimGraph => &[".animgraph.json"],
            AssetKind::Script => &[".luau"],
            AssetKind::Texture => &[".png", ".jpg", ".jpeg"],
            AssetKind::Model => &[".gltf", ".glb"],
            AssetKind::Audio => &[".wav", ".ogg"],
            AssetKind::Clip | AssetKind::Skin => &[],
        }
    }

    /// The kind of the project file `path`, by its suffix in any letter
    /// case; none for a sidecar or a file of no kind.
    pub fn of_path(path: &str) -> Option<AssetKind> {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(SIDECAR) {
            return None;
        }
        AssetKind::ALL
            .into_iter()
            .find(|k| k.suffixes().iter().any(|s| lower.ends_with(s)))
    }

    /// True for the kinds the asset server imports through the processed
    /// cache, which have sidecars and import states.
    pub fn imported(self) -> bool {
        matches!(self, AssetKind::Texture | AssetKind::Model)
    }
}

/// The suffix of an asset's sidecar (`<file>.meta.json`).
const SIDECAR: &str = ".meta.json";

/// Where a texture or model is in its import (ADR-0064 §7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportState {
    /// No sidecar yet: it has never been imported.
    Source,
    /// The processed cache holds its import for the current importer,
    /// settings and bytes.
    Imported,
    /// It has a sidecar but the cache holds no import of it as it is now
    /// (it changed, or the importer did): it imports at its next load.
    Stale,
    /// The App's asset server holds it, at this version (0 when first
    /// loaded, plus one per re-import).
    Loaded(u32),
    /// Its last load failed, or its sidecar cannot be read: why.
    Failed(String),
}

impl ImportState {
    /// Its name, as the Assets panel shows it.
    pub fn name(&self) -> &'static str {
        match self {
            ImportState::Source => "source",
            ImportState::Imported => "imported",
            ImportState::Stale => "stale",
            ImportState::Loaded(_) => "loaded",
            ImportState::Failed(_) => "failed",
        }
    }
}

/// A project file the index knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetFile {
    /// Relative to the project.
    pub path: String,
    /// What it is.
    pub kind: AssetKind,
    /// Its size in bytes.
    pub size: u64,
    /// The asset id its sidecar gives it.
    pub id: Option<String>,
    /// For a texture or a model, its import state as the worker found it:
    /// source, imported, stale, or failed when its sidecar cannot be read.
    pub import: Option<ImportState>,
    /// A model's animations, by the names `<model>#<name>` gives them.
    pub clips: Vec<String>,
    /// A model's skins, by name.
    pub skins: Vec<String>,
}

/// What the worker is asked to do.
enum Job {
    /// Walk the whole project.
    Scan,
    /// Look at these paths again: changed files, folders, sidecars.
    Paths(Vec<String>),
    /// Make these thumbnails: each ask's number, path and kind.
    Thumbs(Vec<(u64, String, AssetKind)>),
    /// Write a thumbnail the preview's renderer drew to the disk cache,
    /// under its key ([`Done::Stored`] when it is).
    Store(String, Image),
}

/// What the worker found: for each path it looked at, the files at and
/// under it now. `""` is the whole project.
type Found = Vec<(String, Vec<AssetFile>)>;

/// What the worker answers.
enum Done {
    /// What a [`Job::Scan`] or [`Job::Paths`] found.
    Found(Found),
    /// A thumbnail: the ask's number, the path, and what was made or why
    /// nothing could be.
    Thumb(u64, String, Result<Made, String>),
    /// A [`Job::Store`] is done.
    Stored,
}

/// The assets of each kind, sorted (a model's clips and skins in its order),
/// as the asset pickers list them; made again when the index changes.
#[derive(Debug, Default)]
pub(crate) struct KindLists(HashMap<AssetKind, Arc<[String]>>);

impl KindLists {
    /// The assets of the kind named `kind` (as `x-real-asset` and the field
    /// hints name it); none for an unknown kind.
    pub(crate) fn of(&self, kind: &str) -> Arc<[String]> {
        AssetKind::from_name(kind)
            .and_then(|k| self.0.get(&k))
            .cloned()
            .unwrap_or_else(|| Arc::new([]))
    }
}

/// The worker thread's end of the index.
struct Worker {
    jobs: mpsc::Sender<Job>,
    done: mpsc::Receiver<Done>,
}

/// The project's files by kind, built and kept current by a worker thread.
#[derive(Default)]
pub struct AssetIndex {
    /// Every file, by path.
    files: BTreeMap<String, AssetFile>,
    /// Every folder holding a file, at any depth.
    folders: BTreeSet<String>,
    /// The project the index is of, once started.
    root: Option<PathBuf>,
    worker: Option<Worker>,
    /// Jobs sent and not answered yet.
    waiting: usize,
    /// Thumbnails sent to the disk cache and not written yet.
    storing: usize,
    /// Goes up with every change, so lists made from the index are kept
    /// until it changes.
    revision: u64,
    /// The Assets panel's thumbnails.
    thumbs: Thumbs,
    /// The assets of each kind, for the pickers.
    lists: Arc<KindLists>,
}

impl std::fmt::Debug for AssetIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AssetIndex")
            .field("files", &self.files.len())
            .field("root", &self.root)
            .field("waiting", &self.waiting)
            .field("revision", &self.revision)
            .field("thumbs", &self.thumbs)
            .finish_non_exhaustive()
    }
}

impl AssetIndex {
    /// Keeps the index current, once a frame: starts it on `app`'s project
    /// (again when the project changes), passes the file changes the App
    /// applied since the last frame to the worker and takes in what it
    /// found.
    pub(crate) fn poll(&mut self, app: &mut App) {
        if self.root.as_deref() != Some(app.project()) {
            self.start(app);
        }
        let changes = app.take_file_changes();
        // A changed `[assets]` section changes every import key.
        if changes
            .iter()
            .any(|c| paths(c).any(|p| p == re_app::CONFIG_FILE))
        {
            self.start(app);
        } else {
            // The engine's own files (`.real/`, where thumbnails are written)
            // and other hidden ones are not assets.
            let mut look: Vec<String> = changes
                .iter()
                .flat_map(paths)
                .filter(|p| !hidden(p))
                .map(str::to_owned)
                .collect();
            look.sort();
            look.dedup();
            if !look.is_empty() {
                self.send(Job::Paths(look));
            }
        }
        let asks = self.thumbs.begin_frame();
        if !asks.is_empty()
            && let Some(w) = &self.worker
        {
            let _ = w.jobs.send(Job::Thumbs(asks));
        }
        self.take(false);
    }

    /// Waits until the worker has answered every job sent so far, the
    /// thumbnails asked for and their disk writes included, taking in what it
    /// found (for tests, and tools that need the index complete).
    pub(crate) fn wait(&mut self, app: &mut App) {
        self.poll(app);
        self.take(true);
    }

    /// The thumbnail of the asset at `path`, of `kind`, to draw now: its
    /// texture and its size in pixels. It is asked for when it is not made
    /// yet ([`crate::thumbs`]).
    pub(crate) fn thumbnail(
        &self,
        path: &str,
        kind: AssetKind,
    ) -> Option<(egui::TextureId, egui::Vec2)> {
        self.thumbs.get(path, kind)
    }

    /// True when the thumbnail of the asset at `path` is made and shown.
    pub fn has_thumbnail(&self, path: &str) -> bool {
        self.thumbs.is_shown(path)
    }

    /// Makes egui textures of the thumbnails made since the last frame, and
    /// returns the notes (`RE8853`) of those that could not be made.
    pub(crate) fn upload_thumbnails(&mut self, ctx: &egui::Context) -> Vec<Diagnostic> {
        self.thumbs.upload(ctx)
    }

    /// The next material thumbnail for the preview's renderer to draw.
    pub(crate) fn next_material_thumbnail(&mut self) -> Option<MaterialJob> {
        self.thumbs.next_material()
    }

    /// Takes in the material thumbnail the preview's renderer drew for
    /// `job`, or why it could not, and has the worker write it to the disk
    /// cache.
    pub(crate) fn material_thumbnail_drawn(
        &mut self,
        job: &MaterialJob,
        drawn: Result<Image, String>,
    ) {
        if let Some((key, image)) = self.thumbs.material_drawn(job, drawn)
            && let Some(w) = &self.worker
            && w.jobs.send(Job::Store(key, image)).is_ok()
        {
            self.storing += 1;
        }
    }

    /// True once the worker has answered every job sent.
    pub fn ready(&self) -> bool {
        self.root.is_some() && self.waiting == 0
    }

    /// Goes up with every change to the index.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Every file, sorted by path.
    pub fn files(&self) -> impl Iterator<Item = &AssetFile> {
        self.files.values()
    }

    /// The file at `path`.
    pub fn file(&self, path: &str) -> Option<&AssetFile> {
        self.files.get(path)
    }

    /// Every folder that holds a file, at any depth, sorted (`a` before
    /// `a/b`); the project itself is not one.
    pub fn folders(&self) -> &BTreeSet<String> {
        &self.folders
    }

    /// The assets of `kind`, sorted: project paths, or for clips and skins
    /// `<model>#<name>`.
    pub fn of_kind(&self, kind: AssetKind) -> Vec<String> {
        self.lists.of(kind.name()).to_vec()
    }

    /// The assets of each kind, as the asset pickers list them: a list that
    /// stays as it is while the index changes.
    pub(crate) fn lists(&self) -> Arc<KindLists> {
        Arc::clone(&self.lists)
    }

    /// The import state of the asset at `path` (a file, or `<model>#<name>`)
    /// now: *failed* when the App's last load of it failed, *loaded* while
    /// its asset server holds it, else what the worker found. `None` for an
    /// asset with no import state (not a texture, model or material, or
    /// not known).
    pub fn state(&self, app: &App, path: &str) -> Option<ImportState> {
        let media = app.media();
        if let Some(d) = media.failure(path) {
            return Some(ImportState::Failed(d.message.clone()));
        }
        let file = path.split_once('#').map_or(path, |(file, _)| file);
        let server = media.asset_server();
        if let Some(info) = server.loaded(file).and_then(|a| server.info(a)) {
            return Some(ImportState::Loaded(info.version));
        }
        self.files.get(file).and_then(|f| f.import.clone())
    }

    /// Starts again on `app`'s project: a new worker walks it.
    fn start(&mut self, app: &mut App) {
        *self = AssetIndex {
            root: Some(app.project().to_path_buf()),
            revision: self.revision + 1,
            ..AssetIndex::default()
        };
        app.keep_file_changes(true);
        let Some(dir) = app.files().project_dir().map(Path::to_path_buf) else {
            return;
        };
        let files = app.files().clone();
        let server = app.media().asset_server();
        let look = Look {
            config: server.config(),
            cache: ProcessedCache::in_dir(server.cache().dir().clone()),
            thumbs: dir.join(".real/cache/thumbs"),
            dir,
            files,
        };
        let (jobs, inbox) = mpsc::channel();
        let (outbox, done) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("re_editor asset index".into())
            .spawn(move || look.serve(&inbox, &outbox));
        if spawned.is_ok() {
            self.worker = Some(Worker { jobs, done });
            self.send(Job::Scan);
        }
    }

    fn send(&mut self, job: Job) {
        if let Some(w) = &self.worker
            && w.jobs.send(job).is_ok()
        {
            self.waiting += 1;
        }
    }

    /// Takes in what the worker answered; with `block`, until it has
    /// answered every job and ask.
    fn take(&mut self, block: bool) {
        loop {
            let Some(w) = &self.worker else {
                return;
            };
            let due = self.waiting > 0 || self.storing > 0 || self.thumbs.due() > 0;
            let done = if block && due {
                w.done.recv().map_err(|_| mpsc::TryRecvError::Disconnected)
            } else {
                w.done.try_recv()
            };
            match done {
                Ok(Done::Found(found)) => {
                    self.waiting = self.waiting.saturating_sub(1);
                    self.apply(found);
                }
                Ok(Done::Thumb(number, path, made)) => self.thumbs.answered(number, path, made),
                Ok(Done::Stored) => self.storing = self.storing.saturating_sub(1),
                Err(mpsc::TryRecvError::Empty) => return,
                // The worker is gone (it panicked): the index stays as it is.
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.waiting = 0;
                    self.storing = 0;
                    return;
                }
            }
        }
    }

    fn apply(&mut self, found: Found) {
        for (path, _) in &found {
            self.thumbs.forget(path);
        }
        for (path, files) in found {
            if path.is_empty() {
                self.files.clear();
            } else {
                let under = format!("{path}/");
                self.files
                    .retain(|p, _| *p != path && !p.starts_with(&under));
            }
            for f in files {
                self.files.insert(f.path.clone(), f);
            }
        }
        self.folders = self
            .files
            .keys()
            .flat_map(|p| p.match_indices('/').map(move |(i, _)| p[..i].to_string()))
            .collect();
        let mut lists: HashMap<AssetKind, Vec<String>> = HashMap::new();
        for f in self.files.values() {
            lists.entry(f.kind).or_default().push(f.path.clone());
            for (kind, names) in [(AssetKind::Clip, &f.clips), (AssetKind::Skin, &f.skins)] {
                let parts = names.iter().map(|n| format!("{}#{n}", f.path));
                lists.entry(kind).or_default().extend(parts);
            }
        }
        let lists = lists.into_iter().map(|(k, v)| (k, v.into())).collect();
        self.lists = Arc::new(KindLists(lists));
        self.revision += 1;
    }
}

/// The paths a change names.
fn paths(change: &AssetChange) -> impl Iterator<Item = &str> {
    let (a, b) = match change {
        AssetChange::Created(p) | AssetChange::Modified(p) | AssetChange::Removed(p) => (p, None),
        AssetChange::Renamed { from, to } => (from, Some(to)),
    };
    std::iter::once(a.as_str()).chain(b.map(String::as_str))
}

/// True when a segment of `path` is hidden (starts with `.`): the engine's
/// `.real/`, version control.
fn hidden(path: &str) -> bool {
    path.split('/').any(|s| s.starts_with('.'))
}

/// What the worker reads the project with.
struct Look {
    dir: PathBuf,
    files: ProjectFiles,
    config: AssetsConfig,
    cache: ProcessedCache,
    /// The thumbnails' disk cache.
    thumbs: PathBuf,
}

impl Look {
    /// The worker thread: answers jobs until the index is dropped, the
    /// index's first, then one thumbnail at a time while none is waiting.
    fn serve(&self, inbox: &mpsc::Receiver<Job>, outbox: &mpsc::Sender<Done>) {
        let mut asks = VecDeque::new();
        loop {
            let job = if asks.is_empty() {
                match inbox.recv() {
                    Ok(job) => Some(job),
                    Err(_) => return,
                }
            } else {
                match inbox.try_recv() {
                    Ok(job) => Some(job),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            };
            let done = match job {
                Some(Job::Scan) => Done::Found(vec![(String::new(), self.under(""))]),
                Some(Job::Paths(paths)) => Done::Found(paths.iter().map(|p| self.at(p)).collect()),
                Some(Job::Thumbs(more)) => {
                    asks.extend(more);
                    continue;
                }
                Some(Job::Store(key, image)) => {
                    crate::thumbs::store(&self.thumbs, &key, &image);
                    Done::Stored
                }
                None => {
                    let Some((number, path, kind)) = asks.pop_front() else {
                        continue;
                    };
                    let made = crate::thumbs::make(&self.files, &self.thumbs, &path, kind);
                    Done::Thumb(number, path, made)
                }
            };
            if outbox.send(done).is_err() {
                return;
            }
        }
    }

    /// The files at and under `path` now, with the path they replace: a
    /// sidecar stands for its file.
    fn at(&self, path: &str) -> (String, Vec<AssetFile>) {
        let path = path
            .strip_suffix(SIDECAR)
            .filter(|p| AssetKind::of_path(p).is_some_and(AssetKind::imported))
            .unwrap_or(path);
        if hidden(path) {
            return (path.to_string(), Vec::new());
        }
        let found = match std::fs::symlink_metadata(self.dir.join(path)) {
            Ok(m) if m.is_dir() => self.under(path),
            Ok(m) if m.is_file() || self.files.is_file(path) => {
                self.file(path).into_iter().collect()
            }
            _ => Vec::new(),
        };
        (path.to_string(), found)
    }

    /// The files under the folder `rel` (`""`: the project), at any depth.
    fn under(&self, rel: &str) -> Vec<AssetFile> {
        let mut out = Vec::new();
        self.walk(rel, &mut out);
        out
    }

    fn walk(&self, rel: &str, out: &mut Vec<AssetFile>) {
        let Ok(entries) = std::fs::read_dir(self.dir.join(rel)) else {
            return;
        };
        for entry in entries.flatten() {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let Ok(ty) = entry.file_type() else {
                continue;
            };
            if ty.is_dir() {
                self.walk(&child, out);
            } else if (ty.is_file() || (ty.is_symlink() && self.files.is_file(&child)))
                && let Some(f) = self.file(&child)
            {
                // A linked file counts when it stays inside the project; a
                // linked folder is not followed.
                out.push(f);
            }
        }
    }

    /// The file at `path`, if it is of a known kind.
    fn file(&self, path: &str) -> Option<AssetFile> {
        let kind = AssetKind::of_path(path)?;
        let size = std::fs::metadata(self.dir.join(path)).ok()?.len();
        let mut file = AssetFile {
            path: path.to_string(),
            kind,
            size,
            id: None,
            import: None,
            clips: Vec::new(),
            skins: Vec::new(),
        };
        if kind.imported() {
            file.id = self
                .files
                .read_str(&format!("{path}{SIDECAR}"))
                .ok()
                .and_then(|t| serde_json::from_str::<AssetMeta>(&t).ok())
                .map(|m| m.id().to_string());
            file.import = Some(
                match re_assets::import_key(&self.files, self.config, path) {
                    Ok(None) => ImportState::Source,
                    Ok(Some(key)) if self.cache.contains(&key) => ImportState::Imported,
                    Ok(Some(_)) => ImportState::Stale,
                    Err(d) => ImportState::Failed(d.message),
                },
            );
        }
        if kind == AssetKind::Model
            && let Ok(bytes) = self.files.read_source(path)
        {
            (file.clips, file.skins) = re_assets::gltf_names(&bytes);
        }
        Some(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_by_suffix() {
        for (path, kind) in [
            ("materials/rock.material.json", Some(AssetKind::Material)),
            ("prefabs/tree.prefab.json", Some(AssetKind::Prefab)),
            ("scenes/main.scene.json", Some(AssetKind::Scene)),
            ("graphs/island.graph.json", Some(AssetKind::Graph)),
            ("anim/hero.animgraph.json", Some(AssetKind::AnimGraph)),
            ("scripts/game/main.luau", Some(AssetKind::Script)),
            ("assets/textures/Rock.PNG", Some(AssetKind::Texture)),
            ("assets/textures/rock.jpeg", Some(AssetKind::Texture)),
            ("assets/models/lid.glb", Some(AssetKind::Model)),
            ("assets/models/lid.gltf", Some(AssetKind::Model)),
            ("assets/sfx/pickup.wav", Some(AssetKind::Audio)),
            ("assets/music/theme.ogg", Some(AssetKind::Audio)),
            ("assets/models/lid.glb.meta.json", None),
            ("real.toml", None),
            ("notes.json", None),
        ] {
            assert_eq!(AssetKind::of_path(path), kind, "{path}");
        }
        for kind in AssetKind::ALL {
            assert_eq!(AssetKind::from_name(kind.name()), Some(kind));
        }
        assert_eq!(AssetKind::from_name("mesh"), None);
    }

    #[test]
    fn a_change_names_its_paths() {
        let renamed = AssetChange::Renamed {
            from: "a.png".into(),
            to: "b/a.png".into(),
        };
        assert_eq!(paths(&renamed).collect::<Vec<_>>(), ["a.png", "b/a.png"]);
        let removed = AssetChange::Removed("c.luau".into());
        assert_eq!(paths(&removed).collect::<Vec<_>>(), ["c.luau"]);
        assert!(hidden(".real/cache/x") && hidden("a/.git/b") && !hidden("a/b.c"));
    }
}
