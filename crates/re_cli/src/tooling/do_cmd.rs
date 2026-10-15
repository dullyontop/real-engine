//! `real do <command> [--args JSON] [--dry-run]`: run one engine command
//! against the project from the shell (ADR-0020, ADR-0027).
//!
//! `real do` is a headless writer. When the editor holds the project, the
//! command goes through the editor's socket and the editor applies it.
//! Otherwise `real do` takes the project lock for the command's duration
//! (so a second headless writer gets `RE8003`), opens the project, applies the
//! command and saves the scene when it changed.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use re_agent::{Holder, Writer};
use re_app::{App, AppConfig};
use re_command::{AllowAll, CommandOutcome, Origin};
use re_core::Diagnostic;

/// Arguments of `real do`.
#[derive(clap::Args, Debug)]
pub struct DoArgs {
    /// The command, such as `entity.spawn` (`real describe` lists them).
    pub command: String,
    /// The command's arguments as a JSON object.
    #[arg(long, default_value = "{}")]
    pub args: String,
    /// Report what the command would change without changing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Print the whole outcome (`ok`, `changes`, `diagnostics`, `data`) as JSON.
    #[arg(long)]
    pub json: bool,
}

/// How `real do` reached the project.
fn via(writer: &Writer) -> &'static str {
    match writer {
        Writer::Local(_) => "local",
        Writer::Remote(_) => "editor",
    }
}

fn open(project: &Path) -> Result<App, Diagnostic> {
    let config = AppConfig {
        persist_edits: true,
        ..AppConfig::load(project)?
    };
    let mut app = App::open(project, config)?;
    crate::explain::register(app.commands_mut());
    re_agent::register_commands(app.commands_mut(), &re_agent::Agent::new(project));
    app.add_verify_stage(Arc::new(super::check::ScanStage));
    Ok(app)
}

/// Runs `real do`.
///
/// # Errors
///
/// Never; problems are diagnostics and the exit code (1 when the command
/// failed, 2 when `--args` is not a JSON object).
pub fn run(args: DoArgs, project: &Path) -> anyhow::Result<ExitCode> {
    let parsed: serde_json::Value = match serde_json::from_str(&args.args) {
        Ok(v @ serde_json::Value::Object(_)) => v,
        Ok(_) => {
            eprintln!("error: --args must be a JSON object, such as '{{\"id\": \"crate_1\"}}'");
            return Ok(ExitCode::from(2));
        }
        Err(e) => {
            eprintln!("error: --args is not valid JSON: {e}");
            return Ok(ExitCode::from(2));
        }
    };
    let writer = match re_agent::open_writer(project, Holder::headless("real do")) {
        Ok(w) => w,
        Err(d) => return Ok(crate::fail(d)),
    };
    let outcome = match &writer {
        Writer::Remote(handle) => handle.call(&args.command, parsed, args.dry_run),
        Writer::Local(_lock) => match open(project) {
            // The lock (`_lock`) is held until `writer` drops, after the save.
            Ok(mut app) => {
                app.dispatch(&args.command, parsed, args.dry_run, &AllowAll, Origin::User)
            }
            Err(d) => CommandOutcome::error(d),
        },
    };
    print(&outcome, args.json, via(&writer));
    drop(writer);
    Ok(if outcome.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

fn print(outcome: &CommandOutcome, json: bool, via: &str) {
    if json {
        let mut value = serde_json::to_value(outcome).unwrap_or_default();
        value["via"] = serde_json::json!(via);
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
        return;
    }
    crate::print_diagnostics(&outcome.diagnostics);
    if !outcome.data.is_null() {
        println!(
            "{}",
            serde_json::to_string_pretty(&outcome.data).unwrap_or_default()
        );
    }
    if outcome.ok {
        eprintln!(
            "ok: {} change{}{}",
            outcome.changes.len(),
            if outcome.changes.len() == 1 { "" } else { "s" },
            if via == "editor" {
                " (applied by the editor)"
            } else {
                ""
            }
        );
    }
}
