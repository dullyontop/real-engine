//! Files that change while the app runs: hot reload of assets, shaders and
//! scripts, and open documents changed on disk (ADR-0044).

use std::path::Path;

use re_core::Diagnostic;

use re_assets::AssetWatcher;

use crate::codes;
use crate::procedural;

use super::{App, rel};

impl App {
    /// Watches the project's files: changed meshes, materials and textures are
    /// re-imported and uploaded again between frames, changed shader files are
    /// rebuilt ([`crate::shaders`]), and changed scripts are collected for
    /// [`App::take_changed_scripts`]. When `RE_ENGINE_SHADERS` names a
    /// directory, its shader files are read and watched too
    /// ([`App::watch_engine_shaders`]).
    ///
    /// # Errors
    ///
    /// The watcher's error.
    pub fn enable_hot_reload(&mut self) -> Result<(), Diagnostic> {
        // Only a project directory changes under a running game.
        let Some(dir) = self.files.project_dir() else {
            return Ok(());
        };
        self.watcher = Some(AssetWatcher::new(dir, AssetWatcher::DEFAULT_DEBOUNCE)?);
        if let Some(engine) = std::env::var_os(crate::shaders::ENGINE_SHADERS_VAR) {
            self.watch_engine_shaders(Path::new(&engine))?;
        }
        Ok(())
    }

    /// Reads the engine's shader files from `dir` (usually
    /// `crates/re_render/src/shaders` in an engine checkout) in place of the
    /// built-in ones, beneath the project's `shaders/`, and rebuilds them when
    /// they change (ADR-0038). Their problems are the app's diagnostics.
    ///
    /// # Errors
    ///
    /// `RE7030` when `dir` cannot be watched; nothing is read then.
    pub fn watch_engine_shaders(&mut self, dir: &Path) -> Result<(), Diagnostic> {
        let watcher = AssetWatcher::new(dir, AssetWatcher::DEFAULT_DEBOUNCE)?;
        let problems = self.shaders.read_engine_dir(dir);
        self.diagnostics.extend(problems);
        self.shader_watcher = Some(watcher);
        self.install_shaders();
        Ok(())
    }

    /// Scripts under `scripts/` that changed on disk since the last call.
    pub fn take_changed_scripts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.changed_scripts)
    }

    pub(super) fn poll_files(&mut self) {
        if let Some(watcher) = &self.watcher {
            let changes = watcher.poll();
            if !changes.is_empty() {
                self.apply_file_changes(&changes);
            }
        }
        if let Some(watcher) = &self.shader_watcher {
            let changes = watcher.poll();
            if !changes.is_empty() {
                let problems = self.shaders.engine_changed(&changes);
                self.diagnostics.extend(problems);
                self.install_shaders();
            }
        }
    }

    /// Applies file changes (from a watcher, or made by a command): re-imports
    /// what changed, uploads it again and forgets collider shapes built from
    /// it, and rebuilds the shaders of changed shader files
    /// ([`crate::shaders`]). Returns the problems re-imports and rebuilds
    /// reported (the last good version of each stays loaded); they are also in
    /// the app's diagnostics.
    pub fn apply_file_changes(&mut self, changes: &[re_assets::AssetChange]) -> Vec<Diagnostic> {
        let (report, stale) = self.media.apply_changes(changes);
        let mut problems = report.problems.clone();
        for key in &stale {
            self.shapes.remove(key);
        }
        self.diagnostics.extend(report.problems);
        self.reload_compat(&report.scripts);
        for path in report.scripts {
            self.reload_script(&path);
            self.changed_scripts.push(path);
        }
        if let Some(gfx) = &mut self.graphics {
            gfx.invalidate(&mut self.render_assets, &stale, &mut self.diagnostics);
        }
        self.anim_sync.invalidate(&stale);
        self.skin_sync.invalidate(&stale);
        // A changed graph file or builder script rebuilds that source's graph
        // (the new graph's hash regenerates it); a changed prefab regenerates
        // every source, since prefab contents are not part of a fingerprint.
        let paths: Vec<String> = changes
            .iter()
            .flat_map(|c| match c {
                re_assets::AssetChange::Created(p)
                | re_assets::AssetChange::Modified(p)
                | re_assets::AssetChange::Removed(p) => vec![p.clone()],
                re_assets::AssetChange::Renamed { from, to } => vec![from.clone(), to.clone()],
            })
            .collect();
        let shaders: Vec<&str> = paths
            .iter()
            .filter_map(|p| crate::shaders::project_name(p))
            .collect();
        if !shaders.is_empty() {
            for name in shaders {
                if let Some(d) = self.shaders.read_project(&self.files, name) {
                    self.diagnostics.push(d.clone());
                    problems.push(d);
                }
            }
            problems.extend(self.install_shaders());
        }
        if paths.iter().any(|p| procedural::is_generator_script(p)) {
            let declared = procedural::declare_generators(
                &mut self.world,
                &self.files,
                self.config.proc.limits(),
            );
            self.diagnostics.extend(declared.iter().cloned());
            problems.extend(declared);
        }
        for p in paths.iter().filter(|p| p.ends_with(".heightfield")) {
            procedural::heightfield_changed(&mut self.world, p);
        }
        let doc_problems = self.reload_open_docs(&paths);
        self.diagnostics.extend(doc_problems.iter().cloned());
        problems.extend(doc_problems);
        let graphs: Vec<String> = paths
            .iter()
            .filter(|p| procedural::is_graph_input(&mut self.world, p))
            .cloned()
            .collect();
        if !graphs.is_empty() {
            let graph_problems =
                procedural::load_graphs(&mut self.world, &self.files, Some(&graphs));
            self.diagnostics.extend(graph_problems.iter().cloned());
            problems.extend(graph_problems);
        }
        if paths.iter().any(|p| p.ends_with(".prefab.json")) {
            let sources: Vec<re_scene::Entity> = self
                .world
                .ecs
                .query_filtered::<re_scene::Entity, re_scene::ecs::query::With<re_scene::Procedural>>()
                .iter(&self.world.ecs)
                .collect();
            for e in sources {
                let report = self.world.regenerate(e);
                self.diagnostics.extend(report.diagnostics.iter().cloned());
                procedural::release(
                    &mut self.world,
                    &mut self.media,
                    &mut self.shapes,
                    &report.released_assets,
                );
            }
        }
        problems
    }

    /// Reloads the open documents (ADR-0044) among the changed project files
    /// `paths`. A document without unsaved edits is reopened from its file
    /// (a new epoch, so its undo history is dropped, and a new revision, so
    /// views rebuild), unless the file holds exactly the open content (as
    /// after a save). A document with unsaved edits is kept, and so is one
    /// whose file is gone or invalid; each gives an `RE0528` warning.
    fn reload_open_docs(&mut self, paths: &[String]) -> Vec<Diagnostic> {
        let mut problems = Vec::new();
        for path in paths {
            let Some(doc) = self.world.doc(path) else {
                continue;
            };
            let path = re_scene::doc_path(path).unwrap_or_else(|_| path.clone());
            let warn = |message: String| {
                Diagnostic::warning(codes::OPEN_DOC_CHANGED_ON_DISK, message).at(rel(&path))
            };
            let read = std::fs::read_to_string(self.project.join(&path))
                .map_err(|e| e.to_string())
                .and_then(|t| {
                    serde_json::from_str::<serde_json::Value>(&t).map_err(|e| e.to_string())
                });
            match open_doc_reload(doc.dirty, &doc.value, read, |v| doc.matches_saved(v)) {
                DocReload::Keep => {}
                DocReload::Conflict => problems.push(warn(format!(
                    "{path} changed on disk while it has unsaved edits; saving will overwrite the file"
                ))),
                DocReload::Unreadable(e) => problems.push(warn(format!(
                    "{path} changed on disk but cannot be reloaded ({e}); the open document is kept"
                ))),
                DocReload::Reopen(value) => {
                    if let Err(e) = self.world.open_doc(&path, value) {
                        problems.push(warn(format!(
                            "{path} changed on disk but cannot be reloaded ({e}); the open document is kept"
                        )));
                    }
                }
            }
        }
        problems
    }
}

