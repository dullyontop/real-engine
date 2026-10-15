//! `RunService`: the frame events, render-step bindings and the context
//! queries.
//!
//! The events fire in the engine phases of ARCHITECTURE §3, driven by
//! [`CompatVm`](crate::runtime::CompatVm)'s phase methods:
//!
//! | phase | what runs, in order |
//! |---|---|
//! | `PreRender` (client only, once per frame, frame `dt`) | render-step bindings by priority, lowest first (equal priorities in bind order); `PreRender`; `RenderStepped` |
//! | `PreAnimation` (each fixed step) | the clock advances by the step; `PreAnimation` |
//! | `PreSimulation` (each fixed step) | `PreSimulation`; `Stepped(time, dt)` |
//! | `PostSimulation` (each fixed step) | `PostSimulation`; `Heartbeat`; threads whose `task.wait` is due |
//!
//! Each event's handlers settle before the next event fires, so the order
//! holds with deferred signals too. A binding runs as its own thread: an error
//! in it is reported and the next binding still runs.

use std::sync::Arc;

use mlua::{
    Function, IntoLuaMulti, Lua, MultiValue, ObjectLike, RegistryKey, Result, Value as LuaValue,
};
use re_scene::Entity;
use re_script::ContextKind;
use re_script::codes::BAD_ARGUMENT;
use re_script::sched;

use super::find_service;
use crate::instance::methods_api::{args, ret};
use crate::runtime::{Bindings, lua_error, state, with_world};
use crate::signal;

/// One `BindToRenderStep` binding.
struct Binding {
    name: String,
    priority: i64,
    seq: u64,
    f: RegistryKey,
    /// The script that bound it; destroying that script unbinds it.
    owner: Option<sched::Owner>,
}

/// The context's render-step bindings.
#[derive(Default)]
pub(crate) struct RenderSteps {
    list: Vec<Binding>,
    next: u64,
}

fn priority_of(v: &LuaValue) -> Option<i64> {
    match v {
        LuaValue::Integer(i) => Some(*i),
        LuaValue::Number(n) if n.is_finite() => Some(n.floor() as i64),
        // An `EnumItem` (`Enum.RenderPriority.Camera`): its `Value`.
        LuaValue::UserData(u) => u.get::<f64>("Value").ok().map(|n| n.floor() as i64),
        _ => None,
    }
}

fn bind_to_render_step(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let (name, priority, f): (String, LuaValue, Function) =
        args(lua, a, "RunService:BindToRenderStep")?;
    let priority = priority_of(&priority).ok_or_else(|| {
        lua_error(
            BAD_ARGUMENT,
            "bad argument #2 to RunService:BindToRenderStep: priority must be a number",
        )
    })?;
    let f = lua.create_registry_value(f)?;
    let owner = sched::current_owner(lua)?;
    let mut steps = lua
        .app_data_mut::<RenderSteps>()
        .ok_or_else(|| mlua::Error::runtime("this VM has no render steps"))?;
    let seq = steps.next;
    steps.next += 1;
    steps.list.push(Binding {
        name,
        priority,
        seq,
        f,
        owner,
    });
    ret(lua, ())
}

fn unbind_from_render_step(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "RunService:UnbindFromRenderStep")?;
    unbind(lua, |b| b.name == name)?;
    ret(lua, ())
}

/// Removes the bindings `gone` picks.
fn unbind(lua: &Lua, gone: impl Fn(&Binding) -> bool) -> Result<()> {
    let removed: Vec<RegistryKey> = {
        let Some(mut steps) = lua.app_data_mut::<RenderSteps>() else {
            return Ok(());
        };
        let (gone, kept) = std::mem::take(&mut steps.list)
            .into_iter()
            .partition(|b| gone(b));
        steps.list = kept;
        gone.into_iter().map(|b: Binding| b.f).collect()
    };
    for k in removed {
        lua.remove_registry_value(k)?;
    }
    Ok(())
}

/// Removes the bindings a stopped script made.
pub(crate) fn drop_owner(lua: &Lua, owner: sched::Owner) -> Result<()> {
    unbind(lua, |b| b.owner == Some(owner))
}

fn is_server(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    ret(lua, state(lua)?.kind == ContextKind::Server)
}

