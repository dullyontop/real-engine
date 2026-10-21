//! `real agent init`'s files, snapshot-tested for every client.

#[path = "../../re_scene/tests/support/mod.rs"]
mod support;

use std::path::Path;

use re_agent::init::{self, InitOptions};
use re_agent::{Agent, register_commands};
use re_command::{AllowAll, CommandRegistry, EditState, register_builtin_commands};
use serde_json::json;

fn registry() -> CommandRegistry<EditState> {
    let mut r = CommandRegistry::new();
    register_builtin_commands(&mut r);
    register_commands(&mut r, &Agent::new(Path::new(".")));
    r
}

fn read(project: &Path, rel: &str) -> String {
    std::fs::read_to_string(project.join(rel)).unwrap()
}

#[test]
fn init_writes_every_client_config() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    let reg = registry();
    let report = init::init(project, reg.specs(), &InitOptions::default()).unwrap();
    assert_eq!(
        report.written,
        vec![
            ".real/agent-policy.toml",
            "AGENTS.md",
            "CLAUDE.md",
            ".real/types/engine.d.luau",
            ".mcp.json",
            ".vscode/mcp.json",
            ".cursor/mcp.json",
            ".gemini/settings.json",
        ]
    );
    assert!(report.kept.is_empty());

    insta::assert_snapshot!("claude_code_mcp_json", read(project, ".mcp.json"));
    insta::assert_snapshot!("vscode_mcp_json", read(project, ".vscode/mcp.json"));
    insta::assert_snapshot!("cursor_mcp_json", read(project, ".cursor/mcp.json"));
    insta::assert_snapshot!(
        "gemini_settings_json",
        read(project, ".gemini/settings.json")
    );
    insta::assert_snapshot!(
        "agent_policy_toml",
        read(project, ".real/agent-policy.toml")
    );
    insta::assert_snapshot!("claude_md", read(project, "CLAUDE.md"));
    insta::assert_snapshot!("agents_md", read(project, "AGENTS.md"));

    let p = Path::new("/home/me/game");
    insta::assert_snapshot!("codex_toml", init::codex_snippet("real", p));
    insta::assert_snapshot!("zed_settings_json", init::zed_snippet("real", p));
    assert_eq!(report.snippets.len(), 2);

    // The generated policy is what `Policy::load` reads back.
    assert_eq!(
        re_agent::Policy::load(project).unwrap(),
        re_agent::Policy::init_default()
    );
}

#[test]
fn init_respects_existing_files() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    std::fs::create_dir_all(project.join(".vscode")).unwrap();
    std::fs::create_dir_all(project.join(".cursor")).unwrap();
    std::fs::create_dir_all(project.join(".real")).unwrap();
    std::fs::write(
        project.join(".vscode/mcp.json"),
        r#"{ "servers": { "other": { "type": "stdio", "command": "x" } }, "inputs": [] }"#,
    )
    .unwrap();
    std::fs::write(project.join(".cursor/mcp.json"), "{ not json").unwrap();
    std::fs::write(project.join("AGENTS.md"), "# My notes\n").unwrap();
    std::fs::write(project.join("CLAUDE.md"), "Be nice.\n").unwrap();
    std::fs::write(
        project.join(".real/agent-policy.toml"),
        "max_tier = \"read\"\n",
    )
    .unwrap();

    let reg = registry();
    let report = init::init(project, reg.specs(), &InitOptions::default()).unwrap();
    let kept: Vec<&str> = report.kept.iter().map(|(p, _)| p.as_str()).collect();
    assert_eq!(
        kept,
        vec![
            ".real/agent-policy.toml",
            "AGENTS.md",
            "CLAUDE.md",
            ".cursor/mcp.json"
        ]
    );
    // Other servers and keys survive the merge.
    let vscode: serde_json::Value =
        serde_json::from_str(&read(project, ".vscode/mcp.json")).unwrap();
    assert_eq!(vscode["servers"]["other"]["command"], "x");
    assert_eq!(vscode["servers"]["real-engine"]["type"], "stdio");
    assert!(vscode["inputs"].is_array());
    assert_eq!(read(project, "AGENTS.md"), "# My notes\n");
    assert_eq!(
        read(project, ".real/agent-policy.toml"),
        "max_tier = \"read\"\n"
    );

    // A generated AGENTS.md is regenerated, and --force replaces the policy.
    std::fs::remove_file(project.join("AGENTS.md")).unwrap();
    init::init(project, reg.specs(), &InitOptions::default()).unwrap();
    let report = init::init(
        project,
        reg.specs(),
        &InitOptions {
            force: true,
            ..InitOptions::default()
        },
    )
    .unwrap();
    assert!(report.written.contains(&"AGENTS.md".to_string()));
    assert!(
        report
            .written
            .contains(&".real/agent-policy.toml".to_string())
    );
}

#[test]
fn init_with_http_points_claude_code_at_the_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path();
    let reg = registry();
    let report = init::init(
        project,
        reg.specs(),
        &InitOptions {
            http: Some("http://127.0.0.1:8765/mcp".to_string()),
            ..InitOptions::default()
        },
    )
    .unwrap();
    let claude: serde_json::Value = serde_json::from_str(&read(project, ".mcp.json")).unwrap();
    assert_eq!(
        claude["mcpServers"]["real-engine"],
        serde_json::json!({
            "type": "http",
            "url": "http://127.0.0.1:8765/mcp",
            "headers": { "Authorization": "Bearer ${REAL_MCP_TOKEN}" }
        })
    );
    // The other clients keep starting `real mcp` over stdio.
    let cursor: serde_json::Value =
        serde_json::from_str(&read(project, ".cursor/mcp.json")).unwrap();
    assert_eq!(cursor["mcpServers"]["real-engine"]["command"], "real");
    let (_, _, how) = report
        .snippets
        .iter()
        .find(|(client, _, _)| client == "HTTP")
        .unwrap();
    assert!(
        how.contains("real mcp --http 127.0.0.1:8765 --project"),
        "{how}"
    );
    assert!(how.contains("REAL_MCP_TOKEN"), "{how}");
}

#[test]
fn agents_md_lists_every_command_and_its_example_applies() {
    let reg = registry();
    let md = init::generate_agents_md(&reg);
    for spec in reg.specs() {
        assert!(
            md.contains(&format!("| `{}` |", spec.name)),
            "{}",
            spec.name
        );
    }
    assert!(md.contains("`path`, `source`"), "script.write arguments");
    // The scene.apply example in the guide is valid and atomic.
    let start = md.find("```json\n").unwrap() + "```json\n".len();
    let end = start + md[start..].find("```").unwrap();
    let example: serde_json::Value = serde_json::from_str(&md[start..end]).unwrap();
    let mut s = EditState {
        world: support::world(),
        history: Default::default(),
    };
    s.world
        .spawn_docs(&[support::entity("world", None, &[])], None)
        .unwrap();
    let out = reg.dispatch(&mut s, "scene.apply", example, false, &AllowAll);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(s.world.name_of(s.world.entity("rock").unwrap()), "Big rock");
    let out = reg.dispatch(&mut s, "undo", json!({}), false, &AllowAll);
    assert!(out.ok);
    assert!(s.world.entity("rock").is_err());
    assert!(md.len() < 32 * 1024, "Codex caps AGENTS.md at 32 KiB");
}
