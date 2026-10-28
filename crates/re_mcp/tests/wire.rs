//! The MCP wire protocol, ported from the AI spike's stdio tests, over both
//! transports (ADR-0046): raw newline-delimited JSON-RPC, exactly as a client
//! writes it to the server's stdin, over an in-memory pipe (`serve_io`;
//! `serve_stdio` binds the same server to the process's stdin and stdout,
//! which `re_cli`'s end-to-end test drives), and streamable HTTP on
//! 127.0.0.1 with a bearer token (`serve_http_async`, driven by
//! `re_mcp::client`). Every protocol test runs once per transport. Both
//! protocol revisions are covered: 2025-11-25 (initialize) and 2026-07-28
//! (stateless, `_meta` on every request).

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
/// with their real tiers, so `tools/list` sees the full M1 set of 25 (29
/// with WP17's `checkpoint.create`, `resource.list` and `resource.read`, and
/// `re_command`'s `project.status`).
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
                paths: Vec::new(),
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

/// An engine thread serving `registry` with `policy`, guarding `project`
/// (as `Policy::load` gives `real mcp`).
fn engine(project: &Path, policy: Policy) -> EngineHandle {
    let (handle, receiver) = channel();
    let reg = registry(project);
    let policy = policy.in_project(project);
    std::thread::spawn(move || {
        let mut state = EditState::new();
        receiver.serve(&mut state, &reg, &policy);
    });
    handle
}

/// The transport a test runs over: the same server, the same assertions.
#[derive(Clone, Copy, Debug)]
enum Transport {
    /// Newline-delimited JSON-RPC over an in-memory pipe (`serve_io`).
    Stdio,
    /// Streamable HTTP on 127.0.0.1 with a bearer token (`serve_http_async`).
    Http,
}

enum Wire {
    Stdio {
        writer: WriteHalf<DuplexStream>,
        lines: Lines<BufReader<tokio::io::ReadHalf<DuplexStream>>>,
        backlog: Vec<Value>,
    },
    Http {
        client: re_mcp::client::HttpClient,
        url: String,
        token: String,
        _stop: tokio::sync::oneshot::Sender<()>,
    },
}

struct Client {
    rt: tokio::runtime::Runtime,
    wire: Wire,
    _dir: tempfile::TempDir,
}

impl Client {
    fn start(transport: Transport, policy: Policy) -> Client {
        let dir = tempfile::tempdir().unwrap();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let handle = engine(dir.path(), policy);
        // Every call is recorded, as `real mcp --record` does.
        let recorder = re_mcp::record::Recorder::create(&dir.path().join("transcript.jsonl"));
        let options = re_mcp::McpOptions::new(dir.path())
            .with_resources(resources())
            .with_recorder(std::sync::Arc::new(recorder.unwrap()));
        let wire = match transport {
            Transport::Stdio => {
                let (client, server) = tokio::io::duplex(1 << 20);
                let (server_read, server_write) = tokio::io::split(server);
                rt.spawn(async move {
                    let _ = re_mcp::serve_io(handle, options, server_read, server_write).await;
                });
                let (read, writer) = tokio::io::split(client);
                Wire::Stdio {
                    writer,
                    lines: BufReader::new(read).lines(),
                    backlog: Vec::new(),
                }
            }
            Transport::Http => {
                let token = re_mcp::http::generate_token();
                let listener =
                    re_mcp::HttpListener::bind("127.0.0.1:0".parse().unwrap(), &token).unwrap();
                let url = listener.url();
                let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
                rt.spawn(async move {
                    let stopped = async move {
                        let _ = stopped.await;
                    };
                    let _ = re_mcp::serve_http_async(handle, options, listener, stopped).await;
                });
                let client = re_mcp::client::HttpClient::new(&url, Some(&token))
                    .unwrap()
                    .with_timeout(TIMEOUT);
                Wire::Http {
                    client,
                    url,
                    token,
                    _stop: stop,
                }
            }
        };
        Client {
            rt,
            wire,
            _dir: dir,
        }
    }

