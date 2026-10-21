//! The VM of the `generator` context: the safe standard libraries without
//! `os`, `math.random` or `print` to the console, the `proc` declaration
//! library, `quat` and `color`, a `require` confined to
//! `scripts/generators/`, the memory limit, and the counting interrupt that
//! makes the budget the same on every machine.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::os::raw::c_int;
use std::rc::Rc;
use std::sync::Arc;

use mlua::{Function, Lua, LuaOptions, MultiValue, StdLib, Table, Value, ffi};
use re_core::time::Instant;

use super::GENERATORS_DIR;
use crate::api::Lib;

/// The marker metatable of a `proc.node{...}` declaration (registry key).
pub(crate) const NODE_META: &str = "re_script.generator.node";
/// The marker metatable of a `proc.param.*{...}` (registry key).
pub(crate) const PARAM_META: &str = "re_script.generator.param";
/// The modules this VM has loaded, by path (registry key).
const MODULES: &str = "re_script.generator.modules";

/// How many safepoints pass between two reads of the backstop clock.
const CHECK_EVERY: u64 = 1024;

/// Why the interrupt stopped a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// Still running.
    None,
    /// Over the safepoint budget (`RE4101`).
    Budget,
    /// Over the wall-clock backstop (`RE4102`).
    Backstop,
}

thread_local! {
    static SPENT: Cell<u64> = const { Cell::new(0) };
    static LIMIT: Cell<u64> = const { Cell::new(u64::MAX) };
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
    static STOPPED: Cell<Stop> = const { Cell::new(Stop::None) };
}

unsafe extern "C-unwind" fn interrupt(state: *mut ffi::lua_State, gc: c_int) {
    if gc >= 0 {
        return; // a GC safepoint: never raise from here, and never count it
    }
    let n = SPENT.with(|c| {
        let n = c.get() + 1;
        c.set(n);
        n
    });
    if STOPPED.with(Cell::get) == Stop::None {
        if n > LIMIT.with(Cell::get) {
            STOPPED.with(|s| s.set(Stop::Budget));
        } else if n.is_multiple_of(CHECK_EVERY)
            && DEADLINE.with(Cell::get).is_some_and(|d| Instant::now() > d)
        {
            STOPPED.with(|s| s.set(Stop::Backstop));
        }
    }
    // Once stopped, every safepoint raises, so a `pcall` cannot swallow it.
    if STOPPED.with(Cell::get) != Stop::None {
        // SAFETY: `state` is the running thread handed to the interrupt;
        // luaL_error raises an ordinary Luau error and does not return.
        unsafe { ffi::luaL_error(state, c"generator stopped: over its budget".as_ptr()) };
    }
}

/// The budget of one run on this thread; [`Budget::finish`] says whether it
/// stopped the script, and puts back the state it replaced.
pub(crate) struct Budget {
    saved: (u64, u64, Option<Instant>, Stop),
}

impl Budget {
    /// Starts counting: at most `limit` safepoints, and `backstop` of wall
    /// time.
    pub(crate) fn arm(limit: u64, backstop: std::time::Duration) -> Budget {
        Budget {
            saved: (
                SPENT.with(|c| c.replace(0)),
                LIMIT.with(|c| c.replace(limit)),
                DEADLINE.with(|c| c.replace(Some(Instant::now() + backstop))),
                STOPPED.with(|c| c.replace(Stop::None)),
            ),
        }
    }

    /// Ends the run; whether (and why) the interrupt stopped it.
    pub(crate) fn finish(self) -> Stop {
        let stop = STOPPED.with(Cell::get);
        let (spent, limit, deadline, stopped) = self.saved;
        SPENT.with(|c| c.set(spent));
        LIMIT.with(|c| c.set(limit));
        DEADLINE.with(|c| c.set(deadline));
        STOPPED.with(|c| c.set(stopped));
        stop
    }
}

/// The scripts a VM may load: bytecode by project path, for every file
/// under `scripts/generators/`.
pub(crate) type Scripts = BTreeMap<String, Arc<[u8]>>;

/// Lines a script printed or logged, shared with the caller.
pub(crate) type Log = Rc<RefCell<Vec<String>>>;

/// Adds a line to `log`, as `NodeCtx::log` will keep it: at most
/// [`re_proc::MAX_LOG_LINES`] lines (one more marks the cut), each cut to
/// [`re_proc::MAX_LOG_LINE`] bytes, so a script cannot fill the host's memory
/// past its own limit.
pub(crate) fn push_log(log: &Log, line: &[u8]) {
    let mut log = log.borrow_mut();
    if log.len() > re_proc::MAX_LOG_LINES {
        return;
    }
    let cut = &line[..line.len().min(re_proc::MAX_LOG_LINE)];
    log.push(String::from_utf8_lossy(cut).into_owned());
}