fn is_client(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    ret(lua, state(lua)?.kind == ContextKind::Client)
}

fn is_studio(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    ret(lua, state(lua)?.info.studio)
}

fn is_running(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    ret(lua, true)
}

fn is_edit(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    ret(lua, false)
}

type Imp = fn(&Lua, Entity, MultiValue) -> Result<MultiValue>;

/// Registers the `RunService` methods.
pub(crate) fn register(b: &mut Bindings) {
    let list: &[(&str, Imp)] = &[
        ("BindToRenderStep", bind_to_render_step),
        ("UnbindFromRenderStep", unbind_from_render_step),
        ("IsServer", is_server),
        ("IsClient", is_client),
        ("IsStudio", is_studio),
        ("IsRunning", is_running),
        ("IsEdit", is_edit),
    ];
    for (name, f) in list {
        b.method("RunService", name, Arc::new(*f));
    }
}

/// Installs the render-step list into a new VM.
pub(crate) fn install(lua: &Lua) {
    lua.set_app_data(RenderSteps::default());
}

/// The `RunService` instance, when the place has one (nothing can listen to
/// a service that does not exist yet).
fn service(lua: &Lua) -> Result<Option<Entity>> {
    let game = state(lua)?.game;
    with_world(lua, |w| find_service(w, game, "RunService"))
}

fn fire(lua: &Lua, name: &str, args: impl IntoLuaMulti) -> Result<()> {
    if let Some(rs) = service(lua)? {
        signal::fire(lua, rs, name, args.into_lua_multi(lua)?)?;
        sched::settle(lua)?;
    }
    Ok(())
}

/// The render step (client only): the bindings by priority, then
/// `PreRender` and `RenderStepped`.
pub(crate) fn pre_render(lua: &Lua, dt: f64) -> Result<()> {
    if state(lua)?.kind != ContextKind::Client {
        return Ok(());
    }
    let order: Vec<u64> = {
        let steps = lua
            .app_data_ref::<RenderSteps>()
            .ok_or_else(|| mlua::Error::runtime("this VM has no render steps"))?;
        let mut v: Vec<&Binding> = steps.list.iter().collect();
        v.sort_by_key(|b| (b.priority, b.seq));
        v.into_iter().map(|b| b.seq).collect()
    };
    for seq in order {
        // A binding an earlier one removed this frame does not run.
        let found = {
            let steps = lua
                .app_data_ref::<RenderSteps>()
                .ok_or_else(|| mlua::Error::runtime("this VM has no render steps"))?;
            match steps.list.iter().find(|b| b.seq == seq) {
                Some(b) => Some((
                    b.name.clone(),
                    lua.registry_value::<Function>(&b.f)?,
                    b.owner,
                )),
                None => None,
            }
        };
        let Some((name, f, owner)) = found else {
            continue;
        };
        let args = (dt,).into_lua_multi(lua)?;
        let label = format!("render step {name}");
        match owner {
            Some(o) => sched::spawn_owned(lua, f, args, &label, o)?,
            None => sched::spawn(lua, f, args, &label)?,
        };
        sched::settle(lua)?;
    }
    fire(lua, "PreRender", dt)?;
    fire(lua, "RenderStepped", dt)
}

/// The start of a fixed step: the clock advances, then `PreAnimation`.
pub(crate) fn pre_animation(lua: &Lua, dt: f64) -> Result<()> {
    sched::advance(lua, dt)?;
    fire(lua, "PreAnimation", dt)
}

/// Before physics: `PreSimulation`, then `Stepped(time, dt)`.
pub(crate) fn pre_simulation(lua: &Lua, dt: f64) -> Result<()> {
    fire(lua, "PreSimulation", dt)?;
    let now = sched::now(lua)?;
    fire(lua, "Stepped", (now, dt))
}

/// After physics: `PostSimulation`, `Heartbeat`, then the threads whose wait
/// is due.
pub(crate) fn post_simulation(lua: &Lua, dt: f64) -> Result<()> {
    fire(lua, "PostSimulation", dt)?;
    fire(lua, "Heartbeat", dt)?;
    sched::resume_due(lua)?;
    Ok(())
}
