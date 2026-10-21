//! Values stored in data stores and memory stores: Luau values that have a
//! JSON form under the stores' rules (ADR-0048).

use mlua::{Lua, Value as LuaValue};
use serde_json::Value as J;

/// How deep tables may nest in a stored value.
const MAX_DEPTH: usize = 64;

/// Why a value cannot be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotStorable(pub String);

/// The JSON form of a value a script stores. Booleans, finite numbers,
/// strings, arrays (keys 1 to n) and tables with string keys only, nested;
/// nil only inside nothing (a nil value is refused at the top too).
pub fn to_stored(v: &LuaValue) -> Result<J, NotStorable> {
    if v.is_nil() {
        return Err(NotStorable("nil cannot be stored".into()));
    }
    let mut seen = Vec::new();
    convert(v, 0, &mut seen)
}

fn convert(v: &LuaValue, depth: usize, seen: &mut Vec<usize>) -> Result<J, NotStorable> {
    if depth > MAX_DEPTH {
        return Err(NotStorable(format!(
            "tables nest deeper than {MAX_DEPTH} levels"
        )));
    }
    Ok(match v {
        LuaValue::Nil => J::Null,
        LuaValue::Boolean(b) => J::Bool(*b),
        LuaValue::Integer(i) => J::from(re_script::lua_int(*i)),
        LuaValue::Number(n) => number(*n)?,
        LuaValue::String(s) => match s.to_str() {
            Ok(s) => J::String(s.to_string()),
            Err(_) => return Err(NotStorable("a string is not valid UTF-8".into())),
        },
        LuaValue::Table(t) => {
            let ptr = t.to_pointer() as usize;
            if seen.contains(&ptr) {
                return Err(NotStorable("a table contains itself".into()));
            }
            seen.push(ptr);
            let mut pairs = Vec::new();
            for pair in t.clone().pairs::<LuaValue, LuaValue>() {
                let pair = pair.map_err(|e| NotStorable(e.to_string()))?;
                pairs.push(pair);
            }
            let len = t.raw_len();
            let out = if pairs.is_empty() {
                J::Array(Vec::new())
            } else if len == pairs.len()
                && pairs.iter().all(|(k, _)| match k {
                    LuaValue::Integer(i) => (1..=len as i64).contains(&re_script::lua_int(*i)),
                    LuaValue::Number(n) => n.fract() == 0.0 && *n >= 1.0 && *n <= len as f64,
                    _ => false,
                })
            {
                // Every key is an index from 1 to the length and there are as
                // many keys as indices, so the array has no holes.
                let mut arr = Vec::with_capacity(len);
                for i in 1..=len {
                    let item: LuaValue = t.raw_get(i).map_err(|e| NotStorable(e.to_string()))?;
                    arr.push(convert(&item, depth + 1, seen)?);
                }
                J::Array(arr)
            } else if pairs.iter().all(|(k, _)| matches!(k, LuaValue::String(_))) {
                let mut keyed: Vec<(String, J)> = Vec::with_capacity(pairs.len());
                for (k, v) in &pairs {
                    let LuaValue::String(k) = k else {
                        unreachable!()
                    };
                    let Ok(k) = k.to_str() else {
                        return Err(NotStorable("a key is not valid UTF-8".into()));
                    };
                    keyed.push((k.to_string(), convert(v, depth + 1, seen)?));
                }
                // Key order is not part of a Luau table, so files list keys
                // sorted and runs repeat.
                keyed.sort_by(|a, b| a.0.cmp(&b.0));
                J::Object(keyed.into_iter().collect())
            } else {
                return Err(NotStorable(
                    "a table mixes array items and keys, or its array has holes".into(),
                ));
            };
            seen.pop();
            out
        }
        LuaValue::UserData(_) => {
            return Err(NotStorable(
                "instances, datatypes and enum items cannot be stored".into(),
            ));
        }
        other => {
            return Err(NotStorable(format!(
                "a {} cannot be stored",
                other.type_name()
            )));
        }
    })
}

fn number(n: f64) -> Result<J, NotStorable> {
    if !n.is_finite() {
        return Err(NotStorable(format!("{n} cannot be stored")));
    }
    if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
        return Ok(J::from(n as i64));
    }
    serde_json::Number::from_f64(n)
        .map(J::Number)
        .ok_or_else(|| NotStorable(format!("{n} cannot be stored")))
}

/// The length in characters of a stored value's JSON text: what the size
/// limits count.
pub fn stored_len(j: &J) -> usize {
    j.to_string().chars().count()
}

/// A stored value read back as a fresh Luau value.
pub fn to_lua(lua: &Lua, j: &J) -> mlua::Result<LuaValue> {
    crate::services::extra::from_json(lua, j.clone())
}

/// A stored value read as an integer, if it is one.
pub fn as_integer(j: &J) -> Option<i64> {
    match j {
        J::Number(n) => n.as_i64().or_else(|| {
            let f = n.as_f64()?;
            (f.fract() == 0.0 && f.abs() < 9.007_199_254_740_992e15).then_some(f as i64)
        }),
        _ => None,
    }
}
