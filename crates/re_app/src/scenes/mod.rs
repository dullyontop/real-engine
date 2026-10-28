//! Scenes (ADR-0059): the scenes a game can load ([`SceneList`]), the ones
//! loaded and the entities kept across switches ([`Scenes`]), the requests
//! scripts make through the `scene` library and the loads that answer them,
//! and the log of the changes applied, checked against a replay's.
//!
//! A request answers at once with a load id. The load reads its scene file,
//! warms the assets the scene names when the run draws, and applies at the
//! EndFrame of a frame once it and every earlier request are ready, after
//! its check passes ([`check`]). How soon it is ready follows the run's
//! [`Lateness`], as streaming does: under [`Lateness::Wait`], every run but a
//! live window, every stage runs at the EndFrame of the frame that asked, so
//! a change applies there on every machine and a replay only checks it;
//! under [`Lateness::Late`], the file is read on a worker and the assets are
//! warmed within [`read::WARM_BUDGET`] a frame, so a loading screen keeps
//! drawing.

mod apply;
pub mod check;
mod fresh;
pub mod list;
pub mod read;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use re_audio::Audio;
use re_core::files::ProjectFiles;
use re_core::time::Instant;
use re_core::{Diagnostic, Location};
use re_proc::stream::{Lateness, StreamConfig};
use re_scene::{SceneDoc, World};
use re_script::{GameAccess, SceneLoadState, SceneRefusal, SceneRequestKind};
use re_types::{SceneChange, SceneChangeKind};

pub use apply::unreferenced;
pub use check::{Existing, check};
pub use fresh::{content_world, ensure_streamer, fork_world, fresh_world};
pub use list::SceneList;

use crate::codes;
use crate::media::Media;

/// What a request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Switch,
    Load,
    Unload,
}

/// Where a switch or load is.
#[derive(Debug)]
enum Stage {
    /// Not started: it starts at the next EndFrame.
    Waiting,
    /// The file is being read on a worker.
    Reading(read::Job),
    /// Read; its assets are warming, from `next` on.
    Warming {
        doc: SceneDoc,
        assets: Vec<read::Asset>,
        next: usize,
    },
    /// Ready to apply.
    Ready(SceneDoc),
    /// The file could not be read: the load fails when its turn comes.
    Failed(Diagnostic),
}

/// A request waiting for its turn.
#[derive(Debug)]
struct Request {
    /// The load answering it; an unload has none.
    id: Option<u64>,
    kind: Kind,
    name: String,
    stage: Stage,
}

impl Request {
    fn ready(&self) -> bool {
        self.kind == Kind::Unload || matches!(self.stage, Stage::Ready(_) | Stage::Failed(_))
    }
}

/// A scene loaded beside the main one.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Added {
    name: String,
    /// The ids of the roots it spawned.
    roots: Vec<String>,
}

/// The changes a replay lists, and how far the run has matched them.
#[derive(Clone, Debug)]
struct Expected {
    changes: Vec<SceneChange>,
    next: usize,
    /// A difference was reported: nothing more is checked.
    failed: bool,
}

/// What the App lends [`Scenes::end_frame`].
pub struct EndFrame<'a> {
    /// The World the changes apply to.
    pub world: &'a mut World,
    /// The project's files, to read scene files from.
    pub files: &'a ProjectFiles,
    /// `[stream]`, for the fresh streamer of a switch.
    pub stream: &'a StreamConfig,
    /// The frame this EndFrame ends.
    pub frame: u64,
    /// How the run treats a load that is not ready yet.
    pub lateness: Lateness,
    /// The caches to warm a scene's assets into, when the run draws.
    pub warm: Option<(&'a mut Media, &'a mut Audio)>,
}

