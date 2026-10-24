//! The VM side of the debugger: the `debugbreak` and `debugstep` callbacks,
//! breakpoint patching and stepping (ADR-0045).
//!
//! Each attached VM has a [`Session`] in a thread-local map, keyed by the
//! address of its main `lua_State`; a VM only runs on the thread that owns it
//! (mlua's `send` feature is off), so the callbacks find their session there.
//! A session never holds its borrow of the map across a call out (the
//! client's lock, a [`Parking`] call, a wait).

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::os::raw::c_int;
use std::rc::Rc;

use mlua::{Function, Lua, WeakLua, ffi};

use super::values::{self, Arena};
use super::{Debugger, Parking, Resume, StopReason, Stopped};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step {
    Over,
    In,
    Out,
    Pause,
}

struct StepReq {
    kind: Step,
    /// The thread that stepped (a pause stops in any thread).
    thread: usize,
    depth: i32,
    /// The closure of the frame the step started in (step over).
    func: usize,
}

struct Session {
    debugger: Debugger,
    context: u32,
    lua: WeakLua,
    parking: Option<Rc<dyn Parking>>,
    /// Registered chunks: (project-relative path, registry reference).
    chunks: Vec<(String, c_int)>,
    /// Chunk names that are not `@path` (compat scripts are named
    /// `=Service.Script`), with the path each was registered under.
    aliases: HashMap<String, String>,
    /// User breakpoints as placed: (path, line).
    user: HashSet<(String, i32)>,
    /// The breakpoint generation last applied.
    generation: u64,
    /// Temporary breakpoints of the step in progress: (reference, line).
    temp: Vec<(c_int, i32)>,
    /// References taken for temporary breakpoints, released with them.
    temp_refs: Vec<c_int>,
    step: Option<StepReq>,
    /// A resumed thread re-executes its `LOP_BREAK`: skip that one hit
    /// (thread, depth, line).
    skip_once: Option<(usize, i32, i32)>,
    /// Step into on a parked thread: it runs in single-step mode from
    /// (thread, depth, line) until the line or the depth changes.
    single: Option<(usize, i32, i32)>,
    parked: Option<Parked>,
}

/// The thread parked at a stop.
#[derive(Clone)]
struct Parked {
    thread: usize,
    depth: i32,
    line: i32,
    source: String,
    /// A registry reference to the thread, so it stays alive (and its
    /// address valid) until it is resumed, whatever its host does with it.
    r: c_int,
}

thread_local! {
    static SESSIONS: RefCell<HashMap<usize, Session>> = RefCell::new(HashMap::new());
}

/// The address of `lua`'s main state.
fn main_of(lua: &Lua) -> usize {
    // SAFETY: `lua_mainthread` only reads the state's global pointer, which
    // is pushed as a light userdata and read back.
    unsafe {
        lua.exec_raw::<mlua::LightUserData>((), |l| {
            ffi::lua_pushlightuserdata(l, ffi::lua_mainthread(l).cast());
        })
    }
    .map_or(0, |p| p.0 as usize)
}

fn with_session<R>(main: usize, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    SESSIONS.with_borrow_mut(|m| {
        // A VM dropped without `detach` leaves its session behind, and a new
        // VM may get the same address: such a session is dropped unused (its
        // registry references belonged to the dead VM).
        if m.get(&main).is_some_and(|s| s.lua.try_upgrade().is_none()) {
            m.remove(&main);
        }
        m.get_mut(&main).map(f)
    })
}

