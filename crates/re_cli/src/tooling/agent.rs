//! `real agent`: sets up and manages agent access (WP7, ADR-0020,
//! ADR-0025).
//!
//! * `real agent init` writes `.real/agent-policy.toml` (the `edit` tier
//!   within the default scopes: committing it is the opt-in), `AGENTS.md`,
//!   `CLAUDE.md` and each MCP client's config, and prints the Codex and Zed
//!   snippets;
//! * `real agent status` shows the policy, who holds the project, and the
//!   agent sessions;
//! * `real agent rollback [session]` returns the scene and every file an
//!   agent session touched to the session's start (or `--checkpoint`).

use std::path::Path;
use std::process::ExitCode;

use re_agent::init::{InitOptions, init};
use re_agent::lock::LockState;
use re_agent::session::list_sessions;
use re_agent::{Holder, Policy, Writer, open_writer, probe};
use re_command::{AllowAll, Origin};
use re_core::Diagnostic;
use serde_json::json;

use super::mcp::open_for_agents;

/// Arguments of `real agent`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct AgentArgs {
    /// What to do (default: `status`).
    #[command(subcommand)]
    pub command: Option<AgentCommand>,
}

/// The `real agent` subcommands.
#[derive(Clone, Debug, clap::Subcommand)]
pub enum AgentCommand {
    /// Write the agent policy, AGENTS.md, CLAUDE.md and MCP client configs.
    Init {
        /// Replace an existing .real/agent-policy.toml.
        #[arg(long)]
        force: bool,
        /// The program MCP clients start.
        #[arg(long, default_value = "real")]
        program: String,
        /// Point Claude Code at `real mcp --http ADDR` (a loopback address
        /// with a fixed port; 127.0.0.1:8765 when no ADDR is given) instead
        /// of starting `real mcp` over stdio. The token comes from
        /// REAL_MCP_TOKEN.
        #[arg(long, value_name = "ADDR", num_args = 0..=1, default_missing_value = "127.0.0.1:8765")]
        http: Option<String>,
    },
    /// Show the agent policy, the project lock and the agent sessions.
    Status,
    /// Return the scene and the files an agent session touched to a checkpoint.
    Rollback {
        /// The session (default: the latest).
        session: Option<String>,
        /// The checkpoint (default: the session's start).
        #[arg(long)]
        checkpoint: Option<String>,
    },
}

fn fail(d: &Diagnostic) -> ExitCode {
    eprint!("{}", re_core::render_human_all(std::slice::from_ref(d)));
    ExitCode::from(1)
}

/// Runs `real agent` for the project at `project`.
pub fn run(args: AgentArgs, project: &Path) -> anyhow::Result<ExitCode> {
    Ok(match args.command.unwrap_or(AgentCommand::Status) {
        AgentCommand::Init {
            force,
            program,
            http,
        } => {
            let http = match http.as_deref().map(http_url).transpose() {
                Ok(url) => url,
                Err(d) => return Ok(fail(&d)),
            };
            cmd_init(
                project,
                InitOptions {
                    program,
                    force,
                    http,
                },
            )
        }
        AgentCommand::Status => cmd_status(project),
        AgentCommand::Rollback {
            session,
            checkpoint,
        } => cmd_rollback(project, session, checkpoint),
    })
}

/// The URL of `real mcp --http ADDR`: a loopback address with a fixed port,
/// since client configs name it (`RE8502`).
fn http_url(addr: &str) -> Result<String, Diagnostic> {
    let addr = re_mcp::http::parse_addr(addr)?;
    if addr.port() == 0 {
        return Err(Diagnostic::error(
            re_mcp::codes::NOT_LOOPBACK,
            "client configs need a fixed port, not 0",
        )
        .with_hint(
            "pass an address such as 127.0.0.1:8765, and start `real mcp --http` with the same one",
        ));
    }
    Ok(format!("http://{addr}{}", re_mcp::http::ENDPOINT))
}

