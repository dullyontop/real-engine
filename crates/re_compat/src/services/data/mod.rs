//! `DataStoreService` and `MemoryStoreService` over local files (ADR-0048).
//!
//! Stores live in a [`DataBackend`]: `.real/data/` in `real run`, or memory
//! seeded from the project's `data/seed/` everywhere else (`real verify`,
//! replays, `play.run`, tests), so those runs repeat and never change what a
//! play saved. The host picks with [`configure`]; a VM nobody configured
//! uses [`DataSettings::session`].
//!
//! Every `…Async` call yields: the caller parks, a runner thread waits for
//! the latency plus any wait for the request budget, then does the request
//! and wakes the caller with its results or its error. Budgets follow the
//! vendor's per-server formulas ([`budget`]); errors start with the
//! vendor's number (`"105: …"`) or, for memory stores, its error name, so
//! scripts that match on those keep working.
//!
//! `DataStoreKeyInfo`, the pages objects and the items they list are plain
//! tables with the same fields and methods, not instances.

mod backend;
mod budget;
mod clock;
mod ds;
mod memory;
mod ms;
mod store;
mod value;

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use mlua::{Function, Lua, MultiValue, RegistryKey, Result, Table, Value as LuaValue};
use re_core::Code;
use re_scene::Entity;
use re_script::sched::{self, WakeMode, YieldToken};
use serde_json::Value as J;

pub use backend::{DataBackend, DirBackend, MemoryBackend, decode, encode};
pub use clock::{Clock, DEFAULT_EPOCH_MS, parse_epoch};

use crate::codes;
use crate::runtime::{Bindings, lua_error, state, with_world};
use crate::scene::{DataConfig, DataMode};

/// How the data services of one VM behave.
pub struct DataSettings {
    /// When false, every call fails with `403` (the local stand-in for
    /// Studio's switch that lets a place reach these services).
    pub enabled: bool,
    /// Simulation seconds a request takes before its caller resumes.
    pub latency: f64,
    /// The data clock.
    pub clock: Clock,
    /// Where the files are.
    pub backend: Box<dyn DataBackend>,
}

impl DataSettings {
    /// An empty in-memory session on a fixed clock, one 60 Hz frame of
    /// latency.
    pub fn session() -> DataSettings {
        DataSettings {
            enabled: true,
            latency: 1.0 / 60.0,
            clock: Clock::default(),
            backend: Box::new(MemoryBackend::new()),
        }
    }

    /// The settings `[compat.data]` gives a project at `project`, stepping
    /// at `step` seconds. With `persist` (`real run`) and `mode = "persist"`
    /// (the default), the files are `.real/data/` and the clock is the wall
    /// clock; otherwise they are a copy of `data/seed/` in memory and the
    /// clock starts at `epoch`.
    ///
    /// # Errors
    ///
    /// The seed directory cannot be read, or `epoch` does not parse.
    pub fn from_config(
        config: &DataConfig,
        project: &Path,
        step: f64,
        persist: bool,
    ) -> std::result::Result<DataSettings, String> {
        let epoch_ms = parse_epoch(&config.epoch).ok_or_else(|| {
            format!(
                "real.toml [compat.data] epoch {:?} is not of the form 2026-01-01T00:00:00Z",
                config.epoch
            )
        })?;
        let latency = f64::from(config.latency_frames) * step;
        let (clock, backend): (Clock, Box<dyn DataBackend>) =
            if persist && config.mode == DataMode::Persist {
                (
                    Clock::Wall,
                    Box::new(DirBackend::new(project.join(".real").join("data"))),
                )
            } else {
                let seed = project.join("data").join("seed");
                let m = MemoryBackend::seeded(&seed)
                    .map_err(|e| format!("cannot read {}: {e}", seed.display()))?;
                (Clock::Fixed { epoch_ms }, Box::new(m))
            };
        Ok(DataSettings {
            enabled: config.enabled,
            latency,
            clock,
            backend,
        })
    }
}

/// Sets how the data services of the VM in `lua` behave. Call it before any
/// script runs; stores already read are dropped.
pub fn configure(lua: &Lua, settings: DataSettings) {
    lua.set_app_data(Data::new(settings));
}