/// A fresh generator VM. `log` receives `print` output.
pub(crate) fn create(scripts: Arc<Scripts>, memory_mb: usize, log: Log) -> mlua::Result<Lua> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
    lua.set_compiler(crate::compile::compiler());
    let g = lua.globals();
    // No clock, no files, no global randomness: output depends only on the
    // parameters, the inputs, the seed and the scripts.
    for name in [
        "os",
        "loadstring",
        "getfenv",
        "setfenv",
        "collectgarbage",
        "gcinfo",
    ] {
        g.raw_set(name, Value::Nil)?;
    }
    // `tostring` of a table, function, userdata or thread without
    // `__tostring` is its type name, never its address.
    let stock: Function = g.get("tostring")?;
    g.raw_set(
        "tostring",
        lua.create_function(move |lua, v: Value| {
            let plain = match &v {
                Value::Table(t) => t
                    .metatable()
                    .is_none_or(|m| m.raw_get::<Value>("__tostring").is_ok_and(|f| f.is_nil())),
                Value::Function(_)
                | Value::Thread(_)
                | Value::LightUserData(_)
                | Value::UserData(_) => true,
                _ => false,
            };
            if plain {
                return Ok(Value::String(lua.create_string(v.type_name())?));
            }
            stock.call::<Value>(v)
        })?,
    )?;
    let math: Table = g.get("math")?;
    for name in ["random", "randomseed"] {
        math.raw_set(name, Value::Nil)?;
    }
    portable_math(&lua)?;
    g.raw_set("proc", proc_lib(&lua)?)?;
    let mut quat = Lib::new(&lua, "quat", "Rotations.")?;
    crate::api::math::quat_lib(&lua, &mut quat)?;
    g.raw_set("quat", quat.finish(&lua)?.0)?;
    let mut color = Lib::new(&lua, "color", "Linear colours.")?;
    crate::api::math::color_lib(&lua, &mut color)?;
    g.raw_set("color", color.finish(&lua)?.0)?;
    let tostring: Function = g.raw_get("tostring")?;
    g.raw_set(
        "print",
        lua.create_function(move |_, args: MultiValue| {
            let mut line = Vec::new();
            for (i, v) in args.into_iter().enumerate() {
                if i > 0 {
                    line.push(b'\t');
                }
                let s: mlua::LuaString = tostring.call(v)?;
                line.extend_from_slice(&s.as_bytes());
                if line.len() > re_proc::MAX_LOG_LINE {
                    break;
                }
            }
            push_log(&log, &line);
            Ok(())
        })?,
    )?;
    lua.set_named_registry_value(MODULES, lua.create_table()?)?;
    let require = lua.create_function(move |lua, name: String| require(lua, &scripts, &name))?;
    g.raw_set("require", require)?;
    lua.sandbox(true)?;
    if memory_mb > 0 {
        lua.set_memory_limit(memory_mb << 20)?;
    }
    // SAFETY: only the `interrupt` field of this VM's callbacks is written,
    // with a function of the right signature, while no Luau code runs.
    unsafe {
        lua.exec_raw::<()>((), |state| {
            (*ffi::lua_callbacks(state)).interrupt = Some(interrupt);
        })?;
    }
    Ok(lua)
}

/// The transcendental `math` functions on every machine alike (ADR-0036).
/// Until WP18's portable maths lands this is the host's `math`, so `sin`,
/// `exp` and friends may differ in the last bit between platforms; landing
/// it bumps `CONTEXT_VERSION`.
fn portable_math(_lua: &Lua) -> mlua::Result<()> {
    Ok(()) // PENDING-D2: crate::portable_math::install(lua) once #100 lands
}

/// Loads the chunk at `path` (a project path) from `scripts`.
pub(crate) fn load(lua: &Lua, scripts: &Scripts, path: &str) -> mlua::Result<Function> {
    let bytecode = scripts
        .get(path)
        .ok_or_else(|| mlua::Error::runtime(format!("no script {path}")))?;
    lua.load(&bytecode[..])
        .set_name(format!("@{path}"))
        .into_function()
}

