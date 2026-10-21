//! Debugger tests on real Luau (the fixtures of the WP25 spike). Each test
//! name contains `debug`, so `cargo test -p re_script debug` runs them all.

use std::io::{BufReader, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use mlua::thread::ThreadStatus;
use mlua::{Function, Lua, Thread};
use serde_json::{Value as Json, json};

use super::*;

/// A fixture: `step(n)` adds up, `run()` calls it three times.
const SCRIPT: &str = r#"local function step(a)
    local b = a * 2
    -- a comment, no code
    local c = b + 1
    return c
end

function run()
    local total = 0
    for i = 1, 3 do
        total = total + step(i)
    end
    local label = "done"
    return total, label
end
"#;

fn load(lua: &Lua, path: &str, source: &str) -> Function {
    let bytecode = mlua::chunk::Compiler::new()
        .set_optimization_level(1)
        .set_debug_level(2)
        .compile(source)
        .unwrap();
    lua.load(&bytecode[..])
        .set_name(format!("@{path}"))
        .into_function()
        .unwrap()
}

/// A VM with the debugger attached and the fixture loaded and registered.
fn vm(debugger: &Debugger, parking: Option<Box<dyn Parking>>) -> (Lua, ContextInfo) {
    let lua = Lua::new();
    let info = debugger.attach(&lua, "game", parking).unwrap();
    let chunk = load(&lua, "scripts/fixture.luau", SCRIPT);
    register_chunk(&lua, "scripts/fixture.luau", &chunk);
    chunk.call::<()>(()).unwrap();
    (lua, info)
}

/// Runs a client on another thread: for each stop, `on_stop` sees the
/// snapshot and says how to resume (`None`: stop the client and continue).
fn client(
    debugger: &Debugger,
    mut on_stop: impl FnMut(&Stopped) -> Option<Resume> + Send + 'static,
) -> std::thread::JoinHandle<Vec<Stopped>> {
    debugger.set_client(true);
    let d = debugger.clone();
    std::thread::spawn(move || {
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            for e in d.wait_events(Duration::from_millis(20)) {
                if let Event::Stopped { .. } = e {
                    let s = d.stopped().expect("a stop");
                    let how = on_stop(&s);
                    seen.push(s);
                    match how {
                        Some(how) => assert!(d.resume(how)),
                        None => {
                            d.set_client(false);
                            return seen;
                        }
                    }
                }
            }
        }
        panic!("the client timed out after {} stops", seen.len());
    })
}

fn line(s: &Stopped) -> u32 {
    s.top().and_then(|f| f.line).unwrap()
}

#[test]
fn debug_breakpoint_stops_on_straight_line_code_with_locals() {
    let debugger = Debugger::new();
    let (lua, _) = vm(&debugger, None);
    debugger.set_breakpoints("scripts/fixture.luau", &[4]);
    service(&lua);
    let mut n = 0;
    let c = client(&debugger, move |_| {
        n += 1;
        if n == 3 { None } else { Some(Resume::Continue) }
    });
    let (total, label): (i64, String) = lua
        .globals()
        .get::<Function>("run")
        .unwrap()
        .call(())
        .unwrap();
    assert_eq!((total, label.as_str()), (15, "done"));
    let stops = c.join().unwrap();
    assert_eq!(stops.len(), 3, "one stop per call");
    for (i, s) in stops.iter().enumerate() {
        assert_eq!(s.reason, StopReason::Breakpoint);
        assert_eq!(line(s), 4);
        let a = (i + 1) as i64;
        assert_eq!(s.local("a").unwrap().value, a.to_string());
        assert_eq!(s.local("b").unwrap().value, (a * 2).to_string());
        // The caller's frame, with its loop variable.
        let caller = &s.frames[1];
        assert_eq!((caller.name.as_str(), caller.line), ("run", Some(11)));
        let locals = s.list(caller.scopes[0].variables);
        assert!(
            locals
                .iter()
                .any(|v| v.name == "i" && v.value == (i + 1).to_string()),
            "{locals:?}"
        );
        assert_eq!(
            s.frames.last().unwrap().source.as_deref(),
            Some("scripts/fixture.luau")
        );
    }
}

