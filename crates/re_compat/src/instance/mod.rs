//! Instance handles: [`InstanceRef`], the Luau userdata scripts hold for an
//! instance (`Instance` in Luau).
//!
//! * One handle per entity per VM, interned in a weak-valued table, so `==`,
//!   `rawequal` and table keys all mean "the same instance".
//! * Indexing resolves, in order, a member of the instance's class (the class
//!   layer: properties through [`ClassRegistry::get_property`], methods as
//!   cached function values, events as signals), then a child with that name,
//!   and otherwise raises `RE9001`. A member that is declared but not
//!   implemented raises `RE9001` naming it.
//! * Methods are plain functions taking the instance first, so
//!   `inst.FindFirstChild(inst, "x")` and `pcall(game.GetService, game, n)`
//!   work; reading the same method twice gives the same function.
//! * A `stub-error` method with a registered implementation runs it when
//!   called (the offline services raise `RE9002` from the call, so `pcall`
//!   catches it); without one, reading it raises `RE9001`.
//! * After `Destroy()` a handle keeps its name and class (a tombstone) and,
//!   as in Roblox, the property values the instance had when it was
//!   despawned: they still read, `Parent` reads `nil` and is locked, writes
//!   are refused (`RE3007`) and queries find nothing.
//!
//! [`ClassRegistry::get_property`]: crate::ClassRegistry::get_property

pub mod attributes;
pub mod clone;
mod methods;
pub mod tree;

pub use methods::destroy;

/// Helpers for the method implementations of other areas.
pub mod methods_api {
    pub use super::methods::{args, ret};
}

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;

use mlua::{
    AnyUserData, Function, LightUserData, Lua, MetaMethod, MultiValue, Result, Table, UserData,
    UserDataFields, UserDataMethods, Value as LuaValue,
};
use re_reflect::Value;
use re_scene::{Entity, World};
use re_script::codes::{BAD_ARGUMENT, STALE_HANDLE, UNSUPPORTED_MEMBER};

use crate::class::{Accessor, ClassMarker, ClassRegistry, Member};
use crate::data::Status;
use crate::error::CompatError;
use crate::runtime::{Bindings, compat_error, lua_error, state, with_world};
use crate::signal;

/// Registry key of the weak-valued handle table.
const HANDLES: &str = "re_compat.instances";

/// The name and class an instance had when it was destroyed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tombstone {
    /// Its `Name`.
    pub name: String,
    /// Its class.
    pub class: String,
}

impl Tombstone {
    /// The tombstone of a live instance.
    pub fn of(w: &World, e: Entity) -> Tombstone {
        Tombstone {
            name: tree::name(w, e),
            class: tree::class(w, e).unwrap_or("Instance").to_string(),
        }
    }
}

/// The userdata scripts hold for an instance.
#[derive(Debug)]
pub struct InstanceRef {
    /// The entity.
    pub entity: Entity,
    tomb: RefCell<Option<Rc<Tombstone>>>,
    /// The property values the instance had when it was despawned.
    props: RefCell<Option<Rc<HashMap<String, Value>>>>,
}

/// The entity of an `Instance` userdata.
pub fn entity_of(ud: &AnyUserData) -> Option<Entity> {
    ud.borrow::<InstanceRef>().ok().map(|r| r.entity)
}

/// The handle of `entity` in this VM, made on first use.
pub fn handle(lua: &Lua, entity: Entity) -> Result<AnyUserData> {
    let cache: Table = lua.named_registry_value(HANDLES)?;
    let key = LightUserData(entity.to_bits() as usize as *mut c_void);
    if let LuaValue::UserData(ud) = cache.raw_get::<LuaValue>(key)? {
        return Ok(ud);
    }
    let ud = lua.create_userdata(InstanceRef {
        entity,
        tomb: RefCell::new(None),
        props: RefCell::new(None),
    })?;
    cache.raw_set(key, &ud)?;
    Ok(ud)
}

/// The Luau value of `entity` (its handle).
pub fn push(lua: &Lua, entity: Entity) -> Result<LuaValue> {
    Ok(LuaValue::UserData(handle(lua, entity)?))
}

