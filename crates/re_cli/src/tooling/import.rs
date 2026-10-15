//! `real import <INPUT> --project <DIR> [--dry-run] [--keep-stale] [--json]`
//! (WP16): bring a Rojo-style project or a Studio loader script into a compat
//! project with `re_import`, and print its `ImportReport`.
//!
//! Importing into an existing project takes the headless project lock for
//! the import's duration, so it never writes under a running editor.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use re_compat::ClassRegistry;
use re_core::Diagnostic;
use re_import::{ImportOptions, ImportReport};

/// Arguments of `real import`.
#[derive(clap::Args, Debug)]
pub struct ImportArgs {
    /// A `*.project.json` file, a directory holding `default.project.json`,
    /// or a Luau loader script.
    pub input: PathBuf,
    /// Report what would change without writing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Keep instances an earlier import created that the input no longer
    /// declares.
    #[arg(long)]
    pub keep_stale: bool,
    /// Print the report as JSON.
    #[arg(long)]
    pub json: bool,
}

/// The class layer the importer checks classes and services against.
pub fn classes() -> Result<ClassRegistry, Diagnostic> {
    ClassRegistry::builtin().map_err(|e| {
        Diagnostic::error(
            re_import::codes::PROJECT_UNSUITABLE,
            format!("the compat class data does not load: {e}"),
        )
    })
}

fn existing_project(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
}

/// Runs `real import`.
///
/// # Errors
///
/// Never; problems are diagnostics and the exit code (1 when the import
/// failed or an instance was not imported).
pub fn run(args: ImportArgs, project: &Path) -> anyhow::Result<ExitCode> {
    let result = classes().and_then(|classes| {
        let _lock = if existing_project(project) && !args.dry_run {
            Some(re_agent::acquire(
                project,
                re_agent::Holder::headless("real import"),
            )?)
        } else {
            None
        };
        re_import::import(
            &args.input,
            project,
            &classes,
            &ImportOptions {
                dry_run: args.dry_run,
                keep_stale: args.keep_stale,
            },
        )
    });
    Ok(match result {
        Ok(report) => {
            print(&report, args.json);
            if report.ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        Err(d) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(
                        &serde_json::json!({ "ok": false, "diagnostics": [d] })
                    )
                    .unwrap_or_default()
                );
            } else {
                crate::print_diagnostics(&[d]);
            }
            ExitCode::from(1)
        }
    })
}

fn print(report: &ImportReport, json: bool) {
    if json {
        let mut value = serde_json::to_value(report).unwrap_or_default();
        value["ok"] = serde_json::json!(report.ok());
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_default()
        );
    } else {
        print!("{}", report.render_human());
    }
}
