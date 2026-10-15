//! What `game` reports about the session (`PlaceId`, `JobId`, ...), from
//! `real.toml` `[compat.game]`, and `game:BindToClose`.
//!
//! The values live in the world as the [`Session`] resource, so the builtin
//! accessors (which see only the world) read them. Shutdown
//! ([`CompatVm::close`](super::CompatVm::close)) runs every `BindToClose`
//! callback as its own thread, in bind order, and keeps stepping the clock
//! until they have all finished or [`CLOSE_TIMEOUT_SECS`] of game time pass,
//! as Roblox gives them 30 seconds.

use std::sync::Arc;

use mlua::thread::ThreadStatus;
use mlua::{Function, Lua, MultiValue, RegistryKey, Result, Thread};
use re_reflect::Value;
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::prelude::Resource;
use re_scene::{Entity, World};

use super::Bindings;
use crate::class::Accessor;
use crate::instance::methods_api::{args, ret};
use crate::scene::GameInfo;

/// How long shutdown waits for the `BindToClose` callbacks, in seconds of
/// game time.
pub const CLOSE_TIMEOUT_SECS: f64 = 30.0;

/// The session values of a world's `game`.
#[derive(Resource, Clone, Debug, Default)]
pub struct Session(pub GameInfo);

/// Puts `info` into the world for the `game` accessors.
pub(crate) fn prepare_world(w: &mut World, info: &GameInfo) {
    w.ecs.insert_resource(Session(info.clone()));
}

fn read<T>(w: &World, f: impl Fn(&GameInfo) -> T) -> T {
    match w.ecs.get_resource::<Session>() {
        Some(s) => f(&s.0),
        None => f(&GameInfo::default()),
    }
}

fn int(f: fn(&GameInfo) -> i64) -> Accessor {
    Accessor {
        get: Arc::new(move |w, _| Ok(Value::Int(read(w, f)))),
        set: None,
    }
}

fn string(f: fn(&GameInfo) -> String) -> Accessor {
    Accessor {
        get: Arc::new(move |w, _| Ok(Value::Str(read(w, f)))),
        set: None,
    }
}

/// The `BindToClose` callbacks of a VM, in bind order.
#[derive(Default)]
pub(crate) struct CloseCallbacks(Vec<RegistryKey>);

fn bind_to_close(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let f: Function = args(lua, a, "DataModel:BindToClose")?;
    let k = lua.create_registry_value(f)?;
    lua.app_data_mut::<CloseCallbacks>()
        .ok_or_else(|| mlua::Error::runtime("this VM has no close callbacks"))?
        .0
        .push(k);
    ret(lua, ())
}

/// Installs the callback list into a new VM.
pub(crate) fn install(lua: &Lua) {
    lua.set_app_data(CloseCallbacks::default());
}

/// Starts every `BindToClose` callback (each once; the list is emptied) and
/// returns their threads.
pub(crate) fn start_close(lua: &Lua) -> Result<Vec<Thread>> {
    let keys = lua
        .app_data_mut::<CloseCallbacks>()
        .map(|mut c| std::mem::take(&mut c.0))
        .unwrap_or_default();
    let mut threads = Vec::with_capacity(keys.len());
    for k in keys {
        let f: Function = lua.registry_value(&k)?;
        lua.remove_registry_value(k)?;
        threads.push(re_script::sched::spawn(
            lua,
            f,
            MultiValue::new(),
            "BindToClose",
        )?);
    }
    Ok(threads)
}

/// True while any of `threads` has not finished.
pub(crate) fn any_running(threads: &[Thread]) -> bool {
    threads
        .iter()
        .any(|t| matches!(t.status(), ThreadStatus::Resumable | ThreadStatus::Running))
}

/// The accessors of `DataModel`'s session properties.
pub fn accessors() -> Vec<(&'static str, Accessor)> {
    vec![
        ("PlaceId", int(|g| g.place_id)),
        ("GameId", int(|g| g.game_id)),
        ("CreatorId", int(|g| g.creator_id)),
        ("JobId", string(|g| g.job_id.clone())),
        ("PrivateServerId", string(|g| g.private_server_id.clone())),
        ("PrivateServerOwnerId", int(|g| g.private_server_owner_id)),
    ]
}

/// Registers the `game` session properties and `BindToClose`.
pub(crate) fn register(b: &mut Bindings) {
    for (prop, a) in accessors() {
        b.accessor("DataModel", prop, a);
    }
    b.method("DataModel", "BindToClose", Arc::new(bind_to_close));
}