/// The value of an instance reference a property holds: the instance's
/// handle, or, for an instance already despawned, the handle a script still
/// holds (with its tombstone and property values) and otherwise `nil` (there
/// is nothing left to read).
pub fn push_ref(lua: &Lua, entity: Entity) -> Result<LuaValue> {
    let alive = with_world(lua, |w| tree::alive(w, entity))?;
    if alive || has_handle(lua, entity)? {
        push(lua, entity)
    } else {
        Ok(LuaValue::Nil)
    }
}

/// The handle of an entity that may already be gone: when it is, the handle
/// gets `tomb` (so its name and class still read).
pub fn push_with_tomb(lua: &Lua, entity: Entity, tomb: &Tombstone) -> Result<LuaValue> {
    let ud = handle(lua, entity)?;
    {
        let r = ud.borrow::<InstanceRef>()?;
        if r.tomb.borrow().is_none() {
            *r.tomb.borrow_mut() = Some(Rc::new(tomb.clone()));
        }
    }
    Ok(LuaValue::UserData(ud))
}

/// True if this VM holds a handle of `entity`.
pub(crate) fn has_handle(lua: &Lua, entity: Entity) -> Result<bool> {
    let cache: Table = lua.named_registry_value(HANDLES)?;
    let key = LightUserData(entity.to_bits() as usize as *mut c_void);
    Ok(matches!(
        cache.raw_get::<LuaValue>(key)?,
        LuaValue::UserData(_)
    ))
}

/// The property values of live instance `e` (except `Parent` and the
/// callbacks), kept for its handle once it is despawned. A property that
/// cannot be read now is left out.
pub(crate) fn snapshot(classes: &ClassRegistry, w: &World, e: Entity) -> HashMap<String, Value> {
    let mut out = HashMap::new();
    let Some(class) = tree::class(w, e) else {
        return out;
    };
    for c in classes.ancestry(class) {
        for p in &c.properties {
            if p.ty == CALLBACK_TYPE || p.name == "Parent" || out.contains_key(&p.name) {
                continue;
            }
            if let Ok(v) = classes.get_property(w, e, &p.name) {
                out.insert(p.name.clone(), v);
            }
        }
    }
    out
}

/// Gives the live handle of `entity`, if one exists, its tombstone and the
/// property values it had.
pub(crate) fn bury(
    lua: &Lua,
    entity: Entity,
    tomb: Tombstone,
    props: Option<HashMap<String, Value>>,
) -> Result<()> {
    let cache: Table = lua.named_registry_value(HANDLES)?;
    let key = LightUserData(entity.to_bits() as usize as *mut c_void);
    if let LuaValue::UserData(ud) = cache.raw_get::<LuaValue>(key)? {
        let r = ud.borrow::<InstanceRef>()?;
        *r.tomb.borrow_mut() = Some(Rc::new(tomb));
        if let Some(p) = props {
            *r.props.borrow_mut() = Some(Rc::new(p));
        }
    }
    Ok(())
}

/// A Luau array of handles.
pub fn push_list(lua: &Lua, entities: &[Entity]) -> Result<LuaValue> {
    let t = lua.create_table_with_capacity(entities.len(), 0)?;
    for &e in entities {
        t.raw_push(handle(lua, e)?)?;
    }
    Ok(LuaValue::Table(t))
}

/// The entity of argument `v`, which must be an instance.
pub fn arg_instance(v: &LuaValue, what: &str) -> Result<Entity> {
    match v {
        LuaValue::UserData(ud) => entity_of(ud),
        _ => None,
    }
    .ok_or_else(|| {
        lua_error(
            BAD_ARGUMENT,
            format!("{what} expects an Instance, got {}", type_name(v)),
        )
    })
}

/// The Roblox-style name of a value's type, for messages.
pub(crate) fn type_name(v: &LuaValue) -> String {
    match v {
        LuaValue::UserData(ud) => ud
            .metatable()
            .ok()
            .and_then(|mt| mt.get::<String>("__type").ok())
            .unwrap_or_else(|| "userdata".into()),
        LuaValue::Vector(_) => "Vector3".into(),
        other => other.type_name().into(),
    }
}

