//! Script threads, waiting and signals, driven by the frame phases (ADR-0009).
//!
//! * `task.wait(t)` is a small Luau stub that yields a private sentinel and the
//!   delay. The scheduler owns the suspended thread and resumes it in the first
//!   `PostSimulation` phase (the "heartbeat" of ADR-0009) whose simulation time has
//!   reached its wake time, passing the elapsed time back as `task.wait`'s result.
//!   Rust cannot yield a Luau thread synchronously through mlua (`yield_with` is
//!   async-only), which is why the yield lives in Luau.
//! * Waiting threads sit in a binary heap keyed by (wake time, sequence number),
//!   so equal wake times resume in the order they went to sleep. A thread that
//!   goes to sleep while the due threads are being resumed waits for the next
//!   pass, so `while true do task.wait() end` cannot spin within one step.
//! * `task.spawn` resumes a new thread at once; `task.defer` queues it to run at
//!   the end of the current resumption cycle; `task.delay` queues it on the heap.
//! * Signals (frame and step callbacks, input actions, contacts) start one thread
//!   per connection per firing, in connection order, so a callback may wait.
//! * Each resume gets its own CPU budget ([`crate::budget`]); a nested resume
//!   (`task.spawn` from a running thread) shares its caller's.
//! * Every thread and connection has an owner: the entry-script run that
//!   (directly, or through threads and callbacks it started) created it. The
//!   runtime stops an owner's threads and callbacks with [`drop_owner`] when its
//!   script is reloaded or its entity goes away.
//!
//! `task.wait` only yields from the thread the scheduler is resuming. Inside a
//! coroutine made with `coroutine.create`/`wrap` it would yield to that coroutine
//! instead, so M1 refuses it with `RE3005` (ADR-0011); `task.spawn` is the
//! supported way to run concurrent code.

use std::cmp::Reverse;
use std::collections::{BTreeMap, BinaryHeap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::Duration;

use mlua::thread::ThreadStatus;
use mlua::{
    Function, Lua, MultiValue, RegistryKey, Result, Table, Thread, UserData, UserDataFields,
    UserDataMethods, Value,
};
use re_core::time::Instant;
use re_scene::Entity;

use crate::api::{self, Lib};
use crate::budget;
use crate::codes;
use crate::diag::ScriptError;

const WAIT_SENTINEL: &str = "re_script.wait_sentinel";

/// The marker a yielding host call returns, and its wrapper yields, to park the
/// calling thread ([`park`]).
const PARK_SENTINEL: &str = "re_script.park_sentinel";

/// The marker a parked thread is woken with when its host call failed
/// ([`wake_error`]); its wrapper raises the message that follows it.
const ERROR_SENTINEL: &str = "re_script.park_error_sentinel";

/// How many times one settle pass may run the settle hook (each run may queue
/// more deferred threads).
const MAX_SETTLE_ROUNDS: usize = 1_000;

/// The most deferred threads one drain runs before it gives up (a `task.defer`
/// that keeps deferring itself would otherwise never end).
pub(crate) const MAX_DEFERS_PER_DRAIN: usize = 10_000;

/// Simulation times closer than this are equal: `now` is a sum of fixed steps,
/// so six steps of 1/60 s add up to slightly less than 0.1.
const TIME_EPSILON: f64 = 1e-6;

/// Who created a thread or connection: an entry-script run of the runtime.
pub type Owner = u64;

/// What a connection listens to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Signal {
    /// Every frame, in `PreRender`, with the frame time.
    Frame,
    /// Every fixed step, in `PreSimulation`, with the step time.
    Step,
    /// Every fixed step, in `PostSimulation`, after contacts and waits.
    PostStep,
    /// Every frame, in `LateFrame`, with the frame time.
    LateFrame,
    /// A button action went down this frame.
    Pressed(String),
    /// A button action went up this frame.
    Released(String),
    /// Any contact began: `(a, b, info)`.
    Contact,
    /// Any contact ended: `(a, b, sensor)`.
    ContactEnd,
    /// A contact involving this entity began: `(other, info)`.
    Touch(Entity),
    /// A streamed cell loaded: `(cell)`.
    StreamLoaded,
    /// A streamed cell unloaded: `(cell)`.
    StreamUnloaded,
}

impl Signal {
    fn label(&self) -> String {
        match self {
            Signal::Frame => "time.on_frame".into(),
            Signal::Step => "time.on_step".into(),
            Signal::PostStep => "time.on_post_step".into(),
            Signal::LateFrame => "time.on_late_frame".into(),
            Signal::Pressed(a) => format!("input.on_pressed({a:?})"),
            Signal::Released(a) => format!("input.on_released({a:?})"),
            Signal::Contact => "physics.on_contact".into(),
            Signal::ContactEnd => "physics.on_contact_end".into(),
            Signal::Touch(_) => "physics.on_touch".into(),
            Signal::StreamLoaded => "stream.on_loaded".into(),
            Signal::StreamUnloaded => "stream.on_unloaded".into(),
        }
    }
}

struct Connection {
    signal: Signal,
    func: RegistryKey,
    owner: Option<Owner>,
}

/// A check made just before a deferred thread first runs; false drops it.
pub type DeferCondition = Rc<dyn Fn(&Lua) -> bool>;

/// A hook run after the deferred threads drain; returns true when it queued
/// more work, so the drain runs again ([`set_settle_hook`]).
pub type SettleHook = Rc<dyn Fn(&Lua) -> Result<bool>>;

struct Task {
    thread: Thread,
    /// Checked before the first resume; the thread is dropped when it fails.
    cond: Option<DeferCondition>,
    /// Arguments of the first resume; `None` for a thread resumed from `task.wait`.
    args: Option<MultiValue>,
    slept_at: f64,
    label: String,
    owner: Option<Owner>,
}

