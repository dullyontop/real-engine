//! `cargo xtask schemas [--check]`: the generated JSON Schemas in `schemas/`.
//!
//! The schemas come from the engine itself (`real schema --out <dir>`), so this
//! command needs `crates/re_cli`; until that crate exists it prints
//! `skipped: re_cli not built yet` and succeeds. With `--check` it generates into a
//! scratch directory under `target/` and fails when the result differs from the
//! committed `schemas/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::{cargo, output};

/// Reads every file below `dir` as (relative path → bytes).
fn read_tree(dir: &Path) -> Result<BTreeMap<PathBuf, Vec<u8>>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) -> Result<()> {
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                walk(base, &path, out)?;
            } else {
                let bytes = std::fs::read(&path)?;
                out.insert(path.strip_prefix(base).unwrap().to_path_buf(), bytes);
            }
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    if dir.is_dir() {
        walk(dir, dir, &mut out)?;
    }
    Ok(out)
}

/// The differences between two schema trees, one line each.
pub fn diff(
    committed: &BTreeMap<PathBuf, Vec<u8>>,
    fresh: &BTreeMap<PathBuf, Vec<u8>>,
) -> Vec<String> {
    let mut lines = Vec::new();
    for (path, bytes) in fresh {
        match committed.get(path) {
            None => lines.push(format!("schemas/{} is missing", path.display())),
            Some(old) if old != bytes => {
                lines.push(format!("schemas/{} is out of date", path.display()))
            }
            Some(_) => {}
        }
    }
    for path in committed.keys() {
        if !fresh.contains_key(path) {
            lines.push(format!("schemas/{} is no longer generated", path.display()));
        }
    }
    lines
}

pub fn run(root: &Path, check: bool) -> Result<()> {
    if !root.join("crates/re_cli").is_dir() {
        println!("schemas: skipped: re_cli not built yet");
        return Ok(());
    }
    let out_dir = if check {
        let dir = root.join("target/xtask/schemas");
        if dir.exists() {
            std::fs::remove_dir_all(&dir).with_context(|| format!("clearing {}", dir.display()))?;
        }
        dir
    } else {
        root.join("schemas")
    };
    std::fs::create_dir_all(&out_dir)?;
    let mut cmd = cargo();
    cmd.args(["run", "--locked", "-p", "re_cli", "--", "schema", "--out"]);
    cmd.arg(&out_dir);
    output(cmd, root)?;
    if !check {
        println!("schemas: written to schemas/");
        return Ok(());
    }
    let differences = diff(&read_tree(&root.join("schemas"))?, &read_tree(&out_dir)?);
    if differences.is_empty() {
        println!("schemas: ok (schemas/ matches the engine)");
        Ok(())
    } else {
        for d in &differences {
            eprintln!("  {d}");
        }
        bail!("schemas: committed files differ; run `cargo xtask schemas` and commit the result")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipped_without_re_cli() {
        let dir = tempfile::tempdir().unwrap();
        assert!(run(dir.path(), true).is_ok());
    }

    #[test]
    fn diff_names_each_kind_of_change() {
        let tree = |items: &[(&str, &str)]| -> BTreeMap<PathBuf, Vec<u8>> {
            items
                .iter()
                .map(|(p, c)| (PathBuf::from(p), c.as_bytes().to_vec()))
                .collect()
        };
        let committed = tree(&[("a.json", "1"), ("b.json", "2"), ("old.json", "x")]);
        let fresh = tree(&[("a.json", "1"), ("b.json", "3"), ("new.json", "y")]);
        assert_eq!(
            diff(&committed, &fresh),
            [
                "schemas/b.json is out of date",
                "schemas/new.json is missing",
                "schemas/old.json is no longer generated"
            ]
        );
        assert!(diff(&committed, &committed).is_empty());
    }
}