/// What [`Scenes::end_frame`] changed.
#[derive(Debug, Default)]
pub struct Applied {
    /// The World changed: generate the new procedural sources, then release
    /// the assets of [`Applied::released`] that nothing draws with
    /// ([`unreferenced`]).
    pub changed: bool,
    /// A switch replaced the main scene: the undo history and the ground the
    /// characters stand on start over.
    pub switched: bool,
    /// The derived assets the despawned entities drew with.
    pub released: BTreeSet<String>,
    /// Problems, and the notes of kept entities standing in (`RE0578`).
    pub diagnostics: Vec<Diagnostic>,
}

/// The scenes of a run: the list, the main scene and the added ones, the
/// kept entities, the requests not applied yet and every load's state, and
/// the log of the changes applied. It lives outside the World: it is not
/// saved in scene files, copied by `World::fork` or hashed.
#[derive(Debug)]
pub struct Scenes {
    list: SceneList,
    /// The scene the run began in, and a restart goes back to.
    start: String,
    start_path: String,
    main: String,
    added: Vec<Added>,
    kept: BTreeSet<String>,
    queue: VecDeque<Request>,
    loads: BTreeMap<u64, SceneLoadState>,
    next_id: u64,
    /// Loads that ended since [`Scenes::take_ended`].
    ended: Vec<u64>,
    log: Vec<SceneChange>,
    /// Changes applied since the app opened, restarts included.
    changes: u64,
    expected: Option<Expected>,
}

/// The message of `d`, with its first related diagnostic's: a load's
/// `error` for scripts.
fn summary(d: &Diagnostic) -> String {
    match d.related.first() {
        Some(r) => format!("{}: {}", d.message, r.message),
        None => d.message.clone(),
    }
}

/// A change in words: `switch to "town"`.
fn describe(change: &SceneChange) -> String {
    match change.kind() {
        Some((SceneChangeKind::Switch, n)) => format!("switch to {n:?}"),
        Some((SceneChangeKind::Load, n)) => format!("load of {n:?}"),
        Some((SceneChangeKind::Unload, n)) => format!("unload of {n:?}"),
        Some((SceneChangeKind::Restart, n)) => format!("restart into {n:?}"),
        None => "a malformed entry".to_string(),
    }
}

impl Scenes {
    /// The scenes of a run that begins in the scene at project path `start`
    /// (the entry scene, unless a play session or a replay begins in
    /// another; one that `list` does not name is listed under its path).
    pub fn new(mut list: SceneList, start: &str) -> Scenes {
        let name = list.name_or_list(start);
        Scenes {
            list,
            start: name.clone(),
            start_path: start.to_string(),
            main: name,
            added: Vec::new(),
            kept: BTreeSet::new(),
            queue: VecDeque::new(),
            loads: BTreeMap::new(),
            next_id: 1,
            ended: Vec::new(),
            log: Vec::new(),
            changes: 0,
            expected: None,
        }
    }

    /// The scene list.
    pub fn list(&self) -> &SceneList {
        &self.list
    }

    /// Takes a new scene list (the config changed); the scene the run began
    /// in stays listed.
    pub fn set_list(&mut self, mut list: SceneList) {
        list.name_or_list(&self.start_path);
        self.list = list;
    }

    /// The scene the run began in.
    pub fn start(&self) -> &str {
        &self.start
    }

    /// The project path of the scene the run began in.
    pub fn start_path(&self) -> &str {
        &self.start_path
    }

    /// The main scene.
    pub fn main(&self) -> &str {
        &self.main
    }

    /// The project path of the main scene.
    pub fn main_path(&self) -> &str {
        self.list.path(&self.main).unwrap_or(&self.start_path)
    }

    /// The loaded scenes: the main scene, then the added ones in load order.
    pub fn loaded(&self) -> Vec<String> {
        std::iter::once(self.main.clone())
            .chain(self.added.iter().map(|a| a.name.clone()))
            .collect()
    }

    /// The changes applied so far, in order (a recording's `scenes`).
    pub fn log(&self) -> &[SceneChange] {
        &self.log
    }

