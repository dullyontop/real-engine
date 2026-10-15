//! `cargo xtask coverage [--check]`: the compatibility layer's coverage table,
//! `crates/re_compat/coverage.json` (docs/M2_PLAN.md, WP9).
//!
//! The table comes from `re_compat`'s class, enum and library files through
//! its `coverage` example, so xtask links no engine crate. Until the crate
//! exists this prints `skipped: re_compat not built yet` and succeeds. With
//! `--check` it fails when the committed file differs from a fresh one.

use std::path::Path;

use anyhow::Result;

use crate::{cargo, output};

pub fn run(root: &Path, check: bool) -> Result<()> {
    if !root.join("crates/re_compat").is_dir() {
        println!("coverage: skipped: re_compat not built yet");
        return Ok(());
    }
    let mut cmd = cargo();
    cmd.args([
        "run",
        "--locked",
        "-q",
        "-p",
        "re_compat",
        "--example",
        "coverage",
        "--",
    ]);
    if check {
        cmd.arg("--check");
    }
    cmd.arg(root.join("crates/re_compat/coverage.json"));
    let out = output(cmd, root)?;
    print!("{out}");
    Ok(())
}
