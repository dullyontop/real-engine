//! `cargo xtask members DIR`: every `implemented` member of the
//! compatibility layer is called at least once from a test fixture
//! (docs/M2_PLAN.md, definition of done item 9).
//!
//! Class members are checked from traces: run the `re_compat` and `re_app`
//! tests with `REAL_COMPAT_MEMBER_TRACE=DIR`, and each test process writes
//! the members its scripts reached to `DIR/<pid>.txt`. Library members
//! (`Vector3.new`, `task.wait`) are plain functions with no class dispatch,
//! so they are checked by finding `Library.member` in the test sources.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

/// Where test fixtures live: Luau files and the Rust tests that embed Luau.
const FIXTURE_ROOTS: [&str; 3] = ["crates/re_compat/tests", "crates/re_app/tests", "examples"];

pub fn run(root: &Path, trace_dir: &Path) -> Result<()> {
    let coverage_path = root.join("crates/re_compat/coverage.json");
    let coverage: Value = serde_json::from_str(
        &std::fs::read_to_string(&coverage_path)
            .with_context(|| format!("reading {}", coverage_path.display()))?,
    )
    .with_context(|| format!("parsing {}", coverage_path.display()))?;

    let traced = read_traces(trace_dir)?;
    let sources = read_sources(root)?;

    let mut missing = Vec::new();
    let mut classes = 0;
    for (class, member) in implemented(&coverage, "classes") {
        classes += 1;
        let path = format!("{class}.{member}");
        if !traced.contains(&path) {
            missing.push(path);
        }
    }
    let mut libraries = 0;
    for (library, member) in implemented(&coverage, "libraries") {
        libraries += 1;
        let path = format!("{library}.{member}");
        if !sources.contains(&path) {
            missing.push(path);
        }
    }
    if missing.is_empty() {
        println!(
            "members: all {classes} implemented class members and {libraries} library members are called from a fixture"
        );
        return Ok(());
    }
    for m in &missing {
        eprintln!("  not called: {m}");
    }
    bail!(
        "{} implemented members are never called from a fixture (add a call under crates/re_compat/tests/, or mark the member `partial`)",
        missing.len()
    )
}

/// `(owner, member)` for every `implemented` member under `section`.
fn implemented<'a>(coverage: &'a Value, section: &str) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    for owner in coverage[section].as_array().into_iter().flatten() {
        let name = owner["name"].as_str().unwrap_or_default();
        for m in owner["members"].as_array().into_iter().flatten() {
            if m["status"] == "implemented" {
                out.push((name, m["name"].as_str().unwrap_or_default()));
            }
        }
    }
    out
}

fn read_traces(dir: &Path) -> Result<BTreeSet<String>> {
    let mut seen = BTreeSet::new();
    let mut files = 0;
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "txt") {
            files += 1;
            for line in std::fs::read_to_string(&path)?.lines() {
                seen.insert(line.trim().to_string());
            }
        }
    }
    if files == 0 {
        bail!(
            "no trace files in {}: run the re_compat and re_app tests with REAL_COMPAT_MEMBER_TRACE={} first",
            dir.display(),
            dir.display()
        );
    }
    Ok(seen)
}

/// All fixture text, concatenated.
fn read_sources(root: &Path) -> Result<String> {
    let mut text = String::new();
    for dir in FIXTURE_ROOTS {
        collect(&root.join(dir), &mut text)?;
    }
    Ok(text)
}

fn collect(dir: &Path, text: &mut String) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, text)?;
        } else if path
            .extension()
            .is_some_and(|e| e == "luau" || e == "lua" || e == "rs")
        {
            text.push_str(&std::fs::read_to_string(&path)?);
            text.push('\n');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_implemented_members_only() {
        let coverage: Value = serde_json::json!({
            "classes": [{"name": "Part", "members": [
                {"name": "Size", "status": "implemented"},
                {"name": "Foo", "status": "partial"}]}],
        });
        assert_eq!(implemented(&coverage, "classes"), vec![("Part", "Size")]);
        assert!(implemented(&coverage, "libraries").is_empty());
    }

    #[test]
    fn an_empty_trace_dir_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_traces(dir.path()).is_err());
        std::fs::write(dir.path().join("1.txt"), "Part.Size\nPart.Size\n").unwrap();
        assert_eq!(read_traces(dir.path()).unwrap().len(), 1);
    }
}
