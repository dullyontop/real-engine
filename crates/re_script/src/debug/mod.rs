//! The script debugger (ADR-0045): breakpoints, stepping, the call stack and
//! locals, for the editor's debugger panel and for DAP clients
//! ([`dap::DapServer`]).
//!
//! **How it works.** Breakpoints are Luau's own: `lua_breakpoint` patches a
//! `LOP_BREAK` into the first instruction of a line, and the VM's
//! `debugbreak` callback runs when it executes. Nothing else in the
//! interpreter changes, so a context with the debugger attached and
//! breakpoints set runs at the same speed until one is hit. Step over and
//! step out set temporary breakpoints on every line of the functions on the
//! stack and stop only in the same frame or a shallower one; step into stops
//! at the next line run anywhere.
//!
//! **Two sides.** A [`Debugger`] is the client's handle: it can be cloned and
//! sent to another thread (the DAP server's, the editor's). It holds the
//! breakpoints, the stop being shown, and the commands for it. Each script VM
//! is [attached](Debugger::attach) on its own thread; [`register_chunk`]
//! (called where a chunk is loaded) makes a file's breakpoints reach it, and
//! [`service`] (called between frames) applies breakpoint changes and
//! carries out commands for a parked thread.
//!
//! **Stopping.** At a breakpoint the stack, locals and upvalues are read
//! inside the callback, raw (no metamethod runs), into a [`Stopped`]
//! snapshot. Then:
//! * where the VM's host can park the thread ([`Parking`]: scheduler threads),
//!   the thread yields with no values and waits, parked, until a command
//!   arrives; the VM thread keeps running everything else;
//! * elsewhere, when a client [waits for stops](Debugger::set_client), the
//!   script waits inside the callback until a command arrives (a
//!   synchronous stop), with the CPU budget's deadline moved by the time spent
//!   paused;
//! * elsewhere with no client waiting, the stop is recorded
//!   ([`Debugger::last_unpaused`]) with an `RE3070` warning and the script
//!   continues, so an editor without a DAP client never freezes.
//!
//! Locals need debug level 2 (`compile::compiler` keeps level 1 until the
//! runtime is switched for development runs).

pub mod dap;
mod session;
#[cfg(test)]
mod tests;
mod values;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::Duration;

use mlua::Lua;
use re_core::{Code, Diagnostic};

pub use session::{is_parked, register_chunk, service, unregister_chunk};

/// The script has stopped at a breakpoint or step where it could not pause
/// (no client waits for synchronous stops and the thread cannot be parked);
/// its stack was recorded and it continued.
pub const NOT_PAUSED: Code = Code::new("RE3070");

/// The DAP server could not listen on its port, or a client sent a message
/// it could not read.
pub const DAP: Code = Code::new("RE3071");

/// A script context known to the debugger (a DAP "thread").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextInfo {
    /// Its id, from 1.
    pub id: u32,
    /// Its name: `game`, `tools`, `server`, `client`.
    pub name: String,
}

/// What the person asks a stopped script to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resume {
    /// Run until the next breakpoint.
    Continue,
    /// Stop at the next line of this function or of its caller.
    StepOver,
    /// Stop at the next line run, in a called function if there is one.
    StepIn,
    /// Stop at the next line of the caller.
    StepOut,
}

/// Why the script stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// A breakpoint.
    Breakpoint,
    /// The end of a step.
    Step,
    /// A pause request.
    Pause,
}

impl StopReason {
    /// The reason's DAP name.
    pub fn dap_name(self) -> &'static str {
        match self {
            StopReason::Breakpoint => "breakpoint",
            StopReason::Step => "step",
            StopReason::Pause => "pause",
        }
    }
}