/// What a store instance stands for.
#[derive(Clone, Debug)]
enum Handle {
    Store(store::StoreId),
    Memory(memory::Kind, String, f64),
}

/// A request between its call and its completion.
struct Pending {
    token: YieldToken,
    op: Op,
    /// Simulation time when the call was made.
    started: f64,
}

/// The requests a runner completes.
enum Op {
    Ds(ds::Op),
    Ms(ms::Op),
    Next(u64),
}

/// The pages a pages table walks.
struct Pages {
    pages: Vec<Vec<J>>,
    index: usize,
    table: RegistryKey,
    kind: budget::Kind,
}

/// Per-VM state of the data services.
struct Data {
    settings: DataSettings,
    stores: store::Stores,
    budgets: budget::Budgets,
    memory: memory::Memory,
    handles: HashMap<Entity, Handle>,
    by_handle: HashMap<String, Entity>,
    set_options: HashMap<Entity, serde_json::Map<String, J>>,
    pending: HashMap<u64, Pending>,
    pages: HashMap<u64, Pages>,
    next_id: u64,
    warned: Vec<String>,
}

impl Data {
    fn new(settings: DataSettings) -> Data {
        Data {
            settings,
            stores: store::Stores::default(),
            budgets: budget::Budgets::default(),
            memory: memory::Memory::default(),
            handles: HashMap::new(),
            by_handle: HashMap::new(),
            set_options: HashMap::new(),
            pending: HashMap::new(),
            pages: HashMap::new(),
            next_id: 1,
            warned: Vec::new(),
        }
    }
}

fn data(lua: &Lua) -> mlua::AppDataRefMut<'_, Data> {
    if lua.app_data_ref::<Data>().is_none() {
        lua.set_app_data(Data::new(DataSettings::session()));
    }
    lua.app_data_mut::<Data>().expect("just set")
}

/// An error whose message starts with the vendor's number.
fn numbered(num: u16, code: Code, message: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(format!("{num}: {message} [{code}]"))
}

/// The error of a request with a bad argument (`RE9400`).
fn bad(num: u16, message: impl std::fmt::Display) -> mlua::Error {
    numbered(num, codes::DATA_REQUEST, message)
}

/// Checks that the services may be used here: on the server, and enabled.
fn usable(lua: &Lua, what: &str) -> Result<()> {
    if state(lua)?.kind != re_script::ContextKind::Server {
        return Err(lua_error(
            codes::DATA_REQUEST,
            format!("{what} can only be used from the server"),
        ));
    }
    if !data(lua).settings.enabled {
        return Err(bad(
            403,
            format!("{what}: data services are turned off ([compat.data] enabled = false)"),
        ));
    }
    Ok(())
}

/// The number of players in the server, for the budgets.
fn players(lua: &Lua) -> Result<u32> {
    Ok(crate::players::players_of(lua)?.len() as u32)
}

/// The data clock now, in Unix milliseconds.
fn now_ms(lua: &Lua) -> Result<i64> {
    let sim = sched::now(lua)?;
    Ok(data(lua).settings.clock.now_ms(sim))
}

/// Warns once per file that did not read.
fn warn_unreadable(lua: &Lua, path: &str, reason: &str) -> Result<()> {
    {
        let mut d = data(lua);
        if d.warned.iter().any(|p| p == path) {
            return Ok(());
        }
        d.warned.push(path.to_string());
    }
    let warn: Function = lua.globals().get("warn")?;
    warn.call::<()>(format!(
        "[{}] 501: the data file {path} did not read ({reason}); it was set aside as {path}.bad \
         and the store starts empty",
        codes::DATA_FILE
    ))
}

/// Writes a store or structure back, warning when the write fails.
fn saved(lua: &Lua, r: std::io::Result<()>, what: &str) -> Result<()> {
    if let Err(e) = r {
        let warn: Function = lua.globals().get("warn")?;
        warn.call::<()>(format!(
            "[{}] could not write {what}: {e}; the change is kept for this run only",
            codes::DATA_FILE
        ))?;
    }
    Ok(())
}

