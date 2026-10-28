//! Edit mode: commands through the registry, the edit frame that follows
//! them, and saving the scene and the open documents.

use std::path::Path;
use std::sync::Arc;

use re_command::{CommandOutcome, EngineReceiver, EngineRequest, Origin, PolicyCheck};
use re_core::Diagnostic;
use re_scene::SaveOptions;

use crate::codes;
use crate::procedural;
use crate::sync::render::{self, RenderInputs};
use crate::ui::hud;

use super::{App, rel};

impl App {
    /// Dispatches a command through the registry and `policy`, as `origin`.
    ///
    /// After a command that changed the world (not a dry run), the edit frame
    /// runs (procedural sources regenerate, transforms propagate) and, when
    /// [`AppConfig::persist_edits`] is set, the scene is written back to its
    /// file; a failed write is a warning (`RE0518`) in the outcome.
    pub fn dispatch(
        &mut self,
        name: &str,
        args: serde_json::Value,
        dry_run: bool,
        policy: &dyn PolicyCheck,
        origin: Origin,
    ) -> CommandOutcome {
        let dry = dry_run || args.get("dry_run").and_then(serde_json::Value::as_bool) == Some(true);
        let registry = Arc::clone(&self.commands);
        let mut outcome = registry.dispatch_as(self, name, args, dry_run, policy, origin);
        drop(registry);
        if outcome.ok && !dry && !outcome.changes.is_empty() {
            self.edit_frame();
            if self.config.persist_edits
                && let Err(d) = self.save_scene()
            {
                outcome.diagnostics.push(Diagnostic {
                    severity: re_core::Severity::Warning,
                    ..d
                });
            }
        }
        outcome
    }

    /// Answers engine requests on the current thread until every
    /// [`EngineHandle`](re_command::EngineHandle) is dropped: each command goes
    /// through [`App::dispatch`] with `policy`.
    ///
    /// This is edit mode: nothing simulates, so the scene an agent or a person
    /// edits is the scene on disk. Running the game is `play.run` (on a copy) or
    /// `real run`.
    pub fn serve(&mut self, receiver: EngineReceiver, policy: &dyn PolicyCheck) {
        while let Some(request) = receiver.recv() {
            self.answer(request, policy);
        }
    }

    /// Answers the requests already waiting, without blocking (for a frame loop
    /// that also serves, such as the editor's). Returns how many were answered,
    /// or `None` once every handle is dropped.
    pub fn serve_pending(
        &mut self,
        receiver: &EngineReceiver,
        policy: &dyn PolicyCheck,
    ) -> Option<usize> {
        let mut served = 0;
        loop {
            match receiver.try_recv() {
                Ok(Some(request)) => {
                    self.answer(request, policy);
                    served += 1;
                }
                Ok(None) => return Some(served),
                Err(()) if served > 0 => return Some(served),
                Err(()) => return None,
            }
        }
    }

    fn answer(&mut self, request: EngineRequest, policy: &dyn PolicyCheck) {
        match request {
            EngineRequest::Command {
                name,
                args,
                dry_run,
                origin,
                reply,
            } => {
                let outcome = self.dispatch(&name, args, dry_run, policy, origin);
                let _ = reply.send(outcome);
            }
            EngineRequest::ListSpecs { reply } => {
                let _ = reply.send(self.commands.specs().to_vec());
            }
        }
    }

    /// Writes the World's authored entities back to the open scene's file
    /// ([`App::scene_path`]: the entry scene, unless another was opened),
    /// and each open document with unsaved changes back to its file
    /// (ADR-0044), which clears its dirty flag; the procedural sources that
    /// read a saved file reload their stored graph from it, so it stays the
    /// file's content once the document is closed.
    ///
    /// # Errors
    ///
    /// `RE0518` when a file cannot be written.
    pub fn save_scene(&mut self) -> Result<(), Diagnostic> {
        let doc = self.world.save(SaveOptions::default());
        let text = re_scene::scene::to_json(&doc);
        let rel_path = self.scene_path().to_string();
        // Only a development run writes project files (ADR-0037).
        let Some(dir) = self.files.project_dir().map(Path::to_path_buf) else {
            return Err(Diagnostic::error(
                codes::PACKAGED_WRITE,
                format!(
                    "cannot write the scene back to {rel_path}: this run has no project directory"
                ),
            ));
        };
        let failed = |what: &str, rel_path: &str, e: std::io::Error| {
            Diagnostic::error(
                codes::SCENE_SAVE_FAILED,
                format!("cannot write {what} back to {rel_path}: {e}"),
            )
            .at(rel(rel_path))
        };
        std::fs::write(dir.join(&rel_path), text).map_err(|e| failed("the scene", &rel_path, e))?;
        let saved = self.world.unsaved_docs();
        for path in &saved {
            let Some(text) = self.world.doc_text(path) else {
                continue;
            };
            std::fs::write(dir.join(path), text)
                .map_err(|e| failed("the open document", path, e))?;
            self.world.mark_doc_saved(path);
        }
        if !saved.is_empty() {
            let problems = procedural::load_graphs(&mut self.world, &self.files, Some(&saved));
            self.diagnostics.extend(problems);
        }
        Ok(())
    }

    /// The work an edit needs outside the simulation: regenerate procedural
    /// sources whose inputs changed, then propagate transforms. With a GPU side
    /// (an editor's), also upload new assets and rebuild the snapshot, the HUD
    /// and the native UI, so [`App::draw_into`] draws the World as it is.
    pub fn edit_frame(&mut self) {
        self.regenerate_changed();
        self.world.propagate();
        if self.graphics.is_some() {
            // An editor's view: the World as it is, with its assets on the GPU.
            self.upload_assets();
            let inputs = RenderInputs {
                assets: &self.render_assets,
                physics: &self.physics_sync,
                skins: &self.skin_sync,
                alpha: self.alpha,
                clear: self.config.render.clear,
                post: self.config.render.post,
                shadows: self.config.render.shadows,
                origin: self.physics.origin(),
            };
            let origin = inputs.origin;
            self.snapshot = render::build(&mut self.world, &inputs);
            self.hud = hud::collect(&mut self.world);
            let view = crate::ui::View {
                size_px: self.view_size,
                pixels_per_point: self.view_ppp,
            };
            self.ui.sync(
                &mut self.world,
                &self.snapshot.camera,
                origin,
                view,
                &mut self.diagnostics,
            );
        }
    }

    /// Regenerates the procedural sources whose inputs changed and forgets the
    /// derived assets nothing uses any more (the GPU copies go at the next
    /// upload).
    pub(super) fn regenerate_changed(&mut self) {
        let problems =
            procedural::refresh_graphs(&mut self.world, &self.files, &mut self.graph_inputs);
        self.diagnostics.extend(problems);
        let report = self.world.regenerate_changed();
        self.diagnostics.extend(report.diagnostics.iter().cloned());
        procedural::release(
            &mut self.world,
            &mut self.media,
            &mut self.shapes,
            &report.released_assets,
        );
    }

    /// Uploads the meshes and materials the World names that are not on the GPU
    /// yet (with the app's [`Graphics`]; nothing without one), recording their
    /// handles in the app's `RenderAssets`. `frame` and `edit_frame` call it.
    pub fn upload_assets(&mut self) {
        if let Some(gfx) = &mut self.graphics {
            procedural::sync_derived(&mut self.world, &mut self.media, &mut self.shapes);
            gfx.sync_assets(
                &mut self.world,
                &mut self.media,
                &mut self.render_assets,
                &mut self.diagnostics,
            );
        }
    }
}
