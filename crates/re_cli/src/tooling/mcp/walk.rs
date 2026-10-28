//! The walk of WP44's acceptance, "a read-only policy cannot write any file
//! through any command" (ADR-0020, the WP44 amendment's decision 8). It
//! calls every agent-facing command, as `real mcp` registers them and with
//! a Luau tool, on a project whose edits are saved as `real mcp` saves
//! them, under three agent policies:
//!
//! * read-only: nothing changes but the engine's own folders (an image
//!   under `.real/out/`, the agent's checkpoints, caches);
//! * `edit` within `scripts/**`: scripts change, and the entry scene, which
//!   the tier governs, but nothing else;
//! * `destructive` within `**`, with `scripts/secret/**` denied: `.real/`
//!   and `.git/` (however they are spelled), the denied folder and the
//!   files outside the project never change, and a screenshot never
//!   replaces a file.
//!
//! Each command is called with valid arguments, and with every path it
//! declares aimed in turn at each file the policy forbids, once as a dry
//! run, which changes nothing, and once as a call, which the policy
//! refuses. A command the walk has no arguments for fails the test, so a
//! new command is walked too.

use std::collections::BTreeMap;
use std::path::Path;

use re_command::{AllowAll, CommandOutcome, PathAccess};

use super::*;

/// The native template's entry scene.
const ENTRY_SCENE: &str = "scenes/main.scene.json";

/// The Luau tool the walk adds: it applies operations through
/// `scene.apply`, so its `set_doc` paths are checked inside the tool.
const TOOL: &str = "walk.apply";

/// Files the walk aims at: the project's own, and the same spelled as
/// Windows or macOS read them.
const FILES: &[&str] = &[
    "real.toml",
    ENTRY_SCENE,
    "scripts/game/main.luau",
    "assets/existing.png",
    ".real/agent-policy.toml",
    ".git/config",
    ".git/hooks/pre-commit",
    "scripts/secret/keep.luau",
    ".Git/hooks/pre-commit",
    ".GIT/config",
    ".git./config",
    ".git /config",
    "GIT~1/config",
    ".REAL/agent-policy.toml",
    "REAL~1/agent-policy.toml",
    ".real/agent-policy.toml::$DATA",
    ".g\u{200C}it/config",
    "scripts/Secret/keep.luau",
    // Outside the project, through a link.
    "scripts/link/keep.txt",
];

/// Folders the walk aims new files at (`<folder>/walk<suffix>`), and the
/// existing ones themselves.
const FOLDERS: &[&str] = &[
    "",
    "scripts",
    "scenes",
    "graphs",
    "notes",
    ".git",
    ".git/hooks",
    ".real",
    ".real/out",
    ".real/agent",
    ".real/cache",
    "scripts/secret",
    "scripts/link",
    ".Git/hooks",
    ".git.",
    "GIT~1",
    ".REAL",
    "REAL~1",
    ".g\u{200C}it",
    "scripts/Secret",
];

/// The first parts of a path that are `.git` or `.real` on some file
/// system, as the walk spells them.
const PROTECTED_DIRS: &[&str] = &[
    ".git",
    ".Git",
    ".GIT",
    ".git.",
    ".git ",
    "GIT~1",
    ".real",
    ".REAL",
    "REAL~1",
    ".g\u{200C}it",
];

/// Under `.real/`, what the engine itself writes for an agent: its
/// checkpoints and caches, left out of the snapshots, and images under
/// `.real/out/`.
fn engine_state(path: &str) -> bool {
    matches!(path, ".real/out" | ".real/agent" | ".real/cache")
        || path.starts_with(".real/agent/")
        || path.starts_with(".real/cache/")
        || (path.starts_with(".real/out/") && path.ends_with(".png"))
}

fn protected(path: &str) -> bool {
    let first = path.split('/').next().unwrap_or("");
    PROTECTED_DIRS.contains(&first)
}

fn denied(path: &str) -> bool {
    path.to_lowercase().starts_with("scripts/secret/")
        || path.eq_ignore_ascii_case("scripts/secret")
}