#[test]
fn debug_breakpoints_snap_to_the_next_line_with_code() {
    let debugger = Debugger::new();
    let (lua, _) = vm(&debugger, None);
    debugger.take_events();
    let set = debugger.set_breakpoints("scripts/fixture.luau", &[3, 7, 400]);
    service(&lua);
    let placed: Vec<(u32, Option<u32>)> = debugger
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            Event::BreakpointChanged(b) => Some((b.requested, b.line)),
            _ => None,
        })
        .collect();
    assert_eq!(
        placed,
        vec![(3, Some(4)), (7, Some(8))],
        "line 400 has no code and stays unplaced"
    );
    assert_eq!(set.len(), 3);
    let lines: Vec<_> = debugger.breakpoints().iter().map(|b| b.line).collect();
    assert_eq!(lines, vec![Some(4), Some(8), None]);
    // A breakpoint set before its file is loaded is placed when it is.
    let lua2 = Lua::new();
    debugger.attach(&lua2, "tools", None).unwrap();
    debugger.set_breakpoints("scripts/other.luau", &[2]);
    service(&lua2);
    debugger.take_events();
    let chunk = load(
        &lua2,
        "scripts/other.luau",
        "local x = 1\nlocal y = x + 1\nreturn y\n",
    );
    register_chunk(&lua2, "scripts/other.luau", &chunk);
    let events = debugger.take_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::BreakpointChanged(b) if b.line == Some(2))),
        "{events:?}"
    );
}

#[test]
fn debug_step_over_stays_in_the_frame() {
    let debugger = Debugger::new();
    let (lua, _) = vm(&debugger, None);
    debugger.set_breakpoints("scripts/fixture.luau", &[9]);
    service(&lua);
    let c = client(&debugger, |s| {
        if line(s) == 14 {
            None
        } else {
            Some(Resume::StepOver)
        }
    });
    lua.globals()
        .get::<Function>("run")
        .unwrap()
        .call::<()>(())
        .unwrap();
    let lines: Vec<u32> = c.join().unwrap().iter().map(line).collect();
    // The loop header runs once (its back edge is on the body's line);
    // `step` is never entered; line 13's constant, folded away at debug
    // level 1, is kept at level 2.
    assert_eq!(lines, vec![9, 10, 11, 11, 11, 13, 14]);
}

#[test]
fn debug_step_in_enters_the_called_function_and_step_out_leaves_it() {
    let debugger = Debugger::new();
    let (lua, _) = vm(&debugger, None);
    debugger.set_breakpoints("scripts/fixture.luau", &[11]);
    service(&lua);
    let mut n = 0;
    let c = client(&debugger, move |_| {
        n += 1;
        match n {
            1 => Some(Resume::StepIn),
            2 => Some(Resume::StepOut),
            _ => None,
        }
    });
    lua.globals()
        .get::<Function>("run")
        .unwrap()
        .call::<()>(())
        .unwrap();
    let stops = c.join().unwrap();
    let at: Vec<(String, u32)> = stops
        .iter()
        .map(|s| (s.top().unwrap().name.clone(), line(s)))
        .collect();
    assert_eq!(
        at,
        vec![("run".into(), 11), ("step".into(), 2), ("run".into(), 11)],
        "into step, then back out to the caller's line"
    );
    assert_eq!(stops[1].reason, StopReason::Step);
}

#[test]
fn debug_without_a_client_the_stop_is_recorded_and_the_script_continues() {
    let debugger = Debugger::new();
    let (lua, _) = vm(&debugger, None);
    debugger.set_breakpoints("scripts/fixture.luau", &[5]);
    service(&lua);
    debugger.take_events();
    let (total, _): (i64, String) = lua
        .globals()
        .get::<Function>("run")
        .unwrap()
        .call(())
        .unwrap();
    assert_eq!(total, 15, "the script ran to the end");
    let last = debugger.last_unpaused().expect("the stop was recorded");
    assert_eq!(line(&last), 5);
    assert_eq!(
        last.local("c").unwrap().value,
        "7",
        "the last of the three calls"
    );
    let warnings: Vec<_> = debugger
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            Event::Diagnostic(d) => Some(d),
            _ => None,
        })
        .collect();
    assert_eq!(warnings.len(), 3);
    assert_eq!(warnings[0].code, NOT_PAUSED);
    assert!(debugger.stopped().is_none());
}

/// Parks the threads the test created; a parked thread is resumed by hand.
struct TestParking {
    threads: std::rc::Rc<std::cell::RefCell<Vec<Thread>>>,
}

impl Parking for TestParking {
    fn can_park(&self, _lua: &Lua, thread: usize) -> bool {
        self.threads
            .borrow()
            .iter()
            .any(|t| t.to_pointer() as usize == thread)
    }

    fn resume(&self, _lua: &Lua, thread: usize) {
        let t = self
            .threads
            .borrow()
            .iter()
            .find(|t| t.to_pointer() as usize == thread)
            .cloned();
        if let Some(t) = t {
            t.resume::<()>(()).unwrap();
        }
    }
}

