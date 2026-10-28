//! The editor socket (ADR-0020, ADR-0026): how other writers send commands to
//! a running editor, so that a project has one writer.
//!
//! The editor holds the project lock ([`crate::lock`]), [`bind`]s
//! `.real/editor.sock` and runs [`serve`]. Other writers call
//! [`open_writer`]: it takes the lock when the project is free, and
//! otherwise [`connect`]s to the editor. The [`EngineHandle`] a connection
//! gives works like a local one, and every call is applied by the editor.
//!
//! # Protocol
//!
//! Newline-delimited JSON over a Unix socket, one request and one response
//! per line, answered in order. This is a frozen contract (docs/M1_PLAN.md,
//! "Cross-WP contracts").
//!
//! ```text
//! → {"id":1,"command":"entity.spawn","args":{"name":"Rock"},"dry_run":false,"origin":{"agent":"mcp"}}
//! ← {"id":1,"outcome":{"ok":true,"changes":[...],"diagnostics":[],"data":{...}}}
//! → {"id":2,"list_specs":true}
//! ← {"id":2,"specs":[{"name":"describe",...},...]}
//! ```
//!
//! * `id`: any JSON value, echoed in the response.
//! * `command`, `args` (default `null`) and `dry_run` (default `false`): the
//!   call, as [`EngineHandle::call`] takes it. The answer is `outcome`, a
//!   [`CommandOutcome`].
//! * `origin` (optional, default [`Origin::Agent`]`("editor-socket")`): who
//!   issued the call, written to the editor's log.
//! * `list_specs: true` instead of `command`: the answer is `specs`, every
//!   command's [`CommandSpec`].
//! * Unknown fields are ignored. A line that is not a valid request gets an
//!   `outcome` with `RE8006` (and `id: null` if no id could be read).
//!
//! The policy is applied where the world is: the editor's engine thread
//! checks every call against the editor's own policy. **A client that needs
//! a stricter policy (`real mcp` with the agent policy) must check it before
//! sending**, and should give its handle an [`Origin::Agent`] origin with
//! [`EngineHandle::with_origin`]; a connected handle starts as
//! [`Origin::User`].
//!
//! Unix only for now. On Windows, [`bind`] and [`connect`] return `RE8004`
//! until the named pipe of ADR-0020's Windows amendment.

use std::path::Path;

use re_command::{CommandOutcome, CommandSpec, EngineHandle, Origin};
use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::{codes, lock};

/// The origin given to socket calls that don't name one.
pub const DEFAULT_ORIGIN: &str = "editor-socket";

/// One request line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Echoed in the response.
    #[serde(default)]
    pub id: Json,
    /// The command to call (absent for `list_specs`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The command's arguments.
    #[serde(default)]
    pub args: Json,
    /// Run the command as a dry run.
    #[serde(default)]
    pub dry_run: bool,
    /// Who issued the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
    /// Ask for every command's spec instead of calling one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub list_specs: bool,
}

/// One response line.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// The request's id.
    #[serde(default)]
    pub id: Json,
    /// The answer to a command call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<CommandOutcome>,
    /// The answer to `list_specs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub specs: Option<Vec<CommandSpec>>,
}

fn protocol_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::SOCKET_PROTOCOL, message).with_hint(
        "each line must be one JSON request: {\"id\", \"command\", \"args\", \"dry_run\"}",
    )
}

#[cfg_attr(not(unix), allow(dead_code))]
fn socket_error(socket: &Path, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::SOCKET_IO, message)
        .at(Location::file(socket.display().to_string()))
        .with_hint(
            "check that the editor is running for this project (`real editor`); if it was \
             closed, run the command again and it will open the project itself",
        )
}

/// Answers one request line through `handle` (the server side, shared by
/// every platform's loop).
pub fn answer(line: &str, handle: &EngineHandle) -> Response {
    let request: Request = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            let id = serde_json::from_str::<Json>(line)
                .ok()
                .and_then(|v| v.get("id").cloned())
                .unwrap_or(Json::Null);
            return Response {
                id,
                outcome: Some(CommandOutcome::error(protocol_error(format!(
                    "invalid editor-socket request: {e}"
                )))),
                specs: None,
            };
        }
    };
    let id = request.id;
    if request.list_specs {
        return match handle.list_specs() {
            Ok(specs) => Response {
                id,
                outcome: None,
                specs: Some(specs),
            },
            Err(d) => Response {
                id,
                outcome: Some(CommandOutcome::error(d)),
                specs: None,
            },
        };
    }
    let Some(command) = request.command else {
        return Response {
            id,
            outcome: Some(CommandOutcome::error(protocol_error(
                "invalid editor-socket request: it has neither `command` nor `list_specs`",
            ))),
            specs: None,
        };
    };
    let origin = request
        .origin
        .unwrap_or_else(|| Origin::Agent(DEFAULT_ORIGIN.to_string()));
    let outcome = handle
        .with_origin(origin)
        .call(&command, request.args, request.dry_run);
    Response {
        id,
        outcome: Some(outcome),
        specs: None,
    }
}