pub(super) fn attach(
    lua: &Lua,
    debugger: Debugger,
    context: u32,
    parking: Option<Box<dyn Parking>>,
) -> mlua::Result<()> {
    let main = main_of(lua);
    detach(lua);
    SESSIONS.with_borrow_mut(|m| {
        m.insert(
            main,
            Session {
                debugger,
                context,
                lua: lua.weak(),
                parking: parking.map(Rc::from),
                chunks: Vec::new(),
                aliases: HashMap::new(),
                user: HashSet::new(),
                generation: u64::MAX,
                temp: Vec::new(),
                temp_refs: Vec::new(),
                step: None,
                skip_once: None,
                single: None,
                parked: None,
            },
        )
    });
    // SAFETY: only the two debugger fields of the VM's callbacks are written,
    // with functions of the right signature, while no Luau code runs.
    unsafe {
        lua.exec_raw::<()>((), |l| {
            let cb = ffi::lua_callbacks(l);
            (*cb).debugbreak = Some(on_break);
            (*cb).debugstep = Some(on_step);
        })
    }
}

/// Removes `lua`'s session: every breakpoint is cleared, a parked thread is
/// resumed. Returns the debugger and the context id it had.
pub(super) fn detach(lua: &Lua) -> Option<(Debugger, u32)> {
    let main = main_of(lua);
    with_session(main, |_| ())?; // drops a stale session first
    let mut s = SESSIONS.with_borrow_mut(|m| m.remove(&main))?;
    // SAFETY: the patches are undone on the VM's own thread while no Luau
    // code runs; every reference taken is released once.
    let _ = unsafe {
        lua.exec_raw::<()>((), |l| {
            clear_temp(l, &mut s);
            for (src, line) in std::mem::take(&mut s.user) {
                for (n, r) in &s.chunks {
                    if *n == src {
                        patch(l, *r, line, false);
                    }
                }
            }
            for (_, r) in s.chunks.drain(..) {
                ffi::lua_unref(l, r);
            }
            if let Some(p) = &s.parked {
                ffi::lua_unref(l, p.r);
            }
            // A thread still in single-step mode keeps running normally: with
            // no `debugstep` callback the flag only slows it. (Its address
            // may be stale, so it is never touched here.)
            let cb = ffi::lua_callbacks(l);
            (*cb).debugbreak = None;
            (*cb).debugstep = None;
        })
    };
    if let (Some(p), Some(parking)) = (&s.parked, &s.parking) {
        parking.resume(lua, p.thread);
    }
    Some((s.debugger, s.context))
}

/// Registers a loaded chunk under its project-relative `path`, so that the
/// file's breakpoints reach it and every function defined in it. Called
/// where a chunk is loaded (module and script loaders, `load_chunk`); does
/// nothing when no debugger is attached to `lua`.
pub fn register_chunk(lua: &Lua, path: &str, chunk: &Function) {
    let main = main_of(lua);
    if with_session(main, |_| ()).is_none() {
        return;
    }
    // SAFETY: the chunk's name is read from a copy of the function mlua
    // pushed (a negative level reads the function on top) and `lua_ref` takes
    // a reference to it; both are popped, and the reference and the name are
    // left as the results.
    let r = unsafe {
        lua.exec_raw::<(c_int, String)>(chunk.clone(), |l| {
            ffi::lua_checkstack(l, 3);
            ffi::lua_pushvalue(l, -1);
            let mut ar: ffi::lua_Debug = std::mem::zeroed();
            let name = if ffi::lua_getinfo(l, -1, c"s".as_ptr(), &mut ar) != 0 {
                values::cstr(ar.source)
            } else {
                None
            };
            let r = ffi::lua_ref(l, -1);
            ffi::lua_pop(l, 2);
            ffi::lua_pushinteger(l, r as _);
            let name = std::ffi::CString::new(name.unwrap_or_default()).unwrap_or_default();
            ffi::lua_pushstring(l, name.as_ptr());
        })
    };
    let Ok((r, name)) = r else { return };
    let placed = with_session(main, |s| {
        s.chunks.push((path.to_string(), r));
        if values::source_path(&name).as_deref() != Some(path) && !name.is_empty() {
            s.aliases.insert(name, path.to_string());
        }
        let (_, bps) = s.debugger.breakpoint_snapshot();
        let mut placed = Vec::new();
        for bp in bps.iter().filter(|b| b.source == path) {
            // SAFETY: as in `apply_breakpoints`.
            let line = unsafe {
                lua.exec_raw::<i32>((), |l| {
                    ffi::lua_pushinteger(l, patch(l, r, bp.requested as i32, true) as _)
                })
            }
            .unwrap_or(-1);
            if line >= 0 {
                s.user.insert((path.to_string(), line));
                placed.push((bp.id, Some(line as u32)));
            }
        }
        (s.debugger.clone(), s.context, placed)
    });
    if let Some((debugger, context, placed)) = placed {
        for (id, line) in placed {
            debugger.placed(context, id, line);
        }
    }
}

