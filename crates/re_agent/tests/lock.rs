//! `re_agent::lock`: the single-writer project lock.

use std::fs;

use re_agent::lock::{self, Holder, LockInfo, LockState};

fn code(d: &re_core::Diagnostic) -> String {
    d.code.to_string()
}

#[test]
fn second_acquire_is_refused_with_re8003() {
    let dir = tempfile::tempdir().unwrap();
    let held = lock::acquire(dir.path(), Holder::headless("real mcp")).unwrap();
    assert_eq!(held.pid(), std::process::id());
    assert_eq!(held.path(), lock::lock_path(dir.path()));

    let err = lock::acquire(dir.path(), Holder::headless("real do")).unwrap_err();
    assert_eq!(code(&err), "RE8003");
    assert!(
        err.message
            .contains(&format!("project is locked by {}", std::process::id())),
        "{}",
        err.message
    );
    assert!(err.message.contains("real mcp"), "{}", err.message);
    assert!(err.hint.is_some());

    // The refused writer did not disturb the holder's lock.
    assert_eq!(
        lock::probe(dir.path()),
        LockState::Headless(std::process::id())
    );
}

#[test]
fn drop_releases_the_lock() {
    let dir = tempfile::tempdir().unwrap();
    let held = lock::acquire(dir.path(), Holder::headless("real do")).unwrap();
    drop(held);
    assert!(!lock::lock_path(dir.path()).exists());
    assert_eq!(lock::probe(dir.path()), LockState::Free);
    // And it can be taken again.
    let _again = lock::acquire(dir.path(), Holder::Editor).unwrap();
}

#[test]
fn probe_reports_the_editor_and_its_socket() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(lock::probe(dir.path()), LockState::Free);
    let _held = lock::acquire(dir.path(), Holder::Editor).unwrap();
    assert_eq!(
        lock::probe(dir.path()),
        LockState::Editor(lock::socket_path(dir.path()))
    );
    let err = lock::acquire(dir.path(), Holder::headless("real mcp")).unwrap_err();
    assert_eq!(code(&err), "RE8003");
    assert!(err.message.contains("the editor"), "{}", err.message);
}

#[test]
fn lock_file_records_pid_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let _held = lock::acquire(dir.path(), Holder::headless("real mcp")).unwrap();
    let text = fs::read_to_string(lock::lock_path(dir.path())).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["pid"], std::process::id());
    assert_eq!(json["kind"], "headless");
    assert_eq!(json["command"], "real mcp");
}

/// The pid of a process that has exited.
#[cfg(unix)]
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

#[cfg(unix)]
#[test]
fn stale_lock_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(lock::state_dir(dir.path())).unwrap();
    let stale = LockInfo {
        pid: dead_pid(),
        holder: Holder::Editor,
    };
    fs::write(
        lock::lock_path(dir.path()),
        serde_json::to_string(&stale).unwrap(),
    )
    .unwrap();
    assert_eq!(lock::probe(dir.path()), LockState::Free);

    let held = lock::acquire(dir.path(), Holder::headless("real do")).unwrap();
    assert_eq!(
        lock::probe(dir.path()),
        LockState::Headless(std::process::id())
    );
    drop(held);
    assert_eq!(lock::probe(dir.path()), LockState::Free);
}

#[test]
fn unreadable_lock_is_stale() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(lock::state_dir(dir.path())).unwrap();
    fs::write(lock::lock_path(dir.path()), "not json").unwrap();
    assert_eq!(lock::probe(dir.path()), LockState::Free);
    let _held = lock::acquire(dir.path(), Holder::headless("real do")).unwrap();
}

#[test]
fn drop_keeps_a_lock_that_is_no_longer_ours() {
    let dir = tempfile::tempdir().unwrap();
    let held = lock::acquire(dir.path(), Holder::Editor).unwrap();
    // Another process replaced the lock (for example after judging ours
    // stale): dropping ours must not delete theirs.
    let theirs = LockInfo {
        pid: std::process::id().wrapping_add(1),
        holder: Holder::headless("real mcp"),
    };
    let text = serde_json::to_string(&theirs).unwrap();
    fs::write(lock::lock_path(dir.path()), &text).unwrap();
    drop(held);
    assert_eq!(
        fs::read_to_string(lock::lock_path(dir.path())).unwrap(),
        text
    );
}

#[test]
fn missing_project_directory_is_re8007() {
    let dir = tempfile::tempdir().unwrap();
    // A file where the project directory should be.
    let file = dir.path().join("not-a-dir");
    fs::write(&file, "").unwrap();
    let err = lock::acquire(&file, Holder::Editor).unwrap_err();
    assert_eq!(code(&err), "RE8007");
}

#[test]
fn diagnostics_catalogue_is_complete() {
    let problems = re_core::validate_catalog(re_agent::DIAGNOSTICS, re_agent::CODE_RANGE);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(re_agent::DIAGNOSTICS.len(), 5);
}