/// `require(name)`: `./x` and `../x` relative to the calling script, any
/// other name relative to `scripts/generators/`; `.luau` may be left out.
/// Only scripts under `scripts/generators/` load. Each module runs once per
/// VM.
fn require(lua: &Lua, scripts: &Scripts, name: &str) -> mlua::Result<Value> {
    let base = if name.starts_with("./") || name.starts_with("../") {
        caller_dir(lua).unwrap_or_else(|| GENERATORS_DIR.to_string())
    } else {
        GENERATORS_DIR.to_string()
    };
    let mut parts: Vec<&str> = base.split('/').filter(|p| !p.is_empty()).collect();
    for part in name.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut path = parts.join("/");
    if !path.ends_with(".luau") {
        path.push_str(".luau");
    }
    if !path.starts_with(&format!("{GENERATORS_DIR}/")) || !scripts.contains_key(&path) {
        return Err(mlua::Error::runtime(format!(
            "require({name:?}): no module {path}; generator scripts can require only modules under {GENERATORS_DIR}/"
        )));
    }
    let modules: Table = lua.named_registry_value(MODULES)?;
    let cached: Value = modules.raw_get(path.as_str())?;
    if let Value::LightUserData(_) = cached {
        return Err(mlua::Error::runtime(format!(
            "require({name:?}): {path} is already loading (a cycle of requires)"
        )));
    }
    if !cached.is_nil() {
        return Ok(cached);
    }
    // A marker while the module runs: requiring it again is a cycle.
    modules.raw_set(
        path.as_str(),
        Value::LightUserData(mlua::LightUserData(std::ptr::null_mut())),
    )?;
    let chunk = load(lua, scripts, &path)?;
    let value: Value = match chunk.call(()) {
        Ok(v) => v,
        Err(e) => {
            modules.raw_set(path.as_str(), Value::Nil)?;
            return Err(e);
        }
    };
    let value = if value.is_nil() {
        Value::Boolean(true)
    } else {
        value
    };
    modules.raw_set(path.as_str(), value.clone())?;
    Ok(value)
}

/// The directory of the nearest calling script (`scripts/generators/...`).
fn caller_dir(lua: &Lua) -> Option<String> {
    for level in 1..8 {
        let src = lua.inspect_stack(level, |d| {
            d.source()
                .source
                .map(|s| s.trim_start_matches('@').to_string())
        });
        match src {
            Some(Some(s)) if s.starts_with("scripts/") => {
                return Some(s.rsplit_once('/').map_or(String::new(), |(d, _)| d.into()));
            }
            None => return None,
            _ => {}
        }
    }
    None
}

/// The `proc` library of the generator context: `proc.node{...}` marks a
/// declaration, `proc.param.*{...}` a parameter.
fn proc_lib(lua: &Lua) -> mlua::Result<Table> {
    let node_meta = lua.create_table()?;
    node_meta.raw_set("__metatable", "proc.node")?;
    node_meta.set_readonly(true);
    lua.set_named_registry_value(NODE_META, &node_meta)?;
    let param_meta = lua.create_table()?;
    param_meta.raw_set("__metatable", "proc.param")?;
    param_meta.set_readonly(true);
    lua.set_named_registry_value(PARAM_META, &param_meta)?;

    let proc = lua.create_table()?;
    proc.raw_set(
        "node",
        lua.create_function(|lua, decl: Table| {
            if decl.metatable().is_some() {
                return Err(mlua::Error::runtime(
                    "proc.node: the declaration must be a plain table",
                ));
            }
            let meta: Table = lua.named_registry_value(NODE_META)?;
            decl.set_metatable(Some(meta))?;
            Ok(decl)
        })?,
    )?;
    let param = lua.create_table()?;
    for kind in ["number", "integer", "bool", "string", "enum", "vec3"] {
        param.raw_set(
            kind,
            lua.create_function(move |lua, opts: Option<Table>| {
                let opts = match opts {
                    Some(t) if t.metatable().is_none() => t,
                    Some(_) => {
                        return Err(mlua::Error::runtime(format!(
                            "proc.param.{kind}: the options must be a plain table"
                        )));
                    }
                    None => lua.create_table()?,
                };
                opts.raw_set("type", kind)?;
                let meta: Table = lua.named_registry_value(PARAM_META)?;
                opts.set_metatable(Some(meta))?;
                Ok(opts)
            })?,
        )?;
    }
    param.set_readonly(true);
    proc.raw_set("param", param)?;
    proc.set_readonly(true);
    Ok(proc)
}

/// True if `t` carries the registry metatable `key`.
pub(crate) fn marked(lua: &Lua, t: &Table, key: &str) -> bool {
    let Ok(meta) = lua.named_registry_value::<Table>(key) else {
        return false;
    };
    t.metatable().is_some_and(|m| m == meta)
}