fn ptr(t: &Thread) -> usize {
    t.to_pointer() as usize
}

/// Per-context scheduler state (stored as the VM's app data).
pub(crate) struct Scheduler {
    pub(crate) now: f64,
    pub(crate) frame_dt: f64,
    pub(crate) frame: u64,
    /// True while a fixed step's callbacks and threads run, where nothing that
    /// varies between runs (such as live audio levels) may be read.
    pub(crate) in_step: bool,
    pub(crate) budget: Duration,
    seq: u64,
    heap: BinaryHeap<Reverse<(WakeTime, u64)>>,
    sleeping: BTreeMap<u64, Task>,
    deferred: VecDeque<Task>,
    connections: BTreeMap<u64, Connection>,
    next_connection: u64,
    /// Running threads that were cancelled; they stop when they next yield.
    cancelled: HashSet<usize>,
    errors: Vec<ScriptError>,
    depth: u32,
    /// The threads being resumed, innermost last, with their owners.
    running: Vec<(usize, Option<Owner>)>,
    /// The owner of code that runs outside any scheduler thread (set by the
    /// runtime around module reloads and `exec`).
    pub(crate) ambient_owner: Option<Owner>,
    /// Threads parked by a yielding host call, by token.
    parked: BTreeMap<u64, Parked>,
    /// Threads the debugger stopped at a breakpoint or step, by thread
    /// ([`DebugParking`]).
    debug_parked: BTreeMap<usize, Task>,
    /// The live chunks of project files, for the debugger: entry scripts by
    /// the run that loaded them (forgotten when it stops, [`drop_owner`]) and
    /// modules by path (replaced when one reloads).
    debug_chunks: BTreeMap<Owner, Vec<(String, RegistryKey)>>,
    debug_modules: BTreeMap<String, RegistryKey>,
    next_token: u64,
    /// Park timeouts: (deadline, token).
    deadlines: BinaryHeap<Reverse<(WakeTime, u64)>>,
    settle: Option<SettleHook>,
    /// Compat contexts: a coroutine made with `coroutine.create` or
    /// `coroutine.wrap` may wait; it is then resumed by the scheduler, and its
    /// resumer gets control back at once (as in Roblox).
    adopt: bool,
}

/// A parked thread's slot. The slot exists from [`park`] until the thread is
/// woken, times out or is cancelled; `task` is filled when the thread yields.
struct Parked {
    task: Option<Task>,
    label: String,
    /// Set by a wake that came before the thread yielded.
    woken: Option<MultiValue>,
    /// What the thread gets when its timeout runs out.
    on_timeout: Option<MultiValue>,
    owner: Option<Owner>,
}

/// Identifies one parked thread ([`park`], [`wake`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct YieldToken(pub u64);

/// When a woken thread runs ([`wake`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakeMode {
    /// At the end of the current resumption cycle, with the deferred threads.
    Deferred,
    /// Now, before `wake` returns (a nested resume, like `task.spawn`).
    Immediate,
}

/// A wake time with a total order (never NaN: delays are sanitised).
#[derive(Clone, Copy, Debug)]
struct WakeTime(f64);
impl PartialEq for WakeTime {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for WakeTime {}
impl PartialOrd for WakeTime {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for WakeTime {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl Scheduler {
    pub(crate) fn new(budget: Duration) -> Scheduler {
        Scheduler {
            now: 0.0,
            frame_dt: 0.0,
            frame: 0,
            in_step: false,
            budget,
            seq: 0,
            heap: BinaryHeap::new(),
            sleeping: BTreeMap::new(),
            deferred: VecDeque::new(),
            connections: BTreeMap::new(),
            next_connection: 1,
            cancelled: HashSet::new(),
            errors: Vec::new(),
            depth: 0,
            running: Vec::new(),
            ambient_owner: None,
            parked: BTreeMap::new(),
            debug_parked: BTreeMap::new(),
            debug_chunks: BTreeMap::new(),
            debug_modules: BTreeMap::new(),
            next_token: 1,
            deadlines: BinaryHeap::new(),
            settle: None,
            adopt: false,
        }
    }

    fn sleep(&mut self, task: Task, delay: f64) {
        let delay = if delay.is_finite() {
            delay.max(0.0)
        } else {
            0.0
        };
        self.seq += 1;
        self.heap
            .push(Reverse((WakeTime(self.now + delay), self.seq)));
        self.sleeping.insert(self.seq, task);
    }

    /// The owner of the code running now.
    fn current_owner(&self) -> Option<Owner> {
        match self.running.last() {
            Some((_, owner)) => *owner,
            None => self.ambient_owner,
        }
    }

    /// Threads waiting in the heap.
    pub(crate) fn sleeping(&self) -> usize {
        self.sleeping.len()
    }

    /// Threads parked by yielding host calls.
    pub(crate) fn parked(&self) -> usize {
        self.parked.values().filter(|p| p.task.is_some()).count()
    }

    /// Connected callbacks.
    pub(crate) fn connections(&self) -> usize {
        self.connections.len()
    }

    pub(crate) fn take_errors(&mut self) -> Vec<ScriptError> {
        std::mem::take(&mut self.errors)
    }

