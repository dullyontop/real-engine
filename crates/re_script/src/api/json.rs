//! Luau tables to and from JSON, for command arguments and results and for
//! ProcGraph node parameters.
//!
//! A table whose keys are exactly `1..n` is an array; any other table is an
//! object, written with its keys sorted so that output does not depend on Luau's
//! hash order. Integral numbers within ±2^53 become JSON integers. Vectors become
//! `[x, y, z]`. Functions, threads and userdata other than entities are refused.

use mlua::{Lua, Result, Table, Value};
use serde_json::{Map, Number, Value as Json};

use super::entity::EntityRef;
use crate::codes;

const MAX_DEPTH: usize = 64;

/// Converts a Luau value to JSON. Entity handles become their stable id (or an
/// error for an entity without one), which is what commands expect.
pub(crate) fn to_json(lua: &Lua, v: &Value) -> Result<Json> {
    convert(lua, v, 0)
}

fn bad(msg: impl std::fmt::Display) -> mlua::Error {
    super::error(codes::BAD_ARGUMENT, msg)
}

fn number(n: f64) -> Result<Json> {
    if n.fract() == 0.0 && n.abs() <= 9.007_199_254_740_992e15 {
        return Ok(Json::from(n as i64));
    }
    Number::from_f64(n)
        .map(Json::Number)
        .ok_or_else(|| bad(format!("{n} cannot be written as JSON (not finite)")))
}

fn convert(lua: &Lua, v: &Value, depth: usize) -> Result<Json> {
    if depth > MAX_DEPTH {
        return Err(bad(
            "table nesting is too deep (or a table contains itself)",
        ));
    }
    Ok(match v {
        Value::Nil => Json::Null,
        Value::Boolean(b) => Json::Bool(*b),
        Value::Integer(i) => Json::from(*i),
        Value::Number(n) => number(*n)?,
        Value::String(s) => Json::String(s.to_str()?.to_string()),
        Value::Vector(v) => Json::Array(vec![
            number(f64::from(v.x()))?,
            number(f64::from(v.y()))?,
            number(f64::from(v.z()))?,
        ]),
        Value::Table(t) => table(lua, t, depth)?,
        Value::UserData(ud) => {
            if let Ok(e) = ud.borrow::<EntityRef>() {
                let e = e.0;
                let id =
                    crate::host::with_host(lua, |h| Ok(h.world().id_of(e).map(str::to_string)))?;
                Json::String(id.ok_or_else(|| bad(format!("Entity({e}) has no stable id")))?)
            } else {
                return Err(bad("this userdata cannot be written as JSON"));
            }
        }
        other => {
            return Err(bad(format!(
                "a {} cannot be written as JSON",
                other.type_name()
            )));
        }
    })
}

fn table(lua: &Lua, t: &Table, depth: usize) -> Result<Json> {
    let mut entries: Vec<(Value, Value)> = Vec::new();
    for pair in t.pairs::<Value, Value>() {
        entries.push(pair?);
    }
    let n = t.raw_len();
    if n == entries.len() && n > 0 {
        let mut out = Vec::with_capacity(n);
        for i in 1..=n {
            out.push(convert(lua, &t.raw_get::<Value>(i)?, depth + 1)?);
        }
        return Ok(Json::Array(out));
    }
    let mut keyed: Vec<(String, Json)> = Vec::with_capacity(entries.len());
    for (k, v) in &entries {
        let key = match k {
            Value::String(s) => s.to_str()?.to_string(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) if n.fract() == 0.0 => (*n as i64).to_string(),
            other => {
                return Err(bad(format!(
                    "a {} cannot be a JSON object key",
                    other.type_name()
                )));
            }
        };
        keyed.push((key, convert(lua, v, depth + 1)?));
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(Json::Object(keyed.into_iter().collect::<Map<_, _>>()))
}

/// Converts JSON to Luau: objects and arrays become tables, `null` becomes `nil`.
pub(crate) fn from_json(lua: &Lua, j: &Json) -> Result<Value> {
    Ok(match j {
        Json::Null => Value::Nil,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => Value::Number(n.as_f64().unwrap_or(f64::NAN)),
        Json::String(s) => Value::String(lua.create_string(s)?),
        Json::Array(a) => {
            let t = lua.create_table_with_capacity(a.len(), 0)?;
            for v in a {
                t.raw_push(from_json(lua, v)?)?;
            }
            Value::Table(t)
        }
        Json::Object(o) => {
            let t = lua.create_table_with_capacity(0, o.len())?;
            for (k, v) in o {
                t.raw_set(k.as_str(), from_json(lua, v)?)?;
            }
            Value::Table(t)
        }
    })
}
