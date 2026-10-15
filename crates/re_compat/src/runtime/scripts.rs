//! Scripts in the tree: which `Script` and `LocalScript` instances start in
//! which context, the `script` global, and `require` by instance.
//!
//! A script's source is the file its `ScriptRef` names, relative to the
//! project root. Each started script owns the threads it starts (its owner is
//! its entity), so destroying it stops them.
//!
//! Where scripts start (docs/M2_PLAN.md, WP11 "runtime"):
//! * a `Script` with run context `Legacy` runs in `server` under `Workspace`
//!   or `ServerScriptService`;
//! * a `LocalScript` runs in `client` under a player's `PlayerScripts`,
//!   `PlayerGui` or `Backpack`, or the player's character;
//! * a script whose `RunContext` is `Server` or `Client` runs in that context
//!   wherever it is, except under `ServerStorage`, `ReplicatedStorage` and the
//!   templates (`StarterPlayer`, `StarterGui`, `StarterPack`);
//! * nothing starts outside `game`.
//!
//! Until players exist (WP12), [`StartOptions::templates_as_player`] starts
//! the client scripts of the templates (`StarterPlayerScripts`, `StarterGui`)
//! in place, as if they had been copied to a player.
//!
//! `require(module)` runs a `ModuleScript` once per context and caches its
//! result. The module runs in the requiring thread, so it may yield. A module
//! required again while it loads in the same thread is a cycle (`RE3004`,
//! with the chain); another thread that requires it meanwhile waits for it.
//! A module that raised an error raises again on every later `require`.

use std::collections::HashMap;
use std::ffi::c_void;

use mlua::{Function, Lua, MultiValue, RegistryKey, Result, Thread, Value as LuaValue};
use re_core::Code;
use re_scene::{Entity, ScriptRef, World};
use re_script::{ContextKind, sched};

use super::{Bindings, lua_error, state, with_world};
use crate::class::ClassRegistry;
use crate::instance::{self, tree};
use crate::scene::script_context;

/// The code of a refused or failed `require`.
const REQUIRE: Code = re_script::codes::REQUIRE;
/// The code of a script whose source cannot be read.
const SCRIPT_READ: Code = re_script::codes::SCRIPT_READ;

/// Which scripts [`startable`] picks.
#[derive(Clone, Copy, Debug, Default)]
pub struct StartOptions {
    /// Start the `LocalScript`s of `StarterPlayerScripts` and `StarterGui` in
    /// place in the client context, standing in for a player's copies until
    /// players exist.
    pub templates_as_player: bool,
}

/// The `RunContext` of a script instance (`Legacy` when it has none).
fn run_context(w: &World, classes: &ClassRegistry, e: Entity) -> String {
    match classes.get_property(w, e, "RunContext") {
        Ok(re_reflect::Value::Enum(v)) if !v.is_empty() => v,
        _ => "Legacy".into(),
    }
}

/// True if an ancestor of `e` has class `class` (exactly).
fn under(w: &World, e: Entity, classes: &[&str]) -> bool {
    tree::ancestors(w, e)
        .into_iter()
        .any(|a| tree::class(w, a).is_some_and(|c| classes.contains(&c)))
}

/// The context script instance `e` starts in, if it starts at all.
pub fn start_side(
    w: &World,
    classes: &ClassRegistry,
    game: Entity,
    e: Entity,
    opts: StartOptions,
) -> Option<ContextKind> {
    let class = tree::class(w, e)?;
    if !classes.is_a(class, "BaseScript") || tree::is_destroyed(w, e) || !tree::in_game(w, game, e)
    {
        return None;
    }
    let rc = run_context(w, classes, e);
    let side = match script_context(class, Some(&rc))? {
        re_scene::ScriptContext::Server => ContextKind::Server,
        re_scene::ScriptContext::Client => ContextKind::Client,
        _ => return None,
    };
    let legacy = rc == "Legacy" || class == "LocalScript";
    let starts = if !legacy {
        !under(
            w,
            e,
            &[
                "ServerStorage",
                "ReplicatedStorage",
                "StarterPlayer",
                "StarterGui",
                "StarterPack",
            ],
        )
    } else if side == ContextKind::Server {
        under(w, e, &["Workspace", "ServerScriptService"])
    } else {
        under(w, e, &["PlayerScripts", "PlayerGui", "Backpack"])
            || (opts.templates_as_player
                && under(w, e, &["StarterPlayerScripts", "StarterGui"])
                && !under(w, e, &["StarterCharacterScripts"]))
    };
    starts.then_some(side)
}

