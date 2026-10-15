//! `real mcp --project examples/island` end to end, over stdio, exactly as an
//! MCP client drives it (M1 definition of done, item 5): list the tools, spawn
//! an entity, change a graph parameter and regenerate, take a screenshot,
//! verify, have a `script.write` outside the policy scopes refused, and
//! return to the session's start with `checkpoint.restore` (the state hash
//! comes back). The demo is copied first, because `real mcp` saves edits.
//!
//! Run with `--test-threads=1` (the screenshot needs the GPU).
#![cfg(feature = "tooling")]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use serde_json::{Value, json};

/// How long one ordinary call may take.
const TIMEOUT: Duration = Duration::from_secs(120);
/// `verify` runs the replays and goldens; `screenshot` may start the GPU.
const SLOW: Duration = Duration::from_secs(600);

fn island() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/island")
}

/// Copies the demo, leaving out local state under `.real/` (sessions, the
/// lock, caches) except the committed agent policy.
fn copy_project(from: &Path, to: &Path) {
    copy_dir(from, to);
    let _ = std::fs::remove_dir_all(to.join(".real"));
    std::fs::create_dir_all(to.join(".real")).unwrap();
    std::fs::copy(
        from.join(".real/agent-policy.toml"),
        to.join(".real/agent-policy.toml"),
    )
    .unwrap();
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if entry.file_name() == ".real" {
            continue;
        }
        if kind.is_dir() {
            copy_dir(&entry.path(), &target);
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Every file under `dir` (relative, `/`-separated) with its contents.
fn snapshot(dir: &Path) -> std::collections::BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                walk(root, &entry.path(), out);
            } else if kind.is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, std::fs::read(entry.path()).unwrap());
            }
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

