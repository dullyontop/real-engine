//! Signals (`RBXScriptSignal`): the events of instances, connected from Luau
//! with `Connect`, `Once` and `Wait`, and fired from Rust with [`fire`].
//!
//! A signal is named by an instance and an event name ([`SignalKey`]):
//! `ChildAdded` on a folder, `Heartbeat` on `RunService`. Derived signals use
//! a suffix: `PropertyChanged:<property>` (`GetPropertyChangedSignal`),
//! `AttributeChanged:<name>` (`GetAttributeChangedSignal`) and
//! `InstanceAdded:<tag>` / `InstanceRemoved:<tag>` on `CollectionService`.
//!
//! Each script context has one [`SignalHub`] holding its connections, in
//! connection order. Firing runs every handler in its own script thread, so a
//! handler may yield:
//! * **deferred** ([`SignalBehavior::Deferred`], the default): each handler
//!   is queued to run at the end of the current resumption cycle, like
//!   `task.defer`. A handler disconnected before its turn does not run.
//! * **immediate**: each handler runs at once, before `fire` returns, like
//!   `task.spawn`.
//!
//! `Wait` parks the calling thread ([`re_script::sched::park`]) until the
//! next firing, which wakes it with the firing's arguments (in the same mode).
//! `Once` disconnects at the firing that runs it.
//!
//! The engine-side events (the tree, properties, attributes and tags) reach
//! the hub through [`events`], which turns world changes into firings.

pub mod events;

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use mlua::{
    AnyUserData, Function, Lua, MetaMethod, MultiValue, RegistryKey, Result, UserData,
    UserDataFields, UserDataMethods, Value as LuaValue,
};
use re_scene::Entity;
use re_script::sched::{self, WakeMode, YieldToken};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::runtime::{lua_error, state};

/// When signal handlers run (`real.toml [compat] signal_behavior`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum SignalBehavior {
    /// At the end of the current resumption cycle (the default).
    #[default]
    Deferred,
    /// At once, inside the call that fired the signal.
    Immediate,
}

impl SignalBehavior {
    /// `deferred` or `immediate`.
    pub fn as_str(self) -> &'static str {
        match self {
            SignalBehavior::Deferred => "deferred",
            SignalBehavior::Immediate => "immediate",
        }
    }

    fn wake_mode(self) -> WakeMode {
        match self {
            SignalBehavior::Deferred => WakeMode::Deferred,
            SignalBehavior::Immediate => WakeMode::Immediate,
        }
    }
}

/// Names one signal: an instance and an event name.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SignalKey {
    /// The instance the signal belongs to.
    pub entity: Entity,
    /// The event (`ChildAdded`, `PropertyChanged:Name`).
    pub name: Rc<str>,
}

impl SignalKey {
    /// The signal `name` of `entity`.
    pub fn new(entity: Entity, name: &str) -> SignalKey {
        SignalKey {
            entity,
            name: Rc::from(name),
        }
    }
}

enum Target {
    Func(RegistryKey),
    Wait(YieldToken),
}

struct Conn {
    key: SignalKey,
    target: Target,
    once: bool,
}

/// The connections of one script context, in connection order.
#[derive(Default)]
pub struct SignalHub {
    conns: BTreeMap<u64, Conn>,
    by_key: HashMap<SignalKey, Vec<u64>>,
    by_entity: HashMap<Entity, Vec<Rc<str>>>,
    next: u64,
}

impl SignalHub {
    fn add(&mut self, key: SignalKey, target: Target, once: bool) -> u64 {
        self.next += 1;
        let id = self.next;
        self.by_key.entry(key.clone()).or_default().push(id);
        let names = self.by_entity.entry(key.entity).or_default();
        if !names.contains(&key.name) {
            names.push(key.name.clone());
        }
        self.conns.insert(id, Conn { key, target, once });
        id
    }