/// Forgets a chunk [registered](register_chunk) before, when the script it
/// ran has ended (its entity was destroyed, it was reloaded), so the debugger
/// stops holding it. Its breakpoints are removed. Does nothing for a chunk
/// that was not registered.
pub fn unregister_chunk(lua: &Lua, chunk: &Function) {
    let main = main_of(lua);
    with_session(main, |s| {
        // SAFETY: on the VM's thread while no Luau code runs: the function is
        // compared raw with each registered one, which is pushed and popped.
        let _ = unsafe {
            lua.exec_raw::<()>(chunk.clone(), |l| {
                ffi::lua_checkstack(l, 2);
                let mut i = 0;
                while i < s.chunks.len() {
                    let (path, r) = s.chunks[i].clone();
                    ffi::lua_rawgeti(l, ffi::LUA_REGISTRYINDEX, r as _);
                    let same = ffi::lua_rawequal(l, -1, -2) != 0;
                    ffi::lua_pop(l, 1);
                    if same {
                        for (src, line) in &s.user {
                            if *src == path {
                                patch(l, r, *line, false);
                            }
                        }
                        ffi::lua_unref(l, r);
                        s.chunks.remove(i);
                        if !s.chunks.iter().any(|(p, _)| *p == path) {
                            s.aliases.retain(|_, p| *p != path);
                        }
                    } else {
                        i += 1;
                    }
                }
                ffi::lua_pop(l, 1);
            })
        };
    });
}

/// Between frames, on the VM's thread: places breakpoint changes, carries out
/// a command for a thread parked at a stop, and arms a pause request. Does
/// nothing when no debugger is attached to `lua`.
pub fn service(lua: &Lua) {
    let main = main_of(lua);
    let Some((debugger, generation, context)) =
        with_session(main, |s| (s.debugger.clone(), s.generation, s.context))
    else {
        return;
    };
    let (current, bps) = debugger.breakpoint_snapshot();
    if current != generation {
        let placed = with_session(main, |s| {
            s.generation = current;
            let mut placed = Vec::new();
            // SAFETY: patching on the VM's own thread while no Luau code runs.
            let _ =
                unsafe { lua.exec_raw::<()>((), |l| apply_breakpoints(l, s, &bps, &mut placed)) };
            placed
        });
        for (id, line) in placed.into_iter().flatten() {
            debugger.placed(context, id, line);
        }
    }

    let parked = with_session(main, |s| s.parked.clone()).flatten();
    if let Some(Parked {
        thread,
        depth,
        line,
        source,
        r,
    }) = parked
    {
        let Some(how) = debugger.take_resume(context) else {
            return;
        };
        let parking = with_session(main, |s| {
            s.parked = None;
            // The resumed thread runs its stopped instruction again; while
            // that is still a breakpoint (the user's, or a step's temporary
            // one), its first hit is the stop itself and is skipped.
            let patched = s.user.contains(&(source.clone(), line))
                || matches!(how, Resume::StepOver | Resume::StepOut);
            s.skip_once = patched.then_some((thread, depth, line));
            if how == Resume::StepIn {
                // The thread is suspended, so its next resume enters the
                // interpreter afresh and picks the single-step variant.
                s.single = Some((thread, depth, line));
                // SAFETY: `thread` is the suspended thread the host keeps parked.
                unsafe { ffi::lua_singlestep(thread as *mut ffi::lua_State, 1) };
            } else {
                // SAFETY: patching through the main state; the suspended
                // thread's stack is only read (its functions are moved over).
                let _ = unsafe {
                    lua.exec_raw::<()>((), |l| arm_step(l, thread as *mut ffi::lua_State, s, how))
                };
            }
            s.parking.clone()
        })
        .flatten();
        if let Some(parking) = parking {
            parking.resume(lua, thread);
        }
        // SAFETY: the reference taken when the thread parked, released once.
        let _ = unsafe {
            lua.exec_raw::<()>((), |l| {
                ffi::lua_unref(l, r);
            })
        };
        return;
    }

    let armed = with_session(main, |s| s.step.as_ref().map(|r| r.kind)).flatten();
    if debugger.pause_requested() && armed != Some(Step::Pause) {
        with_session(main, |s| {
            // SAFETY: patching on the VM's own thread while no Luau code runs.
            // A step still armed (its thread may have finished) gives way.
            let _ = unsafe {
                lua.exec_raw::<()>((), |l| {
                    clear_temp(l, s);
                    patch_every_chunk(l, s);
                    s.step = Some(StepReq {
                        kind: Step::Pause,
                        thread: 0,
                        depth: 0,
                        func: 0,
                    });
                })
            };
        });
    }
}

