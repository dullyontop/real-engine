//! `cargo xtask check-graph`: the crate dependency rules of ARCHITECTURE §2.
//!
//! The graph comes from `cargo metadata --all-features` (normal and build edges;
//! dev-dependencies do not count). Rules about crates that do not exist yet pass
//! vacuously. The `re_cli --no-default-features` rule uses `cargo tree`, which
//! resolves features exactly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::{cargo, output};

/// The subsystem crates: they reach only the contract layer (ADR-0006).
const SUBSYSTEMS: &[&str] = &[
    "re_render",
    "re_physics",
    "re_audio",
    "re_input",
    "re_assets",
    "re_anim",
];

/// The contract layer.
const CONTRACT: &[&str] = &["re_core", "re_types", "re_noise"];

/// The only crates that may name `bevy_ecs` directly (ADR-0005).
const BEVY_USERS: &[&str] = &["re_reflect", "re_scene", "re_command"];

/// The crates.io index, as `cargo metadata` names it.
const CRATES_IO: &[&str] = &[
    "registry+https://github.com/rust-lang/crates.io-index",
    "sparse+https://index.crates.io/",
];

/// What one rule demands of one crate.
#[derive(Debug, Clone, Copy)]
pub enum Check {
    /// The transitive dependencies contain none of these packages.
    Reaches(&'static [&'static str]),
    /// The workspace crates among the transitive dependencies are all in this list.
    WorkspaceOnly(&'static [&'static str]),
    /// The direct dependencies are all in this list.
    DirectOnly(&'static [&'static str]),
}

/// A rule: a crate, what it must satisfy, and the ARCHITECTURE wording.
#[derive(Debug, Clone, Copy)]
pub struct Rule {
    pub krate: &'static str,
    pub check: Check,
    pub why: &'static str,
}

const NOT_SUBSYSTEMS_OR_MLUA: &[&str] = &[
    "re_render",
    "re_physics",
    "re_audio",
    "re_input",
    "re_assets",
    "re_anim",
    "mlua",
];

/// Every rule of ARCHITECTURE §2 ("must not depend on"), except the bevy_ecs and
/// `re_cli --no-default-features` rules, which are checked separately.
pub fn rules() -> Vec<Rule> {
    let mut rules = vec![
        Rule {
            krate: "re_core",
            check: Check::WorkspaceOnly(&[]),
            why: "re_core depends on nothing engine-specific",
        },
        Rule {
            krate: "re_noise",
            check: Check::DirectOnly(&[]),
            why: "re_noise has zero dependencies",
        },
        Rule {
            krate: "re_types",
            check: Check::DirectOnly(&["glam", "serde", "serde_json", "schemars"]),
            why: "re_types depends only on glam, serde and serde_json (and schemars behind \
                  its optional `schemars` feature)",
        },
        Rule {
            krate: "re_types",
            check: Check::WorkspaceOnly(&[]),
            why: "re_types depends on no engine crate (not even re_core)",
        },
        Rule {
            krate: "re_reflect",
            check: Check::Reaches(NOT_SUBSYSTEMS_OR_MLUA),
            why: "re_reflect must not depend on subsystems or mlua",
        },
        Rule {
            krate: "re_scene",
            check: Check::Reaches(NOT_SUBSYSTEMS_OR_MLUA),
            why: "re_scene must not depend on subsystems or mlua",
        },
        Rule {
            krate: "re_command",
            check: Check::Reaches(&[
                "re_render",
                "re_physics",
                "re_audio",
                "re_input",
                "re_assets",
                "re_anim",
                "mlua",
                "tokio",
            ]),
            why: "re_command must not depend on subsystems, mlua or tokio",
        },
        Rule {
            krate: "re_geom",
            check: Check::Reaches(&["wgpu", "oxijolt", "mlua"]),
            why: "re_geom reaches no wgpu, oxijolt or mlua",
        },
        Rule {
            krate: "re_proc",
            check: Check::Reaches(&["wgpu", "oxijolt", "mlua"]),
            why: "re_proc reaches no wgpu, oxijolt or mlua",
        },
        Rule {
            krate: "re_script",
            check: Check::Reaches(&[
                "re_render",
                "re_physics",
                "re_audio",
                "re_input",
                "re_assets",
                "re_anim",
                "full_moon",
            ]),
            why: "re_script reaches subsystems only through HostServices, and never full_moon",
        },
        Rule {
            krate: "re_script_check",
            check: Check::Reaches(&["re_app", "tokio"]),
            why: "re_script_check must not depend on re_app or tokio",
        },
        Rule {
            krate: "re_app",
            check: Check::Reaches(&["tokio", "rmcp", "full_moon"]),
            why: "re_app must not depend on tokio, rmcp or full_moon",
        },
        Rule {
            krate: "re_agent",
            check: Check::Reaches(&["tokio"]),
            why: "re_agent must not depend on tokio",
        },
        Rule {
            krate: "re_mcp",
            check: Check::Reaches(&["re_app"]),
            why: "re_mcp must not depend on re_app",
        },
    ];
    for &subsystem in SUBSYSTEMS {
        rules.push(Rule {
            krate: subsystem,
            check: Check::WorkspaceOnly(CONTRACT),
            why: "subsystem crates reach only the contract layer",
        });
    }
    rules
}

