//! The one worker pool (ADR-0007).
//!
//! The engine runs one `rayon::ThreadPool`, shared by ProcGraph evaluation, Jolt's
//! jobs and asset processing. Its threads are named `re-worker-N` (N from 0), so
//! they can be told apart from any other thread in the process.
//!
//! On wasm without atomics (the browser build, ADR-0049 §5) there are no threads: the pool
//! has one worker, the calling thread, so `scope`, `install` and parallel iterators run
//! their jobs inline, and `spawn` runs its job before returning. Every `TaskPool` there
//! shares that one rayon pool.

use std::sync::{Arc, OnceLock};

use crate::codes;
use crate::diag::Diagnostic;

static GLOBAL: OnceLock<TaskPool> = OnceLock::new();

/// A wrapper around one `rayon::ThreadPool` with threads named `re-worker-N`.
#[derive(Debug)]
pub struct TaskPool {
    pool: Arc<rayon::ThreadPool>,
}

impl TaskPool {
    /// The default worker count: the available cores minus one (the main thread), at least 1.
    pub fn default_threads() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1))
            .unwrap_or(1)
            .max(1)
    }

    /// Creates a pool with `threads` workers; `0` means [`default_threads`](Self::default_threads).
    ///
    /// Fails with `RE0003` if the threads cannot be started.
    pub fn new(threads: usize) -> Result<TaskPool, Diagnostic> {
        let threads = if threads == 0 {
            Self::default_threads()
        } else {
            threads
        };
        #[cfg(not(all(target_family = "wasm", not(target_feature = "atomics"))))]
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .thread_name(|i| format!("re-worker-{i}"))
            .build()
            .map(Arc::new);
        // The calling thread joins a `use_current_thread` pool for good and cannot join a
        // second one, so every TaskPool on the web shares one rayon pool.
        #[cfg(all(target_family = "wasm", not(target_feature = "atomics")))]
        let pool = {
            static SINGLE: OnceLock<Arc<rayon::ThreadPool>> = OnceLock::new();
            match SINGLE.get() {
                Some(pool) => Ok(pool.clone()),
                None => rayon::ThreadPoolBuilder::new()
                    .num_threads(1)
                    .use_current_thread()
                    .build()
                    .map(|pool| SINGLE.get_or_init(|| Arc::new(pool)).clone()),
            }
        };
        pool.map(|pool| TaskPool { pool }).map_err(|e| {
            Diagnostic::error(
                codes::TASK_POOL_BUILD_FAILED,
                format!("could not start {threads} worker threads: {e}"),
            )
            .with_hint("lower the worker count, or check the process thread limit")
        })
    }

    /// The process-wide pool, created with [`default_threads`](Self::default_threads)
    /// workers on first use unless [`init_global`](Self::init_global) ran first.
    ///
    /// # Panics
    ///
    /// Panics if the pool has to be created here and its threads cannot be started.
    pub fn global() -> &'static TaskPool {
        GLOBAL.get_or_init(|| TaskPool::new(0).unwrap_or_else(|d| panic!("{}", d.render_human())))
    }

    /// Creates the process-wide pool with `threads` workers (`0` for the default).
    ///
    /// Fails with `RE0004` if the global pool already exists (because of an earlier
    /// call or a call to [`global`](Self::global)), or `RE0003` if the threads
    /// cannot be started.
    pub fn init_global(threads: usize) -> Result<&'static TaskPool, Diagnostic> {
        let already = || {
            Diagnostic::error(
                codes::TASK_POOL_ALREADY_INITIALISED,
                "the global worker pool already exists",
            )
            .with_hint("call TaskPool::init_global once, at startup, before anything uses the pool")
        };
        if GLOBAL.get().is_some() {
            return Err(already());
        }
        let pool = TaskPool::new(threads)?;
        GLOBAL.set(pool).map_err(|_| already())?;
        Ok(GLOBAL.get().expect("set just above"))
    }

    /// The number of worker threads.
    pub fn threads(&self) -> usize {
        self.pool.current_num_threads()
    }

    /// Runs `job` on a worker without waiting for it. Never blocks.
    ///
    /// On the single-threaded web build the job runs before `spawn` returns: rayon's only
    /// worker there is the calling thread, which would never pick a detached job up.
    pub fn spawn<F>(&self, job: F)
    where
        F: FnOnce() + Send + 'static,
    {
        #[cfg(not(all(target_family = "wasm", not(target_feature = "atomics"))))]
        self.pool.spawn(job);
        #[cfg(all(target_family = "wasm", not(target_feature = "atomics")))]
        job();
    }

    /// Runs `op` with a scope in which borrowed jobs can be spawned; returns once
    /// every job spawned in the scope has finished.
    pub fn scope<'scope, OP, R>(&self, op: OP) -> R
    where
        OP: FnOnce(&rayon::Scope<'scope>) -> R + Send,
        R: Send,
    {
        self.pool.scope(op)
    }

    /// Runs `op` inside the pool, so rayon parallel iterators used by `op` run on
    /// this pool's workers.
    pub fn install<OP, R>(&self, op: OP) -> R
    where
        OP: FnOnce() -> R + Send,
        R: Send,
    {
        self.pool.install(op)
    }

    /// The underlying rayon pool, for adapters such as the Jolt job system.
    pub fn rayon(&self) -> &rayon::ThreadPool {
        &self.pool
    }
}
