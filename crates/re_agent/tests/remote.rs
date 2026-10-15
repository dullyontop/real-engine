//! `re_agent::remote`: the editor socket protocol, server loop and client.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use re_agent::lock::{self, Holder, LockState};
use re_agent::remote;
use re_command::{
    AllowAll, CommandRegistry, EditState, EngineHandle, Origin, channel, register_builtin_commands,
};
use serde_json::{Value as Json, json};

/// An "editor": an engine thread that owns an `EditState`, plus the socket
/// served for it. `stop` ends the engine thread and returns its state.
struct Editor {
    stop: Arc<AtomicBool>,
    engine: JoinHandle<EditState>,
    _lock: lock::ProjectLock,
}

impl Editor {
    fn start(project: &Path) -> Editor {
        let held = lock::acquire(project, Holder::Editor).unwrap();
        let listener = remote::bind(&held).unwrap();
        assert_eq!(listener.path(), lock::socket_path(project));
        let (handle, receiver) = channel();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let engine = std::thread::spawn(move || {
            let mut registry = CommandRegistry::<EditState>::new();
            register_builtin_commands(&mut registry);
            let mut state = EditState::new();
            while !flag.load(Ordering::SeqCst) {
                if let Ok(Some(request)) = receiver.recv_timeout(Duration::from_millis(5)) {
                    request.handle(&mut state, &registry, &AllowAll);
                }
            }
            state
        });
        std::thread::spawn(move || remote::serve(listener, handle));
        Editor {
            stop,
            engine,
            _lock: held,
        }
    }

    fn stop(self) -> EditState {
        self.stop.store(true, Ordering::SeqCst);
        self.engine.join().unwrap()
    }
}

fn connect_to_editor(project: &Path) -> EngineHandle {
    let LockState::Editor(socket) = lock::probe(project) else {
        panic!("the editor should hold the lock");
    };
    remote::connect(&socket).unwrap()
}

#[test]
fn serve_and_connect_round_trip_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let editor = Editor::start(dir.path());
    let handle = connect_to_editor(dir.path());

    let specs = handle.list_specs().unwrap();
    assert!(specs.iter().any(|s| s.name == "entity.spawn"));

    // A dry run changes nothing; the real call is applied by the editor.
    let dry = handle.call(
        "entity.spawn",
        json!({ "id": "rock", "name": "Rock" }),
        true,
    );
    assert!(dry.ok, "{:?}", dry.diagnostics);
    assert!(!dry.changes.is_empty());
    let agent = handle.with_origin(Origin::Agent("mcp".into()));
    let out = agent.call(
        "entity.spawn",
        json!({ "id": "rock", "name": "Rock" }),
        false,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let got = handle.call("entity.get", json!({ "id": "rock" }), false);
    assert!(got.ok, "{:?}", got.diagnostics);
    assert_eq!(got.data["name"], "Rock");

    // Errors come back as outcomes.
    let unknown = handle.call("entity.spwan", json!({}), false);
    assert!(!unknown.ok);
    assert_eq!(unknown.diagnostics[0].code.to_string(), "RE2004");

    let state = editor.stop();
    assert!(state.world.entity("rock").is_ok());
    // The editor's log records the client's origin, and only the real call.
    let log = state.history.log_since_jsonl(0);
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains("\"agent\":\"mcp\""), "{log}");
    // The engine thread is gone: the client now gets RE2006.
    let gone = handle.call("describe", json!({}), false);
    assert_eq!(gone.diagnostics[0].code.to_string(), "RE2006");
}

#[test]
fn several_clients_share_one_editor() {
    let dir = tempfile::tempdir().unwrap();
    let editor = Editor::start(dir.path());
    let clients: Vec<_> = (0..4)
        .map(|i| {
            let handle = connect_to_editor(dir.path());
            std::thread::spawn(move || {
                for j in 0..5 {
                    let out =
                        handle.call("entity.spawn", json!({ "id": format!("e{i}_{j}") }), false);
                    assert!(out.ok, "{:?}", out.diagnostics);
                }
            })
        })
        .collect();
    for c in clients {
        c.join().unwrap();
    }
    let state = editor.stop();
    for i in 0..4 {
        for j in 0..5 {
            assert!(state.world.entity(&format!("e{i}_{j}")).is_ok());
        }
    }
}

