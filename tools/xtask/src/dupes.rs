//! `cargo xtask dupes`: the duplicate-version gate (ADR-0028).
//!
//! For each desktop triple it runs `cargo tree -d -e normal --workspace
//! --all-features --target <triple>` (no cross toolchain needed: cargo only reads
//! the lockfile; `--all-features` matches `[graph] all-features` in deny.toml, so
//! duplicates that only optional features bring in are caught too) and
//! compares the duplicated packages with `[bans] skip` in `deny.toml`, as
//! (name, version) pairs. A crate with several versions passes when at most one of
//! its versions is missing from the skip-list. The same version listed twice (a
//! host and a target build with different features) is not a duplicate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::{cargo, output};

/// The three desktop triples of `deny.toml` `[graph] targets`.
pub const TRIPLES: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
];

/// A (name, version) pair; the version has no leading `v`.
pub type Pair = (String, String);

/// Reads `[bans] skip` from deny.toml. Accepts `{ crate = "name@version" }` tables
/// (cargo-deny 0.18 and later), `{ name, version }` tables and bare `"name@version"`
/// strings.
pub fn skip_list(deny_toml: &str) -> Result<BTreeSet<Pair>> {
    let doc: toml::Table = toml::from_str(deny_toml).context("parsing deny.toml")?;
    let mut pairs = BTreeSet::new();
    let skip = doc
        .get("bans")
        .and_then(|b| b.get("skip"))
        .and_then(|s| s.as_array())
        .cloned()
        .unwrap_or_default();
    for entry in skip {
        let spec = match &entry {
            toml::Value::String(s) => s.clone(),
            toml::Value::Table(t) => {
                if let Some(c) = t.get("crate").and_then(|c| c.as_str()) {
                    c.to_string()
                } else {
                    let name = t.get("name").and_then(|n| n.as_str());
                    let version = t.get("version").and_then(|v| v.as_str());
                    match (name, version) {
                        (Some(n), Some(v)) => format!("{n}@{}", v.trim_start_matches('=')),
                        _ => bail!("deny.toml: skip entry without crate or name/version: {entry}"),
                    }
                }
            }
            other => bail!("deny.toml: unexpected skip entry {other}"),
        };
        let (name, version) = spec
            .split_once('@')
            .with_context(|| format!("deny.toml: skip entry `{spec}` has no exact version"))?;
        let version = version.trim_start_matches('=');
        if version.is_empty() || !version.starts_with(|c: char| c.is_ascii_digit()) {
            bail!("deny.toml: skip entry `{spec}` must name one exact version");
        }
        pairs.insert((name.to_string(), version.to_string()));
    }
    Ok(pairs)
}

/// Parses `cargo tree -d --depth 0 --prefix none` output into (name, version) pairs.
pub fn parse_tree(text: &str) -> BTreeSet<Pair> {
    text.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let name = words.next()?;
            let version = words.next()?.strip_prefix('v')?;
            Some((name.to_string(), version.to_string()))
        })
        .collect()
}

/// The duplicated pairs that the skip-list does not accept: for every crate with
/// more than one version, all versions not in `skip`, if there is more than one.
pub fn unaccepted(found: &BTreeSet<Pair>, skip: &BTreeSet<Pair>) -> Vec<Pair> {
    let mut by_name: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for (name, version) in found {
        by_name.entry(name).or_default().insert(version);
    }
    let mut bad = Vec::new();
    for (name, versions) in by_name {
        if versions.len() < 2 {
            continue; // one version, perhaps listed twice for host and target builds
        }
        let unskipped: Vec<&str> = versions
            .into_iter()
            .filter(|v| !skip.contains(&(name.to_string(), v.to_string())))
            .collect();
        if unskipped.len() > 1 {
            bad.extend(
                unskipped
                    .into_iter()
                    .map(|v| (name.to_string(), v.to_string())),
            );
        }
    }
    bad
}

pub fn run(root: &Path) -> Result<()> {
    let deny = std::fs::read_to_string(root.join("deny.toml")).context("reading deny.toml")?;
    let skip = skip_list(&deny)?;
    let mut failures = Vec::new();
    for triple in TRIPLES {
        let mut cmd = cargo();
        cmd.args([
            "tree",
            "--locked",
            "--workspace",
            "--all-features",
            "-d",
            "-e",
            "normal",
            "--target",
            triple,
            "--depth",
            "0",
            "--prefix",
            "none",
        ]);
        let found = parse_tree(&output(cmd, root)?);
        let bad = unaccepted(&found, &skip);
        if bad.is_empty() {
            let mut versions: BTreeMap<&str, usize> = BTreeMap::new();
            for (name, _) in &found {
                *versions.entry(name).or_default() += 1;
            }
            let multi = versions.values().filter(|&&n| n > 1).count();
            println!("dupes: {triple}: ok ({multi} crates with several versions, all accepted)");
        } else {
            for (name, version) in &bad {
                failures.push(format!("{triple}: {name} {version}"));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        for f in &failures {
            eprintln!("  new duplicate: {f}");
        }
        bail!(
            "dupes: {} duplicate version(s) not in deny.toml [bans] skip; add them with a \
             reason (and a row in docs/DEPENDENCIES.md), or remove the duplicate",
            failures.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> BTreeSet<Pair> {
        list.iter()
            .map(|(n, v)| (n.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn reads_every_skip_form() {
        let toml = r#"
            [bans]
            skip = [
                { crate = "glam@0.33.12", reason = "kira" },
                { name = "syn", version = "=1.0.109" },
                "spin@0.9.9",
            ]
        "#;
        assert_eq!(
            skip_list(toml).unwrap(),
            pairs(&[("glam", "0.33.12"), ("syn", "1.0.109"), ("spin", "0.9.9")])
        );
        assert!(skip_list("[bans]\nskip = [\"glam\"]").is_err());
        assert!(skip_list("").unwrap().is_empty());
    }

    #[test]
    fn parses_cargo_tree_output() {
        let text = "glam v0.33.12\nglam v0.34.1\n\nthiserror-impl v1.0.69 (proc-macro)\ntoml_parser v1.1.5+spec-1.1.0\ntoml_parser v1.1.5+spec-1.1.0 (*)\n";
        assert_eq!(
            parse_tree(text),
            pairs(&[
                ("glam", "0.33.12"),
                ("glam", "0.34.1"),
                ("thiserror-impl", "1.0.69"),
                ("toml_parser", "1.1.5+spec-1.1.0"),
            ])
        );
    }

    #[test]
    fn only_unlisted_second_versions_fail() {
        let found = pairs(&[
            ("glam", "0.33.12"),
            ("glam", "0.34.1"),
            ("serde_json", "1.0.154"),
            ("syn", "1.0.109"),
            ("syn", "2.0.119"),
            ("syn", "3.0.9"),
        ]);
        let skip = pairs(&[("glam", "0.33.12"), ("syn", "1.0.109")]);
        assert_eq!(
            unaccepted(&found, &skip),
            pairs(&[("syn", "2.0.119"), ("syn", "3.0.9")])
                .into_iter()
                .collect::<Vec<_>>()
        );
        let skip = pairs(&[("glam", "0.33.12"), ("syn", "1.0.109"), ("syn", "3.0.9")]);
        assert!(unaccepted(&found, &skip).is_empty());
    }
}
