//! Hot-path profiling through [`profile_scope!`](crate::profile_scope) (ADR-0021).
//!
//! `profile_scope!("name")` records the rest of the enclosing block as a scope:
//!
//! * a puffin scope when `re_core` is built with the `profile-puffin` feature;
//! * a Tracy zone with the `profile-tracy` feature (only while a Tracy client runs);
//! * nothing at all otherwise, so the default build pays no cost.
//!
//! `tracing` spans are for logs and diagnostics, not hot-path timing: the platform
//! spike measured them at about fifty times the cost of a puffin or Tracy scope.
//!
//! ```
//! fn step_physics() {
//!     re_core::profile_scope!("physics.step");
//!     // ... the work being timed ...
//! }
//! # step_physics();
//! ```

/// Profiles the rest of the enclosing block under a literal name.
///
/// Expands to a puffin scope (feature `profile-puffin`), a Tracy zone (feature
/// `profile-tracy`), both, or nothing. See the [module docs](crate::profile).
#[macro_export]
macro_rules! profile_scope {
    ($name:literal) => {
        $crate::__profile_scope_puffin!($name);
        $crate::__profile_scope_tracy!($name);
    };
}

#[cfg(feature = "profile-puffin")]
#[doc(hidden)]
#[macro_export]
macro_rules! __profile_scope_puffin {
    ($name:literal) => {
        $crate::__private::puffin::profile_scope!($name);
    };
}

#[cfg(not(feature = "profile-puffin"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __profile_scope_puffin {
    ($name:literal) => {};
}

#[cfg(feature = "profile-tracy")]
#[doc(hidden)]
#[macro_export]
macro_rules! __profile_scope_tracy {
    ($name:literal) => {
        let _re_tracy_zone = $crate::__private::tracy_client::Client::running()
            .map(|client| client.span($crate::__private::tracy_client::span_location!($name), 0));
    };
}

#[cfg(not(feature = "profile-tracy"))]
#[doc(hidden)]
#[macro_export]
macro_rules! __profile_scope_tracy {
    ($name:literal) => {};
}

/// Turns on the profiler backends that were compiled in: enables puffin scopes and
/// starts the Tracy client (which then waits for a viewer to connect). Does nothing
/// in the default build.
pub fn enable() {
    #[cfg(feature = "profile-puffin")]
    puffin::set_scopes_on(true);
    #[cfg(feature = "profile-tracy")]
    {
        let _client = tracy_client::Client::start();
    }
}

/// Marks the end of a frame for the compiled-in profiler backends. Called once per
/// frame in `EndFrame`. Does nothing in the default build.
pub fn finish_frame() {
    #[cfg(feature = "profile-puffin")]
    puffin::GlobalProfiler::lock().new_frame();
    #[cfg(feature = "profile-tracy")]
    if let Some(client) = tracy_client::Client::running() {
        client.frame_mark();
    }
}

/// True when at least one profiler backend is compiled in.
pub const fn is_compiled_in() -> bool {
    cfg!(any(feature = "profile-puffin", feature = "profile-tracy"))
}