/// How a member name of a class resolves (cached per VM).
#[derive(Clone)]
enum Resolved {
    Prop(Rc<str>),
    Method(Function),
    Event,
    /// Declared but not implemented: the `RE9001` message.
    Unsupported(Rc<str>),
    /// Not a member: maybe a child.
    None(Option<String>),
}

#[derive(Default)]
struct MemberCache {
    by_class: HashMap<String, HashMap<String, Resolved>>,
    methods: HashMap<(String, String), Function>,
}

/// How many names one class's member cache holds before it stops caching
/// misses.
const MAX_CACHED_PER_CLASS: usize = 4096;

fn resolve(lua: &Lua, class: &str, key: &str) -> Result<Resolved> {
    if let Some(r) = lua
        .app_data_ref::<MemberCache>()
        .and_then(|c| c.by_class.get(class).and_then(|m| m.get(key)).cloned())
    {
        return Ok(r);
    }
    let st = state(lua)?;
    let classes = &st.setup.classes;
    let r = match classes.member(class, key) {
        Ok(m) => match m.require_implemented_by_script() {
            // A stub with an implementation raises its own error when called.
            Err(_)
                if matches!(m.member, Member::Method(_))
                    && m.member.status() == Status::StubError
                    && st.setup.method(&m.class.name, key).is_some() =>
            {
                Resolved::Method(method_fn(lua, &m.class.name, key)?)
            }
            Err(e) => Resolved::Unsupported(Rc::from(e.to_string())),
            Ok(()) => match m.member {
                Member::Property(p) => Resolved::Prop(Rc::from(p.ty.as_str())),
                Member::Event(_) => Resolved::Event,
                Member::Method(_) => Resolved::Method(method_fn(lua, &m.class.name, key)?),
            },
        },
        Err(CompatError::UnknownMember { suggestion, .. }) => Resolved::None(suggestion),
        Err(_) => Resolved::None(None),
    };
    // Misses (child names, mostly) are cached up to a bound per class, since
    // scripts index children by arbitrary names.
    if let Some(mut c) = lua.app_data_mut::<MemberCache>() {
        let map = c.by_class.entry(class.to_string()).or_default();
        if !matches!(r, Resolved::None(_)) || map.len() < MAX_CACHED_PER_CLASS {
            map.insert(key.to_string(), r.clone());
        }
    }
    Ok(r)
}

/// The function value of method `name` declared on `class`, made once.
fn method_fn(lua: &Lua, class: &str, name: &str) -> Result<Function> {
    let k = (class.to_string(), name.to_string());
    if let Some(f) = lua
        .app_data_ref::<MemberCache>()
        .and_then(|c| c.methods.get(&k).cloned())
    {
        return Ok(f);
    }
    let st = state(lua)?;
    let imp = st.setup.method(class, name).cloned().ok_or_else(|| {
        lua_error(
            UNSUPPORTED_MEMBER,
            format!("{class}.{name} has no implementation"),
        )
    })?;
    let decl = class.to_string();
    let mname = name.to_string();
    let f = imp.f.clone();
    let raw = lua.create_function(move |lua, args: MultiValue| {
        let mut it = args.into_iter();
        let this = it.next().unwrap_or(LuaValue::Nil);
        let e = match &this {
            LuaValue::UserData(ud) => entity_of(ud),
            _ => None,
        }
        .ok_or_else(|| {
            lua_error(
                BAD_ARGUMENT,
                format!("Expected ':' not '.' calling member function {mname}"),
            )
        })?;
        let st = state(lua)?;
        let ok = with_world(lua, |w| {
            !tree::alive(w, e) || tree::is_a(w, &st.setup.classes, e, &decl)
        })?;
        if !ok {
            return Err(lua_error(
                BAD_ARGUMENT,
                format!("{mname} is a member of {decl}, which this instance is not"),
            ));
        }
        f(lua, e, it.collect())
    })?;
    let func = if imp.yields {
        re_script::sched::yielding(lua, &format!("{class}:{name}"), raw)?
    } else {
        raw
    };
    if let Some(mut c) = lua.app_data_mut::<MemberCache>() {
        c.methods.insert(k, func.clone());
    }
    Ok(func)
}