    /// How many changes were applied since the app opened, restarts and
    /// scenes begun in ([`Scenes::begin_at`]) included: a watcher that sees it
    /// move knows the World's content was replaced, not edited.
    pub fn changes(&self) -> u64 {
        self.changes
    }

    /// True while a request waits for its turn.
    pub fn pending(&self) -> bool {
        !self.queue.is_empty()
    }

    /// The loads that ended (applied, failed, replaced or cancelled) since
    /// the last call: the App wakes the threads waiting on them
    /// (`re_script::wake_scene_loads`).
    pub fn take_ended(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.ended)
    }

    /// Checks the run's changes against `changes`, a replay's `scenes`
    /// list, from now on; `None` checks nothing.
    pub fn expect(&mut self, changes: Option<Vec<SceneChange>>) {
        self.expected = changes.map(|changes| Expected {
            changes,
            next: 0,
            failed: false,
        });
    }

    /// The same scenes for a copy of the run (`play.run`): the list, the
    /// loaded and kept scenes, and the scene the run began in; no requests
    /// and no log.
    pub fn fork(&self) -> Scenes {
        Scenes {
            list: self.list.clone(),
            start: self.start.clone(),
            start_path: self.start_path.clone(),
            main: self.main.clone(),
            added: self.added.clone(),
            kept: self.kept.clone(),
            queue: VecDeque::new(),
            loads: BTreeMap::new(),
            next_id: self.next_id,
            ended: Vec::new(),
            log: Vec::new(),
            changes: 0,
            expected: None,
        }
    }

    /// Begins the run again in the scene at `path` (a replay's
    /// `start.scene`, or the editor's open scene), as [`Scenes::new`] does;
    /// the App loads that scene into a fresh World. Logs nothing, and the
    /// replay's changes are still checked from here on.
    pub fn begin_at(&mut self, path: &str) {
        let list = std::mem::take(&mut self.list);
        let expected = self.expected.take();
        let (next_id, changes) = (self.next_id, self.changes);
        *self = Scenes::new(list, path);
        self.next_id = next_id;
        self.changes = changes + 1;
        self.expected = expected;
    }

    /// A restart (`world.restart()`, ADR-0059 §5) at the end of `frame`:
    /// the App has loaded the scene the run began in into a fresh World with
    /// everything reset. Every request not applied is dropped, nothing is
    /// added or kept any more, and the restart is logged (and checked
    /// against the replay's list).
    pub fn restarted(&mut self, frame: u64) -> Vec<Diagnostic> {
        // The VMs that held the loads are gone with the restart.
        self.queue.clear();
        self.loads.clear();
        self.ended.clear();
        self.added.clear();
        self.kept.clear();
        self.main = self.start.clone();
        let mut out = Vec::new();
        let change = SceneChange::new(frame, SceneChangeKind::Restart, self.start.clone());
        self.record(change, &mut out);
        out
    }

    /// The main scene and the added ones once every pending request applies.
    fn projected(&self) -> (String, Vec<String>) {
        let mut main = self.main.clone();
        let mut added: Vec<String> = self.added.iter().map(|a| a.name.clone()).collect();
        for r in &self.queue {
            match r.kind {
                Kind::Switch => {
                    main = r.name.clone();
                    added.clear();
                }
                Kind::Load => added.push(r.name.clone()),
                Kind::Unload => added.retain(|a| *a != r.name),
            }
        }
        (main, added)
    }

    fn is_loaded(&self, name: &str) -> bool {
        self.main == name || self.added.iter().any(|a| a.name == name)
    }

    /// Ends load `id`: applied when `error` is `None`, else failed.
    fn end(&mut self, id: Option<u64>, error: Option<String>) {
        let Some(id) = id else { return };
        if let Some(load) = self.loads.get_mut(&id) {
            load.done = true;
            if error.is_none() {
                load.progress = 1.0;
            }
            load.error = error;
            self.ended.push(id);
        }
    }

    fn set_progress(&mut self, id: Option<u64>, progress: f64) {
        if let Some(load) = id.and_then(|id| self.loads.get_mut(&id)) {
            load.progress = progress;
        }
    }

    /// Logs `change`, comparing it with the next one the replay lists.
    fn record(&mut self, change: SceneChange, out: &mut Vec<Diagnostic>) {
        if let Some(x) = &mut self.expected
            && !x.failed
        {
            match x.changes.get(x.next) {
                Some(want) if *want == change => x.next += 1,
                want => {
                    x.failed = true;
                    let listed = match want {
                        Some(w) => format!("a {} at frame {}", describe(w), w.frame),
                        None => "no further change".to_string(),
                    };
                    out.push(
                        Diagnostic::error(
                            codes::SCENE_CHANGES_DIFFER,
                            format!(
                                "the run's scene changes differ from the replay's: at the end of frame {} the run made a {}, where the replay lists {listed}",
                                change.frame,
                                describe(&change)
                            ),
                        )
                        .with_hint("record the replay again, or find what made the run change scenes differently"),
                    );
                }
            }
        }
        self.log.push(change);
        self.changes += 1;
    }

    /// Reports a listed change whose frame has ended without it.
    fn check_missed(&mut self, frame: u64, out: &mut Vec<Diagnostic>) {
        if let Some(x) = &mut self.expected
            && !x.failed
            && let Some(want) = x.changes.get(x.next)
            && want.frame <= frame
        {
            x.failed = true;
            out.push(missed(want));
        }
    }

    /// At the end of a replay: a listed change the run never made is an
    /// error (`RE0571`). Nothing is checked after it, so a run that goes on
    /// past its replay changes scenes freely.
    pub fn replay_ended(&mut self) -> Option<Diagnostic> {
        let x = self.expected.take()?;
        if x.failed {
            return None;
        }
        x.changes.get(x.next).map(missed)
    }

    /// Advances the pending loads and applies the requests that are ready,
    /// in order (ADR-0059 §4): the App's EndFrame work, after the frame's
    /// screenshots. A switch that is ready cancels the loads before it that
    /// are not. Under [`Lateness::Wait`] every request is ready here.
    pub fn end_frame(&mut self, w: &mut EndFrame<'_>) -> Applied {
        let mut out = Applied::default();
        self.advance(w);
        while let Some(head) = self.queue.front() {
            if head.ready() {
                let r = self.queue.pop_front().expect("the head is there");
                self.apply(r, w, &mut out);
                continue;
            }
            match self
                .queue
                .iter()
                .find(|r| r.kind == Kind::Switch && r.ready())
                .map(|r| r.name.clone())
            {
                Some(switch) => {
                    let r = self.queue.pop_front().expect("the head is there");
                    self.end(r.id, Some(format!("cancelled by the switch to {switch:?}")));
                }
                None => break,
            }
        }
        self.check_missed(w.frame, &mut out.diagnostics);
        out
    }

    /// Moves each switch and load on through its stages.
    fn advance(&mut self, w: &mut EndFrame<'_>) {
        let wait = w.lateness == Lateness::Wait;
        let start = Instant::now();
        let mut queue = std::mem::take(&mut self.queue);
        for r in queue.iter_mut().filter(|r| r.kind != Kind::Unload) {
            let path = self.list.path(&r.name).unwrap_or(&r.name).to_string();
            loop {
                let stage = std::mem::replace(&mut r.stage, Stage::Waiting);
                r.stage = match stage {
                    Stage::Waiting if wait => match read::read(w.files, &r.name, &path) {
                        Ok(doc) => Stage::Ready(doc),
                        Err(d) => Stage::Failed(d),
                    },
                    Stage::Waiting => Stage::Reading(read::Job::start(w.files, &r.name, &path)),
                    Stage::Reading(job) => match job.poll() {
                        None => {
                            r.stage = Stage::Reading(job);
                            break;
                        }
                        Some(Err(d)) => Stage::Failed(d),
                        Some(Ok(doc)) if w.warm.is_some() => {
                            let assets = read::assets(&doc);
                            Stage::Warming {
                                doc,
                                assets,
                                next: 0,
                            }
                        }
                        Some(Ok(doc)) => Stage::Ready(doc),
                    },
                    Stage::Warming { doc, assets, next } => {
                        let next = match &mut w.warm {
                            Some((media, audio)) => {
                                read::warm_some(&assets, next, start, media, audio)
                            }
                            None => assets.len(),
                        };
                        if next < assets.len() {
                            let progress = 0.2 + 0.7 * next as f64 / assets.len() as f64;
                            if let Some(load) = r.id.and_then(|id| self.loads.get_mut(&id)) {
                                load.progress = progress;
                            }
                            r.stage = Stage::Warming { doc, assets, next };
                            break;
                        }
                        Stage::Ready(doc)
                    }
                    done @ (Stage::Ready(_) | Stage::Failed(_)) => {
                        r.stage = done;
                        break;
                    }
                };
            }
            let progress = match r.stage {
                Stage::Waiting => 0.0,
                Stage::Reading(_) => 0.1,
                Stage::Warming { .. } => continue,
                Stage::Ready(_) | Stage::Failed(_) => 0.9,
            };
            if let Some(load) = r.id.and_then(|id| self.loads.get_mut(&id)) {
                load.progress = progress;
            }
        }
        self.queue = queue;
    }

    /// Applies one request whose turn came.
    fn apply(&mut self, r: Request, w: &mut EndFrame<'_>, out: &mut Applied) {
        let doc = match r.stage {
            _ if r.kind == Kind::Unload => None,
            Stage::Ready(doc) => Some(doc),
            Stage::Failed(d) => {
                self.end(r.id, Some(summary(&d)));
                out.diagnostics.push(d);
                return;
            }
            _ => unreachable!("only ready requests apply"),
        };
        let path = self.list.path(&r.name).unwrap_or(&r.name).to_string();
        let scene = read::describe(&r.name, &path);
        let result = match (&r.kind, &doc) {
            (Kind::Switch, Some(doc)) => apply::switch(w.world, doc, &self.kept, w.stream),
            (Kind::Load, Some(doc)) => apply::load(w.world, doc, &self.kept),
            _ => {
                // An unload: of a scene a switch before it may have taken away.
                let Some(i) = self.added.iter().position(|a| a.name == r.name) else {
                    return;
                };
                let added = self.added.remove(i);
                Ok(apply::unload(w.world, &added.roots, &self.kept))
            }
        };
        let mut change = match result {
            Ok(change) => change,
            Err(e) => {
                let d = Diagnostic::error(
                    codes::SCENE_LOAD_FAILED,
                    format!("scene {scene} does not load"),
                )
                .at(Location::file(path))
                .with_related(e.to_diagnostic());
                self.end(r.id, Some(summary(&d)));
                out.diagnostics.push(d);
                return;
            }
        };
        for id in &change.stood_in {
            out.diagnostics.push(
                Diagnostic::note(
                    codes::KEPT_STANDS_IN,
                    format!(
                        "kept entity {id:?} stands in for the entity of that id in scene {scene}"
                    ),
                )
                .at(Location::entity(id.clone())),
            );
        }
        out.diagnostics.extend(change.diagnostics);
        out.released.extend(change.released);
        out.changed = true;
        let kind = match r.kind {
            Kind::Switch => {
                out.switched = true;
                self.main = r.name.clone();
                self.added.clear();
                // Kept ids that named no live root are forgotten.
                self.kept = std::mem::take(&mut change.kept);
                SceneChangeKind::Switch
            }
            Kind::Load => {
                self.added.push(Added {
                    name: r.name.clone(),
                    roots: change.roots,
                });
                SceneChangeKind::Load
            }
            Kind::Unload => SceneChangeKind::Unload,
        };
        self.set_progress(r.id, 1.0);
        self.end(r.id, None);
        self.record(
            SceneChange::new(w.frame, kind, r.name),
            &mut out.diagnostics,
        );
    }

    /// Queues a request, answering with its load (none for an unload).
    fn push(&mut self, kind: Kind, name: &str) -> Option<u64> {
        let id = (kind != Kind::Unload).then(|| {
            let id = self.next_id;
            self.next_id += 1;
            self.loads.insert(
                id,
                SceneLoadState {
                    name: name.to_string(),
                    progress: 0.0,
                    done: false,
                    error: None,
                },
            );
            id
        });
        self.queue.push_back(Request {
            id,
            kind,
            name: name.to_string(),
            stage: Stage::Waiting,
        });
        id
    }
}