fn raw_exchange(stream: &mut UnixStream, reader: &mut BufReader<UnixStream>, line: &str) -> Json {
    stream.write_all(line.as_bytes()).unwrap();
    stream.write_all(b"\n").unwrap();
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    serde_json::from_str(&response).unwrap()
}

#[test]
fn wire_format() {
    let dir = tempfile::tempdir().unwrap();
    let editor = Editor::start(dir.path());
    let mut stream = UnixStream::connect(lock::socket_path(dir.path())).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());

    let r = raw_exchange(
        &mut stream,
        &mut reader,
        r#"{"id":7,"command":"entity.spawn","args":{"id":"a"},"dry_run":false}"#,
    );
    assert_eq!(r["id"], 7);
    assert_eq!(r["outcome"]["ok"], true);
    assert!(r.get("specs").is_none());

    // Any JSON id is echoed; unknown fields are ignored.
    let r = raw_exchange(
        &mut stream,
        &mut reader,
        r#"{"id":"x","command":"nope","future_field":1}"#,
    );
    assert_eq!(r["id"], "x");
    assert_eq!(r["outcome"]["ok"], false);
    assert_eq!(r["outcome"]["diagnostics"][0]["code"], "RE2004");

    let r = raw_exchange(&mut stream, &mut reader, r#"{"id":8,"list_specs":true}"#);
    assert_eq!(r["id"], 8);
    assert!(r["specs"].as_array().unwrap().len() >= 13);

    // Malformed lines get RE8006 and the connection stays usable.
    let r = raw_exchange(&mut stream, &mut reader, "not json");
    assert_eq!(r["id"], Json::Null);
    assert_eq!(r["outcome"]["diagnostics"][0]["code"], "RE8006");
    let r = raw_exchange(&mut stream, &mut reader, r#"{"id":9}"#);
    assert_eq!(r["id"], 9);
    assert_eq!(r["outcome"]["diagnostics"][0]["code"], "RE8006");
    let r = raw_exchange(&mut stream, &mut reader, r#"{"id":10,"dry_run":"yes"}"#);
    assert_eq!(r["id"], 10);
    assert_eq!(r["outcome"]["diagnostics"][0]["code"], "RE8006");

    // A request without an origin is logged as the socket's default origin.
    let state = editor.stop();
    let log = state.history.log_since_jsonl(0);
    assert!(log.contains("\"agent\":\"editor-socket\""), "{log}");
}

#[test]
fn connect_to_a_missing_socket_is_re8005() {
    let dir = tempfile::tempdir().unwrap();
    let err = remote::connect(&lock::socket_path(dir.path())).unwrap_err();
    assert_eq!(err.code.to_string(), "RE8005");
}

#[test]
fn lost_connection_is_re8005_then_re2006() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fake.sock");
    let listener = UnixListener::bind(&path).unwrap();
    // A server that hangs up after reading one line.
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
    });
    let handle = remote::connect(&path).unwrap();
    let out = handle.call("describe", json!({}), false);
    assert_eq!(out.diagnostics[0].code.to_string(), "RE8005");
    server.join().unwrap();
    let out = handle.call("describe", json!({}), false);
    assert_eq!(out.diagnostics[0].code.to_string(), "RE2006");
    assert!(handle.list_specs().is_err());
}

#[test]
fn bind_replaces_a_leftover_socket_and_restricts_it() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = lock::socket_path(dir.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    drop(UnixListener::bind(&path).unwrap());
    assert!(path.exists());
    let held = lock::acquire(dir.path(), Holder::Editor).unwrap();
    let listener = remote::bind(&held).unwrap();
    let mode = std::fs::metadata(listener.path())
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    // No temporary socket is left behind.
    let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.iter().any(|n| n.ends_with(".tmp")), "{names:?}");
}

#[test]
fn only_the_editor_lock_may_bind() {
    let dir = tempfile::tempdir().unwrap();
    let held = lock::acquire(dir.path(), Holder::headless("real do")).unwrap();
    let err = remote::bind(&held).unwrap_err();
    assert_eq!(err.code.to_string(), "RE8005");
}