/// The scripts under `game` that start in context `kind`, in tree order.
pub fn startable(
    w: &World,
    classes: &ClassRegistry,
    game: Entity,
    kind: ContextKind,
    opts: StartOptions,
) -> Vec<Entity> {
    tree::descendants(w, game)
        .into_iter()
        .filter(|e| start_side(w, classes, game, *e, opts) == Some(kind))
        .collect()
}

/// The chunk name (the instance's full name) and source of a script instance.
fn source_of(lua: &Lua, e: Entity) -> Result<(String, String)> {
    let root = state(lua)?.project_root.clone();
    let (name, path) = with_world(lua, |w| {
        (
            tree::full_name(w, e),
            w.ecs.get::<ScriptRef>(e).map(|r| r.path.clone()),
        )
    })?;
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return Err(lua_error(
            SCRIPT_READ,
            format!("{name} has no source: it has no ScriptRef"),
        ));
    };
    let file = root.join(&path);
    let text = std::fs::read_to_string(&file).map_err(|err| {
        lua_error(
            SCRIPT_READ,
            format!("cannot read {path} (the source of {name}): {err}"),
        )
    })?;
    Ok((name, text))
}

/// Compiles script instance `e` with its own environment (`script` is its
/// handle).
fn compile(lua: &Lua, e: Entity) -> Result<(String, Function)> {
    let (name, text) = source_of(lua, e)?;
    let env = re_script::vm::environment(lua, instance::push(lua, e)?)?;
    let f = super::load_chunk(lua, &name, &text, env)?;
    Ok((name, f))
}

/// The owner id of the threads a script starts.
pub fn owner_of(e: Entity) -> sched::Owner {
    e.to_bits()
}

/// Starts script instance `e` now in a thread it owns.
pub(crate) fn start(lua: &Lua, e: Entity) -> Result<()> {
    let (name, f) = compile(lua, e)?;
    sched::spawn_owned(lua, f, MultiValue::new(), &name, owner_of(e))?;
    Ok(())
}

/// Stops what script instance `e` started (when it is destroyed).
pub(crate) fn stop(lua: &Lua, e: Entity) -> Result<()> {
    crate::signal::disconnect_owner(lua, owner_of(e))?;
    sched::stop_owner(lua, owner_of(e)).map(|_| ())
}

// ---------------------------------------------------------------------------
// require
// ---------------------------------------------------------------------------

enum Module {
    /// Running in the thread with this pointer.
    Loading(*const c_void),
    Done(RegistryKey),
    Failed,
}

#[derive(Default)]
struct Modules {
    by_entity: HashMap<Entity, Module>,
    /// The modules each thread is loading, outermost first.
    stacks: HashMap<*const c_void, Vec<Entity>>,
}

pub(crate) fn install(lua: &Lua) -> Result<()> {
    lua.set_app_data(Modules::default());
    Ok(())
}

/// The modules `require` has run in this context (loaded, loading or
/// failed), in no particular order.
pub fn required(lua: &Lua) -> Vec<Entity> {
    lua.app_data_ref::<Modules>()
        .map(|m| m.by_entity.keys().copied().collect())
        .unwrap_or_default()
}

fn module_arg(lua: &Lua, v: &LuaValue) -> Result<Entity> {
    if let LuaValue::Integer(_) | LuaValue::Number(_) = v {
        return Err(lua_error(
            crate::codes::NOT_AVAILABLE_OFFLINE,
            "require by asset id loads a published module, which is not available offline",
        ));
    }
    let e = match v {
        LuaValue::UserData(ud) => instance::entity_of(ud),
        _ => None,
    };
    let ok = e.is_some_and(|e| {
        with_world(lua, |w| tree::class(w, e) == Some("ModuleScript")).unwrap_or(false)
    });
    match e {
        Some(e) if ok => Ok(e),
        _ => Err(lua_error(
            REQUIRE,
            "Attempted to call require with invalid argument(s): expected a ModuleScript",
        )),
    }
}

