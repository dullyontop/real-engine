//! The single-writer project lock (ADR-0020, ADR-0026).
//!
//! One process at a time writes a project: the editor (`real editor`) or else
//! the first headless writer (`real do`, `real mcp`). The writer holds an
//! operating-system file lock ([`std::fs::File::try_lock`]) on
//! `.real/editor.lock` and writes its pid and kind next to it, in
//! `.real/editor.owner`:
//!
//! ```json
//! {"pid":4242,"kind":"headless","command":"real mcp"}
//! ```
//!
//! The OS releases the lock when the holder exits, however it exits, so a
//! crashed writer never leaves a lock behind, and two writers can never both
//! hold it. A second writer gets `RE8003 project is locked by <pid>`.
//! [`probe`] tells other processes who holds it, so that they can send their
//! commands to the editor through its socket ([`crate::remote`]) instead;
//! [`crate::remote::open_writer`] does the whole probe-then-lock dance. The
//! lock is released when the [`ProjectLock`] is dropped.
//!
//! Readers never take the lock. Every writer goes through this module, and no
//! other crate emits `RE8003`. The owner file is information only: the OS
//! lock decides who holds the project, so a stale owner file is ignored.

use std::fs::{self, File, OpenOptions, TryLockError};
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

/// The lock file of a project (`<project>/.real/editor.lock`). It is never
/// deleted; holding the project means holding an OS lock on it.
pub fn lock_path(project: &Path) -> PathBuf {
    state_dir(project).join("editor.lock")
}

/// The holder's pid and kind (`<project>/.real/editor.owner`), written while
/// the lock is held.
pub fn owner_path(project: &Path) -> PathBuf {
    state_dir(project).join("editor.owner")
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

/// The contents of `.real/editor.owner`.
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
    /// No process holds the lock.
    Free,
    /// The editor holds it; commands go to this socket.
    Editor(PathBuf),
    /// A headless writer with this pid holds it (0 if it has not yet written
    /// its pid).
    Headless(u32),
}

/// A held project lock. Dropping it releases the lock (and, for the editor,
/// removes the socket file).
#[derive(Debug)]
pub struct ProjectLock {
    file: File,
    project: PathBuf,
    info: LockInfo,
}

impl ProjectLock {
    /// The lock file.
    pub fn path(&self) -> PathBuf {
        lock_path(&self.project)
    }

    /// The project root.
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// The holder this lock was taken for.
    pub fn holder(&self) -> &Holder {
        &self.info.holder
    }

    /// The pid written to the owner file (this process).
    pub fn pid(&self) -> u32 {
        self.info.pid
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        // Clean up while still holding the lock, so the next holder's files
        // are never touched.
        let _ = fs::remove_file(owner_path(&self.project));
        if self.info.holder == Holder::Editor {
            let _ = fs::remove_file(socket_path(&self.project));
        }
        let _ = self.file.unlock();
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

fn locked(path: &Path, info: Option<&LockInfo>) -> Diagnostic {
    let hint = match info.map(|i| &i.holder) {
        Some(Holder::Editor) => {
            "the editor is running: send commands through it (`real do` and `real mcp` do this \
             when they find it), or close it first"
        }
        _ => {
            "stop the other process, or start the editor (`real editor`) so that every writer \
             goes through it"
        }
    };
    let message = match info {
        Some(info) => format!(
            "project is locked by {} ({})",
            info.pid,
            info.holder.describe()
        ),
        None => "project is locked by another process".to_string(),
    };
    Diagnostic::error(codes::PROJECT_LOCKED, message)
        .at(Location::file(path.display().to_string()))
        .with_hint(hint)
}

fn read_owner(project: &Path) -> Option<LockInfo> {
    let text = fs::read_to_string(owner_path(project)).ok()?;
    serde_json::from_str(&text).ok()
}

/// The owner of a held lock, waiting briefly for a holder that has just
/// taken the lock to write it.
fn wait_owner(project: &Path) -> Option<LockInfo> {
    for _ in 0..50 {
        if let Some(info) = read_owner(project) {
            return Some(info);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

fn open_lock_file(project: &Path) -> Result<File, Diagnostic> {
    let dir = state_dir(project);
    fs::create_dir_all(&dir).map_err(|e| lock_io(&dir, "create the directory of", &e))?;
    let path = lock_path(project);
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
        .map_err(|e| lock_io(&path, "open", &e))
}

/// Tries to lock `file`. `Ok(false)` if another holder has it.
fn try_lock(file: &File, path: &Path) -> Result<bool, Diagnostic> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(TryLockError::WouldBlock) => Ok(false),
        Err(TryLockError::Error(e)) => Err(lock_io(path, "take", &e)),
    }
}

/// Takes the project's single-writer lock for `holder`.
///
/// Creates `.real/` if needed. If another holder (in this process or any
/// other) has the lock, the result is `RE8003`. I/O failures are `RE8007`.
pub fn acquire(project: &Path, holder: Holder) -> Result<ProjectLock, Diagnostic> {
    let path = lock_path(project);
    let file = open_lock_file(project)?;
    // A `probe` holds the lock for an instant; retry briefly before giving up.
    let mut got = false;
    for _ in 0..10 {
        if try_lock(&file, &path)? {
            got = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if !got {
        return Err(locked(&path, wait_owner(project).as_ref()));
    }
    let info = LockInfo {
        pid: std::process::id(),
        holder,
    };
    let owner = owner_path(project);
    let text = serde_json::to_string(&info).expect("lock info serializes");
    let tmp = state_dir(project).join(format!("editor.owner.{}.tmp", info.pid));
    let written = File::create(&tmp)
        .and_then(|mut f| f.write_all(text.as_bytes()).and_then(|()| f.sync_all()))
        .and_then(|()| fs::rename(&tmp, &owner));
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        let _ = file.unlock();
        return Err(lock_io(&owner, "write the owner of", &e));
    }
    Ok(ProjectLock {
        file,
        project: project.to_path_buf(),
        info,
    })
}

/// Who holds the project's lock. Never keeps the lock (it may take it for an
/// instant to test it).
pub fn probe(project: &Path) -> LockState {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path(project))
    else {
        // No lock file: nobody has ever taken the lock (or `.real/` is
        // unreadable, and then nobody can write either).
        return LockState::Free;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            LockState::Free
        }
        Err(TryLockError::WouldBlock) => match wait_owner(project) {
            Some(LockInfo {
                holder: Holder::Editor,
                ..
            }) => LockState::Editor(socket_path(project)),
            Some(LockInfo { pid, .. }) => LockState::Headless(pid),
            None => LockState::Headless(0),
        },
        Err(TryLockError::Error(_)) => LockState::Free,
    }
}