/// `RE0571` for a listed change the run did not make.
fn missed(want: &SceneChange) -> Diagnostic {
    Diagnostic::error(
        codes::SCENE_CHANGES_DIFFER,
        format!(
            "the run's scene changes differ from the replay's: the replay lists a {} at the end of frame {}, which the run did not make",
            describe(want),
            want.frame
        ),
    )
    .with_hint("record the replay again, or find what kept the run from changing scenes")
}

impl GameAccess for Scenes {
    fn scene_request(&mut self, kind: SceneRequestKind, name: &str) -> Result<u64, SceneRefusal> {
        if self.list.path(name).is_none() {
            return Err(SceneRefusal::Unknown);
        }
        let kind = match kind {
            SceneRequestKind::Switch => Kind::Switch,
            SceneRequestKind::Load => Kind::Load,
        };
        match kind {
            Kind::Load => {
                let (main, added) = self.projected();
                if main == name || added.iter().any(|a| a == name) {
                    let why = if self.is_loaded(name) {
                        format!("scene {name:?} is already loaded")
                    } else {
                        format!("scene {name:?} is already asked for")
                    };
                    return Err(SceneRefusal::Refused(why));
                }
            }
            _ => {
                // A switch replaces the switch still waiting for its turn.
                if let Some(i) = self.queue.iter().position(|r| r.kind == Kind::Switch) {
                    let old = self.queue.remove(i).expect("found just above");
                    self.end(old.id, Some(format!("replaced by the switch to {name:?}")));
                }
            }
        }
        Ok(self
            .push(kind, name)
            .expect("a switch or load has a load id"))
    }