    /// Removes a waiting or deferred thread; false when it is in neither. Its
    /// heap entry stays behind and is skipped.
    fn unschedule(&mut self, p: usize) -> bool {
        let before = self.sleeping.len() + self.deferred.len();
        self.sleeping.retain(|_, t| ptr(&t.thread) != p);
        self.deferred.retain(|t| ptr(&t.thread) != p);
        let parked_before = self.parked.len() + self.debug_parked.len();
        self.parked
            .retain(|_, s| s.task.as_ref().is_none_or(|t| ptr(&t.thread) != p));
        self.debug_parked.remove(&p);
        self.sleeping.len() + self.deferred.len() != before
            || self.parked.len() + self.debug_parked.len() != parked_before
    }
}

fn sched<R>(lua: &Lua, f: impl FnOnce(&mut Scheduler) -> R) -> Result<R> {
    let mut s = lua
        .app_data_mut::<Scheduler>()
        .ok_or_else(|| mlua::Error::runtime("this VM has no scheduler"))?;
    Ok(f(&mut s))
}

/// Reads or changes the scheduler of `lua`.
pub(crate) fn with<R>(lua: &Lua, f: impl FnOnce(&mut Scheduler) -> R) -> Result<R> {
    sched(lua, f)
}

/// Runs `f` (a synchronous call into Luau from Rust) under the CPU budget. A call
/// nested inside a running script shares that script's budget.
pub(crate) fn call_budgeted<R>(lua: &Lua, f: impl FnOnce() -> Result<R>) -> Result<R> {
    let (outer, budget) = sched(lua, |s| {
        s.depth += 1;
        (s.depth == 1, s.budget)
    })?;
    let saved = outer.then(|| budget::arm(Instant::now() + budget));
    let result = f();
    if let Some(saved) = saved {
        budget::restore(saved);
    }
    sched(lua, |s| s.depth -= 1)?;
    if matches!(result, Err(mlua::Error::MemoryError(_))) {
        let _ = lua.gc_collect();
    }
    result
}

/// Resumes one task now under the CPU budget and files the outcome: sleeping
/// again, finished, or an error.
fn resume(lua: &Lua, task: Task, now: f64) -> Result<()> {
    let p = ptr(&task.thread);
    if task.thread.status() != ThreadStatus::Resumable {
        return Ok(()); // already finished (resumed by hand with coroutine.resume)
    }
    let mut task = task;
    if let Some(cond) = task.cond.take()
        && !cond(lua)
    {
        return Ok(());
    }
    let args = task
        .args
        .clone()
        .unwrap_or_else(|| MultiValue::from_iter([Value::Number(now - task.slept_at)]));
    let (outer, budget) = sched(lua, |s| {
        s.depth += 1;
        s.running.push((p, task.owner));
        (s.depth == 1, s.budget)
    })?;
    let saved = outer.then(|| budget::arm(Instant::now() + budget));
    let result = task.thread.resume::<MultiValue>(args);
    if let Some(saved) = saved {
        budget::restore(saved);
    }
    let cancelled = sched(lua, |s| {
        s.depth -= 1;
        s.running.pop();
        s.cancelled.remove(&p)
    })?;
    if cancelled {
        // Cancelled while it ran: a debugger stop it parked at ends with it.
        crate::debug::threads_ended(lua, &[p]);
        return Ok(());
    }
    match result {
        // Stopped by the debugger: kept until it says to go on.
        Ok(_)
            if task.thread.status() == ThreadStatus::Resumable
                && crate::debug::is_parked(lua, p) =>
        {
            sched(lua, |s| s.debug_parked.insert(p, task))?;
        }
        Ok(yielded) if task.thread.status() == ThreadStatus::Resumable => {
            let sentinel: Table = lua.named_registry_value(WAIT_SENTINEL)?;
            let park: Table = lua.named_registry_value(PARK_SENTINEL)?;
            let mut it = yielded.into_iter();
            let (first, second) = (it.next(), it.next());
            if let (Some(Value::Table(t)), Some(token)) = (&first, &second)
                && *t == park
            {
                let token = match token {
                    Value::Integer(i) => *i as u64,
                    Value::Number(n) => *n as u64,
                    _ => 0,
                };
                return park_task(lua, task, token);
            }
            let delay = match (first, second) {
                (Some(Value::Table(t)), Some(d)) if t == sentinel => match d {
                    Value::Integer(i) => i as f64,
                    Value::Number(n) => n,
                    _ => 0.0,
                },
                // A bare coroutine.yield() in a script thread: resume on the next step.
                _ => 0.0,
            };
            sched(lua, |s| {
                let slept_at = s.now;
                s.sleep(
                    Task {
                        args: None,
                        slept_at,
                        ..task
                    },
                    delay,
                )
            })?;
        }
        Ok(_) => {}
        Err(err) => {
            let e = ScriptError::from_thread(lua, &task.thread, &task.label, &err);
            if matches!(err, mlua::Error::MemoryError(_)) {
                let _ = lua.gc_collect();
            }
            sched(lua, |s| s.errors.push(e))?;
        }
    }
    Ok(())
}

/// Files a thread that yielded to park under `token`.
fn park_task(lua: &Lua, task: Task, token: u64) -> Result<()> {
    let ready = sched(lua, |s| {
        let Some(slot) = s.parked.get_mut(&token) else {
            return None; // cancelled before it yielded: the thread is dropped
        };
        match slot.woken.take() {
            Some(args) => {
                s.parked.remove(&token);
                Some(Task {
                    args: Some(args),
                    ..task
                })
            }
            None => {
                slot.task = Some(task);
                None
            }
        }
    })?;
    if let Some(task) = ready {
        sched(lua, |s| s.deferred.push_back(task))?;
    }
    Ok(())
}

/// Starts `f` as a new thread now. It belongs to `owner` when given (an entry
/// script started by the runtime), else to the code running now.
pub(crate) fn start_owned(
    lua: &Lua,
    f: Function,
    args: MultiValue,
    label: String,
    owner: Option<Owner>,
) -> Result<Thread> {
    let thread = lua.create_thread(f)?;
    let (now, owner) = sched(lua, |s| (s.now, owner.or_else(|| s.current_owner())))?;
    resume(
        lua,
        Task {
            thread: thread.clone(),
            cond: None,
            args: Some(args),
            slept_at: now,
            label,
            owner,
        },
        now,
    )?;
    Ok(thread)
}

/// Starts `f` as a new thread now, owned by the code running now.
pub(crate) fn start(lua: &Lua, f: Function, args: MultiValue, label: String) -> Result<Thread> {
    start_owned(lua, f, args, label, None)
}

/// Runs deferred threads until none are left (deferred threads may defer more).
pub(crate) fn drain_deferred(lua: &Lua) -> Result<()> {
    let mut ran = 0;
    loop {
        let Some(task) = sched(lua, |s| s.deferred.pop_front())? else {
            return Ok(());
        };
        ran += 1;
        if ran > MAX_DEFERS_PER_DRAIN {
            let dropped = sched(lua, |s| {
                let n = s.deferred.len() + 1;
                s.deferred.clear();
                n
            })?;
            let e = ScriptError {
                kind: crate::ErrorKind::Runtime,
                code: codes::DEFER_LIMIT,
                context: task.label,
                chunk: None,
                line: None,
                message: format!(
                    "more than {MAX_DEFERS_PER_DRAIN} task.defer calls in one cycle; {dropped} deferred threads were dropped"
                ),
                traceback: String::new(),
            };
            sched(lua, |s| s.errors.push(e))?;
            return Ok(());
        }
        let now = sched(lua, |s| s.now)?;
        resume(lua, task, now)?;
    }
}

/// Resumes every waiting thread whose wake time has come. Threads that go to
/// sleep during this pass wait for the next one.
pub(crate) fn resume_due(lua: &Lua) -> Result<usize> {
    let (now, last_seq) = sched(lua, |s| (s.now, s.seq))?;
    let mut later = Vec::new();
    let mut n = 0;
    loop {
        let next = sched(lua, |s| {
            while let Some(Reverse((wake, seq))) = s.heap.peek().copied() {
                if wake.0 > now + TIME_EPSILON {
                    return None;
                }
                s.heap.pop();
                if seq > last_seq {
                    later.push(Reverse((wake, seq)));
                } else if let Some(task) = s.sleeping.remove(&seq) {
                    return Some(task);
                }
                // else: a cancelled thread's leftover entry.
            }
            None
        })?;
        let Some(task) = next else { break };
        n += 1;
        resume(lua, task, now)?;
    }
    sched(lua, |s| s.heap.extend(later))?;
    // Parked threads whose timeout ran out.
    loop {
        let next = sched(lua, |s| {
            while let Some(Reverse((wake, token))) = s.deadlines.peek().copied() {
                if wake.0 > now + TIME_EPSILON {
                    return None;
                }
                s.deadlines.pop();
                let Some(slot) = s.parked.get_mut(&token) else {
                    continue; // woken or cancelled already
                };
                let args = slot.on_timeout.take().unwrap_or_default();
                match slot.task.take() {
                    Some(task) => {
                        s.parked.remove(&token);
                        return Some(Task {
                            args: Some(args),
                            ..task
                        });
                    }
                    None => slot.woken = Some(args),
                }
            }
            None
        })?;
        let Some(task) = next else { break };
        n += 1;
        resume(lua, task, now)?;
    }
    Ok(n)
}

/// Fires `signal`-matching connections with `args`, each in its own thread
/// owned by the connection's owner, in connection order.
pub(crate) fn fire(
    lua: &Lua,
    matches: impl Fn(&Signal) -> bool,
    args: MultiValue,
) -> Result<usize> {
    let funcs: Vec<(Function, String, Option<Owner>)> = {
        let s = lua
            .app_data_ref::<Scheduler>()
            .ok_or_else(|| mlua::Error::runtime("this VM has no scheduler"))?;
        let mut out = Vec::new();
        for c in s.connections.values().filter(|c| matches(&c.signal)) {
            out.push((
                lua.registry_value::<Function>(&c.func)?,
                c.signal.label(),
                c.owner,
            ));
        }
        out
    };
    let n = funcs.len();
    for (f, label, owner) in funcs {
        let thread = lua.create_thread(f)?;
        let now = sched(lua, |s| s.now)?;
        resume(
            lua,
            Task {
                thread,
                cond: None,
                args: Some(args.clone()),
                slept_at: now,
                label,
                owner,
            },
            now,
        )?;
    }
    Ok(n)
}

/// Connects `f` to `signal`, owned by the code running now.
pub(crate) fn connect(lua: &Lua, signal: Signal, f: Function) -> Result<ConnectionRef> {
    let key = lua.create_registry_value(f)?;
    let id = sched(lua, |s| {
        let id = s.next_connection;
        s.next_connection += 1;
        let owner = s.current_owner();
        s.connections.insert(
            id,
            Connection {
                signal,
                func: key,
                owner,
            },
        );
        id
    })?;
    Ok(ConnectionRef(id))
}

/// Stops everything `owner` started: disconnects its callbacks and drops its
/// waiting and deferred threads, and those parked at a debugger stop (a
/// running one stops when it next yields). Returns how many callbacks and
/// threads went.
pub(crate) fn drop_owner(lua: &Lua, owner: Owner) -> Result<usize> {
    let (removed, threads, chunks, stopped) = sched(lua, |s| {
        let ids: Vec<u64> = s
            .connections
            .iter()
            .filter(|(_, c)| c.owner == Some(owner))
            .map(|(id, _)| *id)
            .collect();
        let removed: Vec<Connection> = ids
            .into_iter()
            .filter_map(|id| s.connections.remove(&id))
            .collect();
        let before = s.sleeping.len() + s.deferred.len();
        s.sleeping.retain(|_, t| t.owner != Some(owner));
        s.deferred.retain(|t| t.owner != Some(owner));
        let parked_before = s.parked.len() + s.debug_parked.len();
        s.parked.retain(|_, p| p.owner != Some(owner));
        let stopped: Vec<usize> = s
            .debug_parked
            .extract_if(.., |_, t| t.owner == Some(owner))
            .map(|(p, _)| p)
            .collect();
        let threads = before - s.sleeping.len() - s.deferred.len() + parked_before
            - s.parked.len()
            - s.debug_parked.len();
        let running: Vec<usize> = s
            .running
            .iter()
            .filter(|(_, o)| *o == Some(owner))
            .map(|(p, _)| *p)
            .collect();
        s.cancelled.extend(running);
        let chunks = s.debug_chunks.remove(&owner).unwrap_or_default();
        (removed, threads, chunks, stopped)
    })?;
    // A debugger stop of a dropped thread ends with it.
    crate::debug::threads_ended(lua, &stopped);
    let n = removed.len() + threads;
    for c in removed {
        lua.remove_registry_value(c.func)?;
    }
    for (_, key) in chunks {
        if let Ok(f) = lua.registry_value::<Function>(&key) {
            crate::debug::unregister_chunk(lua, &f);
        }
        lua.remove_registry_value(key)?;
    }
    Ok(n)
}

/// Registers `chunk`, loaded from the project file `path` by the run of
/// `owner`, with the debugger attached to `lua` (if any), and forgets it when
/// that run stops ([`stop_owner`]).
pub fn register_owned_chunk(lua: &Lua, owner: Owner, path: &str, chunk: &Function) -> Result<()> {
    crate::debug::register_chunk(lua, path, chunk);
    let key = lua.create_registry_value(chunk.clone())?;
    sched(lua, |s| {
        s.debug_chunks
            .entry(owner)
            .or_default()
            .push((path.to_string(), key))
    })
}

/// Registers the module chunk loaded from `path` with the debugger attached
/// to `lua` (if any), in place of the module's previous chunk.
pub fn register_module_chunk(lua: &Lua, path: &str, chunk: &Function) -> Result<()> {
    crate::debug::register_chunk(lua, path, chunk);
    let key = lua.create_registry_value(chunk.clone())?;
    if let Some(old) = sched(lua, |s| s.debug_modules.insert(path.to_string(), key))? {
        if let Ok(f) = lua.registry_value::<Function>(&old) {
            crate::debug::unregister_chunk(lua, &f);
        }
        lua.remove_registry_value(old)?;
    }
    Ok(())
}

/// Registers every live chunk of `lua` with the debugger just attached to
/// it: the scripts and modules loaded before it attached.
pub(crate) fn register_live_chunks(lua: &Lua) -> Result<()> {
    let chunks: Vec<(String, Function)> = {
        let s = lua
            .app_data_ref::<Scheduler>()
            .ok_or_else(|| mlua::Error::runtime("this VM has no scheduler"))?;
        let mut out = Vec::new();
        for (path, key) in s.debug_chunks.values().flatten() {
            out.push((path.clone(), lua.registry_value::<Function>(key)?));
        }
        for (path, key) in &s.debug_modules {
            out.push((path.clone(), lua.registry_value::<Function>(key)?));
        }
        out
    };
    for (path, f) in chunks {
        crate::debug::register_chunk(lua, &path, &f);
    }
    Ok(())
}

/// Parks the scheduler's threads that the script debugger stops (ADR-0045):
/// the thread yields, the scheduler keeps it aside, and everything else in
/// the context keeps running. Pass it to
/// [`Debugger::attach`](crate::debug::Debugger::attach) for a context that
/// runs on this scheduler.
#[derive(Clone, Copy, Debug, Default)]
pub struct DebugParking;

impl crate::debug::Parking for DebugParking {
    /// Only a thread the scheduler resumed itself, and is running now, can
    /// yield back to it; a coroutine inside it stops synchronously.
    fn can_park(&self, lua: &Lua, thread: usize) -> bool {
        lua.app_data_ref::<Scheduler>()
            .is_some_and(|s| s.running.last().is_some_and(|(p, _)| *p == thread))
    }