#[cfg(unix)]
mod imp {
    use std::io::{BufRead, BufReader, ErrorKind, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use re_command::{CommandOutcome, EngineHandle, EngineReceiver, EngineRequest, channel};
    use re_core::time::Instant;
    use re_core::{Diagnostic, Location};
    use serde_json::Value as Json;

    use super::{Request, Response, answer, protocol_error, socket_error};
    use crate::codes;
    use crate::lock::{self, Holder, ProjectLock};

    /// Unix socket paths are limited to about 104 bytes (macOS) or 108
    /// (Linux), including the terminating NUL.
    const MAX_SOCKET_PATH: usize = 100;

    fn check_length(path: &Path) -> Result<(), Diagnostic> {
        if path.as_os_str().len() > MAX_SOCKET_PATH {
            return Err(Diagnostic::error(
                codes::SOCKET_IO,
                format!(
                    "the editor socket path is too long for a Unix socket ({} bytes, at most {MAX_SOCKET_PATH})",
                    path.as_os_str().len()
                ),
            )
            .at(Location::file(path.display().to_string()))
            .with_hint("move the project to a shorter path"));
        }
        Ok(())
    }

    /// A bound editor socket, ready for [`serve`].
    #[derive(Debug)]
    pub struct EditorListener {
        listener: UnixListener,
        path: PathBuf,
        stop: Arc<AtomicBool>,
    }

    /// Stops a running [`serve`] loop (open connections finish their current
    /// request and close when their client does).
    #[derive(Clone, Debug)]
    pub struct StopServe {
        path: PathBuf,
        stop: Arc<AtomicBool>,
    }

    impl StopServe {
        /// Makes [`serve`] return `Ok(())` at its next wake-up (this call
        /// wakes it).
        pub fn stop(&self) {
            self.stop.store(true, Ordering::SeqCst);
            let _ = UnixStream::connect(&self.path);
        }
    }

    impl EditorListener {
        /// The socket file.
        pub fn path(&self) -> &Path {
            &self.path
        }

        /// A handle that stops [`serve`].
        pub fn stopper(&self) -> StopServe {
            StopServe {
                path: self.path.clone(),
                stop: self.stop.clone(),
            }
        }
    }

    /// Binds `<project>/.real/editor.sock` for the editor holding `lock`,
    /// replacing a leftover socket file. The socket is accessible to its
    /// owner only: it is bound under a temporary name, restricted to `0600`,
    /// then renamed into place. A lock not held by [`Holder::Editor`] is
    /// refused with `RE8005`.
    pub fn bind(lock: &ProjectLock) -> Result<EditorListener, Diagnostic> {
        let path = lock::socket_path(lock.project());
        if *lock.holder() != Holder::Editor {
            return Err(Diagnostic::error(
                codes::SOCKET_IO,
                "only the editor's project lock may bind the editor socket",
            )
            .at(Location::file(path.display().to_string())));
        }
        let tmp = path.with_file_name(format!("editor.sock.{}.tmp", std::process::id()));
        check_length(&tmp)?;
        let _ = std::fs::remove_file(&tmp);
        let listener = UnixListener::bind(&tmp)
            .map_err(|e| socket_error(&path, format!("could not bind the editor socket: {e}")))?;
        let placed = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = placed {
            let _ = std::fs::remove_file(&tmp);
            return Err(socket_error(
                &path,
                format!("could not set up the editor socket: {e}"),
            ));
        }
        Ok(EditorListener {
            listener,
            path,
            stop: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Serves the editor socket: accepts connections and answers each one's
    /// requests on its own thread through `handle` (so the engine thread
    /// applies them, one at a time). Blocks until [`StopServe::stop`] (then
    /// `Ok`) or until accepting keeps failing (then `RE8005`); run it on a
    /// thread of its own. Transient accept errors (an aborted connection, no
    /// free file descriptors) are retried after a short pause.
    pub fn serve(listener: EditorListener, handle: EngineHandle) -> Result<(), Diagnostic> {
        let mut failures = 0u32;
        loop {
            let accepted = listener.listener.accept();
            if listener.stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            let stream = match accepted {
                Ok((stream, _)) => {
                    failures = 0;
                    stream
                }
                Err(e) => {
                    failures += 1;
                    if failures > 200 {
                        return Err(socket_error(
                            &listener.path,
                            format!("the editor socket keeps failing: {e}"),
                        ));
                    }
                    let pause = if e.kind() == ErrorKind::Interrupted {
                        0
                    } else {
                        50
                    };
                    std::thread::sleep(Duration::from_millis(pause));
                    continue;
                }
            };
            let handle = handle.clone();
            let spawned = std::thread::Builder::new()
                .name("re-editor-socket".to_string())
                .spawn(move || serve_connection(stream, &handle));
            if spawned.is_err() {
                // Out of threads: the connection is dropped; its client gets
                // RE8005 and can retry.
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }

    fn serve_connection(stream: UnixStream, handle: &EngineHandle) {
        let Ok(read) = stream.try_clone() else {
            return;
        };
        let mut writer = stream;
        for line in BufReader::new(read).lines() {
            let Ok(line) = line else {
                return;
            };
            if line.trim().is_empty() {
                continue;
            }
            let response = answer(&line, handle);
            let mut text = serde_json::to_string(&response).expect("responses serialize");
            text.push('\n');
            if writer
                .write_all(text.as_bytes())
                .and_then(|()| writer.flush())
                .is_err()
            {
                return;
            }
        }
    }

    /// Connects to an editor socket. The returned handle forwards every call
    /// to the editor (one at a time, in order) with the origin of each
    /// request; it starts as `Origin::User`, so an agent sets its own with
    /// `with_origin`. The editor applies its own policy, so a client with a
    /// stricter one checks calls before sending them. While the editor is
    /// still starting (the socket file is missing or refuses connections),
    /// connecting is retried for up to 2 seconds. A call waits for the
    /// editor's answer without a time limit.
    pub fn connect(socket: &Path) -> Result<EngineHandle, Diagnostic> {
        check_length(socket)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        let stream = loop {
            match UnixStream::connect(socket) {
                Ok(s) => break s,
                Err(e)
                    if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused)
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => {
                    return Err(socket_error(
                        socket,
                        format!("could not connect to the editor socket: {e}"),
                    ));
                }
            }
        };
        let read = stream.try_clone().map_err(|e| {
            socket_error(
                socket,
                format!("could not connect to the editor socket: {e}"),
            )
        })?;
        let (handle, receiver) = channel();
        let socket = socket.to_path_buf();
        std::thread::Builder::new()
            .name("re-editor-client".to_string())
            .spawn(move || forward(receiver, stream, BufReader::new(read), &socket))
            .map_err(|e| {
                Diagnostic::error(
                    codes::SOCKET_IO,
                    format!("could not start the editor-socket client thread: {e}"),
                )
            })?;
        Ok(handle)
    }

    /// Why an exchange failed: the connection (it is unusable) or the answer
    /// (also unusable: the stream may be out of step).
    fn exchange(
        writer: &mut UnixStream,
        reader: &mut BufReader<UnixStream>,
        request: &Request,
        socket: &Path,
    ) -> Result<Response, Diagnostic> {
        let lost = |e: std::io::Error| {
            socket_error(socket, format!("lost the connection to the editor: {e}"))
        };
        let mut text = serde_json::to_string(request).expect("requests serialize");
        text.push('\n');
        writer
            .write_all(text.as_bytes())
            .and_then(|()| writer.flush())
            .map_err(lost)?;
        let mut line = String::new();
        let n = reader.read_line(&mut line).map_err(lost)?;
        if n == 0 {
            return Err(socket_error(
                socket,
                "lost the connection to the editor: it closed the socket",
            ));
        }
        let response: Response = serde_json::from_str(&line).map_err(|e| {
            protocol_error(format!("invalid editor-socket response: {e}"))
                .at(Location::file(socket.display().to_string()))
        })?;
        if response.id != request.id {
            return Err(protocol_error(format!(
                "editor-socket response id {} does not match request id {}",
                response.id, request.id
            )));
        }
        Ok(response)
    }

    /// The client thread: turns channel requests into socket lines. After the
    /// connection fails, the current request gets the error and the thread
    /// ends, so later calls get `RE2006` (the engine is unavailable).
    fn forward(
        receiver: EngineReceiver,
        mut writer: UnixStream,
        mut reader: BufReader<UnixStream>,
        socket: &Path,
    ) {
        let mut next_id: u64 = 1;
        while let Some(request) = receiver.recv() {
            let id = Json::from(next_id);
            next_id += 1;
            match request {
                EngineRequest::Command {
                    name,
                    args,
                    dry_run,
                    origin,
                    reply,
                } => {
                    let wire = Request {
                        id,
                        command: Some(name),
                        args,
                        dry_run,
                        origin: Some(origin),
                        list_specs: false,
                    };
                    match exchange(&mut writer, &mut reader, &wire, socket) {
                        Ok(Response {
                            outcome: Some(outcome),
                            ..
                        }) => {
                            let _ = reply.send(outcome);
                        }
                        Ok(_) => {
                            let _ = reply.send(CommandOutcome::error(protocol_error(
                                "editor-socket response has no `outcome`",
                            )));
                        }
                        Err(d) => {
                            let _ = reply.send(CommandOutcome::error(d));
                            return;
                        }
                    }
                }
                EngineRequest::ListSpecs { reply } => {
                    let wire = Request {
                        id,
                        list_specs: true,
                        ..Request::default()
                    };
                    match exchange(&mut writer, &mut reader, &wire, socket) {
                        Ok(Response {
                            specs: Some(specs), ..
                        }) => {
                            let _ = reply.send(specs);
                        }
                        // The editor answered with an error (its engine is
                        // gone, say): dropping `reply` makes `list_specs`
                        // report RE2006, and the connection stays usable.
                        Ok(_) => {}
                        Err(_) => return,
                    }
                }
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::path::Path;

    use re_command::EngineHandle;
    use re_core::{Diagnostic, Location};

    use crate::codes;
    use crate::lock::{self, ProjectLock};

    /// A bound editor socket. On this platform none can be bound.
    #[derive(Debug)]
    pub struct EditorListener {
        _private: (),
    }

    /// Stops a running `serve` loop. On this platform there is none.
    #[derive(Clone, Debug)]
    pub struct StopServe {
        _private: (),
    }

    impl StopServe {
        /// Does nothing on this platform.
        pub fn stop(&self) {}
    }

    impl EditorListener {
        /// The socket file.
        pub fn path(&self) -> &Path {
            Path::new("")
        }

        /// A handle that stops `serve`.
        pub fn stopper(&self) -> StopServe {
            StopServe { _private: () }
        }
    }

    pub(super) fn unsupported(path: &Path) -> Diagnostic {
        Diagnostic::error(
            codes::SOCKET_UNSUPPORTED,
            "this build of `real` has no editor socket on Windows",
        )
        .at(Location::file(path.display().to_string()))
        .with_hint(
            "close the editor and run the command again: without an editor it opens the \
             project itself",
        )
    }

    /// Always `RE8004` on this platform.
    pub fn bind(lock: &ProjectLock) -> Result<EditorListener, Diagnostic> {
        Err(unsupported(&lock::socket_path(lock.project())))
    }

    /// Always `RE8004` on this platform.
    pub fn serve(_listener: EditorListener, _handle: EngineHandle) -> Result<(), Diagnostic> {
        Err(unsupported(Path::new(".real/editor.sock")))
    }

    /// Always `RE8004` on this platform.
    pub fn connect(socket: &Path) -> Result<EngineHandle, Diagnostic> {
        Err(unsupported(socket))
    }
}

pub use imp::{EditorListener, StopServe, bind, connect, serve};

/// How a writer reaches the project: it holds the lock itself, or the
/// editor does and commands go through its socket.
#[derive(Debug)]
pub enum Writer {
    /// The project was free: this process holds the lock and opens the
    /// project itself.
    Local(lock::ProjectLock),
    /// The editor holds the project: send commands through this handle.
    Remote(EngineHandle),
}

/// The probe-then-lock sequence every headless writer (`real do`,
/// `real mcp`) runs: when the editor holds the project, connect to its
/// socket; otherwise take the lock as `holder`. If the editor takes the lock
/// between the probe and the attempt, the editor is used. Another headless
/// writer gives `RE8003`.
pub fn open_writer(project: &Path, holder: lock::Holder) -> Result<Writer, Diagnostic> {
    match lock::probe(project) {
        lock::LockState::Editor(socket) => return connect(&socket).map(Writer::Remote),
        lock::LockState::Free | lock::LockState::Headless(_) => {}
    }
    match lock::acquire(project, holder) {
        Ok(held) => Ok(Writer::Local(held)),
        Err(refused) => match lock::probe(project) {
            lock::LockState::Editor(socket) => connect(&socket).map(Writer::Remote),
            _ => Err(refused),
        },
    }
}
