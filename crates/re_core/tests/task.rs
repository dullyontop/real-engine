//! `TaskPool`: one rayon pool with named threads.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};

use re_core::TaskPool;

#[test]
fn threads_are_named_re_worker_n() {
    let pool = TaskPool::new(3).unwrap();
    assert_eq!(pool.threads(), 3);
    let mut names: Vec<String> = pool.install(|| {
        use std::sync::Mutex;
        let names = Mutex::new(Vec::new());
        pool.scope(|s| {
            for _ in 0..64 {
                s.spawn(|_| {
                    let name = std::thread::current().name().unwrap_or("").to_string();
                    names.lock().unwrap().push(name);
                    std::thread::sleep(std::time::Duration::from_millis(1));
                });
            }
        });
        names.into_inner().unwrap()
    });
    names.sort();
    names.dedup();
    assert!(!names.is_empty());
    for name in &names {
        let n: usize = name
            .strip_prefix("re-worker-")
            .unwrap_or_else(|| panic!("unexpected thread name {name:?}"))
            .parse()
            .unwrap();
        assert!(n < 3);
    }
}

#[test]
fn spawn_runs_without_blocking_and_scope_borrows() {
    let pool = TaskPool::new(2).unwrap();
    let (tx, rx) = mpsc::channel();
    pool.spawn(move || {
        tx.send(std::thread::current().name().map(String::from))
            .unwrap()
    });
    let name = rx.recv().unwrap().unwrap();
    assert!(name.starts_with("re-worker-"), "{name}");

    let counter = Arc::new(AtomicUsize::new(0));
    let data = vec![1usize, 2, 3, 4];
    pool.scope(|s| {
        for x in &data {
            let counter = &counter;
            s.spawn(move |_| {
                counter.fetch_add(*x, Ordering::Relaxed);
            });
        }
    });
    assert_eq!(counter.load(Ordering::Relaxed), 10);
}

#[test]
fn zero_threads_means_the_default() {
    let pool = TaskPool::new(0).unwrap();
    assert_eq!(pool.threads(), TaskPool::default_threads());
    assert!(TaskPool::default_threads() >= 1);
}

#[test]
fn global_pool_is_created_once() {
    let a = TaskPool::global() as *const TaskPool;
    let b = TaskPool::global() as *const TaskPool;
    assert_eq!(a, b);
    let err = TaskPool::init_global(1).expect_err("already created by global()");
    assert_eq!(err.code.to_string(), "RE0004");
}
