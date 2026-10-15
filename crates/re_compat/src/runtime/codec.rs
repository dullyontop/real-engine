//! [`ValueCodec`]: conversions between Luau values and the [`Value`]s the
//! class layer stores, for properties (typed by the property's data-file type)
//! and attributes (typed by the value itself).
//!
//! The codec knows booleans, numbers, strings, `Vector3` (the native Luau
//! vector) and instances. The datatype work package registers hooks for its
//! userdata (`Color3`, `CFrame`, `UDim2`, `EnumItem`, ...). Without a hook, an
//! enum item reads as its name, a colour as a frozen `{R, G, B}` table and a
//! record as the frozen table `re_script` makes.

use std::collections::HashMap;
use std::sync::Arc;

use mlua::{AnyUserData, Lua, Value as LuaValue};
use re_reflect::Value;

use crate::instance;

/// Converts a userdata to a stored value; `Ok(None)` when it is not this
/// hook's type.
pub type FromUserdataFn = dyn Fn(&Lua, &AnyUserData) -> mlua::Result<Option<Value>> + Send + Sync;
/// Converts a stored value of one datatype to Luau.
pub type ToLuaFn = dyn Fn(&Lua, &Value) -> mlua::Result<LuaValue> + Send + Sync;
/// Makes the Luau value of an enum item: `(lua, enum type, item name)`.
pub type EnumToLuaFn = dyn Fn(&Lua, &str, &str) -> mlua::Result<LuaValue> + Send + Sync;
/// Reads an enum item written to a property of enum type `enum type`
/// (an item, its name or its number); `Ok(None)` when the value is not one.
pub type EnumFromLuaFn =
    dyn Fn(&Lua, &str, &LuaValue) -> mlua::Result<Option<String>> + Send + Sync;

/// The conversions between Luau and stored values, extended by hooks.
#[derive(Clone, Default)]
pub struct ValueCodec {
    from_userdata: Vec<Arc<FromUserdataFn>>,
    to_lua: HashMap<String, Arc<ToLuaFn>>,
    enum_to_lua: Option<Arc<EnumToLuaFn>>,
    enum_from_lua: Option<Arc<EnumFromLuaFn>>,
}

impl std::fmt::Debug for ValueCodec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValueCodec")
            .field("from_userdata", &self.from_userdata.len())
            .field("to_lua", &self.to_lua.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// What a property or attribute may not hold.
fn kind_of(v: &LuaValue) -> &'static str {
    match v {
        LuaValue::Table(_) => "a table",
        LuaValue::Function(_) => "a function",
        LuaValue::Thread(_) => "a thread",
        LuaValue::UserData(_) => "this userdata",
        LuaValue::Nil => "nil",
        _ => "this value",
    }
}

impl ValueCodec {
    /// Adds a userdata → value conversion (tried in registration order).
    pub fn add_from_userdata(&mut self, f: Arc<FromUserdataFn>) {
        self.from_userdata.push(f);
    }

    /// Sets the value → Luau conversion of datatype `name`: the property type
    /// (`Color3`, `CFrame`) or, for records, the record type's name.
    pub fn set_to_lua(&mut self, name: &str, f: Arc<ToLuaFn>) {
        self.to_lua.insert(name.to_string(), f);
    }

    /// Sets how enum items are made in Luau.
    pub fn set_enum_to_lua(&mut self, f: Arc<EnumToLuaFn>) {
        self.enum_to_lua = Some(f);
    }

    /// Sets how values written to enum properties are read.
    pub fn set_enum_from_lua(&mut self, f: Arc<EnumFromLuaFn>) {
        self.enum_from_lua = Some(f);
    }

