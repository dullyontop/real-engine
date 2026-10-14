//! `cargo xtask`: repository checks for Real Engine.
//!
//! ```text
//! cargo xtask check-graph      crate dependency rules (ARCHITECTURE §2)
//! cargo xtask dupes            duplicate crate versions against deny.toml's skip-list
//! cargo xtask explain          every RE#### code has an explain page, in range, one crate
//! cargo xtask schemas [--check]  regenerate (or check) the JSON Schemas in schemas/
//! cargo xtask check            all of the above, with `schemas --check`
//! ```
//!
//! The alias lives in `.cargo/config.toml`. xtask links no engine crate: it reads
//! `cargo metadata` and `cargo tree` output and the source tree.

mod dupes;
mod explain;
mod graph;
mod schemas;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use anyhow::{Context, Result, bail};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  check-graph        check the crate dependency rules of ARCHITECTURE.md §2
  dupes              check duplicate crate versions against deny.toml [bans] skip
  explain            check that every RE#### code has an explain page, lies in its
                     crate's range and is used by one crate only
  schemas [--check]  regenerate schemas/ (or fail if the committed files differ)
  check              run check-graph, dupes, explain and schemas --check
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = workspace_root();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["check-graph"] => graph::run(&root),
        ["dupes"] => dupes::run(&root),
        ["explain"] => explain::run(&root),
        ["schemas"] => schemas::run(&root, false),
        ["schemas", "--check"] => schemas::run(&root, true),
        ["check"] => check_all(&root),
        ["help" | "--help" | "-h"] => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn check_all(root: &Path) -> Result<()> {
    type Step = (&'static str, fn(&Path) -> Result<()>);
    let steps: [Step; 4] = [
        ("check-graph", graph::run),
        ("dupes", dupes::run),
        ("explain", explain::run),
        ("schemas --check", |root| schemas::run(root, true)),
    ];
    let mut failed = Vec::new();
    for (name, step) in steps {
        println!("== xtask {name}");
        if let Err(e) = step(root) {
            eprintln!("error: {e:#}");
            failed.push(name);
        }
    }
    if failed.is_empty() {
        println!("== xtask check: all passed");
        Ok(())
    } else {
        bail!("xtask check failed: {}", failed.join(", "))
    }
}

/// The workspace root: two levels above this crate's manifest (`tools/xtask`).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("tools/xtask lies two levels below the workspace root")
        .to_path_buf()
}

/// The cargo binary that runs us (or `cargo` on the PATH).
fn cargo() -> Command {
    let mut cmd = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    // Keep child cargo output plain and predictable.
    cmd.env("CARGO_TERM_COLOR", "never");
    cmd
}

/// Runs a command in `root` and returns its standard output.
fn output(mut cmd: Command, root: &Path) -> Result<String> {
    cmd.current_dir(root);
    let shown = format!("{cmd:?}");
    let out = cmd.output().with_context(|| format!("running {shown}"))?;
    if !out.status.success() {
        bail!(
            "{shown} failed ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8(out.stdout).with_context(|| format!("{shown} printed invalid UTF-8"))
}