/// Inside the narrowed walk's `scripts/**` (which matches the folder too).
fn in_scripts(path: &str) -> bool {
    path == "scripts" || path.starts_with("scripts/")
}

fn through_link(path: &str) -> bool {
    path == "scripts/link" || path.starts_with("scripts/link/")
}

/// A policy the walk runs under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Walk {
    ReadOnly,
    Narrowed,
    Wide,
}

impl Walk {
    fn policy(self) -> Policy {
        let mut p = Policy::init_default();
        match self {
            Walk::ReadOnly => {
                p.max_tier = Tier::Read;
                p.scopes.allow.clear();
            }
            Walk::Narrowed => p.scopes.allow = vec!["scripts/**".into()],
            Walk::Wide => {
                p.max_tier = Tier::Destructive;
                p.scopes.allow = vec!["**".into()];
                p.scopes.deny = vec!["scripts/secret/**".into()];
            }
        }
        p
    }

    /// Whether an agent's call (not a dry run) may change `path`.
    fn may_change(self, path: &str) -> bool {
        if engine_state(path) {
            return true;
        }
        if protected(path) || through_link(path) {
            return false;
        }
        match self {
            Walk::ReadOnly => false,
            Walk::Narrowed => in_scripts(path) || path == ENTRY_SCENE,
            Walk::Wide => !denied(path),
        }
    }

    /// Whether the policy must refuse a call that writes `target` through a
    /// path declared with `access`: a path outside the scopes, in `.real/`
    /// or `.git/` (an image under `.real/out/` aside) or through a link,
    /// and an image that would replace a file.
    fn forbids(self, target: &str, access: PathAccess, project: &Path) -> bool {
        let exists = project.join(target).symlink_metadata().is_ok();
        if access == PathAccess::Output && target.starts_with(".real/out/") {
            return !target.ends_with(".png") || exists;
        }
        if protected(target) || through_link(target) {
            return true;
        }
        let in_scope = match self {
            Walk::ReadOnly => false,
            Walk::Narrowed => in_scripts(target),
            Walk::Wide => !denied(target),
        };
        !in_scope || (access == PathAccess::Output && exists)
    }
}

/// The kind of file a command writes, for the new files the walk aims it
/// at.
fn suffix(command: &str) -> &'static str {
    match command {
        "script.write" => ".luau",
        "screenshot" | "asset.import" => ".png",
        "animgraph.create" | "animgraph.edit" => ".animgraph.json",
        "scene.apply" | TOOL | "proc.create_source" => ".graph.json",
        c if c.starts_with("graph.") => ".graph.json",
        _ => "",
    }
}