/// The class of a handle's instance: live, from its marker; destroyed, from
/// its tombstone.
fn class_of(lua: &Lua, this: &InstanceRef) -> Result<(bool, Option<String>)> {
    let live = with_world(lua, |w| tree::class(w, this.entity).map(str::to_string))?;
    Ok(match live {
        Some(c) => (true, Some(c)),
        None => (false, this.tomb.borrow().as_ref().map(|t| t.class.clone())),
    })
}

fn stale(this: &InstanceRef, what: &str) -> mlua::Error {
    let name = this
        .tomb
        .borrow()
        .as_ref()
        .map(|t| t.name.clone())
        .unwrap_or_default();
    lua_error(
        STALE_HANDLE,
        format!("cannot read {what} of {name:?}: the instance was destroyed"),
    )
}

fn not_member(
    lua: &Lua,
    e: Entity,
    class: &str,
    key: &str,
    suggestion: Option<String>,
) -> mlua::Error {
    let path = with_world(lua, |w| tree::full_name(w, e)).unwrap_or_default();
    let hint = suggestion
        .map(|s| format!(" (did you mean {s:?}?)"))
        .unwrap_or_default();
    crate::runtime::unsupported(
        lua,
        format!("{class}.{key}"),
        format!("{key} is not a valid member of {class} \"{path}\"{hint}"),
    )
}

fn index(lua: &Lua, this: &InstanceRef, key: &str) -> Result<LuaValue> {
    if state(lua)?.loading.get() {
        return Err(mlua::Error::runtime(
            "instances are not read while a chunk loads",
        ));
    }
    let (alive, class) = class_of(lua, this)?;
    let Some(class) = class else {
        return match key {
            "Parent" => Ok(LuaValue::Nil),
            _ => Err(stale(this, key)),
        };
    };
    match resolve(lua, &class, key)? {
        Resolved::Prop(ty) if &*ty == CALLBACK_TYPE => {
            if !alive {
                return Err(stale(this, key));
            }
            callback(lua, this.entity, key).map(|f| f.map_or(LuaValue::Nil, LuaValue::Function))
        }
        Resolved::Prop(ty) => {
            if !alive {
                let tomb = this
                    .tomb
                    .borrow()
                    .clone()
                    .expect("a dead handle with a class has a tombstone");
                return match key {
                    "Name" => Ok(LuaValue::String(lua.create_string(&tomb.name)?)),
                    "ClassName" | "className" => {
                        Ok(LuaValue::String(lua.create_string(&tomb.class)?))
                    }
                    "Parent" => Ok(LuaValue::Nil),
                    _ => {
                        let kept = this
                            .props
                            .borrow()
                            .as_ref()
                            .and_then(|p| p.get(key).cloned());
                        match kept {
                            Some(v) => state(lua)?.setup.codec.to_lua(lua, v, &ty),
                            None => Err(stale(this, key)),
                        }
                    }
                };
            }
            let st = state(lua)?;
            let v = with_world(lua, |w| st.setup.classes.get_property(w, this.entity, key))?
                .map_err(compat_error)?;
            st.setup.codec.to_lua(lua, v, &ty)
        }
        Resolved::Method(f) => Ok(LuaValue::Function(f)),
        Resolved::Event => signal::push(lua, this.entity, key),
        Resolved::Unsupported(msg) => Err(crate::runtime::unsupported(
            lua,
            format!("{class}.{key}"),
            msg,
        )),
        Resolved::None(suggestion) => {
            if !alive {
                return Err(stale(this, key));
            }
            let e = this.entity;
            if let Some(c) = with_world(lua, |w| tree::find_child(w, e, key, false))? {
                return push(lua, c);
            }
            if let Some(s) = crate::services::core::service_by_index(lua, e, &class, key)? {
                return Ok(s);
            }
            Err(not_member(lua, e, &class, key, suggestion))
        }
    }
}

