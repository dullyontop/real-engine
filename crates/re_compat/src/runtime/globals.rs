//! The legacy globals `wait`, `delay` and `spawn`, and `debug.profilebegin`
//! and `debug.profileend`.
//!
//! The legacy globals work over the `task` library and write one `RE9101`
//! note per context the first time each is used:
//! * `wait(t)` is `task.wait` with Roblox's floor of 0.03 s (so `wait()`
//!   waits two 60 Hz steps), and returns the time waited and the game time;
//! * `delay(t, f)` is `task.delay(t, f)`;
//! * `spawn(f)` is `task.defer(f)`: `f` runs later in the same cycle, not at
//!   once.
//!
//! `debug.profilebegin(label)` and `debug.profileend()` keep a per-context
//! stack of open labels (at most [`MAX_PROFILE_DEPTH`]; an unmatched
//! `profileend` is ignored). The engine's profiler takes static scope names
//! only (`profile_scope!`), so the script labels are not recorded yet: script
//! time shows under the runtime's own scopes.

use std::collections::BTreeSet;
use std::sync::Arc;

use mlua::{Function, Lua, Result, Table, Value as LuaValue};

use super::Bindings;
use crate::codes::DEPRECATED_MEMBER;

/// The deepest `debug.profilebegin` nesting kept; deeper labels are dropped.
pub const MAX_PROFILE_DEPTH: usize = 256;

/// The legacy globals already noted in this context.
#[derive(Default)]
struct Noted(BTreeSet<String>);

/// The open `debug.profilebegin` labels.
#[derive(Default)]
struct ProfileStack(Vec<String>);

/// Writes the `RE9101` note for `name` once per context.
fn note(lua: &Lua, name: &str, instead: &str) {
    let first = lua
        .app_data_mut::<Noted>()
        .is_some_and(|mut n| n.0.insert(name.to_string()));
    if first {
        re_script::vm::output(
            lua,
            true,
            format!("[{DEPRECATED_MEMBER}] {name} is deprecated; use {instead}"),
        );
    }
}

/// Installs the note set and the profile stack into a new VM.
pub(crate) fn install(lua: &Lua) {
    lua.set_app_data(Noted::default());
    lua.set_app_data(ProfileStack::default());
}

const LEGACY: &str = r#"
local task, NOTE, NOW = ...
local max = math.max
local function wait(seconds)
    NOTE("wait", "task.wait")
    if type(seconds) ~= "number" then
        seconds = 0
    end
    local waited = task.wait(max(seconds, 0.03))
    return waited, NOW()
end
local function delay(seconds, f, ...)
    NOTE("delay", "task.delay")
    return task.delay(seconds, f, ...)
end
local function spawn(f, ...)
    NOTE("spawn", "task.defer")
    return task.defer(f, ...)
end
return wait, delay, spawn
"#;

/// The three legacy functions, made once per VM.
fn legacy(lua: &Lua) -> Result<(Function, Function, Function)> {
    const KEY: &str = "re_compat.legacy";
    if let Ok(t) = lua.named_registry_value::<Table>(KEY) {
        return Ok((t.raw_get(1)?, t.raw_get(2)?, t.raw_get(3)?));
    }
    let task: Table = lua.globals().raw_get("task")?;
    let note_fn = lua.create_function(|lua, (name, instead): (String, String)| {
        note(lua, &name, &instead);
        Ok(())
    })?;
    let now = lua.create_function(|lua, ()| re_script::sched::now(lua))?;
    let (w, d, s): (Function, Function, Function) = lua
        .load(LEGACY)
        .set_name("=legacy")
        .call((task, note_fn, now))?;
    let t = lua.create_sequence_from([w.clone(), d.clone(), s.clone()])?;
    lua.set_named_registry_value(KEY, t)?;
    Ok((w, d, s))
}

fn profile_begin(lua: &Lua, label: LuaValue) -> Result<()> {
    let label = match label {
        LuaValue::String(s) => s.to_string_lossy(),
        other => other.to_string()?,
    };
    if let Some(mut s) = lua.app_data_mut::<ProfileStack>()
        && s.0.len() < MAX_PROFILE_DEPTH
    {
        s.0.push(label);
    }
    Ok(())
}

fn profile_end(lua: &Lua, (): ()) -> Result<()> {
    if let Some(mut s) = lua.app_data_mut::<ProfileStack>() {
        s.0.pop();
    }
    Ok(())
}

/// The `debug` library with `profilebegin` and `profileend` added.
fn debug_lib(lua: &Lua) -> Result<LuaValue> {
    let std_debug: Table = lua.globals().raw_get("debug")?;
    let t = lua.create_table()?;
    for pair in std_debug.pairs::<LuaValue, LuaValue>() {
        let (k, v) = pair?;
        t.raw_set(k, v)?;
    }
    t.raw_set("profilebegin", lua.create_function(profile_begin)?)?;
    t.raw_set("profileend", lua.create_function(profile_end)?)?;
    Ok(LuaValue::Table(t))
}

/// Registers `wait`, `delay`, `spawn` and the extended `debug`.
pub(crate) fn register(b: &mut Bindings) {
    b.global(
        "wait",
        Arc::new(|lua, _| Ok(LuaValue::Function(legacy(lua)?.0))),
    );
    b.global(
        "delay",
        Arc::new(|lua, _| Ok(LuaValue::Function(legacy(lua)?.1))),
    );
    b.global(
        "spawn",
        Arc::new(|lua, _| Ok(LuaValue::Function(legacy(lua)?.2))),
    );
    b.global("debug", Arc::new(|lua, _| debug_lib(lua)));
}
