//! `real agent`: sets up and manages agent access (policy, client configs, rollback).
//!
//! WP6 created this file as a stub in its milestone "app" PR so that `re_cli`
//! compiles; from that merge on it belongs to WP7 (docs/M1_PLAN.md, ownership
//! rule). The two names below are frozen: `AgentArgs` (deriving `clap::Args`; the
//! owner may add fields, or replace it with a `pub use` of a type with the same
//! name) and `run`.

use std::path::Path;
use std::process::ExitCode;

/// Arguments of `real agent`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct AgentArgs {}

/// Runs `real agent` for the project at `project`.
pub fn run(args: AgentArgs, project: &Path) -> anyhow::Result<ExitCode> {
    let _ = (args, project);
    eprintln!("real agent: not built yet");
    Ok(ExitCode::from(2))
}
