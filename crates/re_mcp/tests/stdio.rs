//! The MCP wire protocol, ported from the AI spike's stdio tests: raw
//! newline-delimited JSON-RPC, exactly as a client writes it to the server's
//! stdin, over an in-memory pipe (`serve_io`; `serve_stdio` binds the same
//! server to the process's stdin and stdout, which `re_cli`'s end-to-end test
//! drives). Both protocol revisions are covered: 2025-11-25 (initialize) and
//! 2026-07-28 (stateless, `_meta` on every request).

use std::path::Path;
use std::time::Duration;

use re_agent::resources::ResourceSet;
use re_agent::{Agent, Policy, register_commands};
use re_command::{
    CommandAnnotations, CommandRegistry, CommandSpec, EditState, EngineHandle, NoArgs, Tier,
    channel, register_builtin_commands,
};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, WriteHalf};

const TIMEOUT: Duration = Duration::from_secs(10);

/// The commands of ARCHITECTURE §6 that other crates register, as stubs
/// with their real tiers, so `tools/list` sees the full M1 set of 25 (28
/// with WP17's `checkpoint.create`, `resource.list` and `resource.read`).
const STUBS: &[(&str, Tier, bool)] = &[
    ("graph.report", Tier::Read, true),
    ("graph.generate", Tier::Read, true),
    ("screenshot", Tier::Read, true),
    ("verify", Tier::Read, true),
    ("play.run", Tier::Read, true),
    ("explain", Tier::Read, true),
    ("graph.set_param", Tier::Edit, true),
    ("graph.regenerate", Tier::Edit, false),
    ("asset.import", Tier::Edit, false),
];

/// The resources the test server serves: one explain page and one Luau file.
fn resources() -> std::sync::Arc<ResourceSet> {
    let mut set = ResourceSet::new();
    set.add(
        "real://explain/RE8001".into(),
        "RE8001".into(),
        "Explain page for RE8001".into(),
        "text/markdown",
        "# RE8001: Tier refused\n",
    );
    set.add(
        "real://luau/engine.d.luau".into(),
        "engine.d.luau".into(),
        "Luau definitions".into(),
        "text/x-luau",
        "declare world: {}\n",
    );
    std::sync::Arc::new(set)
}

fn registry(project: &Path) -> CommandRegistry<EditState> {
    let mut r = CommandRegistry::new();
    register_builtin_commands(&mut r);
    register_commands(&mut r, &Agent::new(project));
    re_agent::resources::register_commands(&mut r, resources());
    for &(name, tier, idempotent) in STUBS {
        let project = project.to_path_buf();
        r.register(
            CommandSpec {
                name: name.into(),
                summary: format!("Stub of {name}."),
                tier,
                input_schema: re_command::input_schema::<NoArgs>(tier),
                annotations: CommandAnnotations::for_tier(tier, idempotent),
                output_schema: None,
            },
            std::sync::Arc::new(move |_t, _args, _call| {
                if name == "screenshot" {
                    // A 1x1 PNG.
                    let png = [
                        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0x0D, 0x49, 0x48,
                        0x44, 0x52, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1F, 0x15, 0xC4, 0x89,
                        0, 0, 0, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0, 1, 0, 0, 5, 0,
                        1, 0x0D, 0x0A, 0x2D, 0xB4, 0, 0, 0, 0, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42,
                        0x60, 0x82,
                    ];
                    std::fs::create_dir_all(project.join(".real/agent")).unwrap();
                    std::fs::write(project.join(".real/agent/shot.png"), png).unwrap();
                    return Ok(json!({ "png": ".real/agent/shot.png", "width": 1 }));
                }
                if name == "verify" {
                    return Ok(json!({ "missing": "gone.png" }));
                }
                Ok(Value::Null)
            }),
        );
    }
    r
}

/// An engine thread serving `registry` with `policy`.
fn engine(project: &Path, policy: Policy) -> EngineHandle {
    let (handle, receiver) = channel();
    let reg = registry(project);
    std::thread::spawn(move || {
        let mut state = EditState::new();
        receiver.serve(&mut state, &reg, &policy);
    });
    handle
}

struct Client {
    rt: tokio::runtime::Runtime,
    writer: WriteHalf<DuplexStream>,
    lines: Lines<BufReader<tokio::io::ReadHalf<DuplexStream>>>,
    backlog: Vec<Value>,
    _dir: tempfile::TempDir,
}

