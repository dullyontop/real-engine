//! Roblox-style datatypes (docs/M2_PLAN.md, WP10): the Rust types, their Luau
//! forms and their stored (`Value`) forms.
//!
//! * `Vector3` is Luau's native `vector`, with a type metatable that adds the
//!   Roblox-style members ([`vector`]). Every other datatype is userdata whose
//!   metatable has a `__type`, so Luau's `typeof` names it (`"CFrame"`).
//! * Arithmetic is `f32`, as in Roblox: a `CFrame` is an `f32` position and an
//!   `f32` rotation matrix ([`CFrame`]).
//! * [`install`] puts the datatype tables, `Enum` and a `typeof` that names
//!   vectors `Vector3` into a VM's globals. The compat runtime calls it for
//!   each script context before the globals are frozen.
//! * [`to_lua`] and [`from_lua`] convert between Luau and the stored form of
//!   an instance property, by the property's data-file type (`"CFrame"`,
//!   `"Enum.Material"`); the stored forms are the records the libraries
//!   declare (`libraries/*.json`).
//!
//! Errors use the shared codes: an unknown member raises `RE9001` and a bad
//! argument `RE3006`, with Roblox-style wording.

mod cframe;
mod color;
mod convert;
mod misc;
mod random;
mod sequence;
mod udim;
mod vector;

pub use cframe::CFrame;
pub use color::{Color3, ColorSequence, ColorSequenceKeypoint};
pub use convert::{datatype_of, from_lua, to_lua, userdata_to_value};
pub use misc::{Font, OverlapParams, PhysicalProperties, RaycastParams, TweenInfo};
pub use random::Random;
pub use sequence::{NumberRange, NumberSequence, NumberSequenceKeypoint};
pub use udim::{Rect, UDim, UDim2};
pub use vector::Vector2;

use mlua::{AnyUserData, Lua, MetaMethod, Result, Table, UserDataMethods, Value};
use re_core::Code;
use re_script::codes;

/// The datatypes whose stored form [`to_lua`] turns into their Luau form.
const STORED_TYPES: &[&str] = &[
    "CFrame",
    "Color3",
    "ColorSequence",
    "ColorSequenceKeypoint",
    "Font",
    "NumberRange",
    "NumberSequence",
    "NumberSequenceKeypoint",
    "PhysicalProperties",
    "Rect",
    "UDim",
    "UDim2",
    "Vector2",
];

/// Registers the datatypes with the compat runtime: the globals of
/// [`install`] (through the `Vector3` global's constructor), the userdata
/// and stored-value conversions, and enum items for enum properties.
///
/// A compat VM installs them with its run seed (`[compat] seed`) first; any
/// other VM gets seed 0 for `Random.new()` without a seed.
///
/// Known limit: an attribute holding a `NumberSequence`, `ColorSequence` or
/// `EnumItem` reads back as a list or the item's name, because attribute
/// values carry no datatype name for the codec to pick a hook by.
pub fn register(b: &mut crate::runtime::Bindings) {
    b.global(
        "Vector3",
        std::sync::Arc::new(|lua: &Lua, _env: &crate::runtime::GlobalEnv| {
            let globals = lua.globals();
            // A VM made by `CompatVm` has them already, with its run seed.
            let made = globals.raw_get::<Value>("Vector3")?;
            if !made.is_nil() {
                return Ok(made);
            }
            install(lua, &globals, 0)?;
            globals.raw_get::<Value>("Vector3")
        }),
    );
    b.codec
        .add_from_userdata(std::sync::Arc::new(userdata_to_value));
    for &ty in STORED_TYPES {
        b.codec.set_to_lua(
            ty,
            std::sync::Arc::new(move |lua: &Lua, v: &re_reflect::Value| to_lua(lua, ty, v.clone())),
        );
    }
    b.codec
        .set_enum_to_lua(std::sync::Arc::new(|lua: &Lua, ty: &str, item: &str| {
            match crate::enums::EnumItem::lookup(ty, item) {
                Some(item) => item.to_lua(lua),
                // An item the enum data does not know stays readable as its name.
                None => Ok(Value::String(lua.create_string(item)?)),
            }
        }));
    b.codec
        .set_enum_from_lua(std::sync::Arc::new(|_lua: &Lua, ty: &str, v: &Value| {
            // Only items, names and numbers can be enum values; anything else
            // is left to the codec, which refuses it with its own message.
            let readable = match v {
                Value::UserData(ud) => ud.is::<crate::enums::EnumItem>(),
                Value::String(_) | Value::Integer(_) | Value::Number(_) => true,
                _ => false,
            };
            if !readable {
                return Ok(None);
            }
            // The codec adds the code and the property; the message is plain.
            match crate::enums::EnumItem::coerce(v, ty) {
                Ok(item) => Ok(Some(item.name().to_string())),
                Err(_) => Err(mlua::Error::external(format!(
                    "{} is not an Enum.{ty} item",
                    describe(v)
                ))),
            }
        }));
}

