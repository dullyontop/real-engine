//! Conversions between Luau values and component field [`Value`]s (ADR-0032).
//!
//! Luau numbers become `Value::Float` (or `Value::Int` when mlua reports an
//! integer); the registry then coerces them to the field type, so `0.1` written to
//! an `f32` field is stored and read back as `0.1f32`.
//!
//! Containers (ADR-0032 amendment): a list is a Luau array, a map is a
//! string-keyed table, and a record is a frozen table of its fields by name
//! whose metatable names the record (`__record`), so a record read from a field
//! can be written back, also inside a map. An empty table is an empty list or
//! map, whichever the field needs.

use std::collections::BTreeMap;

use mlua::{Lua, Result, Table, Value as LuaValue};
use re_reflect::{FieldType, RecordId, Value};
use re_scene::Entity;

use super::entity::{EntityRef, handle};
use super::math::{LColor, LQuat, V3};

/// A Luau value read without touching the world, so that handles can be
/// converted before the world is borrowed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum RawValue {
    Nil,
    Bool(bool),
    Int(i64),
    Num(f64),
    Vec3(glam::Vec3),
    Str(String),
    Quat(glam::Quat),
    Color(re_reflect::Color),
    Entity(Entity),
    /// An array table (or an empty table).
    List(Vec<RawValue>),
    /// A string-keyed table, sorted by key.
    Map(Vec<(String, RawValue)>),
    /// A record table, by the name its metatable gives, with its fields.
    Record(String, Vec<(String, RawValue)>),
}

/// How deep tables may nest when read (containers nest at most a few levels;
/// this also stops cyclic tables).
const MAX_TABLE_DEPTH: usize = 8;

impl RawValue {
    /// Reads a Luau value; functions and threads are refused, and tables must
    /// be arrays, string-keyed or records.
    pub(crate) fn from_lua(v: LuaValue) -> Result<RawValue> {
        Self::read(v, 0)
    }

    fn read(v: LuaValue, depth: usize) -> Result<RawValue> {
        Ok(match v {
            LuaValue::Nil => RawValue::Nil,
            LuaValue::Boolean(b) => RawValue::Bool(b),
            LuaValue::Integer(i) => RawValue::Int(i),
            LuaValue::Number(n) => RawValue::Num(n),
            LuaValue::Vector(v) => RawValue::Vec3(glam::Vec3::new(v.x(), v.y(), v.z())),
            LuaValue::String(s) => RawValue::Str(s.to_str()?.to_string()),
            LuaValue::UserData(ud) => {
                if let Ok(e) = ud.borrow::<EntityRef>() {
                    RawValue::Entity(e.0)
                } else if let Ok(q) = ud.borrow::<LQuat>() {
                    RawValue::Quat(q.0)
                } else if let Ok(c) = ud.borrow::<LColor>() {
                    RawValue::Color(c.0)
                } else {
                    return Err(super::error(
                        crate::codes::BAD_ARGUMENT,
                        "this userdata cannot be stored in a component field",
                    ));
                }
            }
            LuaValue::Table(t) => Self::read_table(t, depth)?,
            other => {
                return Err(super::error(
                    crate::codes::BAD_ARGUMENT,
                    format!(
                        "a {} cannot be stored in a component field",
                        other.type_name()
                    ),
                ));
            }
        })
    }

    fn read_table(t: Table, depth: usize) -> Result<RawValue> {
        let bad = |m: &str| super::error(crate::codes::BAD_ARGUMENT, m.to_string());
        if depth >= MAX_TABLE_DEPTH {
            return Err(bad(
                "tables nest too deeply to be stored in a component field",
            ));
        }
        let record: Option<String> = match t.metatable() {
            Some(mt) => mt.raw_get("__record")?,
            None => None,
        };
        let mut ints = Vec::new();
        let mut strings = Vec::new();
        for pair in t.pairs::<LuaValue, LuaValue>() {
            let (k, v) = pair?;
            let v = Self::read(v, depth + 1)?;
            match k {
                LuaValue::String(s) => strings.push((s.to_str()?.to_string(), v)),
                LuaValue::Integer(i) => ints.push((i, v)),
                LuaValue::Number(n) if n.fract() == 0.0 && n >= 1.0 && n <= 2f64.powi(53) => {
                    ints.push((n as i64, v))
                }
                _ => {
                    return Err(bad(
                        "a table stored in a component field needs string keys or array indices",
                    ));
                }
            }
        }
        if let Some(name) = record {
            if !ints.is_empty() {
                return Err(bad("a record table has only named fields"));
            }
            strings.sort_by(|a, b| a.0.cmp(&b.0));
            return Ok(RawValue::Record(name, strings));
        }
        match (ints.is_empty(), strings.is_empty()) {
            (_, true) => {
                ints.sort_by_key(|(i, _)| *i);
                if ints
                    .iter()
                    .enumerate()
                    .any(|(n, (i, _))| *i != n as i64 + 1)
                {
                    return Err(bad(
                        "an array stored in a component field must have no holes",
                    ));
                }
                Ok(RawValue::List(ints.into_iter().map(|(_, v)| v).collect()))
            }
            (true, false) => {
                strings.sort_by(|a, b| a.0.cmp(&b.0));
                Ok(RawValue::Map(strings))
            }
            (false, false) => Err(bad(
                "a table stored in a component field cannot mix array indices and string keys",
            )),
        }
    }