/// `require`'s first half: `("done", value)`, `("load", chunk)` or
/// `("wait")` (another thread is loading the module).
fn begin(lua: &Lua, (m, t): (LuaValue, Thread)) -> Result<(String, LuaValue)> {
    let e = module_arg(lua, &m)?;
    let me = t.to_pointer();
    let cycle: Option<Vec<Entity>> = {
        let mods = lua.app_data_ref::<Modules>().expect("installed");
        match mods.by_entity.get(&e) {
            Some(Module::Done(k)) => {
                let v: LuaValue = lua.registry_value(k)?;
                return Ok(("done".into(), v));
            }
            Some(Module::Failed) => {
                return Err(lua_error(
                    REQUIRE,
                    "Requested module experienced an error while loading",
                ));
            }
            Some(Module::Loading(by)) => {
                let stack = mods.stacks.get(&me).cloned().unwrap_or_default();
                if *by == me || stack.contains(&e) {
                    Some(stack)
                } else {
                    return Ok(("wait".into(), LuaValue::Nil));
                }
            }
            None => None,
        }
    };
    if let Some(stack) = cycle {
        let names = with_world(lua, |w| {
            let start = stack.iter().position(|x| *x == e).unwrap_or(0);
            stack[start..]
                .iter()
                .chain([&e])
                .map(|x| tree::full_name(w, *x))
                .collect::<Vec<_>>()
                .join(" -> ")
        })?;
        return Err(lua_error(
            REQUIRE,
            format!("Requested module was required recursively: {names}"),
        ));
    }
    let (_, f) = compile(lua, e)?;
    let mut mods = lua.app_data_mut::<Modules>().expect("installed");
    mods.by_entity.insert(e, Module::Loading(me));
    mods.stacks.entry(me).or_default().push(e);
    Ok(("load".into(), LuaValue::Function(f)))
}

/// `require`'s second half: files the module's outcome; returns `(true,
/// value)` or `(false, error)`.
fn finish(lua: &Lua, args: MultiValue) -> Result<(bool, LuaValue)> {
    let mut it = args.into_iter();
    let m = it.next().unwrap_or(LuaValue::Nil);
    let t = match it.next() {
        Some(LuaValue::Thread(t)) => t,
        _ => return Err(mlua::Error::runtime("require: bad internal call")),
    };
    let ok = matches!(it.next(), Some(LuaValue::Boolean(true)));
    let n = match it.next() {
        Some(LuaValue::Integer(n)) => n,
        Some(LuaValue::Number(n)) => n as i64,
        _ => 0,
    };
    let values: Vec<LuaValue> = it.collect();
    let e = module_arg(lua, &m)?;
    let me = t.to_pointer();
    let outcome = if !ok {
        Err(values.into_iter().next().unwrap_or(LuaValue::Nil))
    } else if n != 1 {
        let name = with_world(lua, |w| tree::full_name(w, e))?;
        Err(LuaValue::String(lua.create_string(format!(
            "[{REQUIRE}] Module code did not return exactly one value ({name} returned {n})"
        ))?))
    } else {
        Ok(values.into_iter().next().unwrap_or(LuaValue::Nil))
    };
    let key = match &outcome {
        Ok(v) => Some(lua.create_registry_value(v.clone())?),
        Err(_) => None,
    };
    let mut mods = lua.app_data_mut::<Modules>().expect("installed");
    if let Some(stack) = mods.stacks.get_mut(&me) {
        if let Some(i) = stack.iter().rposition(|x| *x == e) {
            stack.remove(i);
        }
        if stack.is_empty() {
            mods.stacks.remove(&me);
        }
    }
    mods.by_entity.insert(
        e,
        match key {
            Some(k) => Module::Done(k),
            None => Module::Failed,
        },
    );
    Ok(match outcome {
        Ok(v) => (true, v),
        Err(err) => (false, err),
    })
}

fn require_fn(lua: &Lua) -> Result<LuaValue> {
    let begin = lua.create_function(begin)?;
    let finish = lua.create_function(finish)?;
    let f: Function = lua
        .load(
            r##"
            local BEGIN, FINISH = ...
            local running, select, wait = coroutine.running, select, task.wait
            local function done(m, t, ok, ...)
                local good, v = FINISH(m, t, ok, select("#", ...), ...)
                if good then
                    return v
                end
                error(v, 0)
            end
            return function(m)
                local t = running()
                while true do
                    local state, v = BEGIN(m, t)
                    if state == "done" then
                        return v
                    elseif state == "load" then
                        return done(m, t, pcall(v))
                    end
                    wait() -- another thread is loading it
                end
            end
            "##,
        )
        .set_name("=require")
        .call((begin, finish))?;
    Ok(LuaValue::Function(f))
}

/// Registers `require`.
pub(crate) fn register(b: &mut Bindings) {
    b.global("require", std::sync::Arc::new(|lua, _| require_fn(lua)));
}