    fn remove(&mut self, id: u64) -> Option<Conn> {
        let c = self.conns.remove(&id)?;
        if let Some(ids) = self.by_key.get_mut(&c.key) {
            ids.retain(|x| *x != id);
            if ids.is_empty() {
                self.by_key.remove(&c.key);
                if let Some(names) = self.by_entity.get_mut(&c.key.entity) {
                    names.retain(|n| *n != c.key.name);
                    if names.is_empty() {
                        self.by_entity.remove(&c.key.entity);
                    }
                }
            }
        }
        Some(c)
    }

    /// True if anything listens to `name` on `entity`.
    pub fn listens(&self, entity: Entity, name: &str) -> bool {
        self.by_entity
            .get(&entity)
            .is_some_and(|names| names.iter().any(|n| &**n == name))
    }

    /// The signal names with listeners on `entity`.
    pub fn names_on(&self, entity: Entity) -> Vec<Rc<str>> {
        self.by_entity.get(&entity).cloned().unwrap_or_default()
    }

    /// The entities with listeners on a signal whose name starts with `prefix`.
    pub fn entities_listening(&self, prefix: &str) -> Vec<Entity> {
        let mut out: Vec<Entity> = self
            .by_entity
            .iter()
            .filter(|(_, names)| names.iter().any(|n| n.starts_with(prefix)))
            .map(|(e, _)| *e)
            .collect();
        out.sort();
        out
    }

    /// Connections (callbacks and waiting threads).
    pub fn len(&self) -> usize {
        self.conns.len()
    }

    /// True when nothing is connected.
    pub fn is_empty(&self) -> bool {
        self.conns.is_empty()
    }
}

fn hub<R>(lua: &Lua, f: impl FnOnce(&mut SignalHub) -> R) -> Result<R> {
    let mut h = lua
        .app_data_mut::<SignalHub>()
        .ok_or_else(|| mlua::Error::runtime("this VM has no signal hub"))?;
    Ok(f(&mut h))
}

/// Connects `f` to `key`; returns the connection id.
pub fn connect(lua: &Lua, key: SignalKey, f: Function, once: bool) -> Result<u64> {
    // Changes made before the connection fire to the connections that existed
    // when they were made, never to this one.
    events::dispatch(lua)?;
    let reg = lua.create_registry_value(f)?;
    let watch = key.clone();
    let id = hub(lua, |h| h.add(key, Target::Func(reg), once))?;
    events::watch_connected(lua, &watch)?;
    Ok(id)
}

/// Disconnects connection `id` (nothing happens when it is gone already).
pub fn disconnect(lua: &Lua, id: u64) -> Result<()> {
    if let Some(c) = hub(lua, |h| h.remove(id))? {
        drop_target(lua, c.target)?;
    }
    Ok(())
}

fn drop_target(lua: &Lua, t: Target) -> Result<()> {
    match t {
        Target::Func(k) => lua.remove_registry_value(k),
        Target::Wait(token) => sched::cancel_park(lua, token),
    }
}

/// True while connection `id` is connected.
pub fn is_connected(lua: &Lua, id: u64) -> bool {
    lua.app_data_ref::<SignalHub>()
        .is_some_and(|h| h.conns.contains_key(&id))
}

/// True if anything listens to `name` on `entity` in this context.
pub fn listens(lua: &Lua, entity: Entity, name: &str) -> bool {
    lua.app_data_ref::<SignalHub>()
        .is_some_and(|h| h.listens(entity, name))
}

/// Disconnects every connection on the signals of `entities` (a destroyed
/// subtree). Threads waiting on them are dropped.
pub fn disconnect_entities(lua: &Lua, entities: &[Entity]) -> Result<()> {
    let removed = hub(lua, |h| {
        let mut out = Vec::new();
        for e in entities {
            for name in h.names_on(*e) {
                let key = SignalKey { entity: *e, name };
                for id in h.by_key.get(&key).cloned().unwrap_or_default() {
                    out.extend(h.remove(id));
                }
            }
        }
        out
    })?;
    for c in removed {
        drop_target(lua, c.target)?;
    }
    Ok(())
}