    fn kind(&self) -> String {
        match self {
            RawValue::Nil => "nil".into(),
            RawValue::Bool(_) => "boolean".into(),
            RawValue::Int(_) | RawValue::Num(_) => "number".into(),
            RawValue::Vec3(_) => "vector".into(),
            RawValue::Str(_) => "string".into(),
            RawValue::Quat(_) => "quat".into(),
            RawValue::Color(_) => "color".into(),
            RawValue::Entity(_) => "entity".into(),
            RawValue::List(_) => "array".into(),
            RawValue::Map(_) => "table".into(),
            RawValue::Record(name, _) => format!("record {name}"),
        }
    }
}

/// A map value (or a list item inside a map), whose type comes from the value itself.
fn dynamic_from_lua(raw: RawValue) -> std::result::Result<Value, String> {
    Ok(match raw {
        RawValue::Nil => return Err("nil cannot be stored in a map".into()),
        RawValue::Record(name, fields) => {
            let id = RecordId::by_name(&name).ok_or(format!("no record type named {name:?}"))?;
            record_from_fields(id, fields)?
        }
        RawValue::List(items) => Value::List(
            items
                .into_iter()
                .map(dynamic_from_lua)
                .collect::<std::result::Result<_, _>>()?,
        ),
        RawValue::Map(pairs) => Value::Map(
            pairs
                .into_iter()
                .map(|(k, v)| Ok((k, dynamic_from_lua(v)?)))
                .collect::<std::result::Result<_, String>>()?,
        ),
        RawValue::Bool(b) => Value::Bool(b),
        RawValue::Int(i) => Value::Int(i),
        RawValue::Num(n) => Value::Float(n),
        RawValue::Vec3(v) => Value::Vec3(v),
        RawValue::Str(s) => Value::Str(s),
        RawValue::Quat(q) => Value::Quat(q),
        RawValue::Color(c) => Value::Color(c),
        // Refused by the map's coercion, with its own message.
        RawValue::Entity(e) => Value::Entity(Some(e)),
    })
}

fn record_from_fields(
    id: RecordId,
    fields: Vec<(String, RawValue)>,
) -> std::result::Result<Value, String> {
    let schema = id.schema();
    if let Some((unknown, _)) = fields.iter().find(|(k, _)| schema.field_index(k).is_none()) {
        return Err(format!("record {} has no field {unknown:?}", schema.name));
    }
    let mut values = Vec::with_capacity(schema.fields.len());
    for f in &schema.fields {
        let raw = fields
            .iter()
            .find(|(k, _)| *k == f.name)
            .map(|(_, v)| v.clone())
            .ok_or(format!("record {} needs field {:?}", schema.name, f.name))?;
        values.push(field_from_lua(raw, &f.ty).map_err(|m| format!(".{}: {m}", f.name))?);
    }
    Ok(Value::Record(id, values))
}

/// Builds a field value of type `ty` from a raw Luau value. The registry still
/// coerces it (ranges, f32 rounding, enum variants). Errors are plain messages.
pub(crate) fn field_from_lua(raw: RawValue, ty: &FieldType) -> std::result::Result<Value, String> {
    Ok(match (raw, ty) {
        (RawValue::List(items), FieldType::List(item)) => Value::List(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| field_from_lua(v, item).map_err(|m| format!("[{}]: {m}", i + 1)))
                .collect::<std::result::Result<_, _>>()?,
        ),
        (RawValue::List(items), FieldType::Map) if items.is_empty() => Value::Map(BTreeMap::new()),
        (RawValue::Map(pairs), FieldType::Map) => Value::Map(
            pairs
                .into_iter()
                .map(|(k, v)| {
                    let v = dynamic_from_lua(v).map_err(|m| format!("[{k:?}]: {m}"))?;
                    Ok((k, v))
                })
                .collect::<std::result::Result<_, String>>()?,
        ),
        (RawValue::Map(fields), FieldType::Record(id)) => record_from_fields(*id, fields)?,
        (RawValue::Record(name, fields), FieldType::Record(id)) if name == id.name() => {
            record_from_fields(*id, fields)?
        }
        (RawValue::List(items), FieldType::Record(id))
            if items.len() == id.schema().fields.len() =>
        {
            let fields = id
                .schema()
                .fields
                .iter()
                .map(|f| f.name.clone())
                .zip(items)
                .collect();
            record_from_fields(*id, fields)?
        }
        (raw @ (RawValue::List(_) | RawValue::Map(_) | RawValue::Record(..)), _)
        | (raw, FieldType::List(_) | FieldType::Map | FieldType::Record(_)) => {
            return Err(format!("expected {}, got {}", ty.describe(), raw.kind()));
        }
        (raw, ty) => scalar_from_lua(raw, ty)?,
    })
}