#[test]
fn debug_a_parked_thread_waits_while_the_vm_keeps_running() {
    let debugger = Debugger::new();
    let threads = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let (lua, info) = vm(
        &debugger,
        Some(Box::new(TestParking {
            threads: threads.clone(),
        })),
    );
    debugger.set_breakpoints("scripts/fixture.luau", &[4]);
    service(&lua);
    let run: Function = lua.globals().get("run").unwrap();
    let thread = lua.create_thread(run).unwrap();
    threads.borrow_mut().push(thread.clone());
    let p = thread.to_pointer() as usize;

    // The first resume stops at the breakpoint and yields.
    thread.resume::<()>(()).unwrap();
    assert_eq!(thread.status(), ThreadStatus::Resumable);
    assert!(is_parked(&lua, p));
    let s = debugger.stopped().unwrap();
    assert_eq!((s.context, line(&s)), (info.id, 4));
    assert_eq!(s.local("a").unwrap().value, "1");
    // The VM runs other code meanwhile.
    assert_eq!(lua.load("return 1 + 1").eval::<i64>().unwrap(), 2);
    // Nothing happens until a command arrives.
    service(&lua);
    assert!(is_parked(&lua, p));

    // Step over: service resumes the thread, which stops at line 5.
    assert!(debugger.resume(Resume::StepOver));
    service(&lua);
    let s = debugger.stopped().unwrap();
    assert_eq!(line(&s), 5);
    assert_eq!(s.local("c").unwrap().value, "3");
    // Step into from a parked stop single-steps: the next line is the
    // caller's after `step` returns.
    assert!(debugger.resume(Resume::StepIn));
    service(&lua);
    let s = debugger.stopped().unwrap();
    assert_eq!((s.top().unwrap().name.as_str(), line(&s)), ("run", 11));

    // Continue: the next pass stops at line 4 again, then clear and finish.
    assert!(debugger.resume(Resume::Continue));
    service(&lua);
    assert_eq!(debugger.stopped().unwrap().local("a").unwrap().value, "2");
    debugger.set_breakpoints("scripts/fixture.luau", &[]);
    assert!(debugger.resume(Resume::Continue));
    service(&lua);
    assert!(debugger.stopped().is_none());
    assert_eq!(thread.status(), ThreadStatus::Finished);
}

#[test]
fn debug_values_are_read_raw() {
    let debugger = Debugger::new();
    let lua = Lua::new();
    debugger.attach(&lua, "game", None).unwrap();
    lua.globals()
        .set("probe", lua.create_function(|_, ()| Ok(())).unwrap())
        .unwrap();
    let source = r#"
local calls = 0
local meta = { __tostring = function() calls += 1; return "never" end,
               __index = function() calls += 1; return 0 end }
local t = setmetatable({ 1, "two", nested = { deep = { deeper = { deepest = {} } } } }, meta)
t.self = t
local v = vector.create(1, 2.5, -3)
local s = string.rep("x", 300)
local f = function() end
local function g() return calls end
local done = true
return g
"#;
    let chunk = load(&lua, "scripts/values.luau", source);
    register_chunk(&lua, "scripts/values.luau", &chunk);
    debugger.set_breakpoints("scripts/values.luau", &[11]);
    service(&lua);
    let g: Function = chunk.call(()).unwrap();
    assert_eq!(g.call::<i64>(()).unwrap(), 0, "no metamethod ran");
    let s = debugger.last_unpaused().unwrap();
    let t = s.local("t").unwrap();
    assert_eq!(t.type_name, "table");
    let entries = s.list(t.children.unwrap());
    let get = |name: &str| {
        entries
            .iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("{name}: {entries:?}"))
    };
    assert_eq!(get("[1]").value, "1");
    assert_eq!(get("[2]").value, "\"two\"");
    assert_eq!(get("self").children, t.children, "a cycle is read once");
    let nested = s.list(get("nested").children.unwrap());
    let deep = s.list(nested[0].children.unwrap());
    let deeper = &deep[0];
    assert_eq!(deeper.name, "deeper");
    assert_eq!(deeper.children, None, "expansion stops at depth 3");
    assert_eq!(s.local("v").unwrap().value, "vector(1, 2.5, -3)");
    let text = &s.local("s").unwrap().value;
    assert!(text.ends_with('…') && text.len() < 220, "{text}");
    assert!(
        s.local("f").unwrap().value.starts_with("function"),
        "{:?}",
        s.local("f")
    );
    assert_eq!(s.local("calls").unwrap().value, "0");
}

