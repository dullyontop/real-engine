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
use std::time::{Duration, Instant};

use mlua::thread::ThreadStatus;
use mlua::{
    Function, Lua, MultiValue, RegistryKey, Result, Table, Thread, UserData, UserDataFields,
    UserDataMethods, Value,
};
use re_scene::Entity;

use crate::api::{self, Lib};
use crate::budget;
use crate::codes;
use crate::diag::ScriptError;

const WAIT_SENTINEL: &str = "re_script.wait_sentinel";

/// The most deferred threads one drain runs before it gives up (a `task.defer`
/// that keeps deferring itself would otherwise never end).
pub(crate) const MAX_DEFERS_PER_DRAIN: usize = 10_000;

/// Simulation times closer than this are equal: `now` is a sum of fixed steps,
/// so six steps of 1/60 s add up to slightly less than 0.1.
const TIME_EPSILON: f64 = 1e-6;

/// Who created a thread or connection: an entry-script run of the runtime.
pub(crate) type Owner = u64;

/// What a connection listens to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Signal {
    /// Every frame, in `PreRender`, with the frame time.
    Frame,
    /// Every fixed step, in `PreSimulation`, with the step time.
    Step,
    /// Every fixed step, in `PostSimulation`, after contacts and waits.
    PostStep,
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
}

impl Signal {
    fn label(&self) -> String {
        match self {
            Signal::Frame => "time.on_frame".into(),
            Signal::Step => "time.on_step".into(),
            Signal::PostStep => "time.on_post_step".into(),
            Signal::Pressed(a) => format!("input.on_pressed({a:?})"),
            Signal::Released(a) => format!("input.on_released({a:?})"),
            Signal::Contact => "physics.on_contact".into(),
            Signal::ContactEnd => "physics.on_contact_end".into(),
            Signal::Touch(_) => "physics.on_touch".into(),
        }
    }
}

struct Connection {
    signal: Signal,
    func: RegistryKey,
    owner: Option<Owner>,
}

struct Task {
    thread: Thread,
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
        self.sleeping.len() + self.deferred.len() != before
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
        return Ok(());
    }
    match result {
        Ok(yielded) if task.thread.status() == ThreadStatus::Resumable => {
            let sentinel: Table = lua.named_registry_value(WAIT_SENTINEL)?;
            let mut it = yielded.into_iter();
            let delay = match (it.next(), it.next()) {
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
/// waiting and deferred threads (a running one stops when it next yields).
/// Returns how many callbacks and threads went.
pub(crate) fn drop_owner(lua: &Lua, owner: Owner) -> Result<usize> {
    let (removed, threads) = sched(lua, |s| {
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
        let threads = before - s.sleeping.len() - s.deferred.len();
        let running: Vec<usize> = s
            .running
            .iter()
            .filter(|(_, o)| *o == Some(owner))
            .map(|(p, _)| *p)
            .collect();
        s.cancelled.extend(running);
        (removed, threads)
    })?;
    let n = removed.len() + threads;
    for c in removed {
        lua.remove_registry_value(c.func)?;
    }
    Ok(n)
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
    lua.set_app_data(Scheduler::new(budget));
    Ok(())
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
    let wait: Function = lua
        .load(format!(
            r#"
            local SENTINEL, IS_CURRENT = ...
            local yield, isyieldable, running = coroutine.yield, coroutine.isyieldable, coroutine.running
            return function(seconds)
                if not isyieldable() then
                    error("[{yield_code}] task.wait can only be called from a script thread, not from a callback the engine calls directly", 2)
                end
                if not IS_CURRENT(running()) then
                    error("[{yield_code}] task.wait inside a coroutine made with coroutine.create or coroutine.wrap is not supported in M1; use task.spawn", 2)
                end
                return yield(SENTINEL, seconds or 0)
            end
            "#,
            yield_code = codes::YIELD
        ))
        .set_name("=task")
        .call((sentinel, is_current))?;
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
            sched(lua, |s| {
                if !s.unschedule(p) && s.running.iter().any(|(r, _)| *r == p) {
                    // A running thread (itself, or one that spawned the caller)
                    // stops when it next yields.
                    s.cancelled.insert(p);
                }
            })?;
            Ok(())
        },
    )?;
    Ok(())
}