fn scalar_from_lua(raw: RawValue, ty: &FieldType) -> std::result::Result<Value, String> {
    Ok(match raw {
        RawValue::Nil if *ty == FieldType::Entity => Value::Entity(None),
        RawValue::Nil => return Err(format!("expected {}, got nil", ty.name())),
        RawValue::Bool(b) => Value::Bool(b),
        RawValue::Int(i) => Value::Int(i),
        RawValue::Num(n) => Value::Float(n),
        RawValue::Vec3(v) => Value::Vec3(v),
        RawValue::Str(s) if matches!(ty, FieldType::Enum(_)) => Value::Enum(s),
        RawValue::Str(s) => Value::Str(s),
        RawValue::Quat(q) => Value::Quat(q),
        RawValue::Color(c) => Value::Color(c),
        RawValue::Entity(e) => Value::Entity(Some(e)),
        RawValue::List(_) | RawValue::Map(_) | RawValue::Record(..) => {
            return Err(format!("expected {}, got {}", ty.describe(), raw.kind()));
        }
    })
}

/// Converts a field value to Luau. An entity reference becomes a handle even when
/// the entity no longer exists (the handle then reports `alive == false`), which
/// keeps reads cheap; scripts check `alive` when it matters.
pub(crate) fn field_to_lua(lua: &Lua, v: Value) -> Result<LuaValue> {
    Ok(match v {
        Value::Bool(b) => LuaValue::Boolean(b),
        Value::Int(i) => LuaValue::Number(i as f64),
        Value::Float(f) => LuaValue::Number(f),
        Value::Vec3(v) => mlua::IntoLua::into_lua(V3(v), lua)?,
        Value::Quat(q) => LuaValue::UserData(lua.create_userdata(LQuat(q))?),
        Value::Color(c) => LuaValue::UserData(lua.create_userdata(LColor(c))?),
        Value::Str(s) | Value::Enum(s) => LuaValue::String(lua.create_string(&s)?),
        Value::Entity(None) => LuaValue::Nil,
        Value::Entity(Some(e)) => LuaValue::UserData(handle(lua, e)?),
        Value::List(items) => {
            let t = lua.create_table_with_capacity(items.len(), 0)?;
            for item in items {
                t.raw_push(field_to_lua(lua, item)?)?;
            }
            LuaValue::Table(t)
        }
        Value::Map(map) => {
            let t = lua.create_table_with_capacity(0, map.len())?;
            for (k, v) in map {
                t.raw_set(k, field_to_lua(lua, v)?)?;
            }
            LuaValue::Table(t)
        }
        Value::Record(id, values) => {
            let schema = id.schema();
            let t = lua.create_table_with_capacity(0, values.len())?;
            for (f, v) in schema.fields.iter().zip(values) {
                t.raw_set(f.name.as_str(), field_to_lua(lua, v)?)?;
            }
            let mt = lua.create_table()?;
            mt.raw_set("__record", schema.name.as_str())?;
            mt.raw_set("__metatable", "locked")?;
            mt.set_readonly(true);
            t.set_metatable(Some(mt))?;
            t.set_readonly(true);
            LuaValue::Table(t)
        }
    })
}

/// Reads a `{ field = value }` table, sorted by field name (so that errors and
/// insert order do not depend on Luau's hash order).
/// `component` names the component in errors.
pub(crate) fn fields_from_table(component: &str, t: Table) -> Result<Vec<(String, RawValue)>> {
    let mut out = Vec::new();
    for pair in t.pairs::<String, LuaValue>() {
        let (k, v) = pair?;
        let type_name = v.type_name();
        let raw = RawValue::from_lua(v).map_err(|_| {
            super::error(
                crate::codes::BAD_ARGUMENT,
                format!("{component}.{k}: a {type_name} cannot be stored in a component field"),
            )
        })?;
        out.push((k, raw));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}
