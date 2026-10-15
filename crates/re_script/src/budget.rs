//! The CPU budget: a raw Luau interrupt installed straight into `lua_Callbacks`
//! (ADR-0011).
//!
//! `Lua::set_interrupt` routes every safepoint (loop back edge, call, return)
//! through mlua's callback machinery; the spike measured it at about twice the cost
//! of this plain `extern "C-unwind"` function, which bumps a thread-local counter
//! and reads the clock only every [`CHECK_EVERY`] safepoints. The deadline lives in
//! a thread-local published by [`arm`], which is sound because a VM only runs on
//! the thread that owns it (mlua's `send` feature is off).
//!
//! Once the deadline passes the budget is **tripped**, and from then on every
//! safepoint raises until [`arm`] or [`disarm`] resets it. A `pcall` inside a
//! runaway loop therefore cannot swallow the timeout: the loop's own back edge
//! raises outside the `pcall`.
//!
//! The clock is read only every [`CHECK_EVERY`] safepoints, so one long builtin
//! call (`table.sort` on a huge table) can overrun the budget before it is
//! noticed; the next safepoint after it stops the script.

use std::cell::Cell;
use std::os::raw::c_int;
use std::time::Instant;

use mlua::{Lua, ffi};

/// How many safepoints pass between two clock reads.
pub const CHECK_EVERY: u32 = 1024;

const MESSAGE: &std::ffi::CStr = c"script exceeded its time budget";

thread_local! {
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    static TRIPPED: Cell<bool> = const { Cell::new(false) };
    static COUNTER: Cell<u32> = const { Cell::new(0) };
}

unsafe extern "C-unwind" fn interrupt(state: *mut ffi::lua_State, gc: c_int) {
    if gc >= 0 {
        return; // a GC safepoint: never raise from here
    }
    let check = COUNTER.with(|c| {
        let n = c.get().wrapping_add(1);
        c.set(n);
        n % CHECK_EVERY == 0
    });
    let tripped = TRIPPED.with(Cell::get)
        || (check && DEADLINE.with(Cell::get).is_some_and(|d| Instant::now() > d) && {
            TRIPPED.with(|t| t.set(true));
            true
        });
    if tripped {
        // SAFETY: `state` is the running Luau thread handed to the interrupt;
        // luaL_error raises an ordinary Luau error and does not return.
        unsafe { ffi::luaL_error(state, MESSAGE.as_ptr()) };
    }
}

/// Installs the interrupt on this VM's main state (it covers every coroutine).
pub(crate) fn install(lua: &Lua) -> mlua::Result<()> {
    // SAFETY: only the `interrupt` field of the VM's callbacks is written, with a
    // function of the right signature, while no Luau code runs.
    unsafe {
        lua.exec_raw::<()>((), |state| {
            (*ffi::lua_callbacks(state)).interrupt = Some(interrupt);
        })
    }
}

/// The budget state [`arm`] replaced, for [`restore`].
#[must_use = "restore the previous budget when the budgeted call returns"]
pub(crate) struct Saved {
    deadline: Option<Instant>,
    tripped: bool,
}

/// Starts a budget on the current thread: scripts running after this are stopped
/// once `deadline` passes. Returns the previous state, which [`restore`] puts
/// back, so a budgeted call nested in another (two contexts, or two runtimes, on
/// one thread) leaves the outer budget as it was.
pub(crate) fn arm(deadline: Instant) -> Saved {
    let saved = Saved {
        deadline: DEADLINE.with(|d| d.replace(Some(deadline))),
        tripped: TRIPPED.with(|t| t.replace(false)),
    };
    COUNTER.with(|c| c.set(0));
    saved
}

/// Puts back the budget [`arm`] replaced (normally none: Rust-side work such as
/// loading never trips it).
pub(crate) fn restore(saved: Saved) {
    DEADLINE.with(|d| d.set(saved.deadline));
    TRIPPED.with(|t| t.set(saved.tripped));
}

/// Runs `f` outside the budget: the deadline moves back by the time `f` took.
/// For Rust-side work a script asks for that is not script time, such as
/// compiling a module it requires.
pub fn untimed<R>(f: impl FnOnce() -> R) -> R {
    let start = Instant::now();
    let r = f();
    let spent = start.elapsed();
    DEADLINE.with(|d| d.set(d.get().map(|t| t + spent)));
    r
}
