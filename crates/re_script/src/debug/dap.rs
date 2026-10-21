//! The Debug Adapter Protocol over TCP (ADR-0045): `real run --debug` and the
//! editor's debugger setting listen on `127.0.0.1` only (default port
//! [`DEFAULT_PORT`]); an editor with a DAP client (VS Code with a generic
//! adapter, Neovim's nvim-dap) attaches to it.
//!
//! Supported requests: `initialize`, `attach` (and `launch`, treated alike),
//! `setBreakpoints`, `configurationDone`, `threads`, `stackTrace`, `scopes`,
//! `variables`, `continue`, `next`, `stepIn`, `stepOut`, `pause` and
//! `disconnect`. Events: `initialized`, `stopped`, `continued`, `thread`,
//! `breakpoint`, `output` and `terminated`. Threads are script contexts;
//! frame and variable references are valid while the script stays stopped.
//!
//! One client at a time is served; the server reads the [`Debugger`]'s
//! events, so a process has one server per debugger.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use re_core::Diagnostic;
use serde_json::{Value as Json, json};

use super::{DAP, Debugger, Event, Resume};

/// The port the debugger listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 4711;

/// A running DAP server; it stops when dropped.
pub struct DapServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    /// The connected client, shut down on drop so its read returns.
    client: Arc<Mutex<Option<TcpStream>>>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for DapServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DapServer")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl DapServer {
    /// Listens on `127.0.0.1:port` (`0`: any free port) for DAP clients of
    /// `debugger`. Source paths are shown under `root` (the project) and read
    /// relative to it.
    ///
    /// # Errors
    ///
    /// `RE3071` when the port cannot be listened on.
    pub fn listen(debugger: Debugger, port: u16, root: &Path) -> Result<DapServer, Diagnostic> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port)).map_err(|e| {
            Diagnostic::error(
                DAP,
                format!("the debugger cannot listen on 127.0.0.1:{port}: {e}"),
            )
            .with_hint("another process uses the port; pick another one")
        })?;
        let addr = listener
            .local_addr()
            .map_err(|e| Diagnostic::error(DAP, e.to_string()))?;
        listener
            .set_nonblocking(true)
            .map_err(|e| Diagnostic::error(DAP, e.to_string()))?;
        let stop = Arc::new(AtomicBool::new(false));
        let root = canonical(root);
        let client = Arc::new(Mutex::new(None));
        let thread = {
            let (stop, client) = (stop.clone(), client.clone());
            std::thread::Builder::new()
                .name("re_script dap".into())
                .spawn(move || accept_loop(&listener, &debugger, &root, &stop, &client))
                .map_err(|e| Diagnostic::error(DAP, e.to_string()))?
        };
        Ok(DapServer {
            addr,
            stop,
            client,
            thread: Some(thread),
        })
    }

    /// The address it listens on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for DapServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(c) = self
            .client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn accept_loop(
    listener: &TcpListener,
    debugger: &Debugger,
    root: &Path,
    stop: &AtomicBool,
    client: &Mutex<Option<TcpStream>>,
) {
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                *client.lock().unwrap_or_else(|e| e.into_inner()) = stream.try_clone().ok();
                let _ = serve(stream, debugger, root, stop);
                *client.lock().unwrap_or_else(|e| e.into_inner()) = None;
                // The client left: nothing waits for stops any more.
                debugger.set_client(false);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

/// Writes protocol messages, numbering them.
struct Out {
    stream: TcpStream,
    seq: i64,
}

impl Out {
    fn send(&mut self, mut message: Json) -> std::io::Result<()> {
        self.seq += 1;
        message["seq"] = json!(self.seq);
        let body = serde_json::to_vec(&message).expect("JSON always serializes");
        write!(self.stream, "Content-Length: {}\r\n\r\n", body.len())?;
        self.stream.write_all(&body)?;
        self.stream.flush()
    }

    fn event(&mut self, event: &str, body: Json) -> std::io::Result<()> {
        self.send(json!({ "type": "event", "event": event, "body": body }))
    }
}

/// Reads one message; `None` at the end of the stream.
fn read_message(reader: &mut impl BufRead) -> std::io::Result<Option<Json>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = length.filter(|n| *n <= 16 << 20) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a message without a valid Content-Length",
        ));
    };
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