    /// The thread goes on at the end of the current resumption cycle, with
    /// the deferred threads (or in the next phase when none runs now), so it
    /// runs with the host bound like any other.
    fn resume(&self, lua: &Lua, thread: usize) {
        let _ = sched(lua, |s| {
            if let Some(task) = s.debug_parked.remove(&thread) {
                s.deferred.push_back(Task {
                    args: Some(MultiValue::new()),
                    ..task
                });
            }
        });
    }
}

/// The project path of the nearest calling script (`scripts/...`), if any.
pub(crate) fn caller_script(lua: &Lua) -> Option<String> {
    for level in 1..16 {
        let src = lua.inspect_stack(level, |d| {
            d.source()
                .source
                .map(|s| s.trim_start_matches(['@', '=']).to_string())
        });
        match src {
            Some(Some(s)) if s.starts_with("scripts/") => return Some(s),
            None => return None,
            _ => {}
        }
    }
    None
}

/// A connection handle (`Connection` in Luau).
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConnectionRef(u64);

impl UserData for ConnectionRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("connected", |lua, this| {
            sched(lua, |s| s.connections.contains_key(&this.0))
        });
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("disconnect", |lua, this, ()| {
            if let Some(c) = sched(lua, |s| s.connections.remove(&this.0))? {
                lua.remove_registry_value(c.func)?;
            }
            Ok(())
        });
        api::strict_userdata(methods, "Connection");
    }
}

/// Creates the scheduler's registry values. Must run before the sandbox.
pub(crate) fn init(lua: &Lua, budget: Duration) -> Result<()> {
    let sentinel = lua.create_table()?;
    sentinel.set_readonly(true);
    lua.set_named_registry_value(WAIT_SENTINEL, &sentinel)?;
    let park = lua.create_table()?;
    park.set_readonly(true);
    lua.set_named_registry_value(PARK_SENTINEL, &park)?;
    let failed = lua.create_table()?;
    failed.set_readonly(true);
    lua.set_named_registry_value(ERROR_SENTINEL, &failed)?;
    lua.set_app_data(Scheduler::new(budget));
    Ok(())
}

/// Lets coroutines made with `coroutine.create` or `coroutine.wrap` wait
/// (compat contexts).
pub(crate) fn enable_adoption(lua: &Lua) -> Result<()> {
    sched(lua, |s| s.adopt = true)
}

/// For a wait from a coroutine the scheduler did not start: when adoption is
/// on, puts `thread` to sleep for `seconds` as a scheduler thread and returns
/// true (the caller then yields back to its resumer).
fn adopt_sleep(lua: &Lua, thread: Thread, seconds: f64) -> Result<bool> {
    sched(lua, |s| {
        if !s.adopt {
            return false;
        }
        let task = Task {
            thread,
            cond: None,
            args: None,
            slept_at: s.now,
            label: "coroutine".into(),
            owner: s.current_owner(),
        };
        s.sleep(task, seconds);
        true
    })
}

/// For a park from a coroutine the scheduler did not start: when adoption is
/// on, files `thread` under `token` and returns true.
fn adopt_park(lua: &Lua, thread: Thread, token: u64) -> Result<bool> {
    let (adopt, now, owner) = sched(lua, |s| (s.adopt, s.now, s.current_owner()))?;
    if !adopt {
        return Ok(false);
    }
    let task = Task {
        thread,
        cond: None,
        args: None,
        slept_at: now,
        label: "coroutine".into(),
        owner,
    };
    park_task(lua, task, token)?;
    Ok(true)
}

fn task_arg(v: Value, what: &str) -> Result<Function> {
    match v {
        Value::Function(f) => Ok(f),
        other => Err(api::error(
            codes::BAD_ARGUMENT,
            format!("{what} expects a function, got {}", other.type_name()),
        )),
    }
}

/// A new thread for `f`, owned by the code running now, not yet run.
fn queued(lua: &Lua, f: Function, args: MultiValue, label: &str) -> Result<Task> {
    let thread = lua.create_thread(f)?;
    sched(lua, |s| Task {
        thread,
        cond: None,
        args: Some(args),
        slept_at: s.now,
        label: label.into(),
        owner: s.current_owner(),
    })
}

/// The `task` library.
pub(crate) fn task_lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    let sentinel: Table = lua.named_registry_value(WAIT_SENTINEL)?;
    let is_current = lua.create_function(|lua, t: Thread| {
        sched(lua, |s| {
            s.running.last().is_some_and(|(p, _)| *p == ptr(&t))
        })
    })?;
    let adopt = lua.create_function(|lua, (t, secs): (Thread, Option<f64>)| {
        adopt_sleep(lua, t, secs.unwrap_or(0.0))
    })?;
    let wait: Function = lua
        .load(format!(
            r#"
            local SENTINEL, IS_CURRENT, ADOPT = ...
            local yield, isyieldable, running = coroutine.yield, coroutine.isyieldable, coroutine.running
            return function(seconds)
                if not isyieldable() then
                    error("[{yield_code}] task.wait can only be called from a script thread, not from a callback the engine calls directly", 2)
                end
                if not IS_CURRENT(running()) then
                    if ADOPT(running(), seconds) then
                        return yield()
                    end
                    error("[{yield_code}] task.wait inside a coroutine made with coroutine.create or coroutine.wrap is not supported in M1; use task.spawn", 2)
                end
                return yield(SENTINEL, seconds or 0)
            end
            "#,
            yield_code = codes::YIELD
        ))
        .set_name("=task")
        .call((sentinel, is_current, adopt))?;
    lib.value(
        "wait",
        "(seconds: number?) -> number",
        "Suspends this script thread for `seconds` (default: until the next step) and returns the time that passed.",
        wait,
    )?;
    lib.func(
        lua,
        "spawn",
        "(f: (...any) -> ...any, ...any) -> thread",
        "Runs `f` in a new script thread now; it continues here when `f` first waits or ends.",
        |lua, (f, args): (Value, MultiValue)| {
            let f = task_arg(f, "task.spawn")?;
            start(lua, f, args, "task.spawn".into())
        },
    )?;
    lib.func(
        lua,
        "defer",
        "(f: (...any) -> ...any, ...any) -> thread",
        "Runs `f` in a new script thread at the end of the current cycle.",
        |lua, (f, args): (Value, MultiValue)| {
            let f = task_arg(f, "task.defer")?;
            let task = queued(lua, f, args, "task.defer")?;
            let thread = task.thread.clone();
            sched(lua, |s| s.deferred.push_back(task))?;
            Ok(thread)
        },
    )?;
    lib.func(
        lua,
        "delay",
        "(seconds: number, f: (...any) -> ...any, ...any) -> thread",
        "Runs `f` in a new script thread after `seconds` of simulation time.",
        |lua, (secs, f, args): (f64, Value, MultiValue)| {
            let f = task_arg(f, "task.delay")?;
            let task = queued(lua, f, args, "task.delay")?;
            let thread = task.thread.clone();
            sched(lua, |s| s.sleep(task, secs))?;
            Ok(thread)
        },
    )?;
    lib.func(
        lua,
        "cancel",
        "(t: thread) -> ()",
        "Cancels a thread made by task.spawn, task.defer or task.delay, or one waiting in task.wait.",
        |lua, t: Thread| {
            let p = ptr(&t);
            let stopped = sched(lua, |s| {
                let stopped = s.debug_parked.contains_key(&p);
                if !s.unschedule(p) && s.running.iter().any(|(r, _)| *r == p) {
                    // A running thread (itself, or one that spawned the caller)
                    // stops when it next yields.
                    s.cancelled.insert(p);
                }
                stopped
            })?;
            if stopped {
                // It was parked at a debugger stop, which ends with it.
                crate::debug::threads_ended(lua, &[p]);
            }
            Ok(())
        },
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The public scheduler API ([`crate::sched`]), for layers that run their own
// contexts on this scheduler (the compatibility layer's `server` and `client`).
// ---------------------------------------------------------------------------

/// The context's simulation time, in seconds.
pub fn now(lua: &Lua) -> Result<f64> {
    sched(lua, |s| s.now)
}

/// Advances the simulation time by `dt` seconds (the runtime does this once per
/// fixed step; threads become due in [`resume_due`]).
pub fn advance(lua: &Lua, dt: f64) -> Result<()> {
    sched(lua, |s| s.now += dt)
}

/// Resumes every thread whose `task.wait`, `task.delay` or park timeout is due,
/// in wake-time order; returns how many ran.
pub fn resume_due_now(lua: &Lua) -> Result<usize> {
    resume_due(lua)
}

/// Runs deferred threads, then the settle hook, until neither has more work.
pub fn settle(lua: &Lua) -> Result<()> {
    for _ in 0..MAX_SETTLE_ROUNDS {
        drain_deferred(lua)?;
        let hook = sched(lua, |s| s.settle.clone())?;
        let Some(hook) = hook else { return Ok(()) };
        let more = hook(lua)?;
        let pending = sched(lua, |s| !s.deferred.is_empty())?;
        if !more && !pending {
            return Ok(());
        }
    }
    let e = ScriptError {
        kind: crate::ErrorKind::Runtime,
        code: codes::DEFER_LIMIT,
        context: "settle".into(),
        chunk: None,
        line: None,
        message: format!(
            "deferred work kept queuing more work for {MAX_SETTLE_ROUNDS} rounds; the rest waits for the next cycle"
        ),
        traceback: String::new(),
    };
    sched(lua, |s| s.errors.push(e))
}

/// Sets the hook [`settle`] runs after each drain of the deferred threads (the
/// compatibility layer dispatches its pending instance events there).
pub fn set_settle_hook(lua: &Lua, hook: SettleHook) -> Result<()> {
    sched(lua, |s| s.settle = Some(hook))
}

/// Runs `f` in a new script thread now, owned by the code running now
/// (`task.spawn`).
pub fn spawn(lua: &Lua, f: Function, args: MultiValue, label: &str) -> Result<Thread> {
    start(lua, f, args, label.to_string())
}

/// Runs `f` in a new script thread at the end of the current resumption cycle
/// (`task.defer`). With `cond`, the thread is dropped when `cond` is false at
/// that point (a signal connection that was disconnected meanwhile).
pub fn defer(
    lua: &Lua,
    f: Function,
    args: MultiValue,
    label: &str,
    cond: Option<DeferCondition>,
) -> Result<Thread> {
    let mut task = queued(lua, f, args, label)?;
    task.cond = cond;
    let thread = task.thread.clone();
    sched(lua, |s| s.deferred.push_back(task))?;
    Ok(thread)
}

/// [`defer`] with the thread owned by `owner` (a signal connection's script)
/// instead of the code running now; `None` keeps the code running now.
pub fn defer_owned(
    lua: &Lua,
    f: Function,
    args: MultiValue,
    label: &str,
    cond: Option<DeferCondition>,
    owner: Option<Owner>,
) -> Result<Thread> {
    let mut task = queued(lua, f, args, label)?;
    task.cond = cond;
    if owner.is_some() {
        task.owner = owner;
    }
    let thread = task.thread.clone();
    sched(lua, |s| s.deferred.push_back(task))?;
    Ok(thread)
}

/// Parks the calling thread: called by a host function wrapped with
/// [`yielding`], which returns the values this gives back. The wrapper then
/// yields, and the thread waits until [`wake`] (or, with `timeout`, until that
/// many seconds of simulation time pass, when it gets the timeout values).
/// `label` names the wait in [`parked_labels`].
pub fn park(
    lua: &Lua,
    label: &str,
    timeout: Option<(f64, MultiValue)>,
) -> Result<(MultiValue, YieldToken)> {
    let marker: Table = lua.named_registry_value(PARK_SENTINEL)?;
    let token = sched(lua, |s| {
        let token = s.next_token;
        s.next_token += 1;
        let owner = s.current_owner();
        // An infinite timeout never expires; NaN counts as zero.
        let on_timeout = match timeout {
            Some((secs, _)) if secs == f64::INFINITY => None,
            Some((secs, values)) => {
                let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
                s.deadlines.push(Reverse((WakeTime(s.now + secs), token)));
                Some(values)
            }
            None => None,
        };
        s.parked.insert(
            token,
            Parked {
                task: None,
                label: label.to_string(),
                woken: None,
                on_timeout,
                owner,
            },
        );
        token
    })?;
    Ok((
        MultiValue::from_iter([Value::Table(marker), Value::Number(token as f64)]),
        YieldToken(token),
    ))
}

/// Wakes a parked thread with `args` (its host call's results). False when the
/// token is no longer parked (woken, timed out or cancelled).
pub fn wake(lua: &Lua, token: YieldToken, args: MultiValue, mode: WakeMode) -> Result<bool> {
    let task = sched(lua, |s| {
        let slot = s.parked.get_mut(&token.0)?;
        match slot.task.take() {
            Some(task) => {
                s.parked.remove(&token.0);
                Some(Some(task))
            }
            None => {
                slot.woken = Some(args.clone());
                Some(None)
            }
        }
    })?;
    match task {
        None => Ok(false),
        Some(None) => Ok(true),
        Some(Some(task)) => {
            let task = Task {
                args: Some(args),
                ..task
            };
            match mode {
                WakeMode::Deferred => sched(lua, |s| s.deferred.push_back(task))?,
                WakeMode::Immediate => {
                    let now = sched(lua, |s| s.now)?;
                    resume(lua, task, now)?;
                }
            }
            Ok(true)
        }
    }
}

/// Wakes a parked thread so that its host call raises `message` (a call that
/// failed after it parked: the callback of a remote invocation raised). False
/// when the token is no longer parked.
pub fn wake_error(lua: &Lua, token: YieldToken, message: &str, mode: WakeMode) -> Result<bool> {
    let marker: Table = lua.named_registry_value(ERROR_SENTINEL)?;
    let args = MultiValue::from_iter([
        Value::Table(marker),
        Value::String(lua.create_string(message)?),
    ]);
    wake(lua, token, args, mode)
}

/// True while `token` is parked (not yet woken, timed out or cancelled).
pub fn is_parked(lua: &Lua, token: YieldToken) -> Result<bool> {
    sched(lua, |s| s.parked.contains_key(&token.0))
}

/// Drops a parked thread without resuming it.
pub fn cancel_park(lua: &Lua, token: YieldToken) -> Result<()> {
    sched(lua, |s| {
        s.parked.remove(&token.0);
    })
}

/// The labels of the threads parked now, in park order.
pub fn parked_labels(lua: &Lua) -> Result<Vec<String>> {
    sched(lua, |s| {
        s.parked
            .values()
            .filter(|p| p.task.is_some())
            .map(|p| p.label.clone())
            .collect()
    })
}

/// Starts `f` as a new thread now, owned by `owner` (an entry script the
/// caller starts); [`stop_owner`] stops what it started.
pub fn spawn_owned(
    lua: &Lua,
    f: Function,
    args: MultiValue,
    label: &str,
    owner: Owner,
) -> Result<Thread> {
    start_owned(lua, f, args, label.to_string(), Some(owner))
}

/// Stops every thread `owner` started (waiting, deferred or parked; a running
/// one stops when it next yields). Returns how many went.
pub fn stop_owner(lua: &Lua, owner: Owner) -> Result<usize> {
    drop_owner(lua, owner)
}

/// The threads parked now, in park order: each park's label and the label of
/// the thread that waits (the script that started it, for an entry script).
pub fn parked_waits(lua: &Lua) -> Result<Vec<(String, String)>> {
    sched(lua, |s| {
        s.parked
            .values()
            .filter_map(|p| p.task.as_ref().map(|t| (p.label.clone(), t.label.clone())))
            .collect()
    })
}

/// Threads waiting in `task.wait` or `task.delay`.
pub fn sleeping_count(lua: &Lua) -> Result<usize> {
    sched(lua, |s| s.sleeping())
}

/// Wraps a host function so that it can park its caller: when `f` returns the
/// values [`park`] gave it, the wrapper yields the calling thread; otherwise it
/// returns `f`'s results unchanged. Parking from code that is not a scheduler
/// thread (a callback the engine calls directly, or a coroutine made with
/// `coroutine.create`) raises `RE3005`; `name` names the call in that error.
pub fn yielding(lua: &Lua, name: &str, f: Function) -> Result<Function> {
    let marker: Table = lua.named_registry_value(PARK_SENTINEL)?;
    let failed: Table = lua.named_registry_value(ERROR_SENTINEL)?;
    let is_current = lua.create_function(|lua, t: Thread| {
        sched(lua, |s| {
            s.running.last().is_some_and(|(p, _)| *p == ptr(&t))
        })
    })?;
    let cancel =
        lua.create_function(|lua, token: f64| cancel_park(lua, YieldToken(token as u64)))?;
    let adopt =
        lua.create_function(|lua, (t, token): (Thread, f64)| adopt_park(lua, t, token as u64))?;
    lua.load(format!(
        r#"
        local f, MARK, IS_CURRENT, CANCEL, ADOPT, FAILED = ...
        local yield, isyieldable, running, select = coroutine.yield, coroutine.isyieldable, coroutine.running, select
        local function resumed(first, ...)
            if first == FAILED then
                error((...), 0)
            end
            return first, ...
        end
        local function finish(...)
            if (...) ~= MARK then
                return ...
            end
            local token = select(2, ...)
            if not isyieldable() then
                CANCEL(token)
                error({msg:?}, 3)
            end
            if not IS_CURRENT(running()) then
                if ADOPT(running(), token) then
                    return resumed(yield())
                end
                CANCEL(token)
                error({msg:?}, 3)
            end
            return resumed(yield(MARK, token))
        end
        return function(...)
            return finish(f(...))
        end
        "#,
        msg = format!(
            "[{}] {name} cannot yield here: it must run in a script thread, not in a callback the engine calls directly or a coroutine made with coroutine.create or coroutine.wrap",
            codes::YIELD
        )
    ))
    .set_name(format!("={name}"))
    .call((f, marker, is_current, cancel, adopt, failed))
}

/// True while the scheduler still holds thread `t`: running (and not
/// cancelled), waiting, deferred or parked. A thread a stopped script owned,
/// or one cancelled with `task.cancel`, is no longer held.
pub fn is_scheduled(lua: &Lua, t: &Thread) -> Result<bool> {
    let p = ptr(t);
    sched(lua, |s| {
        (s.running.iter().any(|(r, _)| *r == p) && !s.cancelled.contains(&p))
            || s.sleeping.values().any(|x| ptr(&x.thread) == p)
            || s.deferred.iter().any(|x| ptr(&x.thread) == p)
            || s.parked
                .values()
                .any(|x| x.task.as_ref().is_some_and(|x| ptr(&x.thread) == p))
    })
}

/// The owner of the code running now (the entry script that started it).
pub fn current_owner(lua: &Lua) -> Result<Option<Owner>> {
    sched(lua, |s| s.current_owner())
}
