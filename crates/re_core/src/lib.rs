//! `re_core`: the bottom of Real Engine's contract layer.
//!
//! This crate holds the small, dependency-light pieces every other crate shares:
//!
//! * [`diag`]: the one user-facing error type, [`Diagnostic`], with stable `RE####`
//!   [`Code`]s, and the [`DiagInfo`] catalogue entries behind `real explain`;
//! * [`time`]: [`FixedClock`], the fixed-step simulation clock (integer nanoseconds,
//!   at most five catch-up steps per frame);
//! * [`phase`]: the [`Phase`] enum, the order of work inside a frame (ADR-0009);
//! * [`task`]: [`TaskPool`], the one rayon worker pool (ADR-0007);
//! * [`events`]: [`Events<T>`], typed double-buffered event queues;
//! * [`profile`]: the [`profile_scope!`] macro over puffin or Tracy (ADR-0021);
//! * [`log`]: [`init_logging`] and the `tracing` subscriber setup.
//!
//! `re_core` depends on nothing engine-specific. Diagnostic codes owned by this
//! crate lie in `RE0000`–`RE0499`; each one has a page in `explain/` and an entry
//! in [`DIAGNOSTICS`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod codes;
pub mod diag;
pub mod events;
pub mod files;
pub mod log;
pub mod phase;
pub mod profile;
pub mod task;
pub mod time;

pub use diag::{
    Code, DiagInfo, Diagnostic, Location, Severity, render_human_all, render_json_lines,
    validate_catalog,
};
pub use events::{EventCursor, Events};
pub use files::{
    FileData, FileError, FileReader, Form, ProjectFiles, ProjectPath, Writable, WritableDir,
};
pub use log::{LogConfig, init_logging};
pub use phase::Phase;
pub use task::TaskPool;
pub use time::{FixedClock, Steps};

/// The operating system's id of this process, or 0 in a browser, which has none
/// (`std::process::id` panics on wasm32-unknown-unknown, ADR-0049 §5).
pub fn process_id() -> u32 {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    {
        std::process::id()
    }
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        0
    }
}

/// Every diagnostic code `re_core` owns, with its title and explain page.
///
/// `re_cli` collects this list (with those of the other crates) for `real explain`.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::LOG_FILTER_INVALID,
        title: "Invalid log filter",
        page: include_str!("../explain/RE0001.md"),
    },
    DiagInfo {
        code: codes::LOG_ALREADY_INITIALISED,
        title: "Logging is already initialised",
        page: include_str!("../explain/RE0002.md"),
    },
    DiagInfo {
        code: codes::TASK_POOL_BUILD_FAILED,
        title: "The worker pool could not be created",
        page: include_str!("../explain/RE0003.md"),
    },
    DiagInfo {
        code: codes::TASK_POOL_ALREADY_INITIALISED,
        title: "The global worker pool is already initialised",
        page: include_str!("../explain/RE0004.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 0..=499;

#[doc(hidden)]
pub mod __private {
    //! Re-exports used by [`profile_scope!`](crate::profile_scope); not public API.
    #[cfg(feature = "profile-puffin")]
    pub use puffin;
    #[cfg(feature = "profile-tracy")]
    pub use tracy_client;
}