fn serve(
    stream: TcpStream,
    debugger: &Debugger,
    root: &Path,
    stop: &AtomicBool,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_nodelay(true)?;
    let out = Arc::new(Mutex::new(Out {
        stream: stream.try_clone()?,
        seq: 0,
    }));
    let done = Arc::new(AtomicBool::new(false));
    // The files this client set breakpoints in: disconnecting clears those,
    // not the ones the editor's panel set.
    let mut sources = BTreeSet::new();
    let mut events = None;
    let mut reader = BufReader::new(stream);
    let result = (|| {
        while !stop.load(Ordering::Relaxed) && !done.load(Ordering::Relaxed) {
            let Some(request) = read_message(&mut reader)? else {
                break;
            };
            if request["type"] != "request" {
                continue;
            }
            let command = request["command"].as_str().unwrap_or_default().to_string();
            // Holding the writer while handling keeps a request's events
            // (`continued`, a quick `stopped`) behind its response.
            let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
            // What happened before the client came is old news, except a
            // script stopped now; both are read at once, before `initialize`
            // is handled, so no event falls between them and the event thread.
            let current = (command == "initialize" && events.is_none())
                .then(|| debugger.take_events_and_stop().1);
            let (body, error, after) = handle(
                &command,
                &request["arguments"],
                debugger,
                root,
                &mut sources,
            );
            let mut response = json!({
                "type": "response",
                "request_seq": request["seq"],
                "command": command,
                "success": error.is_none(),
                "body": body,
            });
            if let Some(message) = error {
                response["message"] = json!(message);
            }
            o.send(response)?;
            match after {
                After::Nothing => {}
                After::Initialized => {
                    o.event("initialized", json!({}))?;
                    if let Some(current) = current {
                        if let Some(s) = current {
                            let stopped = Event::Stopped {
                                context: s.context,
                                reason: s.reason,
                            };
                            send_event(&mut o, &stopped, root)?;
                        }
                        events = Some(spawn_events(&out, &done, debugger, root)?);
                    }
                }
                After::Disconnect => {
                    o.event("terminated", json!({}))?;
                    break;
                }
            }
        }
        Ok(())
    })();
    done.store(true, Ordering::Relaxed);
    if let Some(events) = events {
        let _ = events.join();
    }
    // A client that went away without `disconnect` leaves no breakpoints.
    for s in sources {
        debugger.set_breakpoints(&s, &[]);
    }
    result
}

/// Sends the debugger's events to the client as they happen, from the
/// `initialize` response on (the queue was emptied then).
fn spawn_events(
    out: &Arc<Mutex<Out>>,
    done: &Arc<AtomicBool>,
    debugger: &Debugger,
    root: &Path,
) -> std::io::Result<JoinHandle<()>> {
    let (out, done, debugger, root) = (
        out.clone(),
        done.clone(),
        debugger.clone(),
        root.to_path_buf(),
    );
    std::thread::Builder::new()
        .name("re_script dap events".into())
        .spawn(move || {
            while !done.load(Ordering::Relaxed) {
                for e in debugger.wait_events(Duration::from_millis(50)) {
                    let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
                    if send_event(&mut out, &e, &root).is_err() {
                        done.store(true, Ordering::Relaxed);
                    }
                }
            }
        })
}

enum After {
    Nothing,
    Initialized,
    Disconnect,
}

/// `path` made absolute and free of symbolic links, without Windows'
/// verbatim prefix (`\\?\C:\…`), which clients do not map to their files.
fn canonical(path: &Path) -> PathBuf {
    let p = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = p.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) => PathBuf::from(rest),
        None => p,
    }
}

/// The source path a client means, relative to the project.
fn relative(root: &Path, path: &str) -> String {
    let p = canonical(Path::new(path));
    let rel = p.strip_prefix(root).map(Path::to_path_buf).unwrap_or(p);
    rel.to_string_lossy().replace('\\', "/")
}

fn source(root: &Path, rel: &str) -> Json {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    // A file outside the project keeps the absolute path it was given.
    let path = if Path::new(rel).is_absolute() {
        PathBuf::from(rel)
    } else {
        rel.split('/')
            .fold(root.to_path_buf(), |p, part| p.join(part))
    };
    json!({ "name": name, "path": path.to_string_lossy() })
}

fn reference(index: Option<usize>) -> usize {
    index.map_or(0, |i| i + 1)
}