/// How an enum value a script wrote reads in a message.
fn describe(v: &Value) -> String {
    match v {
        Value::UserData(ud) => match ud.borrow::<crate::enums::EnumItem>() {
            Ok(item) => format!("Enum.{}.{}", item.enum_name(), item.name()),
            Err(_) => "this value".to_string(),
        },
        Value::String(s) => format!("{:?}", s.to_string_lossy()),
        Value::Integer(i) => i.to_string(),
        Value::Number(n) => n.to_string(),
        _ => "this value".to_string(),
    }
}

/// Installs every datatype table (`Vector3`, `CFrame`, ...), `Enum` and the
/// Roblox-style `typeof` into `globals`. `seed` seeds `Random.new()` called
/// without a seed: the n-th such generator of a VM gets the same state on
/// every run with the same seed.
///
/// Call it before the globals are frozen (`Lua::sandbox`).
pub fn install(lua: &Lua, globals: &Table, seed: u64) -> Result<()> {
    vector::install(lua, globals)?;
    cframe::install(lua, globals)?;
    color::install(lua, globals)?;
    udim::install(lua, globals)?;
    sequence::install(lua, globals)?;
    misc::install(lua, globals)?;
    random::install(lua, globals, seed)?;
    crate::enums::install(lua, globals)?;
    install_typeof(lua, globals)?;
    Ok(())
}

/// `typeof` as in Roblox: `Vector3` for a vector, the `__type` of userdata,
/// otherwise Luau's own answer.
fn install_typeof(lua: &Lua, globals: &Table) -> Result<()> {
    let builtin: mlua::Function = globals.get("typeof")?;
    let f = lua
        .load(
            r#"
local builtin = ...
return function(v)
    local t = builtin(v)
    if t == "vector" then
        return "Vector3"
    end
    return t
end
"#,
        )
        .set_name("=typeof")
        .call::<mlua::Function>(builtin)?;
    globals.raw_set("typeof", f)
}

/// A Luau error with an engine code: `[CODE] message`.
pub(crate) fn error(code: Code, message: impl std::fmt::Display) -> mlua::Error {
    mlua::Error::runtime(format!("[{code}] {message}"))
}

/// The `RE9001` error for a member that does not exist:
/// `Foo is not a valid member of CFrame`.
pub(crate) fn unsupported(owner: &str, member: &str) -> mlua::Error {
    error(
        codes::UNSUPPORTED_MEMBER,
        format!("{member} is not a valid member of {owner}"),
    )
}

/// The `RE3006` error for a bad argument.
pub(crate) fn bad_argument(
    n: usize,
    function: &str,
    message: impl std::fmt::Display,
) -> mlua::Error {
    error(
        codes::BAD_ARGUMENT,
        format!("invalid argument #{n} to '{function}' ({message})"),
    )
}

/// The Luau type name of `v` for error messages, with the Roblox-style names
/// of vectors and of this module's userdata.
pub(crate) fn type_name(v: &Value) -> String {
    match v {
        Value::Vector(_) => "Vector3".into(),
        Value::Integer(_) => "number".into(),
        Value::UserData(ud) => ud
            .metatable()
            .and_then(|mt| mt.get::<String>("__type"))
            .unwrap_or_else(|_| "userdata".into()),
        other => other.type_name().into(),
    }
}

/// Makes reading an unknown member of a datatype raise `RE9001`, and sets the
/// `__type` that `typeof` reports.
pub(crate) fn strict<T, M: UserDataMethods<T>>(methods: &mut M, type_name: &'static str) {
    methods.add_meta_function(
        MetaMethod::Index,
        move |_, (_ud, key): (AnyUserData, Value)| -> Result<Value> {
            let key = key.to_string().unwrap_or_else(|_| "?".into());
            Err(unsupported(type_name, &key))
        },
    );
    methods.add_meta_function(
        MetaMethod::NewIndex,
        move |_, (_ud, key, _v): (AnyUserData, Value, Value)| -> Result<()> {
            let key = key.to_string().unwrap_or_else(|_| "?".into());
            Err(error(
                codes::UNSUPPORTED_MEMBER,
                format!("{key} cannot be assigned to: {type_name} values are immutable"),
            ))
        },
    );
}