#[test]
fn debug_pause_stops_at_the_next_line_run() {
    let debugger = Debugger::new();
    let (lua, _) = vm(&debugger, None);
    debugger.pause();
    service(&lua);
    let c = client(&debugger, |_| None);
    lua.globals()
        .get::<Function>("run")
        .unwrap()
        .call::<()>(())
        .unwrap();
    let stops = c.join().unwrap();
    assert_eq!(stops.len(), 1);
    assert_eq!((stops[0].reason, line(&stops[0])), (StopReason::Pause, 9));
    // The pause is spent: the next run does not stop.
    lua.globals()
        .get::<Function>("run")
        .unwrap()
        .call::<()>(())
        .unwrap();
    assert!(debugger.last_unpaused().is_none());
}

#[test]
fn debug_a_synchronous_pause_does_not_count_against_the_budget() {
    let debugger = Debugger::new();
    let (lua, _) = vm(&debugger, None);
    crate::budget::install(&lua).unwrap();
    debugger.set_breakpoints("scripts/fixture.luau", &[4]);
    service(&lua);
    let mut n = 0;
    let c = client(&debugger, move |_| {
        std::thread::sleep(Duration::from_millis(60));
        n += 1;
        if n == 3 { None } else { Some(Resume::Continue) }
    });
    let saved = crate::budget::arm(Instant::now() + Duration::from_millis(40));
    let result = lua
        .globals()
        .get::<Function>("run")
        .unwrap()
        .call::<(i64, String)>(());
    crate::budget::restore(saved);
    c.join().unwrap();
    assert_eq!(result.unwrap().0, 15, "180 ms paused under a 40 ms budget");
}

#[test]
fn debug_detach_removes_breakpoints_and_the_context() {
    let debugger = Debugger::new();
    let (lua, info) = vm(&debugger, None);
    debugger.set_breakpoints("scripts/fixture.luau", &[4]);
    service(&lua);
    debugger.detach(&lua);
    assert!(debugger.contexts().is_empty());
    assert!(
        debugger
            .take_events()
            .contains(&Event::ContextExited(info.id))
    );
    lua.globals()
        .get::<Function>("run")
        .unwrap()
        .call::<()>(())
        .unwrap();
    assert!(
        debugger.last_unpaused().is_none(),
        "no breakpoint is left in the bytecode"
    );
}

// ---- DAP --------------------------------------------------------------

struct Client {
    stream: TcpStream,
    reader: BufReader<TcpStream>,
    seq: i64,
    /// Events read while waiting for a response.
    events: Vec<Json>,
}

impl Client {
    fn connect(addr: std::net::SocketAddr) -> Client {
        let stream = TcpStream::connect(addr).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        Client {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
            seq: 0,
            events: Vec::new(),
        }
    }