/// What to do with an open document whose file changed on disk.
#[derive(Debug, PartialEq)]
enum DocReload {
    /// The file holds the open content, or the content last saved (our own
    /// save, seen after later edits).
    Keep,
    /// Reopen the document with the file's content.
    Reopen(serde_json::Value),
    /// The document has unsaved edits: keep them and warn.
    Conflict,
    /// The file is gone or not JSON: keep the document and warn.
    Unreadable(String),
}

/// Decides what a change on disk does to an open document: `dirty` and
/// `open` are the document's state, `file` what reading the file gave, and
/// `saved` tells whether a content is what the document was last saved as.
fn open_doc_reload(
    dirty: bool,
    open: &serde_json::Value,
    file: Result<serde_json::Value, String>,
    saved: impl Fn(&serde_json::Value) -> bool,
) -> DocReload {
    match file {
        Ok(value) if value == *open || saved(&value) => DocReload::Keep,
        _ if dirty => DocReload::Conflict,
        Ok(value) => DocReload::Reopen(value),
        Err(e) => DocReload::Unreadable(e),
    }
}

#[cfg(test)]
mod doc_reload_tests {
    use super::{DocReload, open_doc_reload};
    use serde_json::json;

    #[test]
    fn a_change_on_disk_reopens_clean_documents_only() {
        let open = json!({"a": 1});
        assert_eq!(
            open_doc_reload(false, &open, Ok(json!({"a": 1})), |_| false),
            DocReload::Keep
        );
        assert_eq!(
            open_doc_reload(true, &open, Ok(json!({"a": 1})), |_| false),
            DocReload::Keep
        );
        assert_eq!(
            open_doc_reload(false, &open, Ok(json!({"a": 2})), |_| false),
            DocReload::Reopen(json!({"a": 2}))
        );
        assert_eq!(
            open_doc_reload(true, &open, Ok(json!({"a": 2})), |_| false),
            DocReload::Conflict
        );
        assert_eq!(
            open_doc_reload(true, &open, Err("gone".into()), |_| false),
            DocReload::Conflict
        );
        assert_eq!(
            open_doc_reload(false, &open, Err("gone".into()), |_| false),
            DocReload::Unreadable("gone".into())
        );
    }

    /// A save followed by an edit before the watcher reports the save: the
    /// file holds the saved content, which is not a conflict.
    #[test]
    fn our_own_save_seen_after_an_edit_is_kept() {
        let saved = json!({"a": 1});
        let edited = json!({"a": 2});
        assert_eq!(
            open_doc_reload(true, &edited, Ok(saved.clone()), |v| *v == saved),
            DocReload::Keep
        );
        assert_eq!(
            open_doc_reload(true, &edited, Ok(json!({"a": 3})), |v| *v == saved),
            DocReload::Conflict
        );
    }
}
