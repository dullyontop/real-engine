//! Deep copies of Luau values for remotes and bindables ([`NetValue`]).
//!
//! A value crosses from one script context to another (or, for a bindable,
//! back into the same one) as a [`NetValue`]: a tree that owns its data and
//! names instances by stable id, so it can be rebuilt in another VM over
//! another world. The rules follow Roblox's:
//! * numbers are copied bit for bit (`NaN` and `-0` survive), strings and
//!   buffers byte for byte, `Vector3`, the datatypes and enum items by value;
//! * an instance arrives as the receiving world's instance with the same
//!   stable id, or `nil` when that world does not have it (a server-only
//!   instance sent to a client);
//! * a table whose key `1` is set is an array: entries `1..n` up to the first
//!   `nil` are copied and every other key is dropped. Any other table is a
//!   dictionary: string keys are kept and number keys become strings.
//!   Metatables are not copied;
//! * a function, a thread, a cyclic table, a key that is neither a string nor
//!   a number, or a userdata that is not an instance, datatype or enum item
//!   raises `RE9300` naming where it was found, instead of being dropped.

use std::collections::HashSet;

use mlua::{AnyUserData, Lua, MultiValue, Result, Table, Value as LuaValue};
use re_reflect::Value;
use re_scene::{Entity, World};

use crate::codes;
use crate::datatypes;
use crate::enums::EnumItem;
use crate::instance::{self, entity_of};
use crate::runtime::{lua_error, with_world};

/// How deep a copied table may nest.
const MAX_DEPTH: usize = 64;

/// An instance inside a copied value: the entity it was in the sending world
/// and its stable id.
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceKey {
    /// The entity in the sending world.
    pub entity: Entity,
    /// Its stable id, if it has one (every instance does).
    pub id: Option<String>,
}

/// A table key: an array index or a string.
#[derive(Clone, Debug, PartialEq)]
pub enum NetKey {
    /// `1..n`, in order.
    Index,
    /// A string key (number keys of dictionaries become strings).
    Str(Vec<u8>),
}

/// A deep copy of one Luau value.
#[derive(Clone, Debug, PartialEq)]
pub enum NetValue {
    /// `nil`.
    Nil,
    /// A boolean.
    Bool(bool),
    /// A number, bit for bit.
    Number(f64),
    /// A string's bytes.
    Str(Vec<u8>),
    /// A buffer's bytes.
    Buffer(Vec<u8>),
    /// A `Vector3`.
    Vector([f32; 3]),
    /// An instance.
    Instance(InstanceKey),
    /// An enum item.
    Enum(EnumItem),
    /// A datatype (`CFrame`, `Color3`, ...): its type name and stored form.
    Data(&'static str, Value),
    /// A table: array entries first, in order, then dictionary entries.
    Table(Vec<(NetKey, NetValue)>),
}

/// Copies `values` (a call's arguments) out of the VM. `what` names the call
/// in errors (`RemoteEvent:FireServer`).
pub fn encode_all(lua: &Lua, values: MultiValue, what: &str) -> Result<Vec<NetValue>> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .enumerate()
        .map(|(i, v)| encode(lua, v, what, &format!("argument {}", i + 1), &mut seen, 0))
        .collect()
}

fn refuse(what: &str, path: &str, why: impl std::fmt::Display) -> mlua::Error {
    lua_error(
        codes::REMOTE_VALUE,
        format!("{what}: {path} cannot be sent: {why}"),
    )
}

fn encode(
    lua: &Lua,
    v: LuaValue,
    what: &str,
    path: &str,
    seen: &mut HashSet<usize>,
    depth: usize,
) -> Result<NetValue> {
    Ok(match v {
        LuaValue::Nil => NetValue::Nil,
        LuaValue::Boolean(b) => NetValue::Bool(b),
        LuaValue::Integer(i) => NetValue::Number(i as f64),
        LuaValue::Number(n) => NetValue::Number(n),
        LuaValue::String(s) => NetValue::Str(s.as_bytes().to_vec()),
        LuaValue::Buffer(b) => NetValue::Buffer(b.to_vec()),
        LuaValue::Vector(v) => NetValue::Vector([v.x(), v.y(), v.z()]),
        LuaValue::UserData(ud) => encode_userdata(lua, &ud, what, path)?,
        LuaValue::Table(t) => encode_table(lua, t, what, path, seen, depth)?,
        LuaValue::Function(_) => return Err(refuse(what, path, "it is a function")),
        LuaValue::Thread(_) => return Err(refuse(what, path, "it is a thread")),
        other => return Err(refuse(what, path, format!("it is a {}", other.type_name()))),
    })
}

fn encode_userdata(lua: &Lua, ud: &AnyUserData, what: &str, path: &str) -> Result<NetValue> {
    if let Some(e) = entity_of(ud) {
        let id = with_world(lua, |w| w.id_of(e).map(str::to_string))?;
        return Ok(NetValue::Instance(InstanceKey { entity: e, id }));
    }
    if let Ok(item) = ud.borrow::<EnumItem>() {
        return Ok(NetValue::Enum(*item));
    }
    if let Some(ty) = datatypes::datatype_of(ud) {
        let v = datatypes::from_lua(lua, ty, LuaValue::UserData(ud.clone()))?;
        return Ok(NetValue::Data(ty, v));
    }
    let name = instance::type_name(&LuaValue::UserData(ud.clone()));
    Err(refuse(what, path, format!("a {name} is not a value")))
}