/// The store instance for `handle`, made the first time.
fn handle_instance(lua: &Lua, class: &str, name: &str, handle: Handle) -> Result<LuaValue> {
    let key = format!("{class}\u{1}{handle:?}");
    if let Some(&e) = data(lua).by_handle.get(&key) {
        return crate::instance::push(lua, e);
    }
    let classes = state(lua)?.setup.classes.clone();
    let e = with_world(lua, |w| {
        crate::scene::spawn_instance(w, &classes, class, None, name, None)
    })?
    .map_err(crate::runtime::compat_error)?;
    {
        let mut d = data(lua);
        d.by_handle.insert(key, e);
        d.handles.insert(e, handle);
    }
    crate::instance::push(lua, e)
}

fn handle_of(lua: &Lua, e: Entity, what: &str) -> Result<Handle> {
    data(lua).handles.get(&e).cloned().ok_or_else(|| {
        lua_error(
            codes::DATA_REQUEST,
            format!("{what}: this object was not made by DataStoreService or MemoryStoreService"),
        )
    })
}

/// Registry key of the runner that completes requests.
const RUNNER_KEY: &str = "re_compat.data.runner";

fn runner(lua: &Lua) -> Result<Function> {
    if let Some(r) = lua.named_registry_value::<Option<Function>>(RUNNER_KEY)? {
        return Ok(r);
    }
    let raw = lua.create_function(|lua, (label, secs): (String, f64)| {
        let (values, _) = sched::park(lua, &label, Some((secs, MultiValue::new())))?;
        Ok(values)
    })?;
    let sleep = sched::yielding(lua, "sleep", raw)?;
    let finish = lua.create_function(|lua, id: f64| finish(lua, id as u64))?;
    let r: Function = lua
        .load(
            "local sleep, finish = ...\n\
             return function(label, id, delay)\n\
                 while delay do\n\
                     sleep(label, delay)\n\
                     delay = finish(id)\n\
                 end\n\
             end",
        )
        .set_name("=data")
        .call((sleep, finish))?;
    lua.set_named_registry_value(RUNNER_KEY, &r)?;
    Ok(r)
}

/// Parks the caller and starts a runner that completes `op` after `wait`
/// more seconds than the latency. Returns the values the method gives back
/// to park.
fn submit(lua: &Lua, label: &str, op: Op, wait: f64) -> Result<MultiValue> {
    let (values, token) = sched::park(lua, label, None)?;
    let started = sched::now(lua)?;
    let (id, delay) = {
        let mut d = data(lua);
        let id = d.next_id;
        d.next_id += 1;
        d.pending.insert(id, Pending { token, op, started });
        (id, d.settings.latency + wait)
    };
    sched::spawn(
        lua,
        runner(lua)?,
        MultiValue::from_iter([
            LuaValue::String(lua.create_string(label)?),
            LuaValue::Number(id as f64),
            LuaValue::Number(delay),
        ]),
        label,
    )?;
    Ok(values)
}

/// What completing a request gave.
enum Done {
    /// The caller's results.
    Values(MultiValue),
    /// The caller's error.
    Error(mlua::Error),
    /// Not yet: run again after this many seconds (a queue read waiting
    /// for items).
    Again(f64),
}

/// Completes request `id`; returns the delay before the runner tries again,
/// or nil when it is done.
fn finish(lua: &Lua, id: u64) -> Result<Option<f64>> {
    let Some(p) = data(lua).pending.remove(&id) else {
        return Ok(None);
    };
    let done = match &p.op {
        Op::Ds(op) => ds::complete(lua, op),
        Op::Ms(op) => ms::complete(lua, op, p.started),
        Op::Next(pages) => next_page(lua, *pages),
    };
    let done = match done {
        Ok(d) => d,
        Err(e) => Done::Error(e),
    };
    match done {
        Done::Values(v) => {
            sched::wake(lua, p.token, v, WakeMode::Deferred)?;
            Ok(None)
        }
        Done::Error(e) => {
            let message = match &e {
                mlua::Error::RuntimeError(m) => m.clone(),
                other => other.to_string(),
            };
            sched::wake_error(lua, p.token, &message, WakeMode::Deferred)?;
            Ok(None)
        }
        Done::Again(secs) => {
            data(lua).pending.insert(id, p);
            Ok(Some(secs))
        }
    }
}