/// Whether `thread` (a `lua_State` address) yielded because the debugger
/// stopped it: its host keeps it parked until [`Parking::resume`].
pub fn is_parked(lua: &Lua, thread: usize) -> bool {
    with_session(main_of(lua), |s| {
        s.parked.as_ref().is_some_and(|p| p.thread == thread)
    })
    .unwrap_or(false)
}

/// Tells the debugger attached to `lua` that its host dropped `threads`
/// (`lua_State` addresses) for good, such as the threads of a script that was
/// reloaded or cancelled. When one of them is parked at a stop, the stop ends
/// as if continued, so other scripts can stop again. Does nothing when no
/// debugger is attached to `lua`.
pub fn threads_ended(lua: &Lua, threads: &[usize]) {
    let ended = with_session(main_of(lua), |s| {
        let p = s.parked.take_if(|p| threads.contains(&p.thread))?;
        Some((s.debugger.clone(), s.context, p.r))
    })
    .flatten();
    let Some((debugger, context, r)) = ended else {
        return;
    };
    // SAFETY: the reference taken when the thread parked, released once.
    let _ = unsafe {
        lua.exec_raw::<()>((), |l| {
            ffi::lua_unref(l, r);
        })
    };
    debugger.end_stop(context);
}

/// Sets or clears the breakpoint on `line` in the chunk behind reference `r`
/// and every function nested in it. Returns the line it snapped to, or -1.
///
/// # Safety
/// `l` belongs to the VM that holds `r`, with room for one more slot.
unsafe fn patch(l: *mut ffi::lua_State, r: c_int, line: i32, on: bool) -> i32 {
    // SAFETY: the referenced function is pushed, patched and popped.
    unsafe {
        ffi::lua_rawgeti(l, ffi::LUA_REGISTRYINDEX, r as _);
        let t = ffi::lua_breakpoint(l, -1, line, on as c_int);
        ffi::lua_pop(l, 1);
        t
    }
}

/// Removes the user breakpoints and places `bps` instead.
unsafe fn apply_breakpoints(
    l: *mut ffi::lua_State,
    s: &mut Session,
    bps: &[super::Breakpoint],
    placed: &mut Vec<(u32, Option<u32>)>,
) {
    // SAFETY: as in `patch`.
    unsafe {
        for (src, line) in std::mem::take(&mut s.user) {
            for (n, r) in &s.chunks {
                if *n == src {
                    patch(l, *r, line, false);
                }
            }
        }
        for bp in bps {
            let mut actual = None;
            for (n, r) in &s.chunks {
                if *n == bp.source {
                    let t = patch(l, *r, bp.requested as i32, true);
                    if t >= 0 {
                        s.user.insert((bp.source.clone(), t));
                        actual = Some(t as u32);
                    }
                }
            }
            placed.push((bp.id, actual));
        }
        // Removing a user breakpoint removed a temporary one on its line too.
        for &(r, line) in &s.temp {
            patch(l, r, line, true);
        }
    }
}