    fn project(&self) -> &Path {
        self._dir.path()
    }

    /// The tool calls recorded so far.
    fn transcript(&self) -> Vec<re_mcp::record::TranscriptEntry> {
        re_mcp::record::read_transcript(&self.project().join("transcript.jsonl")).unwrap()
    }

    /// Starts a server and completes the 2025-11-25 handshake.
    fn initialized(transport: Transport, policy: Policy) -> Client {
        let mut c = Client::start(transport, policy);
        let init = c.request(
            0,
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "wire-test", "version": "0" }
            }),
        );
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        c.notify("notifications/initialized", json!({}));
        c
    }

    fn send(&mut self, msg: Value) {
        let Wire::Stdio { writer, .. } = &mut self.wire else {
            unreachable!("send is for the stdio pipe")
        };
        let line = format!("{msg}\n");
        self.rt.block_on(async {
            writer.write_all(line.as_bytes()).await.unwrap();
            writer.flush().await.unwrap();
        });
    }

    fn notify(&mut self, method: &str, params: Value) {
        match &mut self.wire {
            Wire::Stdio { .. } => {
                self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
            }
            Wire::Http { client, .. } => client.notify(method, params).unwrap(),
        }
    }

    /// The response with `id` (responses may arrive out of order).
    fn response(&mut self, id: u64) -> Value {
        let Wire::Stdio { lines, backlog, .. } = &mut self.wire else {
            unreachable!("responses are read from the stdio pipe")
        };
        if let Some(i) = backlog.iter().position(|m| m["id"] == id) {
            return backlog.remove(i);
        }
        loop {
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
            backlog.push(msg);
        }
    }

    /// Sends a request and returns its response. Over HTTP, a request the
    /// transport refuses before any JSON-RPC answer is `{ "http_status": N }`.
    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let resp = match &mut self.wire {
            Wire::Stdio { .. } => {
                self.send(msg);
                self.response(id)
            }
            Wire::Http { client, .. } => {
                let reply = client.post(&msg, &[]).unwrap();
                match reply.messages.into_iter().find(|m| m["id"] == id) {
                    Some(resp) => resp,
                    None => return json!({ "http_status": reply.status }),
                }
            }
        };
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