/// The arguments each command is walked with: valid ones on the walk's
/// project, so that a call the policy let through would do what it does.
/// `None` for a command the walk does not know.
fn arguments(command: &str) -> Option<Vec<Json>> {
    let graph = "graphs/rocks.graph.json";
    let set_doc = json!({
        "op": "set_doc", "doc": graph, "pointer": "/nodes/0/params/count", "value": 3
    });
    let args = match command {
        "describe"
        | "project.status"
        | "undo"
        | "redo"
        | "verify.diff"
        | "template.list"
        | "recipe.list"
        | "resource.list"
        | "checkpoint.create"
        | "checkpoint.restore"
        | "graph.migration_report"
        | "compat.coverage"
        | "scene.dump_tree" => vec![json!({})],
        "query" => vec![json!({ "with": ["Transform"] })],
        "entity.get" => vec![json!({ "id": "box" })],
        "command.describe" => vec![json!({ "name": "script.write" })],
        "explain" => vec![json!({ "code": re_agent::codes::OUTSIDE_SCOPE.to_string() })],
        "resource.read" => vec![json!({ "uri": "real://explain/RE8002" })],
        "entity.spawn" => vec![json!({ "id": "walk_spawned", "name": "Spawned" })],
        "entity.set" => vec![json!({
            "id": "box", "component": "Transform", "field": "translation", "value": [1.0, 2.0, 3.0]
        })],
        "entity.reparent" => vec![json!({ "id": "walk_child", "parent": "box" })],
        "entity.delete" => vec![json!({ "id": "walk_doomed" })],
        "component.add" => vec![json!({ "id": "walk_child", "component": "Light" })],
        "component.remove" => vec![json!({ "id": "walk_child", "component": "Light" })],
        "scene.apply" | TOOL => vec![json!({
            "ops": [{ "op": "spawn", "id": "walk_applied", "name": "Applied" }, set_doc]
        })],
        "screenshot" => vec![json!({ "path": ".real/out/walk-base.png" })],
        "play.run" => vec![
            json!({ "frames": 2 }),
            json!({ "frames": 2, "replay": "tests/replays/none.replay.json" }),
        ],
        "verify" => vec![json!({ "boot_frames": 2 })],
        "asset.import" => vec![
            json!({ "source": "assets/existing.png", "dest": "assets/imported.png" }),
            json!({ "source": "assets/existing.png" }),
        ],
        "animgraph.create" => vec![json!({ "path": "anims/made.animgraph.json" })],
        "animgraph.edit" => vec![json!({
            "path": "anims/walk.animgraph.json",
            "ops": [{ "op": "upsert", "element": "param", "value": { "id": "go", "type": "bool" } }]
        })],
        "animgraph.validate" => vec![
            json!({ "path": "anims/walk.animgraph.json" }),
            json!({ "path": "anims/walk.animgraph.json", "skin": "assets/none.glb#skin" }),
        ],
        "anim.set_param" => vec![json!({ "entity": "box", "param": "go", "value": true })],
        "proc.create_source" => vec![
            json!({ "kind": "project.walk", "id": "pebbles", "graph": "graphs/pebbles.graph.json" }),
            json!({ "kind": "project.walk", "id": "stones" }),
        ],
        "graph.set_param" => vec![json!({
            "source": "rocks", "node": "gen", "param": "count", "value": 3
        })],
        "graph.regenerate" => vec![json!({ "source": "rocks", "seed": 2 })],
        "graph.generate" => vec![json!({ "source": "rocks" })],
        "graph.report" => vec![json!({ "source": "rocks" })],
        "graph.add_node" => vec![json!({
            "graph": graph, "kind": "project.walk", "id": "more"
        })],
        "graph.remove_node" => vec![json!({ "graph": graph, "node": "more" })],
        "graph.connect" | "graph.disconnect" => vec![json!({
            "graph": graph, "from": ["feed", "out"], "to": ["gen", "extra"]
        })],
        "graph.move_nodes" => vec![json!({ "graph": graph, "positions": { "gen": [10.0, 20.0] } })],
        "graph.set_default" => vec![json!({
            "graph": graph, "node": "gen", "param": "count", "value": 1
        })],
        "proc.bake" => vec![json!({ "source": "rocks", "keys": ["gen/box3", "feed/mesh"] })],
        "proc.move" => vec![json!({ "id": "rocks/gen/box2", "parent": "rocks/gen/box1" })],
        "proc.replace" => vec![json!({
            "id": "rocks/gen/box1", "prefab": "prefabs/rock.prefab.json"
        })],
        "proc.insert" => vec![json!({
            "source": "rocks", "key": "walk", "prefab": "prefabs/rock.prefab.json"
        })],
        "proc.clear_structure" => vec![json!({ "source": "rocks" })],
        "script.write" => {
            vec![json!({ "path": "scripts/game/walked.luau", "source": "return 1\n" })]
        }
        "file.delete" => vec![json!({ "path": "scripts/game/doomed.luau" })],
        "project.import" => vec![json!({ "input": "place.rbxlx", "write": true })],
        "recipe.apply" => vec![
            json!({ "recipe": "falling-box", "args": { "label": "Crate" } }),
            json!({ "recipe": "spinner", "args": { "label": "Fan" } }),
        ],
        _ => return None,
    };
    Some(args)
}