fn handle(
    command: &str,
    args: &Json,
    debugger: &Debugger,
    root: &Path,
    sources: &mut BTreeSet<String>,
) -> (Json, Option<String>, After) {
    let ok = |body: Json| (body, None, After::Nothing);
    let thread_resume = |how: Resume| {
        if debugger.resume(how) {
            (json!({ "allThreadsContinued": true }), None, After::Nothing)
        } else {
            (
                json!({}),
                Some("no script is stopped".to_string()),
                After::Nothing,
            )
        }
    };
    match command {
        "initialize" => (
            json!({
                "supportsConfigurationDoneRequest": true,
                "supportsConditionalBreakpoints": false,
                "supportsEvaluateForHovers": false,
                "supportTerminateDebuggee": false,
            }),
            None,
            After::Initialized,
        ),
        "attach" | "launch" => {
            debugger.set_client(true);
            ok(json!({}))
        }
        "configurationDone" => ok(json!({})),
        "setBreakpoints" => {
            let Some(path) = args["source"]["path"].as_str() else {
                return (
                    json!({}),
                    Some("setBreakpoints needs source.path".into()),
                    After::Nothing,
                );
            };
            let rel = relative(root, path);
            let lines: Vec<u32> = args["breakpoints"]
                .as_array()
                .map(|bs| {
                    bs.iter()
                        .filter_map(|b| b["line"].as_u64())
                        .map(|l| l as u32)
                        .collect()
                })
                .or_else(|| {
                    args["lines"].as_array().map(|ls| {
                        ls.iter()
                            .filter_map(Json::as_u64)
                            .map(|l| l as u32)
                            .collect()
                    })
                })
                .unwrap_or_default();
            sources.insert(rel.clone());
            let bps = debugger.set_breakpoints(&rel, &lines);
            ok(json!({
                "breakpoints": bps.iter().map(|b| json!({
                    "id": b.id,
                    "verified": false,
                    "line": b.requested,
                    "source": source(root, &b.source),
                    "message": "placed when the script is loaded",
                })).collect::<Vec<_>>()
            }))
        }
        "threads" => ok(json!({
            "threads": debugger.contexts().iter().map(|c| json!({ "id": c.id, "name": c.name })).collect::<Vec<_>>()
        })),
        "stackTrace" => {
            let thread = args["threadId"].as_u64().unwrap_or(0) as u32;
            let frames = match debugger.stopped() {
                Some(s) if s.context == thread => s
                    .frames
                    .iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let mut frame = json!({
                            "id": i + 1,
                            "name": f.name,
                            "line": f.line.unwrap_or(0),
                            "column": if f.line.is_some() { 1 } else { 0 },
                        });
                        match &f.source {
                            Some(src) => frame["source"] = source(root, src),
                            None => frame["presentationHint"] = json!("subtle"),
                        }
                        frame
                    })
                    .collect::<Vec<_>>(),
                _ => Vec::new(),
            };
            ok(json!({ "stackFrames": frames, "totalFrames": frames.len() }))
        }
        "scopes" => {
            let frame = args["frameId"].as_u64().unwrap_or(0) as usize;
            let scopes = debugger
                .stopped()
                .and_then(|s| s.frames.get(frame.wrapping_sub(1)).cloned())
                .map(|f| {
                    f.scopes
                        .iter()
                        .map(|sc| json!({ "name": sc.name, "variablesReference": sc.variables + 1, "expensive": false }))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            ok(json!({ "scopes": scopes }))
        }
        "variables" => {
            let r = args["variablesReference"].as_u64().unwrap_or(0) as usize;
            let vars = debugger
                .stopped()
                .map(|s| {
                    s.list(r.wrapping_sub(1))
                        .iter()
                        .map(|v| {
                            json!({
                                "name": v.name,
                                "value": v.value,
                                "type": v.type_name,
                                "variablesReference": reference(v.children),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            ok(json!({ "variables": vars }))
        }
        "continue" => thread_resume(Resume::Continue),
        "next" => thread_resume(Resume::StepOver),
        "stepIn" => thread_resume(Resume::StepIn),
        "stepOut" => thread_resume(Resume::StepOut),
        "pause" => {
            debugger.pause();
            ok(json!({}))
        }
        "disconnect" => {
            for s in std::mem::take(sources) {
                debugger.set_breakpoints(&s, &[]);
            }
            debugger.set_client(false);
            (json!({}), None, After::Disconnect)
        }
        other => (
            json!({}),
            Some(format!("`{other}` is not supported by this debugger")),
            After::Nothing,
        ),
    }
}

fn send_event(out: &mut Out, e: &Event, root: &Path) -> std::io::Result<()> {
    match e {
        Event::ContextStarted(c) => {
            out.event("thread", json!({ "reason": "started", "threadId": c.id }))
        }
        Event::ContextExited(id) => {
            out.event("thread", json!({ "reason": "exited", "threadId": id }))
        }
        Event::Stopped { context, reason } => out.event(
            "stopped",
            json!({ "reason": reason.dap_name(), "threadId": context, "allThreadsStopped": true }),
        ),
        Event::Continued { context } => out.event(
            "continued",
            json!({ "threadId": context, "allThreadsContinued": true }),
        ),
        Event::BreakpointChanged(b) => out.event(
            "breakpoint",
            json!({
                "reason": "changed",
                "breakpoint": {
                    "id": b.id,
                    "verified": b.line.is_some(),
                    "line": b.line.unwrap_or(b.requested),
                    "source": source(root, &b.source),
                },
            }),
        ),
        Event::Diagnostic(d) => out.event(
            "output",
            json!({ "category": "console", "output": d.render_human() }),
        ),
    }
}

#[cfg(test)]
pub(super) fn read_message_for_tests(reader: &mut impl BufRead) -> std::io::Result<Option<Json>> {
    read_message(reader)
}
