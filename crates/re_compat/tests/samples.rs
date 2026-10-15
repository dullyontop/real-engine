//! The private sample runner (ADR-0033, docs/M2_PLAN.md "The sample runner is
//! public, the kit is private").
//!
//! It reads every `*.expect.json` in the directory `REAL_SAMPLE_KIT` names,
//! imports each sample with `re_import` into a temporary project, and runs the
//! file's checks in order. Without the variable it prints
//! `skipped: REAL_SAMPLE_KIT not set` and passes.
//!
//! An expectation file:
//!
//! ```json
//! {
//!   "name": "sample-a",
//!   "input": "path/to/loader.luau",
//!   "input_sha256": "7aeb6cf2",
//!   "import": { "keep_stale": false },
//!   "checks": [
//!     { "id": "A-1", "step": "import.ok" },
//!     { "id": "A-2", "step": "import.created_by_class", "args": { "Folder": 8 } }
//!   ]
//! }
//! ```
//!
//! Relative paths are relative to the kit directory. `input_sha256` is a
//! prefix of the input's SHA-256 in hex. Each check names a step; the steps
//! live in modules under `tests/samples/`, one per work package, registered
//! in `samples/mod.rs`. A check whose step is not registered yet is counted
//! as pending, not failed, so a kit can list checks for work that has not
//! landed. The run fails when any check fails; it prints, per sample, the
//! pass, fail and pending counts and each failure.

#[path = "samples/mod.rs"]
mod samples;

use std::path::{Path, PathBuf};

use samples::{Ctx, Expect, Outcome};

fn kit_files(kit: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(kit)
        .unwrap_or_else(|e| panic!("REAL_SAMPLE_KIT={}: {e}", kit.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".expect.json"))
        .collect();
    files.sort();
    files
}

#[test]
fn samples() {
    let Some(kit) = std::env::var_os("REAL_SAMPLE_KIT") else {
        println!("skipped: REAL_SAMPLE_KIT not set");
        return;
    };
    let kit = PathBuf::from(kit);
    let only = std::env::var("REAL_SAMPLE_ONLY").ok();
    let mut failures = 0;
    let mut totals = (0, 0, 0);
    for file in kit_files(&kit) {
        let expect = Expect::read(&file);
        if only.as_deref().is_some_and(|o| o != expect.name) {
            continue;
        }
        let mut ctx = Ctx::new(&kit, &expect);
        let (mut pass, mut fail, mut pending) = (0, 0, 0);
        let mut messages = Vec::new();
        for check in &expect.checks {
            match ctx.run(check) {
                Outcome::Pass => pass += 1,
                Outcome::Pending => pending += 1,
                Outcome::Fail(m) => {
                    fail += 1;
                    messages.push(format!("  FAIL {}: {m}", check.id));
                }
            }
        }
        println!(
            "{}: {pass} passed, {fail} failed, {pending} pending",
            expect.name
        );
        for m in &messages {
            println!("{m}");
        }
        failures += fail;
        totals = (totals.0 + pass, totals.1 + fail, totals.2 + pending);
    }
    println!(
        "samples: {} passed, {} failed, {} pending",
        totals.0, totals.1, totals.2
    );
    assert_eq!(failures, 0, "{failures} sample check(s) failed");
}