    fn scene_unload(&mut self, name: &str) -> Result<(), SceneRefusal> {
        if self.list.path(name).is_none() {
            return Err(SceneRefusal::Unknown);
        }
        let (main, added) = self.projected();
        if main == name {
            return Err(SceneRefusal::Refused(format!(
                "{name:?} is the main scene; switch to another scene instead"
            )));
        }
        if !added.iter().any(|a| a == name) {
            return Err(SceneRefusal::Refused(format!(
                "scene {name:?} is not loaded"
            )));
        }
        // Unloading a scene still loading cancels its load.
        if let Some(i) = self
            .queue
            .iter()
            .rposition(|r| r.kind == Kind::Load && r.name == name)
            && !self
                .queue
                .iter()
                .skip(i)
                .any(|r| r.kind != Kind::Load && r.name == name)
        {
            let r = self.queue.remove(i).expect("found just above");
            self.end(r.id, Some("cancelled by scene.unload".to_string()));
            return Ok(());
        }
        self.push(Kind::Unload, name);
        Ok(())
    }

    fn scene_load(&self, id: u64) -> Option<SceneLoadState> {
        self.loads.get(&id).cloned()
    }

    fn scene_current(&self) -> String {
        self.main.clone()
    }

    fn scenes_loaded(&self) -> Vec<String> {
        self.loaded()
    }

    fn scenes_listed(&self) -> Vec<String> {
        self.list.names()
    }

    fn keep(&mut self, id: &str, keep: bool) {
        if keep {
            self.kept.insert(id.to_string());
        } else {
            self.kept.remove(id);
        }
    }

    fn is_kept(&self, id: &str) -> bool {
        self.kept.contains(id)
    }
}

#[cfg(test)]
mod tests;