impl Client {
    fn start(policy: Policy) -> Client {
        let dir = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (client, server) = tokio::io::duplex(1 << 20);
        let (server_read, server_write) = tokio::io::split(server);
        let handle = engine(dir.path(), policy);
        let options = re_mcp::McpOptions::new(dir.path()).with_resources(resources());
        rt.spawn(async move {
            let _ = re_mcp::serve_io(handle, options, server_read, server_write).await;
        });
        let (read, writer) = tokio::io::split(client);
        Client {
            rt,
            writer,
            lines: BufReader::new(read).lines(),
            backlog: Vec::new(),
            _dir: dir,
        }
    }

    fn project(&self) -> &Path {
        self._dir.path()
    }

    /// Starts a server and completes the 2025-11-25 handshake.
    fn initialized(policy: Policy) -> Client {
        let mut c = Client::start(policy);
        let init = c.request(
            0,
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "stdio-test", "version": "0" }
            }),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        c.notify("notifications/initialized", json!({}));
        c
    }

    fn send(&mut self, msg: Value) {
        let line = format!("{msg}\n");
        let writer = &mut self.writer;
        self.rt.block_on(async {
            writer.write_all(line.as_bytes()).await.unwrap();
            writer.flush().await.unwrap();
        });
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    /// The response with `id` (responses may arrive out of order).
    fn response(&mut self, id: u64) -> Value {
        if let Some(i) = self.backlog.iter().position(|m| m["id"] == id) {
            return self.backlog.remove(i);
        }
        loop {
            let lines = &mut self.lines;
            let line = self
                .rt
                .block_on(async { tokio::time::timeout(TIMEOUT, lines.next_line()).await })
                .unwrap_or_else(|_| panic!("no response for id {id} within {TIMEOUT:?}"))
                .unwrap()
                .expect("server closed its output");
            let msg: Value = serde_json::from_str(&line)
                .unwrap_or_else(|e| panic!("server wrote non-JSON ({e}): {line}"));
            if msg["id"] == id && msg.get("method").is_none() {
                return msg;
            }
            self.backlog.push(msg);
        }
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let resp = self.response(id);
        assert_eq!(resp["jsonrpc"], "2.0");
        resp
    }

    fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        let resp = self.request(
            id,
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        );
        resp.get("result")
            .cloned()
            .unwrap_or_else(|| panic!("{tool} returned a protocol error: {resp}"))
    }
}

fn text_json(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
}

#[test]
fn initialize_advertises_capabilities_and_identity() {
    let mut c = Client::start(Policy::read_only());
    let init = c.request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "stdio-test", "version": "0" }
        }),
    );
    let r = &init["result"];
    assert_eq!(r["protocolVersion"], "2025-11-25");
    assert_eq!(r["serverInfo"]["name"], "real-engine");
    assert!(r["capabilities"]["tools"].is_object());
    assert!(r["capabilities"]["resources"].is_object());
    let instructions = r["instructions"].as_str().unwrap();
    assert!(instructions.starts_with("Real Engine"));
    assert!(instructions.len() < 4096);
}

#[test]
fn tools_list_has_28_clean_tools() {
    let mut c = Client::initialized(Policy::read_only());
    let list = c.request(1, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools.len(),
        28,
        "{:#?}",
        tools.iter().map(|t| &t["name"]).collect::<Vec<_>>()
    );

    fn has_key(v: &Value, key: &str) -> bool {
        match v {
            Value::Object(m) => {
                m.get(key).is_some_and(Value::is_string) || m.values().any(|v| has_key(v, key))
            }
            Value::Array(a) => a.iter().any(|v| has_key(v, key)),
            _ => false,
        }
    }
    for tool in tools {
        let name = tool["name"].as_str().unwrap();
        assert!(
            name.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "{name}"
        );
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object", "{name}");
        for key in ["anyOf", "oneOf", "allOf"] {
            assert!(schema.get(key).is_none(), "{name} has a root {key}");
        }
        assert!(
            !has_key(schema, "format"),
            "{name} has a format keyword: {schema}"
        );
        assert!(!has_key(schema, "$schema"), "{name}");
        let description = tool["description"].as_str().unwrap();
        assert!(description.len() < 4096, "{name}");
        let a = &tool["annotations"];
        for hint in [
            "readOnlyHint",
            "destructiveHint",
            "idempotentHint",
            "openWorldHint",
        ] {
            assert!(a[hint].is_boolean(), "{name} lacks {hint}");
        }
        assert_eq!(a["openWorldHint"], false);
        // Every tool describes its structured result: the outcome envelope.
        let out = &tool["outputSchema"];
        assert_eq!(out["type"], "object", "{name}");
        assert_eq!(
            out["required"],
            json!(["ok", "changes", "diagnostics", "data"]),
            "{name}"
        );
        assert!(!has_key(out, "$schema"), "{name}");
    }
    let by_name = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap();
    let query = &by_name("query")["annotations"];
    assert_eq!(
        (
            query["readOnlyHint"].clone(),
            query["destructiveHint"].clone()
        ),
        (json!(true), json!(false))
    );
    let spawn = &by_name("entity_spawn")["annotations"];
    assert_eq!(
        (
            spawn["readOnlyHint"].clone(),
            spawn["destructiveHint"].clone()
        ),
        (json!(false), json!(false))
    );
    let delete = &by_name("file_delete")["annotations"];
    assert_eq!(delete["destructiveHint"], true);
    assert!(by_name("entity_spawn")["inputSchema"]["properties"]["dry_run"].is_object());
    assert!(
        by_name("graph_set_param")["description"]
            .as_str()
            .unwrap()
            .starts_with("Stub of graph.set_param.")
    );
}