/// The paths a walk aims a command's `access` paths at.
fn targets(command: &str, walk: Walk, access: PathAccess, project: &Path) -> Vec<String> {
    let suffix = suffix(command);
    // Windows makes links only with Developer Mode on.
    let linked = project.join("scripts/link").symlink_metadata().is_ok();
    let mut out: Vec<String> = FILES.iter().map(|f| f.to_string()).collect();
    for folder in FOLDERS {
        out.push(if folder.is_empty() {
            format!("walk{suffix}")
        } else {
            format!("{folder}/walk{suffix}")
        });
        if !folder.is_empty() && project.join(folder).exists() {
            out.push(folder.to_string());
        }
    }
    out.retain(|t| walk.forbids(t, access, project) && (linked || !through_link(t)));
    out
}

/// The tokens of a JSON pointer.
fn pointer_tokens(pointer: &str) -> Vec<String> {
    pointer
        .trim_start_matches('/')
        .split('/')
        .map(|t| t.replace("~1", "/").replace("~0", "~"))
        .collect()
}

/// Replaces every string at the pointer `tokens` (`*` for each element)
/// with `target`.
fn aim(value: &mut Json, tokens: &[&str], target: &str) {
    match tokens.split_first() {
        None => {
            if value.is_string() {
                *value = Json::from(target);
            }
        }
        Some((&"*", rest)) => match value {
            Json::Array(items) => items.iter_mut().for_each(|v| aim(v, rest, target)),
            Json::Object(map) => map.values_mut().for_each(|v| aim(v, rest, target)),
            _ => {}
        },
        Some((token, rest)) => {
            let child = match value {
                Json::Object(map) => map.get_mut(*token),
                Json::Array(items) => token.parse::<usize>().ok().and_then(|i| items.get_mut(i)),
                _ => None,
            };
            if let Some(child) = child {
                aim(child, rest, target);
            }
        }
    }
}

/// Every file and folder under `root`, without following links: a file's
/// bytes, a folder or link as `None`. The agent's checkpoints and caches
/// are left out.
fn snapshot(root: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn go(root: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if rel == ".real/agent" || rel == ".real/cache" {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                out.insert(rel, None);
                go(root, &path, out);
            } else if meta.is_file() {
                out.insert(rel, Some(std::fs::read(&path).unwrap_or_default()));
            } else {
                out.insert(rel, None);
            }
        }
    }
    let mut out = BTreeMap::new();
    go(root, root, &mut out);
    out
}

/// The paths that differ between two snapshots.
fn changed(
    before: &BTreeMap<String, Option<Vec<u8>>>,
    after: &BTreeMap<String, Option<Vec<u8>>>,
) -> Vec<String> {
    let mut out: Vec<String> = before
        .iter()
        .filter(|(k, v)| after.get(*k) != Some(*v))
        .map(|(k, _)| k.clone())
        .collect();
    out.extend(after.keys().filter(|k| !before.contains_key(*k)).cloned());
    out.sort();
    out
}

/// A file's content, or `None` when it does not exist (through links).
fn read(path: &Path) -> Option<Vec<u8>> {
    std::fs::read(path).ok()
}

/// A 1×1 PNG.
const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x68, 0x68, 0x68, 0xf8,
    0x0f, 0x00, 0x05, 0x84, 0x02, 0x80, 0x8c, 0xcd, 0x66, 0x26, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

const GENERATOR: &str = r#"return proc.node {
    kind = "project.walk",
    version = 1,
    doc = "Places named boxes.",
    inputs = { { name = "extra", type = "mesh" } },
    output = "fragment",
    params = { count = proc.param.integer { default = 2, min = 0, max = 4 } },
    eval = function(ctx, inputs, params)
        local items = {}
        for i = 1, params.count do
            items[i] = { key = "box" .. i, name = "Box " .. i }
        end
        return { items = items }
    end,
}
"#;

const PREFAB: &str = r#"{
  "format": "real-engine/prefab",
  "version": 1,
  "entities": [{ "id": "rock", "name": "Rock", "components": { "Transform": {} } }]
}
"#;