/// The dependency graph of the workspace, by package name.
#[derive(Debug, Default)]
pub struct Graph {
    /// Workspace member names.
    pub members: BTreeSet<String>,
    /// Direct normal and build dependencies of each package.
    pub deps: BTreeMap<String, BTreeSet<String>>,
    /// Packages that come from neither crates.io nor the workspace: (name, source).
    pub foreign: Vec<(String, String)>,
}

impl Graph {
    /// Every package `krate` reaches through normal and build edges.
    pub fn closure(&self, krate: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut stack = vec![krate.to_string()];
        while let Some(next) = stack.pop() {
            for dep in self.deps.get(&next).into_iter().flatten() {
                if seen.insert(dep.clone()) {
                    stack.push(dep.clone());
                }
            }
        }
        seen
    }

    /// Parses `cargo metadata --format-version 1` output.
    pub fn from_metadata(metadata: &Value) -> Result<Graph> {
        let packages = metadata["packages"]
            .as_array()
            .context("metadata has no packages")?;
        let mut names = BTreeMap::new();
        let mut sources = BTreeMap::new();
        for p in packages {
            let id = p["id"].as_str().context("package without id")?;
            let name = p["name"].as_str().context("package without name")?;
            names.insert(id.to_string(), name.to_string());
            sources.insert(id.to_string(), p["source"].as_str().map(String::from));
        }
        let mut graph = Graph::default();
        for id in metadata["workspace_members"]
            .as_array()
            .context("metadata has no workspace_members")?
        {
            let id = id.as_str().context("bad workspace member id")?;
            graph.members.insert(names[id].clone());
        }
        let nodes = metadata["resolve"]["nodes"]
            .as_array()
            .context("metadata has no resolve graph")?;
        for node in nodes {
            let id = node["id"].as_str().context("node without id")?;
            let name = names[id].clone();
            let entry = graph.deps.entry(name.clone()).or_default();
            for dep in node["deps"].as_array().into_iter().flatten() {
                let counts = dep["dep_kinds"].as_array().into_iter().flatten().any(|k| {
                    // `null` is a normal dependency; "build" counts too; "dev" does not.
                    k["kind"].is_null() || k["kind"].as_str() == Some("build")
                });
                if counts {
                    let dep_id = dep["pkg"].as_str().context("dep without pkg")?;
                    entry.insert(names[dep_id].clone());
                }
            }
            match &sources[id] {
                Some(source) if CRATES_IO.contains(&source.as_str()) => {}
                None if graph.members.contains(&name) => {}
                Some(source) => graph.foreign.push((name, source.clone())),
                None => graph
                    .foreign
                    .push((name, "a path outside the workspace".into())),
            }
        }
        Ok(graph)
    }
}

/// Checks every rule against `graph`. Returns the violations and the number of
/// rules that applied to an existing crate.
pub fn check(graph: &Graph, rules: &[Rule]) -> (Vec<String>, usize) {
    let mut violations = Vec::new();
    let mut applied = 0;
    for rule in rules {
        if !graph.members.contains(rule.krate) {
            continue; // the crate does not exist yet: the rule passes vacuously
        }
        applied += 1;
        let bad: Vec<String> = match rule.check {
            Check::Reaches(forbidden) => graph
                .closure(rule.krate)
                .into_iter()
                .filter(|d| forbidden.contains(&d.as_str()))
                .collect(),
            Check::WorkspaceOnly(allowed) => graph
                .closure(rule.krate)
                .into_iter()
                .filter(|d| graph.members.contains(d) && !allowed.contains(&d.as_str()))
                .collect(),
            Check::DirectOnly(allowed) => graph
                .deps
                .get(rule.krate)
                .into_iter()
                .flatten()
                .filter(|d| !allowed.contains(&d.as_str()))
                .cloned()
                .collect(),
        };
        if !bad.is_empty() {
            violations.push(format!(
                "{}: depends on {} ({})",
                rule.krate,
                bad.join(", "),
                rule.why
            ));
        }
    }
    applied += 1;
    for member in &graph.members {
        let names_bevy = graph
            .deps
            .get(member)
            .is_some_and(|deps| deps.contains("bevy_ecs"));
        if names_bevy && !BEVY_USERS.contains(&member.as_str()) {
            violations.push(format!(
                "{member}: depends on bevy_ecs directly (only re_reflect, re_scene and re_command \
                 may; use the `re_scene::ecs` re-export)"
            ));
        }
    }
    applied += 1;
    for (name, source) in &graph.foreign {
        violations.push(format!(
            "{name}: comes from {source} (only crates.io dependencies are allowed)"
        ));
    }
    (violations, applied)
}