/// A pages table over `pages` (at least one, possibly empty, page) whose
/// `AdvanceToNextPageAsync` charges `kind`.
fn pages_table(lua: &Lua, mut pages: Vec<Vec<J>>, kind: budget::Kind) -> Result<LuaValue> {
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    let finished = pages.len() == 1;
    let t = lua.create_table()?;
    t.raw_set("IsFinished", finished)?;
    let id = {
        let mut d = data(lua);
        let id = d.next_id;
        d.next_id += 1;
        id
    };
    let current = lua.create_function(move |lua, _this: LuaValue| {
        let items = {
            let d = data(lua);
            let p = d
                .pages
                .get(&id)
                .ok_or_else(|| lua_error(codes::DATA_REQUEST, "these pages are gone"))?;
            p.pages[p.index].clone()
        };
        let out = lua.create_table()?;
        for (i, item) in items.iter().enumerate() {
            out.raw_set(i + 1, value::to_lua(lua, item)?)?;
        }
        Ok(out)
    })?;
    let raw = lua.create_function(move |lua, _this: LuaValue| -> Result<MultiValue> {
        usable(lua, "Pages:AdvanceToNextPageAsync")?;
        let (kind, finished) = {
            let d = data(lua);
            let p = d
                .pages
                .get(&id)
                .ok_or_else(|| lua_error(codes::DATA_REQUEST, "these pages are gone"))?;
            (p.kind, p.index + 1 >= p.pages.len())
        };
        if finished {
            return Err(lua_error(
                codes::DATA_REQUEST,
                "AdvanceToNextPageAsync: there are no more pages (IsFinished is true)",
            ));
        }
        let (now, players) = (sched::now(lua)?, players(lua)?);
        let wait = data(lua)
            .budgets
            .reserve(&[kind], now, players)
            .map_err(|_| queue_full(305, "AdvanceToNextPageAsync"))?;
        submit(lua, "Pages:AdvanceToNextPageAsync", Op::Next(id), wait)
    })?;
    let advance = sched::yielding(lua, "Pages:AdvanceToNextPageAsync", raw)?;
    t.raw_set("GetCurrentPage", current)?;
    t.raw_set("AdvanceToNextPageAsync", advance)?;
    let key = lua.create_registry_value(&t)?;
    data(lua).pages.insert(
        id,
        Pages {
            pages,
            index: 0,
            table: key,
            kind,
        },
    );
    Ok(LuaValue::Table(t))
}

fn next_page(lua: &Lua, id: u64) -> Result<Done> {
    let (table, finished) = {
        let mut d = data(lua);
        let Some(p) = d.pages.get_mut(&id) else {
            return Ok(Done::Values(MultiValue::new()));
        };
        p.index = (p.index + 1).min(p.pages.len() - 1);
        let finished = p.index + 1 >= p.pages.len();
        (lua.registry_value::<Table>(&p.table)?, finished)
    };
    table.raw_set("IsFinished", finished)?;
    Ok(Done::Values(MultiValue::new()))
}

/// Splits `items` into pages of `size`.
fn paginate(items: Vec<J>, size: usize) -> Vec<Vec<J>> {
    let size = size.max(1);
    let mut pages: Vec<Vec<J>> = items.chunks(size).map(<[J]>::to_vec).collect();
    if pages.is_empty() {
        pages.push(Vec::new());
    }
    pages
}

/// The error of a request dropped because its queue is full (`RE9401`).
fn queue_full(num: u16, what: &str) -> mlua::Error {
    numbered(
        num,
        codes::DATA_QUEUE_FULL,
        format!(
            "{what} was dropped: the request queue is full ({} waiting for budget)",
            budget::QUEUE_LIMIT
        ),
    )
}

/// Registers both services' methods.
pub fn register(b: &mut Bindings) {
    ds::register(b);
    ms::register(b);
}

type Imp = fn(&Lua, Entity, MultiValue) -> Result<MultiValue>;

fn add(b: &mut Bindings, class: &str, name: &str, f: Imp, yields: bool) {
    if yields {
        b.yielding_method(class, name, Arc::new(f));
    } else {
        b.method(class, name, Arc::new(f));
    }
}