/// Patches every line of the function on top of `l`'s stack (and of every
/// function nested in it) with a temporary breakpoint; pops the function.
unsafe fn patch_all_lines(l: *mut ffi::lua_State, s: &mut Session, from: i32) {
    // SAFETY: the function on top is referenced (and popped) so it can be
    // patched again and cleared later.
    unsafe {
        let r = ffi::lua_ref(l, -1);
        ffi::lua_pop(l, 1);
        s.temp_refs.push(r);
        let mut line = from.max(1);
        loop {
            let t = patch(l, r, line, true);
            if t < 0 {
                break;
            }
            s.temp.push((r, t));
            line = t + 1;
        }
    }
}

unsafe fn patch_every_chunk(l: *mut ffi::lua_State, s: &mut Session) {
    // SAFETY: as in `patch_all_lines`.
    unsafe {
        let refs: Vec<c_int> = s.chunks.iter().map(|(_, r)| *r).collect();
        ffi::lua_checkstack(l, 2);
        for r in refs {
            ffi::lua_rawgeti(l, ffi::LUA_REGISTRYINDEX, r as _);
            patch_all_lines(l, s, 1);
        }
    }
}

unsafe fn clear_temp(l: *mut ffi::lua_State, s: &mut Session) {
    // SAFETY: as in `patch`; each temporary reference is released once.
    unsafe {
        for (r, line) in s.temp.drain(..) {
            patch(l, r, line, false);
        }
        for r in s.temp_refs.drain(..) {
            ffi::lua_unref(l, r);
        }
        // A temporary breakpoint on a user breakpoint's line removed it; put
        // the user's back.
        for (src, line) in &s.user {
            for (n, r) in &s.chunks {
                if n == src {
                    patch(l, *r, *line, true);
                }
            }
        }
    }
}

/// The closure running at `level` of `thread`, as an identity.
unsafe fn func_at(thread: *mut ffi::lua_State, level: c_int) -> usize {
    // SAFETY: "f" pushes the closure, which is popped.
    unsafe {
        let mut ar: ffi::lua_Debug = std::mem::zeroed();
        if ffi::lua_checkstack(thread, 1) == 0
            || ffi::lua_getinfo(thread, level, c"f".as_ptr(), &mut ar) == 0
        {
            return 0;
        }
        let p = ffi::lua_topointer(thread, -1) as usize;
        ffi::lua_pop(thread, 1);
        p
    }
}

/// Arms `how` from the top frame of `thread`, patching through `l` (the
/// thread itself in a synchronous stop, else the main state, so nothing is
/// left on a suspended thread's stack).
unsafe fn arm_step(
    l: *mut ffi::lua_State,
    thread: *mut ffi::lua_State,
    s: &mut Session,
    how: Resume,
) {
    // SAFETY: functions read from `thread`'s frames are moved to `l` and
    // patched there; see `patch_all_lines`.
    unsafe {
        clear_temp(l, s);
        let kind = match how {
            Resume::Continue => {
                s.step = None;
                return;
            }
            Resume::StepOver => Step::Over,
            Resume::StepIn => Step::In,
            Resume::StepOut => Step::Out,
        };
        let depth = ffi::lua_stackdepth(thread);
        s.step = Some(StepReq {
            kind,
            thread: thread as usize,
            depth,
            func: func_at(thread, 0),
        });
        ffi::lua_checkstack(l, 4);
        ffi::lua_checkstack(thread, 4);
        if kind == Step::In {
            // Every line of every registered chunk.
            patch_every_chunk(l, s);
            return;
        }
        // Every line of every Luau function on the stack: the current one
        // (step over) and its callers (where a return lands).
        for level in 0..depth {
            let mut ar: ffi::lua_Debug = std::mem::zeroed();
            if ffi::lua_getinfo(thread, level, c"fs".as_ptr(), &mut ar) == 0 {
                break;
            }
            if !ar.what.is_null() && *ar.what == b'C' as std::os::raw::c_char {
                ffi::lua_pop(thread, 1);
                continue;
            }
            if thread != l {
                ffi::lua_xmove(thread, l, 1);
            }
            patch_all_lines(l, s, ar.linedefined);
        }
    }
}