/// How [`fire_with`] queues deferred handlers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guard {
    /// A handler disconnected before its turn does not run (the normal case).
    Connected,
    /// Every handler queued runs (`Destroying`, whose connections the
    /// destruction itself removes).
    None,
}

/// Fires `name` on `entity` with `args`, in the context's signal behaviour.
pub fn fire(lua: &Lua, entity: Entity, name: &str, args: MultiValue) -> Result<()> {
    fire_with(lua, &SignalKey::new(entity, name), args, Guard::Connected)
}

/// Fires `key` with `args`: every handler connected now runs once (in
/// connection order), and every thread waiting on it resumes with `args`.
pub fn fire_with(lua: &Lua, key: &SignalKey, args: MultiValue, guard: Guard) -> Result<()> {
    let behavior = state(lua)?.behavior;
    enum Act {
        Call(u64, Function, bool),
        Wake(YieldToken),
    }
    let mut free = Vec::new();
    let acts: Vec<Act> = {
        let mut h = lua
            .app_data_mut::<SignalHub>()
            .ok_or_else(|| mlua::Error::runtime("this VM has no signal hub"))?;
        let Some(ids) = h.by_key.get(key).cloned() else {
            return Ok(());
        };
        let mut acts = Vec::with_capacity(ids.len());
        for id in ids {
            let Some(c) = h.conns.get(&id) else { continue };
            match &c.target {
                Target::Wait(t) => {
                    let t = *t;
                    h.remove(id);
                    acts.push(Act::Wake(t));
                }
                Target::Func(reg) => {
                    let f: Function = lua.registry_value(reg)?;
                    let once = c.once;
                    if once
                        && let Some(Conn {
                            target: Target::Func(k),
                            ..
                        }) = h.remove(id)
                    {
                        free.push(k);
                    }
                    acts.push(Act::Call(id, f, once));
                }
            }
        }
        acts
    };
    for k in free {
        lua.remove_registry_value(k)?;
    }
    let label = format!("{} handler", key.name);
    for act in acts {
        match act {
            Act::Wake(t) => {
                sched::wake(lua, t, args.clone(), behavior.wake_mode())?;
            }
            Act::Call(id, f, once) => match behavior {
                SignalBehavior::Immediate => {
                    if once || guard == Guard::None || is_connected(lua, id) {
                        sched::spawn(lua, f, args.clone(), &label)?;
                    }
                }
                SignalBehavior::Deferred => {
                    let cond: Option<sched::DeferCondition> = if once || guard == Guard::None {
                        None
                    } else {
                        Some(Rc::new(move |lua: &Lua| is_connected(lua, id)))
                    };
                    sched::defer(lua, f, args.clone(), &label, cond)?;
                }
            },
        }
    }
    Ok(())
}

/// The Luau value of signal `name` on `entity` (an `RBXScriptSignal`).
pub fn push(lua: &Lua, entity: Entity, name: &str) -> Result<LuaValue> {
    Ok(LuaValue::UserData(lua.create_userdata(SignalRef(
        SignalKey::new(entity, name),
    ))?))
}

/// A signal value in Luau (`RBXScriptSignal`).
#[derive(Clone, Debug)]
pub struct SignalRef(pub SignalKey);

/// A connection value in Luau (`RBXScriptConnection`).
#[derive(Clone, Copy, Debug)]
pub struct ConnectionRef(pub u64);

fn handler(v: LuaValue, what: &str) -> Result<Function> {
    match v {
        LuaValue::Function(f) => Ok(f),
        other => Err(lua_error(
            re_script::codes::BAD_ARGUMENT,
            format!("{what} expects a function, got {}", other.type_name()),
        )),
    }
}

