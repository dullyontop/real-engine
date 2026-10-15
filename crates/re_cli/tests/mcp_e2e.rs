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
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use serde_json::{Value, json};

const TIMEOUT: Duration = Duration::from_secs(120);

fn island() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/island")
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
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
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let line = self
                .lines
                .recv_timeout(TIMEOUT)
                .unwrap_or_else(|_| panic!("no response to {method} (id {id})"));
            let msg: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("stdout carried a non-JSON line ({e}): {line}"));
            assert_eq!(msg["jsonrpc"], "2.0", "{line}");
            if msg["id"] == id && msg.get("method").is_none() {
                return msg;
            }
        }
    }

    /// Calls a tool; protocol errors fail the test.
    fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        let resp = self.request(
            id,
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
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
fn body(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn ok(tool: &str, result: &Value) -> Value {
    assert_eq!(result["isError"], false, "{tool} failed: {result}");
    body(result)
}

#[test]
fn an_agent_edits_checks_and_restores_the_island() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    copy_dir(&island(), &project);
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
    assert_eq!(names.len(), 25, "{names:?}");
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

    // Restoring the start right away changes nothing and reports its hash.
    let start = ok(
        "checkpoint_restore",
        &s.call(3, "checkpoint_restore", json!({})),
    );
    let start_hash = start["data"]["state_hash"].as_str().unwrap().to_string();

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
    ok(
        "graph_regenerate",
        &s.call(7, "graph_regenerate", json!({ "source": "island" })),
    );

    let shot = s.call(8, "screenshot", json!({}));
    ok("screenshot", &shot);
    let content = shot["content"].as_array().unwrap();
    let image = content
        .iter()
        .find(|c| c["type"] == "image")
        .unwrap_or_else(|| panic!("screenshot returned no image: {shot}"));
    assert_eq!(image["mimeType"], "image/png");
    assert!(image["data"].as_str().unwrap().starts_with("iVBORw0KGgo"));

    // `verify` runs; the edits may make the goldens differ, so only its
    // report is checked.
    let verified = s.call(9, "verify", json!({}));
    let report = body(&verified);
    assert!(
        report.get("data").is_some() || report.get("code").is_some(),
        "{verified}"
    );

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
    let gone = s.call(13, "entity_get", json!({ "id": "e2e_rock" }));
    assert_eq!(gone["isError"], true, "{gone}");
    assert!(!project.join("scripts/game/e2e.luau").exists());

    s.finish();
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

#[test]
fn a_second_headless_writer_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    copy_dir(&island(), &project);
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
    let second = Command::new(env!("CARGO_BIN_EXE_real"))
        .args(["mcp", "--project"])
        .arg(&project)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(second.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&second.stderr).contains("RE8003"),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(second.stdout.is_empty());
    first.finish();
}
