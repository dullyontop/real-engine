//! The single-writer project lock (ADR-0020, ADR-0026).
//!
//! One process at a time writes a project: the editor (`real editor`) or else
//! the first headless writer (`real do`, `real mcp`). The writer holds
//! `.real/editor.lock`, a small JSON file with its pid and kind:
//!
//! ```json
//! {"pid":4242,"kind":"headless","command":"real mcp"}
//! ```
//!
//! [`acquire`] creates it with `create_new`, so two writers can't both create
//! it. A lock whose pid is no longer alive is stale and is replaced; a live
//! one gives `RE8003 project is locked by <pid>`. [`probe`] tells other
//! processes who holds it, so that they can send their commands to the editor
//! through its socket ([`crate::remote`]) instead. The lock is released when
//! the [`ProjectLock`] is dropped.
//!
//! Readers never take the lock. Every writer goes through this module, and no
//! other crate emits `RE8003`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};

use crate::codes;

/// The project's state directory (`<project>/.real`).
pub fn state_dir(project: &Path) -> PathBuf {
    project.join(".real")
}

/// The lock file of a project (`<project>/.real/editor.lock`).
pub fn lock_path(project: &Path) -> PathBuf {
    state_dir(project).join("editor.lock")
}

/// The editor socket of a project (`<project>/.real/editor.sock`).
pub fn socket_path(project: &Path) -> PathBuf {
    state_dir(project).join("editor.sock")
}

/// Who holds (or wants) the lock.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Holder {
    /// The editor. It serves `.real/editor.sock`, and other writers send their
    /// commands there.
    Editor,
    /// A headless writer, named by its command (for example `"real mcp"`).
    Headless {
        /// The command that holds the lock, shown in `RE8003`.
        command: String,
    },
}

impl Holder {
    /// A headless holder named `command`.
    pub fn headless(command: impl Into<String>) -> Holder {
        Holder::Headless {
            command: command.into(),
        }
    }

    fn describe(&self) -> String {
        match self {
            Holder::Editor => "the editor".to_string(),
            Holder::Headless { command } => format!("`{command}`"),
        }
    }
}

/// The contents of `.real/editor.lock`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInfo {
    /// The holder's process id.
    pub pid: u32,
    /// The holder's kind.
    #[serde(flatten)]
    pub holder: Holder,
}

/// What [`probe`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockState {
    /// No live process holds the lock (there is no lock file, or it is stale).
    Free,
    /// The editor holds it; commands go to this socket.
    Editor(PathBuf),
    /// A headless writer with this pid holds it.
    Headless(u32),
}

/// A held project lock. Dropping it releases the lock (and, for the editor,
/// removes the socket file).
#[derive(Debug)]
pub struct ProjectLock {
    path: PathBuf,
    socket: PathBuf,
    info: LockInfo,
}

impl ProjectLock {
    /// The lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The holder this lock was taken for.
    pub fn holder(&self) -> &Holder {
        &self.info.holder
    }

    /// The pid written to the lock file (this process).
    pub fn pid(&self) -> u32 {
        self.info.pid
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        // Only remove the file if it is still ours: never delete a lock that
        // another process took after ours was (wrongly) judged stale.
        if read_once(&self.path).ok().flatten().as_ref() == Some(&self.info) {
            let _ = fs::remove_file(&self.path);
            if self.info.holder == Holder::Editor {
                let _ = fs::remove_file(&self.socket);
            }
        }
    }
}

fn lock_io(path: &Path, action: &str, e: &io::Error) -> Diagnostic {
    Diagnostic::error(
        codes::LOCK_IO,
        format!("could not {action} the project lock: {e}"),
    )
    .at(Location::file(path.display().to_string()))
    .with_hint("check that the project directory exists and that `.real/` is writable")
}

fn locked(path: &Path, info: &LockInfo) -> Diagnostic {
    let hint = match info.holder {
        Holder::Editor => {
            "the editor is running: send commands through it (`real do` and `real mcp` do this \
             when they find it), or close it first"
        }
        Holder::Headless { .. } => {
            "stop the other process, or start the editor (`real editor`) so that every writer \
             goes through it"
        }
    };
    Diagnostic::error(
        codes::PROJECT_LOCKED,
        format!(
            "project is locked by {} ({})",
            info.pid,
            info.holder.describe()
        ),
    )
    .at(Location::file(path.display().to_string()))
    .with_hint(hint)
}