#[test]
fn dropping_the_editor_lock_removes_the_socket() {
    let dir = tempfile::tempdir().unwrap();
    let held = lock::acquire(dir.path(), Holder::Editor).unwrap();
    let listener = remote::bind(&held).unwrap();
    drop(held);
    assert!(!lock::socket_path(dir.path()).exists());
    drop(listener);
}

#[test]
fn stop_ends_serve() {
    let dir = tempfile::tempdir().unwrap();
    let held = lock::acquire(dir.path(), Holder::Editor).unwrap();
    let listener = remote::bind(&held).unwrap();
    let stopper = listener.stopper();
    let (handle, _receiver) = channel();
    let server = std::thread::spawn(move || remote::serve(listener, handle));
    stopper.stop();
    assert!(server.join().unwrap().is_ok());
}

#[test]
fn open_writer_locks_a_free_project_and_proxies_to_the_editor() {
    let dir = tempfile::tempdir().unwrap();
    match remote::open_writer(dir.path(), Holder::headless("real do")).unwrap() {
        remote::Writer::Local(held) => {
            // A second headless writer is refused.
            let err = remote::open_writer(dir.path(), Holder::headless("real mcp")).unwrap_err();
            assert_eq!(err.code.to_string(), "RE8003");
            drop(held);
        }
        remote::Writer::Remote(_) => panic!("the project was free"),
    }
    let editor = Editor::start(dir.path());
    match remote::open_writer(dir.path(), Holder::headless("real do")).unwrap() {
        remote::Writer::Remote(handle) => {
            let out = handle.call("entity.spawn", json!({ "id": "via_editor" }), false);
            assert!(out.ok, "{:?}", out.diagnostics);
        }
        remote::Writer::Local(_) => panic!("the editor holds the project"),
    }
    let state = editor.stop();
    assert!(state.world.entity("via_editor").is_ok());
}

#[test]
fn connect_waits_for_an_editor_that_is_starting() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().to_path_buf();
    let held = lock::acquire(&project, Holder::Editor).unwrap();
    // The editor binds its socket a moment after taking the lock.
    let binder = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        let listener = remote::bind(&held).unwrap();
        let (handle, receiver) = channel();
        std::thread::spawn(move || remote::serve(listener, handle));
        let mut registry = CommandRegistry::<EditState>::new();
        register_builtin_commands(&mut registry);
        let mut state = EditState::new();
        let request = receiver.recv().unwrap();
        request.handle(&mut state, &registry, &AllowAll);
        held
    });
    let LockState::Editor(socket) = lock::probe(dir.path()) else {
        panic!("the editor holds the lock");
    };
    let handle = remote::connect(&socket).unwrap();
    assert!(handle.list_specs().is_ok());
    drop(binder.join().unwrap());
}

#[test]
fn too_long_socket_paths_are_reported() {
    let long = std::path::PathBuf::from(format!("/tmp/{}/.real/editor.sock", "x".repeat(120)));
    let err = remote::connect(&long).unwrap_err();
    assert_eq!(err.code.to_string(), "RE8005");
    assert!(err.message.contains("too long"), "{}", err.message);
}

#[test]
fn guarded_handle_applies_the_agent_policy_before_the_editor() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let editor = Editor::start(dir.path());
    let handle = connect_to_editor(dir.path());
    let guarded = re_agent::guard::guarded(
        handle,
        Arc::new(re_agent::Policy::read_only()),
        Origin::Agent("mcp".into()),
    );
    let refused = guarded.call("entity.spawn", json!({ "id": "x" }), false);
    assert_eq!(refused.diagnostics[0].code.to_string(), "RE8001");
    let dry = guarded.call("entity.spawn", json!({ "id": "x", "dry_run": true }), false);
    assert!(dry.ok, "{:?}", dry.diagnostics);
    assert!(guarded.list_specs().unwrap().len() >= 13);
    let unknown = guarded.call("nope", json!({}), false);
    assert_eq!(unknown.diagnostics[0].code.to_string(), "RE2004");
    let state = editor.stop();
    assert!(state.world.entity("x").is_err());
}