fn connection(
    lua: &Lua,
    this: &SignalRef,
    f: LuaValue,
    once: bool,
    what: &str,
) -> Result<AnyUserData> {
    let f = handler(f, what)?;
    let id = connect(lua, this.0.clone(), f, once)?;
    lua.create_userdata(ConnectionRef(id))
}

/// `Signal:Wait()`: parks the caller until the next firing.
fn wait(lua: &Lua, this: &SignalRef) -> Result<MultiValue> {
    let label = format!("{}:Wait()", this.0.name);
    events::dispatch(lua)?;
    let (values, token) = sched::park(lua, &label, None)?;
    hub(lua, |h| h.add(this.0.clone(), Target::Wait(token), true))?;
    events::watch_connected(lua, &this.0)?;
    Ok(values)
}

impl UserData for SignalRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "RBXScriptSignal");
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Connect", |lua, this, f: LuaValue| {
            connection(lua, this, f, false, "Connect")
        });
        // No parallel Luau in M2: a parallel connection runs like any other.
        methods.add_method("ConnectParallel", |lua, this, f: LuaValue| {
            connection(lua, this, f, false, "ConnectParallel")
        });
        methods.add_method("Once", |lua, this, f: LuaValue| {
            connection(lua, this, f, true, "Once")
        });
        methods.add_method("connect", |lua, this, f: LuaValue| {
            connection(lua, this, f, false, "connect")
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("Signal {}", this.0.name))
        });
        methods.add_meta_method(MetaMethod::Eq, |_, this, other: AnyUserData| {
            Ok(other.borrow::<SignalRef>().is_ok_and(|o| o.0 == this.0))
        });
        // `Wait` and `wait` are added by `install` as yielding functions.
        methods.add_meta_function(
            MetaMethod::Index,
            |lua, (_ud, key): (AnyUserData, LuaValue)| -> Result<LuaValue> {
                let key = key.to_string().unwrap_or_else(|_| "?".into());
                if key == "Wait" || key == "wait" {
                    let t: mlua::Table = lua.named_registry_value(WAIT_KEY)?;
                    return t.raw_get(key);
                }
                Err(lua_error(
                    re_script::codes::UNSUPPORTED_MEMBER,
                    format!("unsupported API member: RBXScriptSignal.{key}"),
                ))
            },
        );
    }
}

/// Registry key of the table holding the yielding `Wait` functions.
const WAIT_KEY: &str = "re_compat.signal_wait";

impl UserData for ConnectionRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "RBXScriptConnection");
        fields.add_field_method_get("Connected", |lua, this| Ok(is_connected(lua, this.0)));
        fields.add_field_method_get("connected", |lua, this| Ok(is_connected(lua, this.0)));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Disconnect", |lua, this, ()| disconnect(lua, this.0));
        methods.add_method("disconnect", |lua, this, ()| disconnect(lua, this.0));
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("Connection"));
        methods.add_meta_function(
            MetaMethod::Index,
            |_, (_ud, key): (AnyUserData, LuaValue)| -> Result<LuaValue> {
                let key = key.to_string().unwrap_or_else(|_| "?".into());
                Err(lua_error(
                    re_script::codes::UNSUPPORTED_MEMBER,
                    format!("unsupported API member: RBXScriptConnection.{key}"),
                ))
            },
        );
    }
}

/// Installs the hub and the yielding `Wait` functions into a new VM.
pub(crate) fn install(lua: &Lua) -> Result<()> {
    lua.set_app_data(SignalHub::default());
    let raw = lua.create_function(|lua, (ud, _rest): (AnyUserData, MultiValue)| {
        let this = ud.borrow::<SignalRef>()?.clone();
        wait(lua, &this)
    })?;
    let f = sched::yielding(lua, "Signal:Wait", raw)?;
    let t = lua.create_table()?;
    t.raw_set("Wait", &f)?;
    t.raw_set("wait", &f)?;
    lua.set_named_registry_value(WAIT_KEY, t)?;
    Ok(())
}