#[test]
fn edit_call_with_read_policy_is_refused_but_dry_run_works() {
    let mut c = Client::initialized(Policy::read_only());
    let refused = c.call(1, "entity_spawn", json!({ "id": "rock" }));
    assert_eq!(refused["isError"], true);
    let body = text_json(&refused);
    assert_eq!(body["code"], "RE8001");
    assert_eq!(body["explain"], "real explain RE8001");
    assert!(body["message"].as_str().unwrap().contains("entity.spawn"));
    assert!(body["hint"].as_str().unwrap().contains("dry_run"));

    let dry = c.call(2, "entity_spawn", json!({ "id": "rock", "dry_run": true }));
    assert_eq!(dry["isError"], false);
    let outcome = text_json(&dry);
    assert_eq!(outcome["ok"], true);
    assert_eq!(outcome["changes"][0]["op"], "spawn");

    // Nothing was applied.
    let get = c.call(3, "entity_get", json!({ "id": "rock" }));
    assert_eq!(get["isError"], true);
}

#[test]
fn scene_apply_is_atomic_and_one_undo_reverts_it() {
    let mut c = Client::initialized(Policy::init_default());
    let three = json!({ "ops": [
        { "op": "spawn", "id": "a" },
        { "op": "spawn", "id": "b", "parent": "a" },
        { "op": "rename", "id": "b", "name": "Bee" }
    ]});
    let out = c.call(1, "scene_apply", three);
    assert_eq!(out["isError"], false, "{out}");
    let q = text_json(&c.call(2, "query", json!({})));
    assert_eq!(q["data"]["total"], 2, "{q}");

    // A failing op leaves nothing behind.
    let bad = json!({ "ops": [
        { "op": "spawn", "id": "c" },
        { "op": "rename", "id": "nope", "name": "x" }
    ]});
    let out = c.call(3, "scene_apply", bad);
    assert_eq!(out["isError"], true);
    assert_eq!(text_json(&out)["code"], "RE2001");
    assert_eq!(
        c.call(4, "entity_get", json!({ "id": "c" }))["isError"],
        true
    );

    let undo = c.call(5, "undo", json!({}));
    assert_eq!(undo["isError"], false);
    let q = text_json(&c.call(6, "query", json!({})));
    assert_eq!(q["data"]["total"], 0, "{q}");
}

#[test]
fn script_write_with_a_syntax_error_writes_nothing() {
    let mut c = Client::initialized(Policy::init_default());
    let out = c.call(
        1,
        "script_write",
        json!({ "path": "scripts/bad.luau", "source": "local x = = 1" }),
    );
    assert_eq!(out["isError"], true);
    let body = text_json(&out);
    assert!(body["code"].as_str().unwrap().starts_with("RE3"), "{body}");
    assert!(!c.project().join("scripts/bad.luau").exists());
    let out = c.call(
        2,
        "script_write",
        json!({ "path": "notes/x.luau", "source": "return 1" }),
    );
    assert_eq!(text_json(&out)["code"], "RE8002");
}