/// One frame of a stopped thread, innermost first.
#[derive(Clone, Debug, PartialEq)]
pub struct StackFrame {
    /// The function's name (`main chunk` for a chunk's top level).
    pub name: String,
    /// The project-relative path of its chunk; `None` for a Rust function.
    pub source: Option<String>,
    /// The current line; `None` for a Rust function.
    pub line: Option<u32>,
    /// Its scopes: locals, then upvalues when it has any.
    pub scopes: Vec<Scope>,
}

/// A frame's locals or upvalues.
#[derive(Clone, Debug, PartialEq)]
pub struct Scope {
    /// `Locals` or `Upvalues`.
    pub name: String,
    /// Its variables: an index into [`Stopped::variables`].
    pub variables: usize,
}

/// One variable or table entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Variable {
    /// The local's name, or the table key (`name`, `[1]`, `["a b"]`).
    pub name: String,
    /// The value as text, read without running Luau.
    pub value: String,
    /// Its Luau type (`number`, `table`, …; a userdata's `typeof`).
    pub type_name: String,
    /// For a table: its entries, an index into [`Stopped::variables`]
    /// (`None` below the expansion depth).
    pub children: Option<usize>,
}

/// A stopped script, as read when it stopped.
#[derive(Clone, Debug, PartialEq)]
pub struct Stopped {
    /// The context it runs in.
    pub context: u32,
    /// Why it stopped.
    pub reason: StopReason,
    /// The stack, innermost frame first.
    pub frames: Vec<StackFrame>,
    /// Every variable list: scopes and table entries.
    pub variables: Vec<Vec<Variable>>,
}

impl Stopped {
    /// The variables of list `index` (a scope's or a table's), empty when
    /// there is no such list.
    pub fn list(&self, index: usize) -> &[Variable] {
        self.variables.get(index).map_or(&[], Vec::as_slice)
    }

    /// The innermost frame with a source line: where the script stopped.
    pub fn top(&self) -> Option<&StackFrame> {
        self.frames.iter().find(|f| f.line.is_some())
    }

    /// The local `name` of the innermost frame with a line, for tests and
    /// quick looks.
    pub fn local(&self, name: &str) -> Option<&Variable> {
        let frame = self.top()?;
        let scope = frame.scopes.iter().find(|s| s.name == "Locals")?;
        self.list(scope.variables).iter().find(|v| v.name == name)
    }
}

/// A breakpoint as the debugger reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breakpoint {
    /// Its id, from 1, unique for this debugger.
    pub id: u32,
    /// The file, project-relative.
    pub source: String,
    /// The line asked for.
    pub requested: u32,
    /// The line it stops on (the next line with code), once a loaded chunk of
    /// the file has it; `None` while no loaded chunk has code at or after the
    /// requested line.
    pub line: Option<u32>,
}

/// Something that happened, for a client to show.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A context attached.
    ContextStarted(ContextInfo),
    /// A context detached.
    ContextExited(u32),
    /// A script stopped; [`Debugger::stopped`] has the snapshot.
    Stopped {
        /// The context.
        context: u32,
        /// Why.
        reason: StopReason,
    },
    /// The stopped script runs again.
    Continued {
        /// The context.
        context: u32,
    },
    /// A breakpoint was placed on a line, or lost its line.
    BreakpointChanged(Breakpoint),
    /// A diagnostic, such as `RE3070` for a stop that could not pause.
    Diagnostic(Diagnostic),
}

/// How a context's host parks a thread the debugger stopped, so that the
/// VM thread keeps running while it waits (ADR-0045: the scheduler parks
/// script threads). Without one, every stop is synchronous.
///
/// After such a yield the host asks [`is_parked`] and, when it says so, keeps
/// the thread instead of treating the yield as a plain `coroutine.yield()`.
pub trait Parking {
    /// Whether `thread` (a `lua_State` address) can be stopped by a yield of
    /// no values and parked until [`Parking::resume`]. Asked inside the
    /// breakpoint callback, only when the thread can yield at all.
    fn can_park(&self, lua: &Lua, thread: usize) -> bool;

