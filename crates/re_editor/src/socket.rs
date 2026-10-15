//! [`EditorSocket`]: the editor's hold on its project. It owns the
//! single-writer lock (as [`Holder::Editor`]) and hosts the editor socket
//! (`.real/editor.sock`) with `re_agent::remote::serve`.

use std::path::Path;
use std::thread::JoinHandle;

use re_agent::lock::{self, Holder, ProjectLock};
use re_agent::remote::{self, StopServe};
use re_app::App;
use re_command::{EngineReceiver, PolicyCheck, channel};
use re_core::Diagnostic;

/// The editor's project lock and its socket server.
///
/// [`EditorSocket::open`] takes the lock; then other writers (`real do`,
/// `real mcp`) find the editor with `re_agent::lock::probe` and send their
/// commands through the socket. A server thread accepts connections and
/// forwards each request to this value's receiver; the editor answers them on
/// its own thread with [`EditorSocket::serve_pending`], between frames, so a
/// remote command is applied exactly like one from the editor's own panels.
///
/// Dropping it stops the server, removes the socket file and releases the
/// lock.
#[derive(Debug)]
pub struct EditorSocket {
    receiver: EngineReceiver,
    server: Option<(StopServe, JoinHandle<Result<(), Diagnostic>>)>,
    warning: Option<Diagnostic>,
    // Dropped last: the lock removes the socket file while it is still held.
    lock: ProjectLock,
}

impl EditorSocket {
    /// Takes the lock of the project at `project` for the editor and starts
    /// serving `.real/editor.sock`.
    ///
    /// When another process holds the project, the result is `re_agent`'s
    /// `RE8003`. Where the platform has no editor socket (Windows in M1) the
    /// editor still holds the lock, and [`EditorSocket::warning`] carries
    /// `RE8004`: other writers are refused instead of proxied.
    ///
    /// # Errors
    ///
    /// `RE8003` (locked), `RE8007` (lock file I/O) or `RE8005` (the socket
    /// could not be bound).
    pub fn open(project: &Path) -> Result<EditorSocket, Diagnostic> {
        let lock = lock::acquire(project, Holder::Editor)?;
        let (handle, receiver) = channel();
        let (server, warning) = match remote::bind(&lock) {
            Ok(listener) => {
                let stop = listener.stopper();
                let thread = std::thread::Builder::new()
                    .name("re-editor-socket-server".to_string())
                    .spawn(move || remote::serve(listener, handle))
                    .map_err(|e| {
                        Diagnostic::error(
                            re_agent::codes::SOCKET_IO,
                            format!("could not start the editor socket thread: {e}"),
                        )
                    })?;
                (Some((stop, thread)), None)
            }
            Err(d) if d.code == re_agent::codes::SOCKET_UNSUPPORTED => {
                drop(handle);
                (None, Some(d))
            }
            Err(d) => return Err(d),
        };
        Ok(EditorSocket {
            receiver,
            server,
            warning,
            lock,
        })
    }

    /// The project lock the editor holds.
    pub fn lock(&self) -> &ProjectLock {
        &self.lock
    }

    /// The socket file, while the socket is served.
    pub fn path(&self) -> Option<std::path::PathBuf> {
        self.server
            .as_ref()
            .map(|_| lock::socket_path(self.lock.project()))
    }

    /// Why the socket is not served (`RE8004` on platforms without one).
    pub fn warning(&self) -> Option<&Diagnostic> {
        self.warning.as_ref()
    }

    /// Applies the commands that other writers have sent, without blocking,
    /// through [`App::serve_pending`] with `policy`. Returns how many were
    /// answered.
    pub fn serve_pending(&self, app: &mut App, policy: &dyn PolicyCheck) -> usize {
        app.serve_pending(&self.receiver, policy).unwrap_or(0)
    }
}

impl Drop for EditorSocket {
    fn drop(&mut self) {
        if let Some((stop, thread)) = self.server.take() {
            stop.stop();
            // `stop` wakes the server by connecting to the socket file; if
            // that file was removed (`git clean`, a moved project), the
            // server thread cannot be woken: leave it rather than hang.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
            while !thread.is_finished() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
    }
}