pub fn run(root: &Path) -> Result<()> {
    let mut cmd = cargo();
    cmd.args([
        "metadata",
        "--format-version",
        "1",
        "--all-features",
        "--locked",
    ]);
    let metadata: Value =
        serde_json::from_str(&output(cmd, root)?).context("parsing cargo metadata output")?;
    let graph = Graph::from_metadata(&metadata)?;
    let (mut violations, mut applied) = check(&graph, &rules());

    // `re_cli --no-default-features` (the runtime-only build) reaches no tooling.
    if graph.members.contains("re_cli") {
        applied += 1;
        let mut cmd = cargo();
        cmd.args([
            "tree",
            "--locked",
            "-p",
            "re_cli",
            "--no-default-features",
            "-e",
            "normal,build",
            "--prefix",
            "none",
            "--format",
            "{p}",
        ]);
        let tree = output(cmd, root)?;
        let reached: BTreeSet<&str> = tree
            .lines()
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        for forbidden in ["tokio", "rmcp", "full_moon"] {
            if reached.contains(forbidden) {
                violations.push(format!(
                    "re_cli --no-default-features reaches {forbidden} (the runtime-only build \
                     links no tooling, ADR-0027)"
                ));
            }
        }
    }

    if violations.is_empty() {
        println!(
            "check-graph: ok ({} workspace crates, {applied} rules applied; rules for crates \
             that do not exist yet pass vacuously)",
            graph.members.len()
        );
        Ok(())
    } else {
        for v in &violations {
            eprintln!("  {v}");
        }
        bail!("check-graph: {} rule violation(s)", violations.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph(members: &[&str], edges: &[(&str, &str)]) -> Graph {
        let mut g = Graph {
            members: members.iter().map(|s| s.to_string()).collect(),
            ..Graph::default()
        };
        for (from, to) in edges {
            g.deps
                .entry(from.to_string())
                .or_default()
                .insert(to.to_string());
        }
        g
    }

    #[test]
    fn rules_for_missing_crates_pass_vacuously() {
        let g = graph(
            &["re_core", "re_types", "re_noise", "xtask"],
            &[
                ("re_core", "rayon"),
                ("re_types", "glam"),
                ("re_types", "serde"),
            ],
        );
        let (violations, applied) = check(&g, &rules());
        assert!(violations.is_empty(), "{violations:?}");
        // re_core, re_noise, two re_types rules, plus bevy_ecs and sources.
        assert_eq!(applied, 6);
    }

    #[test]
    fn transitive_violations_are_found() {
        let g = graph(
            &["re_render", "re_scene", "re_reflect", "re_core", "re_types"],
            &[
                ("re_render", "re_types"),
                ("re_render", "helper"),
                ("helper", "re_scene"),
                ("re_scene", "re_reflect"),
                ("re_reflect", "mid"),
                ("mid", "mlua"),
                ("re_types", "re_core"),
            ],
        );
        let (violations, _) = check(&g, &rules());
        let text = violations.join("\n");
        assert!(
            text.contains("re_render: depends on re_reflect, re_scene"),
            "{text}"
        );
        assert!(text.contains("re_scene: depends on mlua"), "{text}");
        assert!(text.contains("re_types: depends on re_core"), "{text}");
    }

    #[test]
    fn only_model_crates_name_bevy_ecs() {
        let g = graph(
            &["re_scene", "re_proc"],
            &[("re_scene", "bevy_ecs"), ("re_proc", "bevy_ecs")],
        );
        let (violations, _) = check(&g, &rules());
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].starts_with("re_proc: depends on bevy_ecs directly"));
    }

    #[test]
    fn foreign_sources_are_rejected() {
        let mut g = graph(&["re_core"], &[]);
        g.foreign
            .push(("x".into(), "git+https://example.invalid/x".into()));
        let (violations, _) = check(&g, &rules());
        assert_eq!(violations.len(), 1);
    }

    #[test]
    fn parses_cargo_metadata() {
        let metadata = serde_json::json!({
            "packages": [
                { "id": "a 0.1.0 (path+file:///w/crates/re_core)", "name": "re_core", "source": null },
                { "id": "b", "name": "rayon", "source": "registry+https://github.com/rust-lang/crates.io-index" },
                { "id": "c", "name": "tempfile", "source": "registry+https://github.com/rust-lang/crates.io-index" },
                { "id": "d", "name": "cc", "source": "registry+https://github.com/rust-lang/crates.io-index" }
            ],
            "workspace_members": ["a 0.1.0 (path+file:///w/crates/re_core)"],
            "resolve": { "nodes": [
                { "id": "a 0.1.0 (path+file:///w/crates/re_core)", "deps": [
                    { "pkg": "b", "dep_kinds": [{ "kind": null, "target": null }] },
                    { "pkg": "c", "dep_kinds": [{ "kind": "dev", "target": null }] },
                    { "pkg": "d", "dep_kinds": [{ "kind": "build", "target": null }] }
                ]},
                { "id": "b", "deps": [] },
                { "id": "c", "deps": [] },
                { "id": "d", "deps": [] }
            ]}
        });
        let g = Graph::from_metadata(&metadata).unwrap();
        assert_eq!(
            g.closure("re_core").into_iter().collect::<Vec<_>>(),
            ["cc", "rayon"],
            "dev-dependencies do not count"
        );
        assert!(g.foreign.is_empty());
    }
}