/// The state hash recorded in the only agent session's start checkpoint.
fn session_start_hash(project: &Path) -> String {
    let sessions: Vec<_> = std::fs::read_dir(project.join(".real/agent/sessions"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    let start: Value =
        serde_json::from_slice(&std::fs::read(sessions[0].join("checkpoints/start.json")).unwrap())
            .unwrap();
    start["state_hash"].as_str().unwrap().to_string()
}

/// A `real mcp` process and the JSON-RPC lines it writes.
struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl Server {
    fn start(project: &Path) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_real"))
            .args(["mcp", "--project"])
            .arg(project)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("real mcp starts");
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stdin = child.stdin.take();
        Server {
            child,
            stdin,
            lines,
        }
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    /// Sends a request and returns its response. Every line on stdout must be
    /// JSON-RPC.
    #[track_caller]
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.request_within(id, method, params, TIMEOUT)
    }

    #[track_caller]
    fn request_within(&mut self, id: u64, method: &str, params: Value, limit: Duration) -> Value {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = match self.lines.recv_timeout(limit) {
                Ok(line) => line,
                Err(RecvTimeoutError::Timeout) => {
                    panic!("no response to {method} (id {id}) within {limit:?}")
                }
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.wait().ok();
                    panic!(
                        "real mcp closed its output before answering {method} (id {id}); exit status {status:?}"
                    )
                }
            };
            let msg: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout carried a non-JSON line ({e}): {line}"));
            assert_eq!(msg["jsonrpc"], "2.0", "{line}");
            if msg["id"] == id && msg.get("method").is_none() {
                return msg;
            }
        }
    }

    /// Calls a tool; protocol errors fail the test.
    #[track_caller]
    fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        self.call_within(id, tool, arguments, TIMEOUT)
    }

    #[track_caller]
    fn call_within(&mut self, id: u64, tool: &str, arguments: Value, limit: Duration) -> Value {
        let resp = self.request_within(
            id,
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
            limit,
        );
        resp.get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{tool} gave a protocol error: {resp}"))
    }

    /// Closes stdin and waits for a clean exit.
    fn finish(mut self) {
        drop(self.stdin.take());
        for _ in 0..600 {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "real mcp exited with {status}");
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        panic!("real mcp did not exit after its client closed stdin");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// The JSON in a tool result's first content block.
#[track_caller]
fn body(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[track_caller]
fn ok(tool: &str, result: &Value) -> Value {
    assert_eq!(result["isError"], false, "{tool} failed: {result}");
    body(result)
}

#[test]
fn an_agent_edits_checks_and_restores_the_island() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    copy_project(&island(), &project);
    let scene_before = std::fs::read(project.join("scenes/main.scene.json")).unwrap();

    let mut s = Server::start(&project);
    let init = s.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "mcp_e2e", "version": "0" }
        }),
    );
    assert_eq!(init["result"]["serverInfo"]["name"], "real-engine");
    s.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    // The tools: one per command of ARCHITECTURE §6.
    let list = s.request(2, "tools/list", json!({}));
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    // The 25 of M1, plus WP17's resource.list, resource.read,
    // checkpoint.create, compat.coverage, scene.dump_tree, verify.diff and
    // project.import.
    assert_eq!(names.len(), 32, "{names:?}");
    for tool in [
        "entity_spawn",
        "graph_set_param",
        "graph_regenerate",
        "screenshot",
        "verify",
        "script_write",
        "checkpoint_restore",
    ] {
        assert!(names.contains(&tool), "{tool} is missing: {names:?}");
    }

    // The session's start checkpoint, written when `real mcp` opened the
    // project, holds the state the restore must return to.
    let start_hash = session_start_hash(&project);
    let files_before = snapshot(&project);

    let spawned = ok(
        "entity_spawn",
        &s.call(
            4,
            "entity_spawn",
            json!({ "id": "e2e_rock", "name": "E2E rock" }),
        ),
    );
    assert_eq!(spawned["changes"][0]["op"], "spawn");
    ok(
        "entity_get",
        &s.call(5, "entity_get", json!({ "id": "e2e_rock" })),
    );

    ok(
        "graph_set_param",
        &s.call(
            6,
            "graph_set_param",
            json!({ "source": "island", "node": "gem_spots", "param": "radius", "value": 12.0 }),
        ),
    );
    let regen = ok(
        "graph_regenerate",
        &s.call(7, "graph_regenerate", json!({ "source": "island" })),
    );
    let r = &regen["data"]["regen"];
    let touched = ["spawned", "updated", "despawned"]
        .iter()
        .filter_map(|k| r[*k].as_array())
        .map(Vec::len)
        .sum::<usize>();
    assert!(touched > 0, "the regeneration changed nothing: {regen}");

    let shot = s.call_within(8, "screenshot", json!({}), SLOW);
    ok("screenshot", &shot);
    let content = shot["content"].as_array().unwrap();
    let image = content
        .iter()
        .find(|c| c["type"] == "image")
        .unwrap_or_else(|| panic!("screenshot returned no image: {shot}"));
    assert_eq!(image["mimeType"], "image/png");
    assert!(image["data"].as_str().unwrap().starts_with("iVBORw0KGgo"));

    // `verify` runs and reports its stages; the edits may make the goldens
    // differ, so whether it passes is not checked.
    let report = ok("verify", &s.call_within(9, "verify", json!({}), SLOW));
    let stages: Vec<&str> = report["data"]["stages"]
        .as_array()
        .unwrap_or_else(|| panic!("verify reported no stages: {report}"))
        .iter()
        .filter_map(|st| st["name"].as_str().or(st["stage"].as_str()))
        .collect();
    for stage in ["schema", "boot"] {
        assert!(stages.contains(&stage), "{stage} missing from {stages:?}");
    }

    // The policy keeps scripts inside its scopes.
    let refused = s.call(
        10,
        "script_write",
        json!({ "path": "notes/hack.luau", "source": "return 1" }),
    );
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(body(&refused)["code"], "RE8002");
    assert!(!project.join("notes/hack.luau").exists());

    // A script inside the scopes is written, and the restore removes it.
    ok(
        "script_write",
        &s.call(
            11,
            "script_write",
            json!({ "path": "scripts/game/e2e.luau", "source": "return 1\n" }),
        ),
    );
    assert!(project.join("scripts/game/e2e.luau").is_file());

    let restored = ok(
        "checkpoint_restore",
        &s.call(12, "checkpoint_restore", json!({})),
    );
    assert_eq!(restored["data"]["state_hash"], start_hash.as_str());
    // The state saved before the restore differs: the edits were real.
    let saved = restored["data"]["saved"].as_str().unwrap();
    let session = std::fs::read_dir(project.join(".real/agent/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let saved_cp: Value = serde_json::from_slice(
        &std::fs::read(session.join(format!("checkpoints/{saved}.json"))).unwrap(),
    )
    .unwrap();
    assert_ne!(saved_cp["state_hash"], start_hash.as_str());
    let gone = s.call(13, "entity_get", json!({ "id": "e2e_rock" }));
    assert_eq!(gone["isError"], true, "{gone}");
    assert_eq!(body(&gone)["code"], "RE1200", "{gone}");
    assert!(!project.join("scripts/game/e2e.luau").exists());

    s.finish();
    // Nothing was written outside `.real/` and the policy's scopes.
    let scopes = [
        "scripts/",
        "scenes/",
        "graphs/",
        "prefabs/",
        "materials/",
        "assets/",
    ];
    for (path, bytes) in snapshot(&project) {
        if files_before.get(&path) == Some(&bytes) {
            continue;
        }
        assert!(
            path.starts_with(".real/") || scopes.iter().any(|s| path.starts_with(s)),
            "real mcp wrote {path}, outside the policy scopes"
        );
    }
    // Edits were saved as they were made; after the restore the scene file
    // holds the same entities as before (written in the saver's own form).
    let ids = |bytes: &[u8]| -> Vec<String> {
        let scene: Value = serde_json::from_slice(bytes).unwrap();
        let mut ids: Vec<String> = scene["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap().to_string())
            .collect();
        ids.sort();
        ids
    };
    let scene_after = std::fs::read(project.join("scenes/main.scene.json")).unwrap();
    assert_eq!(ids(&scene_after), ids(&scene_before));

    // The saved project opens again, generation included.
    let mut again = Server::start(&project);
    again.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "mcp_e2e", "version": "0" }
        }),
    );
    again.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    ok(
        "graph_regenerate",
        &again.call(2, "graph_regenerate", json!({ "source": "island" })),
    );
    again.finish();
}