/// Formats an `f32` component the way `tostring` shows it: the shortest
/// decimal that reads back to the same `f32`, `inf`, `-inf` or `nan`.
pub(crate) fn fmt_f32(x: f32) -> String {
    if x.is_nan() {
        "nan".into()
    } else if x.is_infinite() {
        if x > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        let a = x.abs();
        if a == 0.0 || (1e-5..1e16).contains(&a) {
            format!("{x}")
        } else {
            format!("{x:e}").replace("e", "e+").replace("e+-", "e-")
        }
    }
}

/// Formats an `f64` the way Luau's `tostring` does for numbers.
pub(crate) fn fmt_f64(x: f64) -> String {
    if x.is_nan() {
        "nan".into()
    } else if x.is_infinite() {
        if x > 0.0 { "inf".into() } else { "-inf".into() }
    } else if x == x.trunc() && x.abs() < 1e15 {
        format!("{}", x as i64)
    } else {
        format!("{x}")
    }
}

/// Reads number argument `n` of `function`; `nil` is `default`.
pub(crate) fn num_arg(v: &Value, n: usize, function: &str, default: Option<f64>) -> Result<f64> {
    match v {
        Value::Number(x) => Ok(*x),
        Value::Integer(i) => Ok(*i as f64),
        Value::String(s) => s
            .to_str()
            .ok()
            .and_then(|s| parse_number(&s))
            .ok_or_else(|| bad_argument(n, function, "number expected, got string")),
        Value::Nil if default.is_some() => Ok(default.unwrap()),
        other => Err(bad_argument(
            n,
            function,
            format!("number expected, got {}", type_name(other)),
        )),
    }
}

/// Reads a numeric string the way Luau's `tonumber` does: decimal with an
/// optional exponent, or hexadecimal with `0x`; no `inf` or `nan` words.
fn parse_number(s: &str) -> Option<f64> {
    let t = s.trim();
    let (neg, body) = match t.strip_prefix('-') {
        Some(b) => (true, b),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let v = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()? as f64
    } else if !body.is_empty()
        && body
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        body.parse::<f64>().ok()?
    } else {
        return None;
    };
    Some(if neg { -v } else { v })
}

/// Reads a `Vector3` argument.
pub(crate) fn vec_arg(v: &Value, n: usize, function: &str) -> Result<[f32; 3]> {
    match v {
        Value::Vector(v) => Ok([v.x(), v.y(), v.z()]),
        other => Err(bad_argument(
            n,
            function,
            format!("Vector3 expected, got {}", type_name(other)),
        )),
    }
}

/// A `Vector3` as a Luau value.
pub(crate) fn vec_value(v: [f32; 3]) -> Value {
    Value::Vector(mlua::Vector::new(v[0], v[1], v[2]))
}

/// Reads userdata argument `n` of type `T` (named `name` in errors).
pub(crate) fn ud_arg<T: Clone + 'static>(
    v: &Value,
    n: usize,
    function: &str,
    name: &str,
) -> Result<T> {
    if let Value::UserData(ud) = v
        && let Ok(t) = ud.borrow::<T>()
    {
        return Ok(t.clone());
    }
    Err(bad_argument(
        n,
        function,
        format!("{name} expected, got {}", type_name(v)),
    ))
}

/// Builds a frozen library table from `(name, value)` pairs and sets it as
/// global `name`.
pub(crate) fn set_lib(
    lua: &Lua,
    globals: &Table,
    name: &str,
    members: Vec<(&str, Value)>,
) -> Result<()> {
    let t = lua.create_table_with_capacity(0, members.len())?;
    for (k, v) in members {
        t.raw_set(k, v)?;
    }
    t.set_readonly(true);
    globals.raw_set(name, t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_formats() {
        assert_eq!(fmt_f32(1.0), "1");
        assert_eq!(fmt_f32(0.1), "0.1");
        assert_eq!(fmt_f32(-2.5), "-2.5");
        assert_eq!(fmt_f32(f32::INFINITY), "inf");
        assert_eq!(fmt_f32(f32::NAN), "nan");
        assert_eq!(fmt_f32(1e20), "1e+20");
        assert_eq!(fmt_f32(1e-7), "1e-7");
        assert_eq!(fmt_f64(3.0), "3");
        assert_eq!(fmt_f64(0.5), "0.5");
    }
}
