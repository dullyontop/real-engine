//! `real mcp`: serves the project's commands to MCP clients over stdio
//! (ADR-0025, ADR-0026; WP7).
//!
//! When the editor holds the project, `real mcp` connects to its socket and
//! serves MCP over that handle, with the agent policy checked here before a
//! call reaches the editor. Otherwise it takes the project lock itself, opens
//! the project, starts an agent session (its start checkpoint is what
//! `checkpoint.restore` returns to), and serves MCP on a background thread
//! while this thread runs the engine. Edits are saved to the scene as they
//! are made. A second headless writer gets `RE8003`.
//!
//! stdout carries JSON-RPC, so everything else goes to stderr.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use re_agent::{Agent, Holder, Policy, Writer, guard, open_writer};
use re_app::{App, AppConfig};
use re_command::{Origin, channel};
use re_core::Diagnostic;
use re_mcp::McpOptions;

/// Arguments of `real mcp`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct McpArgs {}

/// The origin of every MCP call in the history.
pub const ORIGIN: &str = "mcp";

fn fail(d: &Diagnostic) -> ExitCode {
    eprint!("{}", re_core::render_human_all(std::slice::from_ref(d)));
    ExitCode::from(1)
}

/// Opens the project for agents: the app, its `explain` and agent commands,
/// and edits saved as they are made.
pub(crate) fn open_for_agents(project: &Path) -> Result<(App, Agent), Diagnostic> {
    let mut config = AppConfig::load(project)?;
    config.persist_edits = true;
    let mut app = App::open(project, config)?;
    crate::explain::register(app.commands_mut());
    let agent = Agent::new(project);
    re_agent::register_commands(app.commands_mut(), &agent);
    Ok((app, agent))
}

/// Runs `real mcp` for the project at `project`.
pub fn run(args: McpArgs, project: &Path) -> anyhow::Result<ExitCode> {
    let McpArgs {} = args;
    let policy = match Policy::load(project) {
        Ok(p) => p,
        Err(d) => return Ok(fail(&d)),
    };
    let options = McpOptions::new(project);
    let origin = Origin::Agent(ORIGIN.to_string());
    let lock = match open_writer(project, Holder::headless("real mcp")) {
        Ok(Writer::Local(lock)) => lock,
        Ok(Writer::Remote(handle)) => {
            eprintln!("real mcp: the editor has the project open; serving through it");
            let handle = guard::guarded(handle, Arc::new(policy), origin);
            re_mcp::serve_stdio(handle, options)?;
            return Ok(ExitCode::SUCCESS);
        }
        Err(d) => return Ok(fail(&d)),
    };

    let (mut app, agent) = match open_for_agents(project) {
        Ok(x) => x,
        Err(d) => return Ok(fail(&d)),
    };
    match agent.start_session(app.world()) {
        Ok(id) => eprintln!(
            "real mcp: agent session {id} (`real agent rollback {id}` returns to its start)"
        ),
        Err(d) => return Ok(fail(&d)),
    }
    let (unnamed, receiver) = channel();
    let handle = unnamed.with_origin(origin);
    // `serve` returns when every handle is gone, so keep only the one the
    // server thread owns.
    drop(unnamed);
    let server = std::thread::Builder::new()
        .name("real-mcp".to_string())
        .spawn(move || re_mcp::serve_stdio(handle, options))?;
    // Returns once the server thread drops its handle (the client closed
    // stdin).
    app.serve(receiver, &policy);
    drop(lock);
    match server.join() {
        Ok(result) => result?,
        Err(_) => anyhow::bail!("the MCP server thread panicked"),
    }
    Ok(ExitCode::SUCCESS)
}