fn newindex(lua: &Lua, this: &InstanceRef, key: &str, value: LuaValue) -> Result<()> {
    let (alive, class) = class_of(lua, this)?;
    if !alive {
        return Err(if key == "Parent" {
            lua_error(
                crate::codes::INVALID_PARENT,
                "The Parent property is locked: the instance was destroyed",
            )
        } else {
            stale(this, key)
        });
    }
    let class = class.expect("a live instance has a class");
    match resolve(lua, &class, key)? {
        Resolved::Prop(ty) if &*ty == CALLBACK_TYPE => set_callback(lua, this.entity, key, value),
        Resolved::Prop(ty) => {
            let st = state(lua)?;
            let v = st.setup.codec.from_lua(lua, value, &ty).map_err(|m| {
                lua_error(BAD_ARGUMENT, format!("bad value for {class}.{key}: {m}"))
            })?;
            with_world(lua, |w| {
                st.setup.classes.set_property(w, this.entity, key, v)
            })?
            .map_err(compat_error)?;
            if key == "Enabled" || key == "Disabled" {
                crate::runtime::scripts::enabled_written(lua, this.entity)?;
            }
            signal::events::after_write(lua)
        }
        Resolved::Method(_) | Resolved::Event => Err(compat_error(CompatError::NotAProperty {
            class: class.clone(),
            member: key.to_string(),
            kind: "method or event",
        })),
        Resolved::Unsupported(msg) => Err(crate::runtime::unsupported(
            lua,
            format!("{class}.{key}"),
            msg,
        )),
        Resolved::None(suggestion) => Err(not_member(lua, this.entity, &class, key, suggestion)),
    }
}

fn key_str(key: &LuaValue) -> Result<String> {
    match key {
        LuaValue::String(s) => Ok(s.to_str()?.to_string()),
        other => Err(lua_error(
            BAD_ARGUMENT,
            format!(
                "an instance is indexed with a string, not a {}",
                type_name(other)
            ),
        )),
    }
}

impl UserData for InstanceRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "Instance");
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Index, |lua, this, key: LuaValue| {
            index(lua, this, &key_str(&key)?)
        });
        methods.add_meta_method(
            MetaMethod::NewIndex,
            |lua, this, (key, value): (LuaValue, LuaValue)| {
                newindex(lua, this, &key_str(&key)?, value)
            },
        );
        methods.add_meta_method(MetaMethod::ToString, |lua, this, ()| {
            let live = with_world(lua, |w| {
                tree::alive(w, this.entity).then(|| tree::name(w, this.entity))
            })?;
            Ok(live.unwrap_or_else(|| {
                this.tomb
                    .borrow()
                    .as_ref()
                    .map(|t| t.name.clone())
                    .unwrap_or_default()
            }))
        });
        methods.add_meta_method(MetaMethod::Eq, |_, this, other: AnyUserData| {
            Ok(entity_of(&other) == Some(this.entity))
        });
    }
}

/// The declared type of a callback property (`RemoteFunction.OnServerInvoke`,
/// `BindableFunction.OnInvoke`): a function the engine calls, kept in the VM
/// rather than in the world. Such a property is declared with `builtin`
/// storage; its accessor is never used.
pub const CALLBACK_TYPE: &str = "function";

/// Registry key of the callback table: entity bits → { property → function }.
const CALLBACKS: &str = "re_compat.callbacks";

/// The function set on callback property `name` of `e`, if any.
pub fn callback(lua: &Lua, e: Entity, name: &str) -> Result<Option<Function>> {
    let all: Table = lua.named_registry_value(CALLBACKS)?;
    match all.raw_get::<Option<Table>>(e.to_bits())? {
        Some(t) => t.raw_get(name),
        None => Ok(None),
    }
}