const TOOL_SOURCE: &str = r#"return {
    name = "walk.apply",
    summary = "Apply operations through scene.apply.",
    tier = "edit",
    input = {
        type = "object",
        properties = { ops = { type = "array" } },
        required = { "ops" },
    },
    run = function(args)
        local out = command.run("scene.apply", { ops = args.ops })
        if not out.ok then
            error(`{out.diagnostics[1].code}: {out.diagnostics[1].message}`)
        end
        return { changes = out.changes }
    end,
}
"#;

/// The walk's project, `game` in the returned folder: the native template
/// with Git's files, scripts, the denied folder, an image, a prefab, a
/// procedural source with its graph file (open), an animation graph, the
/// Luau tool, `walk`'s policy, and a link to the folder `outside`, all made
/// by a person before the agent's session starts.
fn walk_project(walk: Walk) -> (tempfile::TempDir, AgentApp) {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("game");
    let outside = dir.path().join("outside");
    let options = crate::new::NewOptions {
        name: None,
        template: "native",
        set: &[],
        check: false,
    };
    crate::new::create_from(&project, options).unwrap();
    let policy = walk.policy().to_toml();
    let files: &[(&str, &[u8])] = &[
        (".git/config", b"[core]\n\tbare = false\n"),
        (".git/hooks/pre-commit", b"#!/bin/sh\nexit 0\n"),
        ("scripts/game/main.luau", b"print(\"main\")\n"),
        ("scripts/game/doomed.luau", b"return nil\n"),
        ("scripts/secret/keep.luau", b"return \"secret\"\n"),
        ("scripts/generators/walk.luau", GENERATOR.as_bytes()),
        ("scripts/tools/commands/walk.luau", TOOL_SOURCE.as_bytes()),
        ("assets/existing.png", PNG),
        ("prefabs/rock.prefab.json", PREFAB.as_bytes()),
        (".real/agent-policy.toml", policy.as_bytes()),
    ];
    for (path, bytes) in files {
        let path = project.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("keep.txt"), "outside\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, project.join("scripts/link")).unwrap();
    #[cfg(windows)]
    let _ = std::os::windows::fs::symlink_dir(&outside, project.join("scripts/link"));

    let mut opened = open_for_agents(&project).unwrap();
    // What a person set up before the agent came.
    for (name, args) in [
        (
            "proc.create_source",
            json!({ "kind": "project.walk", "id": "rocks", "graph": "graphs/rocks.graph.json" }),
        ),
        // A second node to connect, which also opens the graph's document
        // for `set_doc`.
        (
            "graph.add_node",
            json!({ "graph": "graphs/rocks.graph.json", "kind": "mesh.primitive", "id": "feed" }),
        ),
        (
            "animgraph.create",
            json!({ "path": "anims/walk.animgraph.json" }),
        ),
        (
            "entity.spawn",
            json!({ "id": "walk_doomed", "name": "Doomed" }),
        ),
        (
            "entity.spawn",
            json!({ "id": "walk_child", "name": "Child" }),
        ),
    ] {
        let out = opened
            .app
            .dispatch(name, args, false, &AllowAll, Origin::User);
        assert!(out.ok, "{name}: {:?}", out.diagnostics);
    }
    opened.agent.start_session(opened.app.world()).unwrap();
    (dir, opened)
}

/// One walk: every command, as `real mcp` dispatches an agent's calls.
fn walk(walk: Walk) {
    let (dir, mut opened) = walk_project(walk);
    let project = dir.path().join("game");
    let outside = dir.path().join("outside");
    let policy = Policy::load(&project).unwrap();
    assert_eq!(policy, walk.policy().in_project(&project));
    let app = &mut opened.app;
    let specs = app.commands().specs().to_vec();
    assert!(
        specs.iter().any(|s| s.name == TOOL),
        "the Luau tool is registered"
    );

    let mut failures: Vec<String> = Vec::new();
    let mut calls = 0;
    for spec in &specs {
        let Some(bases) = arguments(&spec.name) else {
            panic!(
                "the walk has no arguments for {}: add them to `arguments` in {}",
                spec.name,
                file!()
            );
        };
        // The paths to aim: those the command declares, and for the Luau
        // tool, the documents its `scene.apply` writes.
        let mut paths = spec.paths.clone();
        if spec.name == TOOL {
            paths.push(re_command::PathArg {
                pointer: "/ops/*/doc".into(),
                access: PathAccess::Write,
            });
        }
        for path in &paths {
            assert!(
                bases.iter().any(|b| !path.values(b).is_empty()),
                "the walk's arguments for {} leave out {}",
                spec.name,
                path.pointer
            );
        }
        // Each set of arguments as it is, then with each path aimed at
        // each target in turn.
        let mut cases: Vec<(Json, Option<(String, PathAccess)>)> = Vec::new();
        for base in &bases {
            cases.push((base.clone(), None));
            for path in &paths {
                if path.values(base).is_empty() {
                    continue;
                }
                let tokens = pointer_tokens(&path.pointer);
                let tokens: Vec<&str> = tokens.iter().map(String::as_str).collect();
                for target in targets(&spec.name, walk, path.access, &project) {
                    let mut args = base.clone();
                    aim(&mut args, &tokens, &target);
                    cases.push((args, Some((target, path.access))));
                }
            }
        }

        for (args, aimed) in cases {
            for dry_run in [true, false] {
                calls += 1;
                let before = snapshot(&project);
                let outside_before = snapshot(&outside);
                let target_before = aimed.as_ref().map(|(t, _)| read(&project.join(t)));
                let out: CommandOutcome = app.dispatch(
                    &spec.name,
                    args.clone(),
                    dry_run,
                    &policy,
                    Origin::Agent(ORIGIN.into()),
                );
                let after = snapshot(&project);
                let what = format!(
                    "{walk:?} {}{} {args}",
                    spec.name,
                    if dry_run { " (dry run)" } else { "" }
                );
                for path in changed(&before, &after) {
                    // The engine makes folders for its images.
                    let folder = after.get(&path).is_some_and(Option::is_none);
                    let allowed = !dry_run
                        && (walk.may_change(&path) || (folder && path.starts_with(".real/out/")));
                    if !allowed {
                        failures.push(format!("{what} changed {path:?}"));
                    }
                }
                if changed(&outside_before, &snapshot(&outside)) != Vec::<String>::new() {
                    failures.push(format!("{what} changed a file outside the project"));
                }
                if let Some((target, access)) = &aimed {
                    if read(&project.join(target)) != target_before.unwrap() {
                        failures.push(format!("{what} changed its target {target:?}"));
                    }
                    // The policy's refusal: its own code, or inside the
                    // Luau tool, the error the tool raised with it.
                    let policy_codes = [
                        re_agent::codes::OUTSIDE_SCOPE,
                        re_agent::codes::TIER_REFUSED,
                    ];
                    let refused = !out.ok
                        && out.diagnostics.first().is_some_and(|d| {
                            policy_codes.contains(&d.code)
                                || (spec.name == TOOL
                                    && policy_codes
                                        .iter()
                                        .any(|c| d.message.contains(&c.to_string())))
                        });
                    if !dry_run && *access != PathAccess::Read && !refused {
                        failures.push(format!(
                            "{what} was not refused by the policy ({access:?} {target:?}): {}",
                            out.diagnostics
                                .first()
                                .map_or("ok".to_string(), |d| format!("{}: {}", d.code, d.message))
                        ));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {calls} calls broke the policy:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn a_read_only_policy_writes_nothing_through_any_command() {
    walk(Walk::ReadOnly);
}

#[test]
fn a_narrowed_policy_writes_only_inside_its_scopes() {
    walk(Walk::Narrowed);
}

#[test]
fn even_a_wide_policy_never_writes_protected_denied_or_outside_files() {
    walk(Walk::Wide);
}
