//! One sandboxed Luau VM per context (ADR-0011).
//!
//! Each VM:
//!
//! * opens `StdLib::ALL_SAFE` and removes `loadstring`, `getfenv` and `setfenv`
//!   **before** `sandbox(true)` freezes the globals (afterwards they could not be
//!   removed, and `getfenv`/`setfenv` would turn off `safeenv`);
//! * installs the engine API ([`crate::api`]), `print`, `warn` and the
//!   project-confined `require` ([`crate::modules`]);
//! * has a memory limit and the raw CPU-budget interrupt (`budget`).
//!
//! Every script and module runs in its own environment table whose metatable
//! `__index`es the frozen globals. Writes stay in the script's table, so a global
//! one script sets is `nil` in another; the table is marked `safeenv`, so builtin
//! fastcalls and import caching stay on.

use std::path::Path;
use std::time::Duration;

use mlua::{Function, Lua, LuaOptions, Result, StdLib, Table, Value};
use re_scene::{Entity, ScriptContext};

use crate::diag::TRACEBACK_FN;
use crate::host::HostSlot;
use crate::modules::{Loaded, ProjectRequirer};
use crate::{api, scheduler};

/// Registry key of the frozen globals table that environments `__index`.
pub(crate) const GLOBALS_KEY: &str = "re_script.globals";

/// The most output lines a context keeps before dropping the oldest.
pub const MAX_OUTPUT_LINES: usize = 10_000;

/// A named script context: one VM with its own globals, modules and scheduler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContextKind {
    /// Gameplay scripts (`ScriptRef` with context `Game`).
    Game,
    /// Editor and agent tools, ProcGraph builders; also has the `command` library.
    Tools,
}

impl ContextKind {
    /// Both contexts, in the order the runtime runs them.
    pub const ALL: [ContextKind; 2] = [ContextKind::Game, ContextKind::Tools];

    /// `"game"` or `"tools"` (the value of `script.context`).
    pub fn name(self) -> &'static str {
        match self {
            ContextKind::Game => "game",
            ContextKind::Tools => "tools",
        }
    }
}

impl ContextKind {
    /// The native context a `ScriptRef` with `context` runs in; `None` for the
    /// compatibility layer's contexts (`server`, `client`, `module`), which
    /// the native runtime does not start.
    pub fn of_script(context: ScriptContext) -> Option<ContextKind> {
        match context {
            ScriptContext::Game => Some(ContextKind::Game),
            ScriptContext::Tools => Some(ContextKind::Tools),
            ScriptContext::Server | ScriptContext::Client | ScriptContext::Module => None,
        }
    }
}

/// A line written by `print` or `warn`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputLine {
    /// The context that wrote it.
    pub context: ContextKind,
    /// True for `warn`.
    pub warn: bool,
    /// The arguments, converted with `tostring` and separated by tabs.
    pub text: String,
}

/// The output buffer of one VM (app data).
pub(crate) struct Output {
    kind: ContextKind,
    pub(crate) lines: Vec<OutputLine>,
}

/// Appends a line to the context's output and the log.
pub(crate) fn push_output(lua: &Lua, warn: bool, text: String) {
    let Some(mut out) = lua.app_data_mut::<Output>() else {
        return;
    };
    if warn {
        tracing::warn!(target: "re_script", context = out.kind.name(), "{text}");
    } else {
        tracing::info!(target: "re_script", context = out.kind.name(), "{text}");
    }
    if out.lines.len() >= MAX_OUTPUT_LINES {
        out.lines.remove(0);
    }
    let kind = out.kind;
    out.lines.push(OutputLine {
        context: kind,
        warn,
        text,
    });
}

/// Installs the engine into `lua`'s globals (everything except `require`, the
/// sandbox and the limits). Returns the API declarations.
pub(crate) fn install_engine(
    lua: &Lua,
    kind: ContextKind,
    budget: Duration,
) -> Result<Vec<String>> {
    let globals = lua.globals();
    // Untrusted scripts must not compile code at runtime; getfenv/setfenv expose
    // environments and deoptimise them (they clear safeenv).
    for name in ["loadstring", "getfenv", "setfenv"] {
        globals.raw_set(name, Value::Nil)?;
    }
    let debug: Table = globals.get("debug")?;
    lua.set_named_registry_value(TRACEBACK_FN, debug.get::<Function>("traceback")?)?;

    lua.set_app_data(HostSlot::default());
    lua.set_app_data(Output {
        kind,
        lines: Vec::new(),
    });
    scheduler::init(lua, budget)?;
    api::entity::init(lua)?;

    let decls = api::install(lua, &globals, kind)?;
    globals.raw_set("print", api::libs::output_fn(lua, false)?)?;
    globals.raw_set("warn", api::libs::output_fn(lua, true)?)?;
    lua.set_named_registry_value(GLOBALS_KEY, &globals)?;
    Ok(decls)
}

/// Creates the VM of one context.
pub(crate) fn create(
    root: &Path,
    kind: ContextKind,
    budget: Duration,
    memory_limit: usize,
    loaded: Loaded,
) -> Result<Lua> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
    install_engine(&lua, kind, budget)?;
    let require =
        lua.create_require_function(ProjectRequirer::new(root.to_path_buf(), kind, loaded))?;
    lua.globals().raw_set("require", require)?;
    // Freezes the globals and every library table, and turns safeenv on.
    lua.sandbox(true)?;
    if memory_limit > 0 {
        lua.set_memory_limit(memory_limit)?;
    }
    crate::budget::install(&lua)?;
    Ok(lua)
}

/// A fresh environment for the script or module at `path`: writes stay local,
/// reads fall through to the frozen globals, and `script` describes it.
pub(crate) fn new_env(
    lua: &Lua,
    path: &str,
    kind: ContextKind,
    entity: Option<Entity>,
) -> Result<Table> {
    let globals: Table = lua.named_registry_value(GLOBALS_KEY)?;
    let env = lua.create_table()?;
    env.raw_set("script", api::libs::script_table(lua, path, kind, entity)?)?;
    let mt = lua.create_table()?;
    mt.raw_set("__index", &globals)?;
    mt.set_readonly(true);
    env.set_metatable(Some(mt))?;
    env.set_safeenv(true);
    Ok(env)
}