/// What the callback decided, read while the session was borrowed.
struct Decision {
    reason: StopReason,
    debugger: Debugger,
    context: u32,
    lua: Option<Lua>,
    parking: Option<Rc<dyn Parking>>,
}

/// Reads the snapshot and stops `state` as [`Decision`] allows: parked,
/// synchronously, or recorded and continued.
///
/// # Safety
/// Called from a debug callback of `state`.
unsafe fn stop(
    state: *mut ffi::lua_State,
    main: usize,
    d: Decision,
    depth: i32,
    line: i32,
    source: String,
) {
    let thread = state as usize;
    let mut arena = Arena::default();
    arena.aliases = with_session(main, |s| s.aliases.clone()).unwrap_or_default();
    // SAFETY: inside the callback the running thread's stack is readable.
    let frames = unsafe { values::read_stack(state, &mut arena) };
    let stopped = Stopped {
        context: d.context,
        reason: d.reason,
        frames,
        variables: arena.lists,
    };
    // SAFETY: `lua_isyieldable` only reads the state.
    let yieldable = unsafe { ffi::lua_isyieldable(state) } != 0;
    let park = yieldable
        && match (&d.parking, &d.lua) {
            (Some(p), Some(lua)) => p.can_park(lua, thread),
            _ => false,
        };
    if park {
        if !d.debugger.publish_stop(stopped) {
            return; // another script stopped first
        }
        // SAFETY: the thread is pushed and referenced, then popped; the
        // callback's stack has room (luau_callhook reserves LUA_MINSTACK).
        let r = unsafe {
            ffi::lua_pushthread(state);
            let r = ffi::lua_ref(state, -1);
            ffi::lua_pop(state, 1);
            r
        };
        with_session(main, |s| {
            s.parked = Some(Parked {
                thread,
                depth,
                line,
                source,
                r,
            })
        });
        // SAFETY: the thread can yield (checked above); a yield from the
        // callback suspends it at this instruction, which its next resume
        // runs again (see `skip_once`).
        unsafe { ffi::lua_yield(state, 0) };
        return;
    }
    if !d.debugger.client_waiting() {
        d.debugger.record_unpaused(stopped);
        return;
    }
    if !d.debugger.publish_stop(stopped) {
        return; // another script stopped first
    }
    let how = crate::budget::untimed(|| d.debugger.wait_resume(d.context));
    let (_, bps) = d.debugger.breakpoint_snapshot();
    let placed = with_session(main, |s| {
        let mut placed = Vec::new();
        // SAFETY: inside the callback; the stack has room (luau_callhook
        // reserves LUA_MINSTACK slots, and `arm_step` checks for more).
        unsafe {
            if s.generation != d.debugger.generation() {
                s.generation = d.debugger.generation();
                apply_breakpoints(state, s, &bps, &mut placed);
            }
            arm_step(state, state, s, how);
        }
        placed
    });
    for (id, line) in placed.into_iter().flatten() {
        d.debugger.placed(d.context, id, line);
    }
}

