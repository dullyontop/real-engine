//! The editor socket (ADR-0020, ADR-0026): how other writers send commands to
//! a running editor, so that a project has one writer.
//!
//! The editor holds the project lock ([`crate::lock`]) and serves
//! `.real/editor.sock` with [`serve`]. `real do` and `real mcp` find it with
//! [`crate::lock::probe`] and [`connect`] to it; the [`EngineHandle`] they get
//! back works like a local one, and every call is applied by the editor.
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
//! checks every call against its own policy. A client that needs a stricter
//! policy (such as `real mcp` with the agent policy) checks it before sending.
//!
//! Unix only in M1. On Windows, [`bind`] and [`connect`] return `RE8004`
//! (named pipes are later work).

use std::path::Path;

use re_command::{CommandOutcome, CommandSpec, EngineHandle, Origin};
use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::codes;

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
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    use re_command::{CommandOutcome, EngineHandle, EngineReceiver, EngineRequest, channel};
    use re_core::{Diagnostic, Location};
    use serde_json::Value as Json;

    use super::{Request, Response, answer, protocol_error, socket_error};
    use crate::codes;
    use crate::lock;

    /// A bound editor socket, ready for [`serve`].
    #[derive(Debug)]
    pub struct EditorListener {
        listener: UnixListener,
        path: PathBuf,
    }

    impl EditorListener {
        /// The socket file.
        pub fn path(&self) -> &Path {
            &self.path
        }
    }

    impl From<UnixListener> for EditorListener {
        fn from(listener: UnixListener) -> Self {
            let path = listener
                .local_addr()
                .ok()
                .and_then(|a| a.as_pathname().map(Path::to_path_buf))
                .unwrap_or_default();
            EditorListener { listener, path }
        }
    }

    /// Binds `<project>/.real/editor.sock`, replacing a leftover socket file,
    /// and makes it accessible to its owner only. Call it after taking the
    /// lock as [`lock::Holder::Editor`], so no other editor uses the file.
    pub fn bind(project: &Path) -> Result<EditorListener, Diagnostic> {
        let path = lock::socket_path(project);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| {
                socket_error(dir, format!("could not create {}: {e}", dir.display()))
            })?;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(socket_error(
                    &path,
                    format!("could not remove the old editor socket: {e}"),
                ));
            }
        }
        let listener = UnixListener::bind(&path)
            .map_err(|e| socket_error(&path, format!("could not bind the editor socket: {e}")))?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            socket_error(
                &path,
                format!("could not restrict the editor socket's permissions: {e}"),
            )
        })?;
        Ok(EditorListener { listener, path })
    }

    /// Serves the editor socket: accepts connections and answers each one's
    /// requests on its own thread through `handle` (so the engine thread
    /// applies them, one at a time). Blocks until accepting fails; run it on
    /// a thread of its own.
    pub fn serve(listener: EditorListener, handle: EngineHandle) -> Result<(), Diagnostic> {
        loop {
            let (stream, _) = listener.listener.accept().map_err(|e| {
                socket_error(&listener.path, format!("the editor socket failed: {e}"))
            })?;
            let handle = handle.clone();
            std::thread::Builder::new()
                .name("re-editor-socket".to_string())
                .spawn(move || serve_connection(stream, &handle))
                .map_err(|e| {
                    socket_error(
                        &listener.path,
                        format!("could not start a connection thread: {e}"),
                    )
                })?;
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
    /// to the editor (one at a time, in order) and carries the origin of each
    /// request, so `with_origin` works as it does locally.
    pub fn connect(socket: &Path) -> Result<EngineHandle, Diagnostic> {
        let stream = UnixStream::connect(socket).map_err(|e| {
            socket_error(
                socket,
                format!("could not connect to the editor socket: {e}"),
            )
        })?;
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

    /// Sends one request and reads its response.
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
                            return;
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
                        // The specs reply channel carries no error: dropping
                        // `reply` makes `list_specs` report RE2006.
                        _ => return,
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
    use crate::lock;

    /// A bound editor socket. On this platform none can be bound.
    #[derive(Debug)]
    pub struct EditorListener {
        _private: (),
    }

    impl EditorListener {
        /// The socket file.
        pub fn path(&self) -> &Path {
            Path::new("")
        }
    }

    fn unsupported(path: &Path) -> Diagnostic {
        Diagnostic::error(
            codes::SOCKET_UNSUPPORTED,
            "the editor socket is not available on Windows in M1",
        )
        .at(Location::file(path.display().to_string()))
        .with_hint(
            "close the editor and run the command again: without an editor it opens the \
             project itself",
        )
    }

    /// Always `RE8004` on this platform.
    pub fn bind(project: &Path) -> Result<EditorListener, Diagnostic> {
        Err(unsupported(&lock::socket_path(project)))
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

pub use imp::{EditorListener, bind, connect, serve};