    /// Resumes the parked `thread` now (it runs to its next yield or stop).
    fn resume(&self, lua: &Lua, thread: usize);
}

/// Kept in an attached VM's app data: when the VM is dropped without
/// [`Debugger::detach`], its context ends with it.
struct ContextGuard {
    debugger: Debugger,
    context: u32,
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        self.debugger.forget(self.context);
    }
}

#[derive(Default)]
struct State {
    next_context: u32,
    contexts: Vec<ContextInfo>,
    next_breakpoint: u32,
    /// Breakpoints by file.
    breakpoints: BTreeMap<String, Vec<Breakpoint>>,
    /// Where each context placed each breakpoint (id → context → line): a
    /// breakpoint is placed when any context has it.
    placements: BTreeMap<u32, BTreeMap<u32, Option<u32>>>,
    /// Bumped on every breakpoint change, so each VM re-applies them once.
    generation: u64,
    stopped: Option<Stopped>,
    /// The command for the stopped script.
    resume: Option<Resume>,
    pause: bool,
    client: bool,
    last_unpaused: Option<Stopped>,
    events: VecDeque<Event>,
}

/// At most this many events wait for a reader; older ones are dropped.
const MAX_EVENTS: usize = 1000;

impl State {
    fn event(&mut self, e: Event) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(e);
    }
}

struct Shared {
    state: Mutex<State>,
    /// Signalled when a command arrives or an event is queued.
    signal: Condvar,
}

/// The debugger: the handle clients use (cloneable and `Send`). See the
/// [module documentation](self).
#[derive(Clone)]
pub struct Debugger {
    shared: Arc<Shared>,
}

impl Default for Debugger {
    fn default() -> Self {
        Debugger::new()
    }
}

impl std::fmt::Debug for Debugger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = self.lock();
        f.debug_struct("Debugger")
            .field("contexts", &s.contexts)
            .field("stopped", &s.stopped.as_ref().map(|s| s.context))
            .finish_non_exhaustive()
    }
}