/// A Luau tool the e2e test adds to the copied demo.
const MARKERS: &str = r#"
return {
    name = "e2e.markers",
    summary = "Spawn N marker entities named e2e_marker_1 to e2e_marker_N.",
    input = {
        type = "object",
        properties = { count = { type = "integer", minimum = 1, maximum = 10 } },
        required = { "count" },
    },
    output = {
        type = "object",
        properties = { placed = { type = "integer" } },
        required = { "placed" },
    },
    run = function(args)
        for i = 1, args.count do
            local out = command.run("entity.spawn", { id = `e2e_marker_{i}` })
            assert(out.ok, out.diagnostics[1] and out.diagnostics[1].message)
        end
        return { placed = args.count }
    end,
}
"#;

/// A tool that asks for more than tools may have; it is never registered.
const WIPE: &str = r#"
return {
    name = "e2e.wipe",
    summary = "Delete things.",
    tier = "destructive",
    run = function() end,
}
"#;

#[test]
fn an_agent_uses_luau_tools_resources_and_verify_diff() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    copy_project(&island(), &project);
    let tools = project.join("scripts/tools/commands");
    std::fs::create_dir_all(&tools).unwrap();
    std::fs::write(tools.join("markers.luau"), MARKERS).unwrap();
    std::fs::write(tools.join("wipe.luau"), WIPE).unwrap();

    let mut s = Server::start(&project);
    let init = s.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "mcp_e2e", "version": "0" }
        }),
    );
    assert!(init["result"]["capabilities"]["resources"].is_object());
    s.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));

    // The Luau tool is listed like any command, with its schemas; the
    // destructive one is not.
    let list = s.request(2, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let markers = tools
        .iter()
        .find(|t| t["name"] == "e2e_markers")
        .unwrap_or_else(|| panic!("the Luau tool is not listed: {list}"));
    assert_eq!(markers["inputSchema"]["required"], json!(["count"]));
    assert_eq!(
        markers["outputSchema"]["properties"]["data"]["required"],
        json!(["placed"])
    );
    assert!(tools.iter().all(|t| t["name"] != "e2e_wipe"));
    // Every tool carries an output schema.
    for tool in tools {
        assert_eq!(tool["outputSchema"]["type"], "object", "{}", tool["name"]);
    }

    // verify.diff before any change: this run is the baseline.
    let first = ok(
        "verify_diff",
        &s.call_within(3, "verify_diff", json!({}), SLOW),
    );
    assert_eq!(first["data"]["checkpoint"], "start");
    assert_eq!(first["data"]["baseline"], "current");
    assert_eq!(first["data"]["diff"]["changed"], false, "{first}");

    // The Luau tool runs as one undoable transaction.
    let placed = s.call(4, "e2e_markers", json!({ "count": 3 }));
    let placed_body = ok("e2e_markers", &placed);
    assert_eq!(placed["structuredContent"]["data"]["placed"], 3);
    assert_eq!(placed_body["changes"].as_array().unwrap().len(), 3);
    ok(
        "entity_get",
        &s.call(5, "entity_get", json!({ "id": "e2e_marker_3" })),
    );
    // Its arguments are checked against its schema.
    let bad = s.call(6, "e2e_markers", json!({ "count": 0 }));
    assert_eq!(bad["isError"], true);
    assert_eq!(body(&bad)["code"], "RE2005");
    // A destructive call stays refused: the policy grants `edit`.
    let refused = s.call(7, "file_delete", json!({ "path": "scripts/game/x.luau" }));
    assert_eq!(refused["isError"], true);
    assert_eq!(body(&refused)["code"], "RE8001");

    // verify.diff after the change compares against the stored baseline.
    let second = ok(
        "verify_diff",
        &s.call_within(8, "verify_diff", json!({}), SLOW),
    );
    assert_eq!(second["data"]["baseline"], "stored");
    assert!(second["data"]["diff"]["summary"].is_string());
    // Other verify options have no stored baseline, and the project is no
    // longer at the checkpoint: verify runs on a copy restored to it.
    let rebuilt = ok(
        "verify_diff",
        &s.call_within(
            80,
            "verify_diff",
            json!({ "checkpoint": "start", "boot_frames": 2 }),
            SLOW,
        ),
    );
    assert_eq!(rebuilt["data"]["baseline"], "rebuilt", "{rebuilt}");
    let session = std::fs::read_dir(project.join(".real/agent/sessions"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(
        !session.join("baseline-start").exists(),
        "the copy is removed"
    );
    assert!(session.join("checkpoints/start.verify-2.json").is_file());

    // One undo reverts the whole tool.
    ok("undo", &s.call(9, "undo", json!({})));
    let gone = s.call(10, "entity_get", json!({ "id": "e2e_marker_1" }));
    assert_eq!(gone["isError"], true);

    // A resource, read both ways.
    let page = s.request(
        11,
        "resources/read",
        json!({ "uri": "real://explain/RE8015" }),
    );
    let text = page["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("# RE8015"), "{text}");
    let via_tool = ok(
        "resource_read",
        &s.call(
            12,
            "resource_read",
            json!({ "uri": "real://docs/ROADMAP.md" }),
        ),
    );
    assert!(
        via_tool["data"]["text"]
            .as_str()
            .unwrap()
            .contains("AI track")
    );
    let listed = s.request(13, "resources/list", json!({}));
    assert!(
        listed["result"]["resources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["uri"] == "real://luau/engine.d.luau")
    );

    // The coverage table and the entity tree.
    let cov = ok(
        "compat_coverage",
        &s.call(14, "compat_coverage", json!({ "name": "Folder" })),
    );
    assert_eq!(cov["data"]["rows"][0]["key"], "Folder");
    let tree = ok(
        "scene_dump_tree",
        &s.call(15, "scene_dump_tree", json!({ "depth": 0 })),
    );
    assert!(tree["data"]["nodes"].as_u64().unwrap() > 0);

    s.finish();
}

#[test]
fn an_agent_imports_a_loader_into_a_place_and_plays_it() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("place");
    let made = Command::new(env!("CARGO_BIN_EXE_real"))
        .args(["new", "place", "--template", "roblox"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(made.status.success(), "{made:?}");
    std::fs::create_dir_all(project.join("import")).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../re_import/tests/fixtures/loader_basic.luau"),
        project.join("import/garden.luau"),
    )
    .unwrap();
    std::fs::create_dir_all(project.join(".real")).unwrap();
    std::fs::write(
        project.join(".real/agent-policy.toml"),
        "max_tier = \"edit\"\n[scopes]\nallow = [\"scripts/**\", \"scenes/**\"]\n",
    )
    .unwrap();
    let scene = project.join("scenes/main.scene.json");
    let scene_before = std::fs::read(&scene).unwrap();

    let mut s = Server::start(&project);
    s.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "mcp_e2e", "version": "0" }
        }),
    );
    s.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    let nodes = |s: &mut Server, id: u64| -> u64 {
        let tree = ok("scene_dump_tree", &s.call(id, "scene_dump_tree", json!({})));
        tree["data"]["nodes"].as_u64().unwrap()
    };
    let nodes_before = nodes(&mut s, 2);

    // A dry run by default: the report, and nothing written.
    let dry = ok(
        "project_import",
        &s.call(
            3,
            "project_import",
            json!({ "input": "import/garden.luau" }),
        ),
    );
    assert_eq!(dry["data"]["written"], false);
    assert_eq!(dry["data"]["report"]["format"], "loader");
    let created = dry["data"]["report"]["created"].as_array().unwrap().len();
    assert!(created > 0, "{dry}");
    assert_eq!(std::fs::read(&scene).unwrap(), scene_before);
    assert_eq!(nodes(&mut s, 4), nodes_before);

    // Only inputs inside the project.
    let outside = s.call(5, "project_import", json!({ "input": "../elsewhere.luau" }));
    assert_eq!(outside["isError"], true);
    assert_eq!(body(&outside)["code"], "RE8002");

    // The write: files, and the World, as one undoable step.
    let wrote = ok(
        "project_import",
        &s.call(
            6,
            "project_import",
            json!({ "input": "import/garden.luau", "write": true }),
        ),
    );
    assert_eq!(wrote["data"]["written"], true, "{wrote}");
    assert_eq!(
        wrote["data"]["report"]["created"],
        dry["data"]["report"]["created"]
    );
    assert_ne!(std::fs::read(&scene).unwrap(), scene_before);
    assert!(nodes(&mut s, 7) > nodes_before);

    // The imported place plays: a player joins and the tree shows the import.
    let played = ok(
        "play_run",
        &s.call_within(
            70,
            "play_run",
            json!({
                "frames": 10,
                "screenshot": false,
                "steps": ["add_player", { "dump_tree": { "path": "ReplicatedStorage", "depth": 3 } }]
            }),
            SLOW,
        ),
    );
    let played_text = played.to_string();
    assert!(played_text.contains("add_player"), "{played}");
    assert!(played_text.contains("Garden"), "{played}");

    // The Roblox-style API's types are a resource.
    let defs = s.request(
        71,
        "resources/read",
        json!({ "uri": "real://luau/roblox.d.luau" }),
    );
    let defs = defs["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(defs.contains("Instance"), "{defs}");
    ok("undo", &s.call(8, "undo", json!({})));
    assert_eq!(nodes(&mut s, 9), nodes_before);
    // The scripts it wrote stay until the checkpoint it took is restored.
    let written: Vec<String> = wrote["data"]["report"]["files"]["written"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap().to_string())
        .collect();
    let script = written
        .iter()
        .find(|f| f.ends_with(".luau"))
        .unwrap_or_else(|| panic!("no script written: {wrote}"));
    assert!(project.join(script).is_file());
    let cp = wrote["data"]["checkpoint"].as_str().unwrap();
    ok(
        "checkpoint_restore",
        &s.call(10, "checkpoint_restore", json!({ "checkpoint": cp })),
    );
    assert!(!project.join(script).exists(), "{script} is put back");
    assert_eq!(nodes(&mut s, 11), nodes_before);

    s.finish();
}

#[test]
fn a_second_headless_writer_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    copy_project(&island(), &project);
    let mut first = Server::start(&project);
    // Wait until the first server has the project.
    first.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "mcp_e2e", "version": "0" }
        }),
    );
    let mut second = Command::new(env!("CARGO_BIN_EXE_real"))
        .args(["mcp", "--project"])
        .arg(&project)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + TIMEOUT;
    while second.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            let _ = second.kill();
            panic!("a second real mcp did not exit");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let second = second.wait_with_output().unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("RE8003"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(second.stdout.is_empty());
    first.finish();
}
