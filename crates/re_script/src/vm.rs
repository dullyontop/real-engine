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
    /// The compatibility layer's server scripts (ADR-0012). Created and driven
    /// by `re_compat` with [`Context::compat`](crate::Context::compat), never
    /// by [`ScriptRuntime`](crate::ScriptRuntime).
    Server,
    /// The compatibility layer's client scripts, like [`ContextKind::Server`].
    Client,
}

impl ContextKind {
    /// The native contexts, in the order [`ScriptRuntime`](crate::ScriptRuntime)
    /// runs them.
    pub const ALL: [ContextKind; 2] = [ContextKind::Game, ContextKind::Tools];

    /// The compatibility layer's contexts.
    pub const COMPAT: [ContextKind; 2] = [ContextKind::Server, ContextKind::Client];

    /// `"game"`, `"tools"`, `"server"` or `"client"` (the value of `script.context`).
    pub fn name(self) -> &'static str {
        match self {
            ContextKind::Game => "game",
            ContextKind::Tools => "tools",
            ContextKind::Server => "server",
            ContextKind::Client => "client",
        }
    }

    /// True for [`ContextKind::Server`] and [`ContextKind::Client`].
    pub fn is_compat(self) -> bool {
        matches!(self, ContextKind::Server | ContextKind::Client)
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
    files: &re_core::files::ProjectFiles,
    kind: ContextKind,
    budget: Duration,
    memory_limit: usize,
    loaded: Loaded,
) -> Result<Lua> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
    install_engine(&lua, kind, budget)?;
    let require = lua.create_require_function(ProjectRequirer::new(files.clone(), kind, loaded))?;
    lua.globals().raw_set("require", require)?;
    // Freezes the globals and every library table, and turns safeenv on.
    lua.sandbox(true)?;
    if memory_limit > 0 {
        lua.set_memory_limit(memory_limit)?;
    }
    crate::budget::install(&lua)?;
    Ok(lua)
}

/// Creates the VM of a compatibility context: the safe standard library, the
/// sandbox, the memory limit and the CPU budget as for the native contexts,
/// with `print`, `warn` and `task`, but none of the native engine libraries and
/// no `require`. `install` adds the layer's globals before the sandbox freezes
/// them.
pub(crate) fn create_compat(
    kind: ContextKind,
    budget: Duration,
    memory_limit: usize,
    install: &dyn Fn(&Lua) -> Result<()>,
) -> Result<Lua> {
    let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default())?;
    let globals = lua.globals();
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
    scheduler::init(&lua, budget)?;
    scheduler::enable_adoption(&lua)?;
    api::entity::init(&lua)?;
    install_pcall(&lua, &globals)?;
    let mut task = api::Lib::new(
        &lua,
        "task",
        "Script threads: waiting, spawning and deferring.",
    )?;
    scheduler::task_lib(&lua, &mut task)?;
    // M2 has no parallel Luau: every script runs serially already.
    for name in ["synchronize", "desynchronize"] {
        task.func(
            &lua,
            name,
            "() -> ()",
            "Does nothing: scripts always run serially (there is no parallel Luau).",
            |_, ()| Ok(()),
        )?;
    }
    let (task, _) = task.finish(&lua)?;
    globals.raw_set("task", task)?;
    globals.raw_set("print", api::libs::output_fn(&lua, false)?)?;
    globals.raw_set("warn", api::libs::output_fn(&lua, true)?)?;
    install(&lua)?;
    lua.set_named_registry_value(GLOBALS_KEY, &globals)?;
    lua.sandbox(true)?;
    if memory_limit > 0 {
        lua.set_memory_limit(memory_limit)?;
    }
    crate::budget::install(&lua)?;
    Ok(lua)
}

/// The message of an error raised by Rust code, without mlua's wrapping (the
/// callback chain and its traceback): what `pcall` gives scripts in compat
/// contexts, where scripts treat error values as strings.
pub fn error_message(e: &mlua::Error) -> String {
    match e {
        mlua::Error::RuntimeError(m) => m.clone(),
        mlua::Error::CallbackError { cause, .. } => error_message(cause),
        mlua::Error::WithContext { cause, .. } => error_message(cause),
        other => other.to_string(),
    }
}

/// Replaces `pcall` and `xpcall` with Luau wrappers (so they still yield)
/// that turn an error raised by Rust, which reaches Luau as an error object,
/// into its message string. Errors scripts raise with `error` are unchanged.
fn install_pcall(lua: &Lua, globals: &Table) -> Result<()> {
    let norm = lua.create_function(|lua, v: Value| match v {
        Value::Error(e) => Ok(Value::String(lua.create_string(error_message(&e))?)),
        other => Ok(other),
    })?;
    let (pcall, xpcall): (Function, Function) = lua
        .load(
            r#"
            local pcall_, xpcall_, NORM = pcall, xpcall, ...
            local function fix(ok, ...)
                if ok then
                    return ok, ...
                end
                return false, NORM((...))
            end
            local function pcall(f, ...)
                return fix(pcall_(f, ...))
            end
            local function xpcall(f, handler, ...)
                return xpcall_(f, function(e)
                    return handler(NORM(e))
                end, ...)
            end
            return pcall, xpcall
            "#,
        )
        .set_name("=pcall")
        .call(norm)?;
    globals.raw_set("pcall", pcall)?;
    globals.raw_set("xpcall", xpcall)?;
    Ok(())
}

/// Writes a line to the context's output (as `print` does, or `warn` when
/// `warn` is true) and to the log.
pub fn output(lua: &Lua, warn: bool, text: impl Into<String>) {
    push_output(lua, warn, text.into());
}

/// A fresh environment table over the context's frozen globals: writes stay
/// in it, reads fall through. `script` is set as its `script` global.
pub fn environment(lua: &Lua, script: Value) -> Result<Table> {
    let globals: Table = lua.named_registry_value(GLOBALS_KEY)?;
    let env = lua.create_table()?;
    env.raw_set("script", script)?;
    let mt = lua.create_table()?;
    mt.raw_set("__index", &globals)?;
    mt.set_readonly(true);
    env.set_metatable(Some(mt))?;
    env.set_safeenv(true);
    Ok(env)
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