impl Debugger {
    /// A debugger with no contexts and no breakpoints.
    pub fn new() -> Debugger {
        Debugger {
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                signal: Condvar::new(),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves plain data behind; keep going.
        self.shared.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn notify(&self) {
        self.shared.signal.notify_all();
    }

    /// Attaches the VM `lua` as the context `name`, on the thread that runs
    /// it. Its chunks reach breakpoints once they are [registered](register_chunk).
    /// `parking` lets stops in its scheduler threads park instead of blocking.
    ///
    /// # Errors
    ///
    /// When the VM's callbacks cannot be installed (never in practice).
    pub fn attach(
        &self,
        lua: &Lua,
        name: &str,
        parking: Option<Box<dyn Parking>>,
    ) -> mlua::Result<ContextInfo> {
        let info = {
            let mut s = self.lock();
            s.next_context += 1;
            let info = ContextInfo {
                id: s.next_context,
                name: name.to_string(),
            };
            s.contexts.push(info.clone());
            s.event(Event::ContextStarted(info.clone()));
            info
        };
        self.notify();
        // A VM attached again leaves its old context.
        if let Some((old, id)) = session::detach(lua) {
            old.forget(id);
        }
        session::attach(lua, self.clone(), info.id, parking)?;
        // Dropping the VM without `detach` still ends its context.
        let _ = lua.set_app_data(ContextGuard {
            debugger: self.clone(),
            context: info.id,
        });
        Ok(info)
    }

    /// Detaches `lua`: its breakpoints are removed and a thread it had
    /// stopped is resumed.
    pub fn detach(&self, lua: &Lua) {
        if let Some((debugger, id)) = session::detach(lua) {
            debugger.forget(id);
        }
        let _ = lua.remove_app_data::<ContextGuard>();
    }

    /// Ends the context `id`: its placements and a stop of it are dropped.
    /// Does nothing for a context already ended.
    fn forget(&self, id: u32) {
        let mut s = self.lock();
        if !s.contexts.iter().any(|c| c.id == id) {
            return;
        }
        s.contexts.retain(|c| c.id != id);
        for per in s.placements.values_mut() {
            per.remove(&id);
        }
        if s.stopped.as_ref().is_some_and(|st| st.context == id) {
            s.stopped = None;
            s.resume = None;
        }
        s.event(Event::ContextExited(id));
        drop(s);
        self.notify();
    }

    /// The attached contexts.
    pub fn contexts(&self) -> Vec<ContextInfo> {
        self.lock().contexts.clone()
    }

    /// Replaces the breakpoints of `source` (a project-relative path) with
    /// `lines`. Returns them with their ids; each VM places them at its next
    /// [`service`] and reports the line each snapped to with
    /// [`Event::BreakpointChanged`].
    pub fn set_breakpoints(&self, source: &str, lines: &[u32]) -> Vec<Breakpoint> {
        let mut s = self.lock();
        let mut list = Vec::new();
        for &line in lines {
            s.next_breakpoint += 1;
            list.push(Breakpoint {
                id: s.next_breakpoint,
                source: source.to_string(),
                requested: line,
                line: None,
            });
        }
        if let Some(old) = s.breakpoints.get(source) {
            let ids: Vec<u32> = old.iter().map(|b| b.id).collect();
            for id in ids {
                s.placements.remove(&id);
            }
        }
        if list.is_empty() {
            s.breakpoints.remove(source);
        } else {
            s.breakpoints.insert(source.to_string(), list.clone());
        }
        s.generation += 1;
        drop(s);
        self.notify();
        list
    }

    /// Every breakpoint, by file.
    pub fn breakpoints(&self) -> Vec<Breakpoint> {
        self.lock()
            .breakpoints
            .values()
            .flatten()
            .cloned()
            .collect()
    }

    /// Whether a client waits for stops: then a stop that cannot park waits
    /// inside the script (synchronously) for its command, instead of being
    /// recorded and continued. A DAP client sets this while attached.
    pub fn set_client(&self, waiting: bool) {
        let mut s = self.lock();
        s.client = waiting;
        if !waiting && s.stopped.is_some() && s.resume.is_none() {
            s.resume = Some(Resume::Continue);
        }
        drop(s);
        self.notify();
    }

    /// The stopped script's snapshot, if one is stopped.
    pub fn stopped(&self) -> Option<Stopped> {
        self.lock().stopped.clone()
    }

    /// The last stop that could not pause (see [`NOT_PAUSED`]).
    pub fn last_unpaused(&self) -> Option<Stopped> {
        self.lock().last_unpaused.clone()
    }

    /// Resumes the stopped script. False when none is stopped.
    pub fn resume(&self, how: Resume) -> bool {
        let mut s = self.lock();
        if s.stopped.is_none() || s.resume.is_some() {
            return false;
        }
        s.resume = Some(how);
        drop(s);
        self.notify();
        true
    }

    /// Asks the scripts to stop at the next line any of them runs (nothing
    /// while one is stopped).
    pub fn pause(&self) {
        let mut s = self.lock();
        if s.stopped.is_none() {
            s.pause = true;
        }
        drop(s);
        self.notify();
    }

    /// The events so far, oldest first, removed from the queue.
    pub fn take_events(&self) -> Vec<Event> {
        self.lock().events.drain(..).collect()
    }

    /// Waits up to `timeout` for an event and returns the queued events
    /// (empty after a timeout).
    pub fn wait_events(&self, timeout: Duration) -> Vec<Event> {
        let s = self.lock();
        let (mut s, _) = self
            .shared
            .signal
            .wait_timeout_while(s, timeout, |s| s.events.is_empty())
            .unwrap_or_else(|e| e.into_inner());
        s.events.drain(..).collect()
    }

    // ---- the VM side ------------------------------------------------------

    /// Publishes a stop. False, and nothing changes, when a script is
    /// already stopped (one stop at a time, across every context).
    fn publish_stop(&self, stopped: Stopped) -> bool {
        let mut s = self.lock();
        if s.stopped.is_some() {
            return false;
        }
        let event = Event::Stopped {
            context: stopped.context,
            reason: stopped.reason,
        };
        s.stopped = Some(stopped);
        s.resume = None;
        s.pause = false;
        s.event(event);
        drop(s);
        self.notify();
        true
    }

    /// The queued events, taken, and the current stop, read at the same
    /// moment: a client that connects sends that stop, then later events.
    fn take_events_and_stop(&self) -> (Vec<Event>, Option<Stopped>) {
        let mut s = self.lock();
        (s.events.drain(..).collect(), s.stopped.clone())
    }

    fn record_unpaused(&self, stopped: Stopped) {
        let top = stopped
            .top()
            .map(|f| {
                format!(
                    "{}:{}",
                    f.source.as_deref().unwrap_or("?"),
                    f.line.unwrap_or(0)
                )
            })
            .unwrap_or_default();
        let d = Diagnostic::warning(
            NOT_PAUSED,
            format!("the script reached a breakpoint at {top} where it cannot pause; its stack was recorded and it continued"),
        )
        .with_hint("attach a DAP client to stop here (the script then waits for it), or set the breakpoint in a script thread");
        let mut s = self.lock();
        s.last_unpaused = Some(stopped);
        s.pause = false;
        s.event(Event::Diagnostic(d));
        drop(s);
        self.notify();
    }

    /// The command for the stop of `context`, taken; the stop is cleared.
    fn take_resume(&self, context: u32) -> Option<Resume> {
        let mut s = self.lock();
        if s.stopped.as_ref().is_none_or(|st| st.context != context) {
            return None;
        }
        let how = s.resume.take()?;
        s.stopped = None;
        s.event(Event::Continued { context });
        drop(s);
        self.notify();
        Some(how)
    }

    /// Blocks until the stop of `context` gets a command (or the client
    /// leaves, which continues it).
    fn wait_resume(&self, context: u32) -> Resume {
        let mut s = self.lock();
        loop {
            if s.stopped.as_ref().is_none_or(|st| st.context != context) {
                return Resume::Continue; // detached while stopped
            }
            if s.resume.is_some() || !s.client {
                let how = s.resume.take().unwrap_or(Resume::Continue);
                s.stopped = None;
                s.event(Event::Continued { context });
                drop(s);
                self.notify();
                return how;
            }
            s = self
                .shared
                .signal
                .wait(s)
                .unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Whether a script is stopped (in any context).
    fn has_stop(&self) -> bool {
        self.lock().stopped.is_some()
    }

    fn client_waiting(&self) -> bool {
        self.lock().client
    }

    fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Whether a pause was asked for and no script has stopped since.
    fn pause_requested(&self) -> bool {
        let s = self.lock();
        s.pause && s.stopped.is_none()
    }

    fn breakpoint_snapshot(&self) -> (u64, Vec<Breakpoint>) {
        let s = self.lock();
        (
            s.generation,
            s.breakpoints.values().flatten().cloned().collect(),
        )
    }

    /// Records the line `context` placed a breakpoint on.
    fn placed(&self, context: u32, id: u32, line: Option<u32>) {
        let mut s = self.lock();
        let per = s.placements.entry(id).or_default();
        per.insert(context, line);
        let line = per.values().flatten().min().copied();
        let Some(bp) = s.breakpoints.values_mut().flatten().find(|b| b.id == id) else {
            return;
        };
        if bp.line == line {
            return;
        }
        bp.line = line;
        let bp = bp.clone();
        s.event(Event::BreakpointChanged(bp));
        drop(s);
        self.notify();
    }
}