fn set_callback(lua: &Lua, e: Entity, name: &str, value: LuaValue) -> Result<()> {
    let f = match value {
        LuaValue::Function(f) => Some(f),
        LuaValue::Nil => None,
        other => {
            return Err(lua_error(
                BAD_ARGUMENT,
                format!(
                    "{name} must be a function or nil, got a {}",
                    type_name(&other)
                ),
            ));
        }
    };
    let all: Table = lua.named_registry_value(CALLBACKS)?;
    let t = match all.raw_get::<Option<Table>>(e.to_bits())? {
        Some(t) => t,
        None => {
            let t = lua.create_table()?;
            all.raw_set(e.to_bits(), &t)?;
            t
        }
    };
    // The script that set it; destroying that script clears it.
    let owner = match &f {
        Some(_) => re_script::sched::current_owner(lua)?.map(|o| o.to_string()),
        None => None,
    };
    t.raw_set(format!("{OWNER_KEY}{name}"), owner)?;
    t.raw_set(name, f)
}

/// The prefix of the key that records which script set a callback.
const OWNER_KEY: &str = "\0owner ";

/// Clears the callbacks a stopped script set.
pub(crate) fn drop_owner_callbacks(lua: &Lua, owner: re_script::sched::Owner) -> Result<()> {
    let owner = owner.to_string();
    let all: Table = lua.named_registry_value(CALLBACKS)?;
    let mut gone = Vec::new();
    for pair in all.pairs::<LuaValue, Table>() {
        let (_, t) = pair?;
        for pair in t.pairs::<String, LuaValue>() {
            let (k, v) = pair?;
            if let (Some(name), LuaValue::String(o)) = (k.strip_prefix(OWNER_KEY), &v)
                && o.to_str().is_ok_and(|o| *o == *owner)
            {
                gone.push((t.clone(), k.clone(), name.to_string()));
            }
        }
    }
    for (t, k, name) in gone {
        t.raw_set(k, LuaValue::Nil)?;
        t.raw_set(name, LuaValue::Nil)?;
    }
    Ok(())
}

/// Drops the callbacks of a destroyed instance.
pub(crate) fn drop_callbacks(lua: &Lua, e: Entity) -> Result<()> {
    let all: Table = lua.named_registry_value(CALLBACKS)?;
    all.raw_set(e.to_bits(), LuaValue::Nil)
}

/// Installs the handle table and the member cache into a new VM.
pub(crate) fn install(lua: &Lua) -> Result<()> {
    let cache = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.raw_set("__mode", "v")?;
    cache.set_metatable(Some(mt))?;
    lua.set_named_registry_value(HANDLES, cache)?;
    lua.set_named_registry_value(CALLBACKS, lua.create_table()?)?;
    lua.set_app_data(MemberCache::default());
    Ok(())
}

/// Registers the `Instance` accessors (`Name`, `Parent`, `ClassName`), its
/// methods and the `Instance` global.
pub fn register(b: &mut Bindings) {
    for (prop, a) in accessors() {
        b.accessor("Instance", prop, a);
    }
    methods::register(b);
}

/// The accessors of `Instance`'s builtin properties, for registries built
/// outside [`Bindings`] (tools, tests).
pub fn accessors() -> Vec<(&'static str, Accessor)> {
    let name = Accessor {
        get: Arc::new(|w, e| Ok(Value::Str(tree::name(w, e)))),
        set: Some(Arc::new(|w, e, v| match v {
            Value::Str(s) => {
                tree::set_name(w, e, &s);
                Ok(())
            }
            other => Err(CompatError::Runtime {
                code: BAD_ARGUMENT,
                message: format!("Name must be a string, got {other:?}"),
            }),
        })),
    };
    let parent = Accessor {
        get: Arc::new(|w, e| Ok(Value::Entity(tree::parent(w, e)))),
        set: Some(Arc::new(|w, e, v| match v {
            Value::Entity(p) => tree::set_parent(w, e, p),
            other => Err(CompatError::Runtime {
                code: BAD_ARGUMENT,
                message: format!("Parent must be an Instance or nil, got {other:?}"),
            }),
        })),
    };
    let class_name = Accessor {
        get: Arc::new(|w, e| {
            Ok(Value::Str(
                w.ecs
                    .get::<ClassMarker>(e)
                    .map(|m| m.class.clone())
                    .unwrap_or_default(),
            ))
        }),
        set: None,
    };
    vec![
        ("Name", name),
        ("Parent", parent),
        ("ClassName", class_name.clone()),
        ("className", class_name),
    ]
}
