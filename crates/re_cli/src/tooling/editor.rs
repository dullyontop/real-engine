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
    /// Also serve the editor's script debugger over DAP on 127.0.0.1
    /// (default port 4711).
    #[arg(long, value_name = "PORT", num_args = 0..=1, default_missing_value = "4711")]
    pub debug: Option<u16>,
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
    let (mut app, agent) = match open(project, config) {
        Ok(x) => x,
        Err(d) => return Ok(fail(&d)),
    };
    // Play in the editor keeps saved data and reloads changed files, as
    // `real run` does (ADR-0048). Without a watcher it goes on, without reload.
    app.persist_data();
    if let Err(d) = app.enable_hot_reload() {
        eprint!("{}", d.render_human());
    }
    let debugger = re_script::debug::Debugger::new();
    // Kept while the editor runs; dropping it closes the client's connection.
    let _dap = match args.debug {
        Some(port) => {
            match re_script::debug::dap::DapServer::listen(debugger.clone(), port, project) {
                Ok(server) => {
                    eprintln!("debugger: DAP on {}", server.addr());
                    Some(server)
                }
                Err(d) => return Ok(fail(&d)),
            }
        }
        None => None,
    };
    let options = EditorOptions {
        max_frames: args.frames,
        socket: Some(socket),
        agent: Some(agent),
        debugger: Some(debugger),
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

/// Opens the project for the editor, with the same agent-facing commands
/// `real mcp` registers (so `real mcp` through the editor serves the same
/// tools) and the scan verify stage.
fn open(project: &Path, config: AppConfig) -> Result<(App, re_agent::Agent), Diagnostic> {
    // The editor is the project's writer: every change, its own and those
    // that other writers send through its socket, is saved to the scene.
    let config = AppConfig {
        persist_edits: true,
        ..config
    };
    let mut app = App::open(project, config)?;
    let agent = re_agent::Agent::new(project);
    super::mcp::register_agent_tools(&mut app, &agent);
    Ok((app, agent))
}

fn fail(d: &Diagnostic) -> ExitCode {
    eprint!("{}", d.render_human());
    ExitCode::from(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(app: &App) -> Vec<String> {
        let mut names: Vec<String> = app
            .commands()
            .specs()
            .iter()
            .map(|s| s.name.clone())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_editor_serves_the_same_tools_as_real_mcp() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p");
        crate::new::create(&project, None).unwrap();
        let tools = project.join(re_agent::luau_tools::TOOLS_DIR);
        std::fs::create_dir_all(&tools).unwrap();
        std::fs::write(
            tools.join("hi.luau"),
            "return { name = \"demo.hi\", summary = \"Say hi.\", tier = \"read\", run = function() return \"hi\" end }",
        )
        .unwrap();
        let (editor, _agent) = open(&project, AppConfig::load(&project).unwrap()).unwrap();
        let mcp = super::super::mcp::open_for_agents(&project).unwrap();
        let editor_names = names(&editor);
        assert!(
            editor_names.iter().any(|n| n == "demo.hi"),
            "{editor_names:?}"
        );
        assert!(
            editor_names.iter().any(|n| n == "verify.diff"),
            "{editor_names:?}"
        );
        assert_eq!(editor_names, names(&mcp.app));
    }
}
