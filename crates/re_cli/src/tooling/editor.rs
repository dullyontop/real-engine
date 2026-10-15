//! `real editor`: opens the editor (`re_editor`, docs/M1_PLAN.md WP8).
//!
//! WP6 created this file as a stub in its milestone "app" PR; from that merge
//! on it belongs to WP8 (docs/M1_PLAN.md, ownership rule). The two names below
//! are frozen: `EditorArgs` (deriving `clap::Args`; the owner may add fields)
//! and `run`.

use std::path::Path;
use std::process::ExitCode;

use re_app::{App, AppConfig};
use re_core::Diagnostic;
use re_editor::{EditorOptions, EditorSocket};

/// Arguments of `real editor`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct EditorArgs {
    /// Close after this many frames (smoke tests); by default the editor runs
    /// until its window is closed.
    #[arg(long)]
    pub frames: Option<u64>,
}

/// Runs `real editor` for the project at `project`: takes the project lock as
/// the editor and serves `.real/editor.sock`, opens the project with edits
/// written back to the entry scene, and shows the editor window.
pub fn run(args: EditorArgs, project: &Path) -> anyhow::Result<ExitCode> {
    // The project's config first (is it a project?), then the lock, then the
    // scene: no other writer can change the scene between loading and locking.
    let config = match AppConfig::load(project) {
        Ok(config) => config,
        Err(d) => return Ok(fail(&d)),
    };
    let socket = match EditorSocket::open(project) {
        Ok(socket) => socket,
        Err(d) => return Ok(fail(&d)),
    };
    let app = match open(project, config) {
        Ok(app) => app,
        Err(d) => return Ok(fail(&d)),
    };
    let options = EditorOptions {
        max_frames: args.frames,
        socket: Some(socket),
        ..EditorOptions::default()
    };
    match re_editor::run_editor(app, options) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(e) => match e.downcast_ref::<Diagnostic>() {
            Some(d) => Ok(fail(d)),
            None => Err(e),
        },
    }
}

fn open(project: &Path, config: AppConfig) -> Result<App, Diagnostic> {
    // The editor is the project's writer: every change, its own and those
    // that other writers send through its socket, is saved to the scene.
    let config = AppConfig {
        persist_edits: true,
        ..config
    };
    let mut app = App::open(project, config)?;
    crate::explain::register(app.commands_mut());
    Ok(app)
}

fn fail(d: &Diagnostic) -> ExitCode {
    eprint!("{}", d.render_human());
    ExitCode::from(1)
}