/// Reads the lock file once. `Ok(None)` if it is missing; `Err` with the raw
/// text if it doesn't parse (it may be half written).
fn read_once(path: &Path) -> Result<Option<LockInfo>, String> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map(Some).map_err(|_| text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// Reads the lock file, waiting briefly if a writer has created it but not
/// yet written it. `None` if it is missing, or still unreadable after the
/// wait (a writer died between creating and writing it: stale).
fn read_lock(path: &Path) -> Option<LockInfo> {
    for _ in 0..20 {
        match read_once(path) {
            Ok(info) => return info,
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    None
}

/// The live holder of the lock, if any.
fn live_holder(path: &Path) -> Option<LockInfo> {
    read_lock(path).filter(|info| pid_alive(info.pid))
}

/// Takes the project's single-writer lock for `holder`.
///
/// Creates `.real/` if needed. A stale lock (its pid is dead, or the file is
/// unreadable) is replaced. If a live process holds the lock (this process
/// included), the result is `RE8003`. I/O failures are `RE8007`.
pub fn acquire(project: &Path, holder: Holder) -> Result<ProjectLock, Diagnostic> {
    let dir = state_dir(project);
    let path = lock_path(project);
    fs::create_dir_all(&dir).map_err(|e| lock_io(&dir, "create the directory of", &e))?;
    let info = LockInfo {
        pid: std::process::id(),
        holder,
    };
    let text = serde_json::to_string(&info).expect("lock info serializes");
    // A few attempts: each lost race against another writer re-reads the
    // winner's lock, which then refuses us with RE8003.
    for _ in 0..4 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => {
                let written = file
                    .write_all(text.as_bytes())
                    .and_then(|()| file.sync_all());
                if let Err(e) = written {
                    drop(file);
                    let _ = fs::remove_file(&path);
                    return Err(lock_io(&path, "write", &e));
                }
                return Ok(ProjectLock {
                    path,
                    socket: socket_path(project),
                    info,
                });
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if let Some(other) = live_holder(&path) {
                    return Err(locked(&path, &other));
                }
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(lock_io(&path, "replace the stale", &e)),
                }
            }
            Err(e) => return Err(lock_io(&path, "create", &e)),
        }
    }
    match live_holder(&path) {
        Some(other) => Err(locked(&path, &other)),
        None => Err(lock_io(
            &path,
            "take",
            &io::Error::other("other processes kept replacing it"),
        )),
    }
}

/// Who holds the project's lock. A stale or unreadable lock counts as
/// [`LockState::Free`]. Never takes the lock.
pub fn probe(project: &Path) -> LockState {
    match live_holder(&lock_path(project)) {
        None => LockState::Free,
        Some(LockInfo {
            holder: Holder::Editor,
            ..
        }) => LockState::Editor(socket_path(project)),
        Some(LockInfo { pid, .. }) => LockState::Headless(pid),
    }
}

/// True if a process with this pid is running. When liveness can't be
/// determined, the answer is `true`: keeping a stale lock is safer than
/// breaking a live one.
#[cfg(target_os = "linux")]
pub(crate) fn pid_alive(pid: u32) -> bool {
    match fs::read_to_string(format!("/proc/{pid}/stat")) {
        // The state follows the parenthesised command name; `Z` (zombie) and
        // `X` (dead) have exited.
        Ok(stat) => !matches!(
            stat.rsplit_once(')')
                .and_then(|(_, rest)| rest.trim_start().chars().next()),
            Some('Z' | 'X')
        ),
        Err(e) if e.kind() == io::ErrorKind::NotFound => false,
        Err(_) => true,
    }
}

/// True if a process with this pid is running (`kill -0`).
#[cfg(all(unix, not(target_os = "linux")))]
pub(crate) fn pid_alive(pid: u32) -> bool {
    match std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => true,
        // EPERM: the process exists but belongs to another user.
        Ok(out) => String::from_utf8_lossy(&out.stderr)
            .to_lowercase()
            .contains("not permitted"),
        Err(_) => true,
    }
}

/// True if a process with this pid is running (`tasklist`).
#[cfg(windows)]
pub(crate) fn pid_alive(pid: u32) -> bool {
    match std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\""))
        }
        _ => true,
    }
}

/// Unknown platform: assume the holder is alive.
#[cfg(not(any(unix, windows)))]
pub(crate) fn pid_alive(_pid: u32) -> bool {
    true
}