fn initialize_advertises_capabilities_and_identity(t: Transport) {
    let mut c = Client::start(t, Policy::read_only());
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

fn tools_list_has_29_clean_tools(t: Transport) {
    let mut c = Client::initialized(t, Policy::read_only());
    let list = c.request(1, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools.len(),
        29,
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
        // The policy's file declarations are the engine's, not the client's.
        assert!(
            !has_key(schema, "x-real-path"),
            "{name} has an x-real-path keyword: {schema}"
        );
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

fn edit_call_with_read_policy_is_refused_but_dry_run_works(t: Transport) {
    let mut c = Client::initialized(t, Policy::read_only());
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

    // The server recorded each call with its outcome.
    let calls = c.transcript();
    let seen: Vec<(&str, bool, Option<&str>)> = calls
        .iter()
        .map(|e| (e.tool.as_str(), e.ok, e.code.as_deref()))
        .collect();
    assert_eq!(seen[0], ("entity_spawn", false, Some("RE8001")));
    assert_eq!(seen[1], ("entity_spawn", true, None));
    assert_eq!(seen[2].0, "entity_get");
    assert!(!seen[2].1);
    assert_eq!(calls[1].arguments, json!({ "id": "rock", "dry_run": true }));
    assert_eq!(calls[1].command.as_deref(), Some("entity.spawn"));
    assert!(calls[0].t_ms <= calls[1].t_ms);
}

fn scene_apply_is_atomic_and_one_undo_reverts_it(t: Transport) {
    let mut c = Client::initialized(t, Policy::init_default());
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

fn script_write_with_a_syntax_error_writes_nothing(t: Transport) {
    let mut c = Client::initialized(t, Policy::init_default());
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

fn screenshot_returns_image_content_and_its_path(t: Transport) {
    let mut c = Client::initialized(t, Policy::read_only());
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

fn bad_input_and_unknown_tools(t: Transport) {
    let mut c = Client::initialized(t, Policy::init_default());
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
    // An unknown tool is recorded with no command.
    let unknown: Vec<_> = c
        .transcript()
        .into_iter()
        .filter(|e| e.command.is_none())
        .collect();
    assert_eq!(unknown.len(), 2);
    assert!(unknown.iter().all(|e| e.is_error && e.code.is_none()));
}

fn stateless_2026_07_28_requests_carry_meta(t: Transport) {
    let mut c = Client::start(t, Policy::read_only());
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
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 29);
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
    match t {
        Transport::Stdio => assert_eq!(
            no_meta["error"]["code"], -32602,
            "requests without initialize or _meta are rejected"
        ),
        // Over HTTP the transport refuses it first: no session, no revision.
        Transport::Http => assert!(
            no_meta["error"].is_object() || no_meta["http_status"].as_u64() >= Some(400),
            "requests without initialize or _meta are rejected: {no_meta}"
        ),
    }
}

#[test]
fn diagnostics_catalogue_is_complete() {
    let problems = re_core::validate_catalog(re_mcp::DIAGNOSTICS, re_mcp::CODE_RANGE);
    assert!(problems.is_empty(), "{problems:?}");
}

fn typed_results_come_back_as_structured_content(t: Transport) {
    let mut c = Client::initialized(t, Policy::read_only());
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

fn resources_are_listed_and_read(t: Transport) {
    let mut c = Client::initialized(t, Policy::read_only());
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

/// Each protocol test, once per transport.
macro_rules! over_both_transports {
    ($($name:ident),* $(,)?) => {
        mod stdio {
            $(#[test] fn $name() { super::$name(super::Transport::Stdio) })*
        }
        mod http {
            $(#[test] fn $name() { super::$name(super::Transport::Http) })*
        }
    };
}

over_both_transports!(
    initialize_advertises_capabilities_and_identity,
    tools_list_has_29_clean_tools,
    edit_call_with_read_policy_is_refused_but_dry_run_works,
    scene_apply_is_atomic_and_one_undo_reverts_it,
    script_write_with_a_syntax_error_writes_nothing,
    screenshot_returns_image_content_and_its_path,
    bad_input_and_unknown_tools,
    stateless_2026_07_28_requests_carry_meta,
    typed_results_come_back_as_structured_content,
    resources_are_listed_and_read,
);

fn initialize() -> Value {
    json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "wire-test", "version": "0" }
        }
    })
}

/// The HTTP endpoint of a client started with [`Transport::Http`].
fn endpoint(c: &Client) -> (String, String) {
    match &c.wire {
        Wire::Http { url, token, .. } => (url.clone(), token.clone()),
        Wire::Stdio { .. } => unreachable!(),
    }
}

#[test]
fn http_refuses_a_missing_or_wrong_token() {
    let c = Client::start(Transport::Http, Policy::read_only());
    let (url, token) = endpoint(&c);
    let mut anonymous = re_mcp::client::HttpClient::new(&url, None).unwrap();
    let reply = anonymous.post(&initialize(), &[]).unwrap();
    assert_eq!(reply.status, 401);
    assert!(reply.messages.is_empty());

    let wrong = format!("{}x", &token[..token.len() - 1]);
    let mut guesser = re_mcp::client::HttpClient::new(&url, Some(&wrong)).unwrap();
    assert_eq!(guesser.post(&initialize(), &[]).unwrap().status, 401);

    let mut right = re_mcp::client::HttpClient::new(&url, Some(&token)).unwrap();
    assert_eq!(right.post(&initialize(), &[]).unwrap().status, 200);
    assert!(right.session().is_some(), "2025-11-25 gets a session");
}

/// A refusal reads the request it refuses: closing a socket with an unread
/// body resets it, and the client can lose the 401 (always on Windows).
#[test]
fn http_refuses_a_large_body_without_a_reset() {
    let c = Client::start(Transport::Http, Policy::read_only());
    let (url, _) = endpoint(&c);
    let mut padded = initialize();
    padded["params"]["padding"] = json!("x".repeat(48 * 1024));
    for _ in 0..20 {
        let mut anonymous = re_mcp::client::HttpClient::new(&url, None).unwrap();
        assert_eq!(anonymous.post(&padded, &[]).unwrap().status, 401);
    }
}

/// So does rmcp's refusal of a foreign `Host` or a browser `Origin`, which
/// never reads the body itself. A refusal that has read the body keeps the
/// connection open, so a second request on it is answered; one that has not
/// closes it with the body unread.
#[test]
fn http_refuses_a_foreign_host_s_large_body_without_a_reset() {
    use std::io::{BufRead, BufReader, Read, Write};
    let c = Client::start(Transport::Http, Policy::read_only());
    let (url, token) = endpoint(&c);
    let addr = url.trim_start_matches("http://").split('/').next().unwrap();
    let padded = json!({ "padding": "x".repeat(48 * 1024) }).to_string();
    for foreign in [
        "Host: evil.example:80".to_string(),
        format!("Host: {addr}\r\nOrigin: http://evil.example"),
    ] {
        let stream = std::net::TcpStream::connect(addr).unwrap();
        stream.set_read_timeout(Some(TIMEOUT)).unwrap();
        let mut writer = stream.try_clone().unwrap();
        let mut reader = BufReader::new(stream);
        for (body, close) in [(padded.as_str(), ""), ("{}", "Connection: close\r\n")] {
            write!(
                writer,
                "POST /mcp HTTP/1.1\r\n{foreign}\r\nAuthorization: Bearer {token}\r\n\
                 Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
                 {close}Content-Length: {}\r\n\r\n",
                body.len()
            )
            .unwrap();
            // The body comes after rmcp has refused the head.
            std::thread::sleep(Duration::from_millis(50));
            writer.write_all(body.as_bytes()).unwrap();
            let mut status = String::new();
            reader.read_line(&mut status).unwrap();
            assert!(status.starts_with("HTTP/1.1 403"), "{foreign}: {status:?}");
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim_end().to_ascii_lowercase();
                if line.is_empty() {
                    break;
                }
                if let Some(value) = line.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
        }
    }
}

#[test]
fn http_refuses_foreign_hosts_browser_origins_and_other_paths() {
    let c = Client::start(Transport::Http, Policy::read_only());
    let (url, token) = endpoint(&c);
    let mut client = re_mcp::client::HttpClient::new(&url, Some(&token)).unwrap();
    // DNS rebinding: a page on evil.example resolving to 127.0.0.1.
    let host = client
        .post(&initialize(), &[("Host", "evil.example:80")])
        .unwrap();
    assert_eq!(host.status, 403);
    // The right host on another port is foreign too.
    let port = client
        .post(&initialize(), &[("Host", "127.0.0.1:1")])
        .unwrap();
    assert_eq!(port.status, 403);
    // Any browser origin, even a loopback one.
    for origin in ["http://evil.example", "http://127.0.0.1:3000", "null"] {
        let reply = client.post(&initialize(), &[("Origin", origin)]).unwrap();
        assert_eq!(reply.status, 403, "{origin}");
    }
    // `localhost` with the right port is this server.
    let port = url.rsplit(':').next().unwrap().trim_end_matches("/mcp");
    let local = client
        .post(&initialize(), &[("Host", &format!("localhost:{port}"))])
        .unwrap();
    assert_eq!(local.status, 200);

    let other = url.replace("/mcp", "/other");
    let mut elsewhere = re_mcp::client::HttpClient::new(&other, Some(&token)).unwrap();
    assert_eq!(elsewhere.post(&initialize(), &[]).unwrap().status, 404);
}

#[test]
fn http_sessions_share_one_engine() {
    let mut a = Client::initialized(Transport::Http, Policy::init_default());
    let (url, token) = endpoint(&a);
    let mut b = re_mcp::client::HttpClient::new(&url, Some(&token)).unwrap();
    b.request(1, "initialize", initialize()["params"].clone())
        .unwrap();
    b.notify("notifications/initialized", json!({})).unwrap();
    assert_ne!(
        b.session(),
        match &a.wire {
            Wire::Http { client, .. } => client.session(),
            Wire::Stdio { .. } => unreachable!(),
        }
    );

    let spawned = a.call(1, "entity_spawn", json!({ "id": "shared" }));
    assert_eq!(spawned["isError"], false, "{spawned}");
    let seen = b
        .request(
            2,
            "tools/call",
            json!({ "name": "entity_get", "arguments": { "id": "shared" } }),
        )
        .unwrap();
    assert_eq!(seen["result"]["isError"], false, "{seen}");
}

#[test]
fn http_stops_when_asked_and_releases_the_engine() {
    let token = re_mcp::http::generate_token();
    let listener = re_mcp::HttpListener::bind("127.0.0.1:0".parse().unwrap(), &token).unwrap();
    let url = listener.url();
    let dir = tempfile::tempdir().unwrap();
    let (handle, receiver) = channel();
    let reg = registry(dir.path());
    let engine = std::thread::spawn(move || {
        let mut state = EditState::new();
        receiver.serve(&mut state, &reg, &Policy::read_only());
    });
    let (stop, stopped) = std::sync::mpsc::channel::<()>();
    let options = re_mcp::McpOptions::new(dir.path());
    let server = std::thread::spawn(move || {
        let stopped = async move {
            let _ = tokio::task::spawn_blocking(move || stopped.recv()).await;
        };
        re_mcp::serve_http(handle, options, listener, stopped)
    });
    let mut client = re_mcp::client::HttpClient::new(&url, Some(&token)).unwrap();
    client
        .request(1, "initialize", initialize()["params"].clone())
        .unwrap();

    stop.send(()).unwrap();
    server.join().unwrap().unwrap();
    // Every engine handle is gone, so the engine's serve loop has returned.
    engine.join().unwrap();
    assert!(client.post(&initialize(), &[]).is_err(), "nothing listens");
}

#[test]
fn http_stop_releases_the_engine_while_the_runtime_lives() {
    // A caller on a long-lived runtime (the eval harness) must get the
    // engine back as soon as the server stops, not after rmcp's idle
    // timeout: every open session is closed on stop.
    let token = re_mcp::http::generate_token();
    let listener = re_mcp::HttpListener::bind("127.0.0.1:0".parse().unwrap(), &token).unwrap();
    let url = listener.url();
    let dir = tempfile::tempdir().unwrap();
    let (handle, receiver) = channel();
    let reg = registry(dir.path());
    let (engine_done, engine_exited) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut state = EditState::new();
        receiver.serve(&mut state, &reg, &Policy::read_only());
        let _ = engine_done.send(());
    });
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let options = re_mcp::McpOptions::new(dir.path());
    let server = rt.spawn(async move {
        let stopped = async move {
            let _ = stopped.await;
        };
        re_mcp::serve_http_async(handle, options, listener, stopped).await
    });
    // Two sessions with a call each, so both hold engine handles.
    for id in 0..2 {
        let mut client = re_mcp::client::HttpClient::new(&url, Some(&token)).unwrap();
        client
            .request(id, "initialize", initialize()["params"].clone())
            .unwrap();
        client
            .notify("notifications/initialized", json!({}))
            .unwrap();
        let listed = client.request(10 + id, "tools/list", json!({})).unwrap();
        assert!(listed["result"]["tools"].is_array());
    }

    stop.send(()).unwrap();
    rt.block_on(server).unwrap().unwrap();
    engine_exited
        .recv_timeout(Duration::from_secs(10))
        .expect("the engine stops once the server has stopped, with the runtime still running");
    drop(rt);
}