#[test]
fn screenshot_returns_image_content_and_its_path() {
    let mut c = Client::initialized(Policy::read_only());
    let out = c.call(1, "screenshot", json!({}));
    assert_eq!(out["isError"], false);
    let content = out["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(text_json(&out)["data"]["png"], ".real/agent/shot.png");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["mimeType"], "image/png");
    assert!(
        content[1]["data"]
            .as_str()
            .unwrap()
            .starts_with("iVBORw0KGgo")
    );

    // A PNG that can't be read is a warning, not a failure.
    let out = c.call(2, "verify", json!({}));
    assert_eq!(out["isError"], false);
    assert_eq!(out["content"].as_array().unwrap().len(), 1);
    assert_eq!(text_json(&out)["diagnostics"][0]["code"], "RE8501");
}

#[test]
fn bad_input_and_unknown_tools() {
    let mut c = Client::initialized(Policy::init_default());
    // Arguments that don't match the schema are a tool error (RE2005).
    let bad = c.call(1, "entity_spawn", json!({ "id": "x", "colour": "red" }));
    assert_eq!(bad["isError"], true);
    assert_eq!(text_json(&bad)["code"], "RE2005");
    // An unknown tool is a protocol error.
    let unknown = c.request(
        2,
        "tools/call",
        json!({ "name": "delete_everything", "arguments": {} }),
    );
    assert_eq!(unknown["error"]["code"], -32602);
    // A dotted name is not a tool name.
    let dotted = c.request(
        3,
        "tools/call",
        json!({ "name": "entity.spawn", "arguments": {} }),
    );
    assert_eq!(dotted["error"]["code"], -32602);
    // Missing arguments are an empty object.
    let describe = c.request(4, "tools/call", json!({ "name": "describe" }));
    assert_eq!(describe["result"]["isError"], false);
}

#[test]
fn stateless_2026_07_28_requests_carry_meta() {
    let mut c = Client::start(Policy::read_only());
    let meta = json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientCapabilities": {}
    });
    let discover = c.request(1, "server/discover", json!({ "_meta": meta }));
    let versions = discover["result"]["supportedVersions"].as_array().unwrap();
    assert!(versions.contains(&json!("2026-07-28")) && versions.contains(&json!("2025-11-25")));
    assert_eq!(
        discover["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "real-engine"
    );

    let list = c.request(2, "tools/list", json!({ "_meta": meta }));
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 28);
    let query = c.request(
        3,
        "tools/call",
        json!({ "name": "query", "arguments": {}, "_meta": meta }),
    );
    assert_eq!(query["result"]["resultType"], "complete");
    assert_eq!(query["result"]["isError"], false);
    let refused = c.request(
        4,
        "tools/call",
        json!({ "name": "entity_spawn", "arguments": { "id": "x" }, "_meta": meta }),
    );
    assert_eq!(refused["result"]["isError"], true);
    assert_eq!(text_json(&refused["result"])["code"], "RE8001");

    let no_meta = c.request(5, "tools/list", json!({}));
    assert_eq!(
        no_meta["error"]["code"], -32602,
        "requests without initialize or _meta are rejected"
    );
}

#[test]
fn diagnostics_catalogue_is_complete() {
    let problems = re_core::validate_catalog(re_mcp::DIAGNOSTICS, re_mcp::CODE_RANGE);
    assert!(problems.is_empty(), "{problems:?}");
}

#[test]
fn typed_results_come_back_as_structured_content() {
    let mut c = Client::initialized(Policy::read_only());
    let list = c.request(1, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    let read = tools.iter().find(|t| t["name"] == "resource_read").unwrap();
    let data = &read["outputSchema"]["properties"]["data"];
    assert_eq!(data["type"], "object", "{read}");
    assert!(data["properties"]["text"].is_object(), "{read}");
    // A list result refers to its item type through the root `$defs`.
    let listed = tools.iter().find(|t| t["name"] == "resource_list").unwrap();
    let schema = &listed["outputSchema"];
    assert!(schema["$defs"]["Resource"].is_object(), "{schema}");

    let out = c.call(
        2,
        "resource_read",
        json!({ "uri": "real://explain/RE8001" }),
    );
    assert_eq!(out["isError"], false, "{out}");
    let structured = &out["structuredContent"];
    assert_eq!(structured["ok"], true);
    assert_eq!(structured["data"]["text"], "# RE8001: Tier refused\n");
    // The text block carries the same JSON for clients that read only text.
    assert_eq!(&text_json(&out), structured);

    let missing = c.call(
        3,
        "resource_read",
        json!({ "uri": "real://explain/RE8000" }),
    );
    assert_eq!(missing["isError"], true);
    assert_eq!(text_json(&missing)["code"], "RE8014");
}

#[test]
fn resources_are_listed_and_read() {
    let mut c = Client::initialized(Policy::read_only());
    let list = c.request(1, "resources/list", json!({}));
    let uris: Vec<&str> = list["result"]["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert_eq!(uris, ["real://explain/RE8001", "real://luau/engine.d.luau"]);
    let read = c.request(
        2,
        "resources/read",
        json!({ "uri": "real://luau/engine.d.luau" }),
    );
    let contents = &read["result"]["contents"][0];
    assert_eq!(contents["text"], "declare world: {}\n");
    assert_eq!(contents["mimeType"], "text/x-luau");
    let missing = c.request(3, "resources/read", json!({ "uri": "real://nope" }));
    assert_eq!(missing["error"]["code"], -32002, "{missing}");
}
