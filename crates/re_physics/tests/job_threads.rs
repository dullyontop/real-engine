//! The pool is shared: Jolt's jobs run on the engine's `re-worker-N` threads (and the
//! stepping thread), and physics starts no thread of its own. Alone in its process so
//! that no other test's threads are listed.

mod common;

use common::*;

/// The names of every thread of this process except the main thread and the calling
/// thread (Linux only). A new thread inherits its creator's name on Linux, so threads
/// are told apart by id, not only by name.
#[cfg(target_os = "linux")]
fn other_thread_names() -> Vec<String> {
    let pid = std::process::id().to_string();
    // `/proc/thread-self` links to `<pid>/task/<tid>`.
    let link = std::fs::read_link("/proc/thread-self").expect("/proc/thread-self");
    let me = link.file_name().unwrap().to_string_lossy().into_owned();
    let mut names = Vec::new();
    for entry in std::fs::read_dir("/proc/self/task").expect("/proc/self/task") {
        let entry = entry.unwrap();
        let tid = entry.file_name().to_string_lossy().into_owned();
        if tid == pid || tid == me {
            continue;
        }
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        names.push(comm.trim().to_owned());
    }
    names
}

#[test]
fn jolt_jobs_run_on_the_shared_pool() {
    let mut p = world(3);
    apply(&mut p, pile(300));
    steps(&mut p, 60);
    assert!(p.jobs_queued() > 0, "Jolt handed no job to the pool");

    #[cfg(target_os = "linux")]
    {
        // Besides the main thread and this (stepping) thread, only the pool's three
        // workers exist.
        let mut others = other_thread_names();
        others.sort();
        assert_eq!(others, ["re-worker-0", "re-worker-1", "re-worker-2"]);
    }
}