    fn userdata_value(&self, lua: &Lua, ud: &AnyUserData) -> mlua::Result<Option<Value>> {
        if let Some(e) = instance::entity_of(ud) {
            return Ok(Some(Value::Entity(Some(e))));
        }
        for f in &self.from_userdata {
            if let Some(v) = f(lua, ud)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    /// The stored value of `v` for a property of data-file type `ty`
    /// (`bool`, `number`, `string`, `Vector3`, `Instance`, `Enum.Material`,
    /// `Color3`, ...). Numbers and strings convert into each other where the
    /// type asks for it, as Roblox does. The class layer coerces the result to
    /// the field type. Errors are plain messages.
    pub fn from_lua(&self, lua: &Lua, v: LuaValue, ty: &str) -> Result<Value, String> {
        let is_instance = ty == "Instance" || ty.starts_with("Instance<");
        if let Some(enum_type) = ty.strip_prefix("Enum.") {
            if let Some(hook) = &self.enum_from_lua
                && let Some(item) = hook(lua, enum_type, &v).map_err(|e| e.to_string())?
            {
                return Ok(Value::Enum(item));
            }
            return match v {
                LuaValue::String(s) => Ok(Value::Enum(
                    s.to_str().map_err(|e| e.to_string())?.to_string(),
                )),
                other => Err(format!(
                    "expected an Enum.{enum_type} item, got {}",
                    kind_of(&other)
                )),
            };
        }
        Ok(match v {
            LuaValue::Nil if is_instance => Value::Entity(None),
            LuaValue::Boolean(b) => Value::Bool(b),
            LuaValue::Integer(i) if ty == "string" => Value::Str(i.to_string()),
            LuaValue::Number(n) if ty == "string" => Value::Str(number_text(n)),
            LuaValue::Integer(i) => Value::Float(i as f64),
            LuaValue::Number(n) => Value::Float(n),
            LuaValue::String(s) => {
                let s = s.to_str().map_err(|e| e.to_string())?.to_string();
                if matches!(ty, "number" | "int")
                    && let Ok(n) = s.trim().parse::<f64>()
                {
                    Value::Float(n)
                } else {
                    Value::Str(s)
                }
            }
            LuaValue::Vector(v) => Value::Vec3(re_types::glam::Vec3::new(v.x(), v.y(), v.z())),
            LuaValue::UserData(ud) => {
                match self.userdata_value(lua, &ud).map_err(|e| e.to_string())? {
                    Some(v) => v,
                    None => return Err(format!("expected {ty}, got {}", userdata_name(&ud))),
                }
            }
            LuaValue::Table(_) => re_script::api::value::lua_to_value(v).map_err(strip_code)?,
            other => return Err(format!("expected {ty}, got {}", kind_of(&other))),
        })
    }

    /// The Luau value of a stored value of a property of data-file type `ty`.
    pub fn to_lua(&self, lua: &Lua, v: Value, ty: &str) -> mlua::Result<LuaValue> {
        if let (Value::Enum(item), Some(enum_type)) = (&v, ty.strip_prefix("Enum.")) {
            return match &self.enum_to_lua {
                Some(hook) => hook(lua, enum_type, item),
                None => Ok(LuaValue::String(lua.create_string(item)?)),
            };
        }
        if let Some(hook) = self.to_lua.get(ty) {
            return hook(lua, &v);
        }
        self.dyn_to_lua(lua, v)
    }

    /// The Luau value of a stored value whose type comes from the value itself
    /// (an attribute).
    pub fn dyn_to_lua(&self, lua: &Lua, v: Value) -> mlua::Result<LuaValue> {
        Ok(match v {
            Value::Bool(b) => LuaValue::Boolean(b),
            Value::Int(i) => LuaValue::Number(i as f64),
            Value::Float(f) => LuaValue::Number(f),
            Value::Str(s) | Value::Enum(s) => LuaValue::String(lua.create_string(&s)?),
            Value::Vec3(v) => LuaValue::Vector(mlua::Vector::new(v.x, v.y, v.z)),
            Value::Entity(None) => LuaValue::Nil,
            Value::Entity(Some(e)) => instance::push_ref(lua, e)?,
            Value::Color(c) => match self.to_lua.get("Color3") {
                Some(hook) => hook(lua, &Value::Color(c))?,
                None => {
                    let t = lua.create_table()?;
                    t.raw_set("R", c.r)?;
                    t.raw_set("G", c.g)?;
                    t.raw_set("B", c.b)?;
                    t.set_readonly(true);
                    LuaValue::Table(t)
                }
            },
            Value::Record(id, ref _fields) => match self.to_lua.get(id.name()) {
                Some(hook) => hook(lua, &v)?,
                None => re_script::api::value::value_to_lua(lua, v)?,
            },
            other => re_script::api::value::value_to_lua(lua, other)?,
        })
    }

    /// The stored value of an attribute value, or a message saying why it is
    /// refused. `nil` gives `None` (the attribute is removed).
    pub fn attribute_from_lua(&self, lua: &Lua, v: LuaValue) -> Result<Option<Value>, String> {
        Ok(Some(match v {
            LuaValue::Nil => return Ok(None),
            LuaValue::Boolean(b) => Value::Bool(b),
            LuaValue::Integer(i) => Value::Float(i as f64),
            LuaValue::Number(n) => Value::Float(n),
            LuaValue::String(s) => Value::Str(s.to_str().map_err(|e| e.to_string())?.to_string()),
            LuaValue::Vector(v) => Value::Vec3(re_types::glam::Vec3::new(v.x(), v.y(), v.z())),
            LuaValue::UserData(ud) => {
                if instance::entity_of(&ud).is_some() {
                    return Err("an attribute cannot hold an Instance".into());
                }
                match self.userdata_value(lua, &ud).map_err(|e| e.to_string())? {
                    Some(v) => v,
                    None => return Err(format!("an attribute cannot hold {}", userdata_name(&ud))),
                }
            }
            LuaValue::Table(ref t)
                if t.metatable().is_some_and(|m| {
                    m.raw_get::<Option<String>>("__record")
                        .ok()
                        .flatten()
                        .is_some()
                }) =>
            {
                re_script::api::value::lua_to_value(v).map_err(strip_code)?
            }
            other => return Err(format!("an attribute cannot hold {}", kind_of(&other))),
        }))
    }
}

/// Roblox writes whole numbers without a fraction when it turns them into text.
fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

fn userdata_name(ud: &AnyUserData) -> String {
    ud.metatable()
        .ok()
        .and_then(|mt| mt.get::<String>("__type").ok())
        .map(|t| format!("a {t}"))
        .unwrap_or_else(|| "this userdata".to_string())
}

/// The message of an `[RE####] message` error.
fn strip_code(e: mlua::Error) -> String {
    let s = e.to_string();
    match s.find("] ") {
        Some(i) if s.starts_with("runtime error: [RE") || s.starts_with("[RE") => {
            s[i + 2..].to_string()
        }
        _ => s,
    }
}
