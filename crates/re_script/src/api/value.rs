//! Conversions between Luau values and component field [`Value`]s (ADR-0032).
//!
//! Luau numbers become `Value::Float` (or `Value::Int` when mlua reports an
//! integer); the registry then coerces them to the field type, so `0.1` written to
//! an `f32` field is stored and read back as `0.1f32`.

use mlua::{Lua, Result, Table, Value as LuaValue};
use re_reflect::{FieldType, Value};
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
}

impl RawValue {
    /// Reads a Luau value; tables, functions and threads are refused.
    pub(crate) fn from_lua(v: LuaValue) -> Result<RawValue> {
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
}

/// Builds a field value of type `ty` from a raw Luau value. The registry still
/// coerces it (ranges, f32 rounding, enum variants). Errors are plain messages.
pub(crate) fn field_from_lua(raw: RawValue, ty: &FieldType) -> std::result::Result<Value, String> {
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
