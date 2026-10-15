//! `real editor`: opens the editor.
//!
//! WP6 created this file as a stub in its milestone "app" PR so that `re_cli`
//! compiles; from that merge on it belongs to WP8 (docs/M1_PLAN.md, ownership
//! rule). The two names below are frozen: `EditorArgs` (deriving `clap::Args`; the
//! owner may add fields, or replace it with a `pub use` of a type with the same
//! name) and `run`.

use std::path::Path;
use std::process::ExitCode;

/// Arguments of `real editor`.
#[derive(Clone, Debug, Default, clap::Args)]
pub struct EditorArgs {}

/// Runs `real editor` for the project at `project`.
pub fn run(args: EditorArgs, project: &Path) -> anyhow::Result<ExitCode> {
    let _ = (args, project);
    eprintln!("real editor: not built yet");
    Ok(ExitCode::from(2))
}