fn cmd_init(project: &Path, options: InitOptions) -> ExitCode {
    // AGENTS.md lists the commands an agent will see, so open the project.
    let app = match open_for_agents(project) {
        Ok(x) => x.app,
        Err(d) => return fail(&d),
    };
    // The Codex and Zed snippets name the project by its full path.
    let root = std::path::absolute(project).unwrap_or_else(|_| project.to_path_buf());
    let report = match init(&root, app.commands().specs(), &options) {
        Ok(r) => r,
        Err(d) => return fail(&d),
    };
    for path in &report.written {
        println!("wrote {path}");
    }
    for (path, why) in &report.kept {
        println!("kept {path}: {why}");
    }
    for (client, place, snippet) in &report.snippets {
        println!("\n{client} ({place}):\n{snippet}");
    }
    println!(
        "Agents may now edit within .real/agent-policy.toml's scopes; commit it to keep that choice. \
         Restart MCP clients that are already running so `real mcp` reads the new policy."
    );
    ExitCode::SUCCESS
}

fn cmd_status(project: &Path) -> ExitCode {
    match Policy::load(project) {
        Ok(p) => {
            println!("policy: max tier {}", p.max_tier.as_str());
            println!("  allow: {}", p.scopes.allow.join(", "));
            if !p.scopes.deny.is_empty() {
                println!("  deny: {}", p.scopes.deny.join(", "));
            }
            let b = p.budgets();
            println!(
                "  budgets: {} frames, {} ms, {} MB, {} operations per transaction",
                b.frames, b.milliseconds, b.memory_mb, b.entities_per_transaction
            );
        }
        Err(d) => return fail(&d),
    }
    match probe(project) {
        LockState::Free => println!("project: not open"),
        LockState::Editor(socket) => {
            println!("project: open in the editor ({})", socket.display());
        }
        LockState::Headless(0) => println!("project: open by a headless writer"),
        LockState::Headless(pid) => println!("project: open by a headless writer (pid {pid})"),
    }
    let sessions = list_sessions(project);
    if sessions.is_empty() {
        println!("sessions: none");
    } else {
        println!("sessions: {}", sessions.join(", "));
    }
    ExitCode::SUCCESS
}

fn cmd_rollback(project: &Path, session: Option<String>, checkpoint: Option<String>) -> ExitCode {
    let Some(session) = session.or_else(|| list_sessions(project).pop()) else {
        return fail(
            &Diagnostic::error(
                re_agent::codes::NO_SESSION,
                "this project has no agent sessions",
            )
            .with_hint("sessions start when an agent connects through `real mcp`"),
        );
    };
    let lock = match open_writer(project, Holder::headless("real agent rollback")) {
        Ok(Writer::Local(lock)) => lock,
        Ok(Writer::Remote(_)) => {
            return fail(
                &Diagnostic::error(
                    re_agent::codes::PROJECT_LOCKED,
                    "the editor has the project open",
                )
                .with_hint("close the editor first, then run `real agent rollback` again"),
            );
        }
        Err(d) => return fail(&d),
    };
    let (mut app, agent) = match open_for_agents(project) {
        Ok(x) => (x.app, x.agent),
        Err(d) => return fail(&d),
    };
    if let Err(d) = agent.resume_session(&session) {
        return fail(&d);
    }
    let outcome = app.dispatch(
        "checkpoint.restore",
        json!({ "checkpoint": checkpoint }),
        false,
        &AllowAll,
        Origin::User,
    );
    drop(lock);
    if !outcome.ok {
        eprint!("{}", re_core::render_human_all(&outcome.diagnostics));
        return ExitCode::from(1);
    }
    let d = &outcome.data;
    println!(
        "restored {} of session {session}; the state before is checkpoint {} (`real agent rollback {session} --checkpoint {}` goes back)",
        d["restored"].as_str().unwrap_or("?"),
        d["saved"].as_str().unwrap_or("?"),
        d["saved"].as_str().unwrap_or("?"),
    );
    ExitCode::SUCCESS
}