fn encode_table(
    lua: &Lua,
    t: Table,
    what: &str,
    path: &str,
    seen: &mut HashSet<usize>,
    depth: usize,
) -> Result<NetValue> {
    if depth >= MAX_DEPTH {
        return Err(refuse(
            what,
            path,
            format!("tables nest deeper than {MAX_DEPTH}"),
        ));
    }
    let ptr = t.to_pointer() as usize;
    if !seen.insert(ptr) {
        return Err(refuse(what, path, "the table contains itself"));
    }
    let mut out = Vec::new();
    let first: LuaValue = t.raw_get(1)?;
    if !first.is_nil() {
        // An array: 1..n up to the first nil; every other key is dropped.
        let mut i = 1i64;
        loop {
            let v: LuaValue = t.raw_get(i)?;
            if v.is_nil() {
                break;
            }
            let p = format!("{path}[{i}]");
            out.push((NetKey::Index, encode(lua, v, what, &p, seen, depth + 1)?));
            i += 1;
        }
    } else {
        let mut entries: Vec<(Vec<u8>, LuaValue)> = Vec::new();
        for pair in t.pairs::<LuaValue, LuaValue>() {
            let (k, v) = pair?;
            let key = match k {
                LuaValue::String(s) => s.as_bytes().to_vec(),
                LuaValue::Integer(i) => i.to_string().into_bytes(),
                LuaValue::Number(n) => number_key(n).into_bytes(),
                other => {
                    let name = instance::type_name(&other);
                    return Err(refuse(
                        what,
                        path,
                        format!("a table key must be a string or a number, not a {name}"),
                    ));
                }
            };
            entries.push((key, v));
        }
        // Sorted, so the copy does not depend on hash order.
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        for (k, v) in entries {
            let p = format!("{path}.{}", String::from_utf8_lossy(&k));
            let v = encode(lua, v, what, &p, seen, depth + 1)?;
            out.push((NetKey::Str(k), v));
        }
    }
    seen.remove(&ptr);
    Ok(NetValue::Table(out))
}

/// The string a number key of a dictionary becomes.
fn number_key(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// Where a copied value is rebuilt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// In the world it came from (a bindable): instances keep their entity.
    SameWorld,
    /// In the other world (a remote): instances are found by stable id.
    OtherWorld,
}

/// Rebuilds copied values in this VM.
pub fn decode_all(lua: &Lua, values: &[NetValue], target: Target) -> Result<MultiValue> {
    values.iter().map(|v| decode(lua, v, target)).collect()
}

/// Rebuilds one copied value in this VM.
pub fn decode(lua: &Lua, v: &NetValue, target: Target) -> Result<LuaValue> {
    Ok(match v {
        NetValue::Nil => LuaValue::Nil,
        NetValue::Bool(b) => LuaValue::Boolean(*b),
        NetValue::Number(n) => LuaValue::Number(*n),
        NetValue::Str(s) => LuaValue::String(lua.create_string(s)?),
        NetValue::Buffer(b) => LuaValue::Buffer(lua.create_buffer(b)?),
        NetValue::Vector([x, y, z]) => LuaValue::Vector(mlua::Vector::new(*x, *y, *z)),
        NetValue::Instance(k) => match resolve_instance(lua, k, target)? {
            Some(e) => instance::push(lua, e)?,
            None => LuaValue::Nil,
        },
        NetValue::Enum(item) => item.to_lua(lua)?,
        NetValue::Data(ty, v) => datatypes::to_lua(lua, ty, v.clone())?,
        NetValue::Table(entries) => {
            let t = lua.create_table()?;
            // An element that arrives as nil (an instance the receiver does
            // not have) keeps its index: later elements do not shift.
            let mut i = 0i64;
            for (k, v) in entries {
                let v = decode(lua, v, target)?;
                match k {
                    NetKey::Index => {
                        i += 1;
                        t.raw_set(i, v)?;
                    }
                    NetKey::Str(s) => t.raw_set(lua.create_string(s)?, v)?,
                }
            }
            LuaValue::Table(t)
        }
    })
}

fn resolve_instance(lua: &Lua, k: &InstanceKey, target: Target) -> Result<Option<Entity>> {
    match target {
        Target::SameWorld => Ok(Some(k.entity)),
        Target::OtherWorld => with_world(lua, |w| instance_by_id(w, k.id.as_deref())),
    }
}

/// The live instance with stable id `id` in `w`.
pub(crate) fn instance_by_id(w: &World, id: Option<&str>) -> Option<Entity> {
    let e = w.entity(id?).ok()?;
    (crate::instance::tree::is_instance(w, e) && !crate::instance::tree::is_destroyed(w, e))
        .then_some(e)
}