    fn request(&mut self, command: &str, arguments: Json) -> Json {
        self.seq += 1;
        let body = serde_json::to_vec(&json!({
            "seq": self.seq, "type": "request", "command": command, "arguments": arguments
        }))
        .unwrap();
        write!(self.stream, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
        self.stream.write_all(&body).unwrap();
        loop {
            let m = dap::read_message_for_tests(&mut self.reader)
                .unwrap()
                .expect("a message");
            if m["type"] == "response" && m["request_seq"] == self.seq {
                assert_eq!(m["command"], command);
                return m;
            }
            self.events.push(m);
        }
    }

    fn event(&mut self, name: &str) -> Json {
        if let Some(i) = self.events.iter().position(|e| e["event"] == name) {
            return self.events.remove(i);
        }
        loop {
            let m = dap::read_message_for_tests(&mut self.reader)
                .unwrap()
                .expect("a message");
            if m["type"] == "event" && m["event"] == name {
                return m;
            }
            self.events.push(m);
        }
    }
}

#[test]
fn debug_a_dap_client_sets_breakpoints_inspects_and_steps() {
    let dir = tempfile::tempdir().unwrap();
    let debugger = Debugger::new();
    let server = dap::DapServer::listen(debugger.clone(), 0, dir.path()).unwrap();
    let (lua, info) = vm(&debugger, None);
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let file = root.join("scripts/fixture.luau");

    let (tx, rx) = mpsc::channel();
    let addr = server.addr();
    let file2 = file.clone();
    let client = std::thread::spawn(move || {
        let mut c = Client::connect(addr);
        let r = c.request("initialize", json!({ "adapterID": "real" }));
        assert_eq!(r["success"], true, "{r}");
        c.event("initialized");
        assert_eq!(c.request("attach", json!({}))["success"], true);
        let r = c.request(
            "setBreakpoints",
            json!({ "source": { "path": file2 }, "breakpoints": [{ "line": 3 }] }),
        );
        assert_eq!(r["body"]["breakpoints"][0]["line"], 3);
        assert_eq!(c.request("configurationDone", json!({}))["success"], true);
        tx.send(()).unwrap();
        // Placed on line 4, the next line with code.
        let e = c.event("breakpoint");
        assert_eq!(
            (
                e["body"]["breakpoint"]["line"].clone(),
                e["body"]["breakpoint"]["verified"].clone()
            ),
            (json!(4), json!(true))
        );
        let e = c.event("stopped");
        assert_eq!(e["body"]["reason"], "breakpoint");
        let thread = e["body"]["threadId"].clone();
        let threads = c.request("threads", json!({}));
        assert_eq!(threads["body"]["threads"][0]["name"], "game");
        let st = c.request("stackTrace", json!({ "threadId": thread }));
        let top = &st["body"]["stackFrames"][0];
        assert_eq!(
            (top["name"].clone(), top["line"].clone()),
            (json!("step"), json!(4))
        );
        assert!(
            top["source"]["path"]
                .as_str()
                .unwrap()
                .ends_with("fixture.luau")
        );
        let scopes = c.request("scopes", json!({ "frameId": top["id"] }));
        assert_eq!(scopes["body"]["scopes"][0]["name"], "Locals");
        let vars = c.request(
            "variables",
            json!({ "variablesReference": scopes["body"]["scopes"][0]["variablesReference"] }),
        );
        let names: Vec<_> = vars["body"]["variables"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["name"].clone())
            .collect();
        assert_eq!(names, vec![json!("a"), json!("b")]);
        assert_eq!(
            c.request("next", json!({ "threadId": thread }))["success"],
            true
        );
        c.event("continued");
        let e = c.event("stopped");
        assert_eq!(e["body"]["reason"], "step");
        let st = c.request("stackTrace", json!({ "threadId": thread }));
        assert_eq!(st["body"]["stackFrames"][0]["line"], 5);
        // Disconnecting clears the breakpoints and lets the script finish.
        let r = c.request("disconnect", json!({}));
        assert_eq!(r["success"], true);
        c.event("terminated");
        thread
    });

    rx.recv_timeout(Duration::from_secs(20)).unwrap();
    // The breakpoints reach the VM between frames.
    let deadline = Instant::now() + Duration::from_secs(20);
    while debugger
        .breakpoints()
        .first()
        .and_then(|b| b.line)
        .is_none()
    {
        service(&lua);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    let (total, _): (i64, String) = lua
        .globals()
        .get::<Function>("run")
        .unwrap()
        .call(())
        .unwrap();
    assert_eq!(total, 15);
    assert_eq!(client.join().unwrap(), json!(info.id));
    service(&lua);
    assert!(debugger.breakpoints().is_empty());
    drop(server);
}

#[test]
fn debug_a_dap_server_refuses_a_port_in_use() {
    let dir = tempfile::tempdir().unwrap();
    let first = dap::DapServer::listen(Debugger::new(), 0, dir.path()).unwrap();
    let e = dap::DapServer::listen(Debugger::new(), first.addr().port(), dir.path()).unwrap_err();
    assert_eq!(e.code, DAP);
}

#[test]
fn debug_a_dap_disconnect_keeps_the_editors_breakpoints_and_drop_does_not_wait_for_the_client() {
    let dir = tempfile::tempdir().unwrap();
    let debugger = Debugger::new();
    debugger.set_breakpoints("scripts/editor.luau", &[2]);
    let server = dap::DapServer::listen(debugger.clone(), 0, dir.path()).unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let mut c = Client::connect(server.addr());
    assert_eq!(c.request("initialize", json!({}))["success"], true);
    c.event("initialized");
    c.request(
        "setBreakpoints",
        json!({ "source": { "path": root.join("scripts/dap.luau") }, "breakpoints": [{ "line": 1 }] }),
    );
    assert_eq!(debugger.breakpoints().len(), 2);
    c.request("disconnect", json!({}));
    let sources: Vec<_> = debugger
        .breakpoints()
        .into_iter()
        .map(|b| b.source)
        .collect();
    assert_eq!(sources, vec!["scripts/editor.luau".to_string()]);

    // A second client connects and sits idle: dropping the server still returns.
    let mut idle = Client::connect(server.addr());
    assert_eq!(idle.request("initialize", json!({}))["success"], true);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        drop(server);
        tx.send(()).unwrap();
    });
    rx.recv_timeout(Duration::from_secs(10))
        .expect("dropping the server returns with a client connected");
}
