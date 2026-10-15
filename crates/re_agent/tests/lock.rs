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
    assert!(!lock::owner_path(dir.path()).exists());
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
fn owner_file_records_pid_and_kind() {
    let dir = tempfile::tempdir().unwrap();
    let held = lock::acquire(dir.path(), Holder::headless("real mcp")).unwrap();
    let text = fs::read_to_string(lock::owner_path(dir.path())).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["pid"], std::process::id());
    assert_eq!(json["kind"], "headless");
    assert_eq!(json["command"], "real mcp");
    drop(held);
    assert!(!lock::owner_path(dir.path()).exists());
    // The lock file stays: holding the project is holding an OS lock on it.
    assert!(lock::lock_path(dir.path()).exists());
}

#[test]
fn leftover_owner_file_of_a_dead_holder_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(lock::state_dir(dir.path())).unwrap();
    // A holder that crashed: its owner file remains, but nobody holds the OS lock.
    let stale = LockInfo {
        pid: 999_999,
        holder: Holder::Editor,
    };
    fs::write(lock::lock_path(dir.path()), "").unwrap();
    fs::write(
        lock::owner_path(dir.path()),
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

    fs::write(lock::owner_path(dir.path()), "not json").unwrap();
    assert_eq!(lock::probe(dir.path()), LockState::Free);
    let _held = lock::acquire(dir.path(), Holder::Editor).unwrap();
}

#[test]
fn racing_writers_get_exactly_one_lock() {
    for _ in 0..20 {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().to_path_buf();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
        let threads: Vec<_> = (0..6)
            .map(|i| {
                let project = project.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let got = lock::acquire(&project, Holder::headless(format!("w{i}")));
                    // Hold it until everyone has tried.
                    std::thread::sleep(std::time::Duration::from_millis(150));
                    got.map(drop).map_err(|e| e.code.to_string())
                })
            })
            .collect();
        let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
        let winners = results.iter().filter(|r| r.is_ok()).count();
        assert_eq!(winners, 1, "{results:?}");
        assert!(
            results
                .iter()
                .filter_map(|r| r.as_ref().err())
                .all(|c| c == "RE8003")
        );
    }
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
