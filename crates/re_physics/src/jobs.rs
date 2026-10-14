//! Jolt's jobs on the engine's worker pool (ADR-0007).
//!
//! Jolt splits a step into jobs and hands each one to a job system. [`JoltRayonJobs`]
//! is that job system for Real Engine: it runs every job on the shared
//! [`TaskPool`](re_core::TaskPool), so physics adds no threads of its own and does not
//! oversubscribe the machine. The thread that calls [`Physics::step`](crate::Physics::step)
//! also runs jobs while it waits (Jolt's barrier), which is why the concurrency is
//! the pool's thread count plus one.

use std::sync::atomic::{AtomicU64, Ordering};

use oxijolt::{Job, JobSystem, WorldSettings};
use re_core::TaskPool;

/// An `oxijolt::JobSystem` that spawns each Jolt job on a [`TaskPool`].
pub(crate) struct JoltRayonJobs {
    pool: &'static TaskPool,
    concurrency: u32,
    queued: AtomicU64,
}

impl JoltRayonJobs {
    pub(crate) fn new(pool: &'static TaskPool) -> JoltRayonJobs {
        let threads = u32::try_from(pool.threads()).unwrap_or(u32::MAX);
        JoltRayonJobs {
            pool,
            concurrency: threads
                .saturating_add(1)
                .clamp(1, WorldSettings::MAX_CONCURRENCY),
            queued: AtomicU64::new(0),
        }
    }

    /// The number of jobs Jolt handed over so far.
    pub(crate) fn queued(&self) -> u64 {
        self.queued.load(Ordering::Relaxed)
    }
}

impl JobSystem for JoltRayonJobs {
    fn max_concurrency(&self) -> u32 {
        self.concurrency
    }

    fn queue_job(&self, job: Job) {
        self.queued.fetch_add(1, Ordering::Relaxed);
        // A job that Jolt's barrier already ran on the stepping thread is a no-op here.
        self.pool.spawn(move || job.run());
    }
}