unsafe extern "C-unwind" fn on_break(state: *mut ffi::lua_State, ar: *mut ffi::lua_Debug) {
    // SAFETY: Luau calls this with the running thread and a filled `ar`;
    // every read below is of that thread.
    unsafe {
        let main = ffi::lua_mainthread(state) as usize;
        let line = (*ar).currentline;
        let depth = ffi::lua_stackdepth(state);
        let thread = state as usize;
        let mut info: ffi::lua_Debug = std::mem::zeroed();
        ffi::lua_getinfo(state, 0, c"s".as_ptr(), &mut info);
        let chunk = values::cstr(info.source).unwrap_or_default();
        let source = with_session(main, |s| values::source_of(&s.aliases, &chunk))
            .flatten()
            .unwrap_or_default();

        let decision = with_session(main, |s| {
            if s.skip_once.is_some_and(|(t, ..)| t == thread) {
                let skip = s.skip_once.take() == Some((thread, depth, line));
                if skip {
                    return None;
                }
            }
            // One stop at a time: while a script is stopped (in this VM or
            // another), other threads run past breakpoints.
            if s.debugger.has_stop() {
                return None;
            }
            let reason = if s.user.contains(&(source.clone(), line)) {
                Some(StopReason::Breakpoint)
            } else {
                match &s.step {
                    None => None,
                    Some(r) => match r.kind {
                        Step::In => Some(StopReason::Step),
                        Step::Pause => s.debugger.pause_requested().then_some(StopReason::Pause),
                        Step::Out => {
                            (r.thread == thread && depth < r.depth).then_some(StopReason::Step)
                        }
                        Step::Over => (r.thread == thread
                            && (depth < r.depth
                                || (depth == r.depth && func_at(state, 0) == r.func)))
                            .then_some(StopReason::Step),
                    },
                }
            };
            let Some(reason) = reason else {
                if s.step.as_ref().is_some_and(|r| r.kind == Step::Pause)
                    && !s.debugger.pause_requested()
                {
                    // Another context took the pause.
                    clear_temp(state, s);
                    s.step = None;
                }
                return None;
            };
            clear_temp(state, s);
            s.step = None;
            if s.single.is_some_and(|(t, ..)| t == thread) {
                // A step into that reached a breakpoint first.
                s.single = None;
                ffi::lua_singlestep(state, 0);
            }
            Some(Decision {
                reason,
                debugger: s.debugger.clone(),
                context: s.context,
                lua: s.lua.try_upgrade(),
                parking: s.parking.clone(),
            })
        })
        .flatten();
        if let Some(d) = decision {
            stop(state, main, d, depth, line, source);
        }
    }
}

/// `debugstep`: runs before each instruction of a thread in single-step mode
/// (step into from a parked stop).
unsafe extern "C-unwind" fn on_step(state: *mut ffi::lua_State, ar: *mut ffi::lua_Debug) {
    // SAFETY: as in `on_break`.
    unsafe {
        let main = ffi::lua_mainthread(state) as usize;
        let line = (*ar).currentline;
        let depth = ffi::lua_stackdepth(state);
        let thread = state as usize;
        let mut info: ffi::lua_Debug = std::mem::zeroed();
        ffi::lua_getinfo(state, 0, c"s".as_ptr(), &mut info);
        let chunk = values::cstr(info.source).unwrap_or_default();
        let source = with_session(main, |s| values::source_of(&s.aliases, &chunk))
            .flatten()
            .unwrap_or_default();
        let decision = with_session(main, |s| {
            match s.single {
                Some((t, d, l)) if t == thread && (d != depth || l != line) && line >= 0 => {}
                _ => return None,
            }
            s.single = None;
            ffi::lua_singlestep(state, 0);
            clear_temp(state, s);
            s.step = None;
            Some(Decision {
                reason: StopReason::Step,
                debugger: s.debugger.clone(),
                context: s.context,
                lua: s.lua.try_upgrade(),
                parking: s.parking.clone(),
            })
        })
        .flatten();
        if let Some(d) = decision {
            stop(state, main, d, depth, line, source);
        }
    }
}
