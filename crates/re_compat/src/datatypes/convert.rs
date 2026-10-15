//! Conversions between Luau values and the stored forms of instance
//! properties, by the property's data-file type.
//!
//! | Data-file type | Stored form |
//! |---|---|
//! | `bool`, `number`, `int`, `string` | `Bool`, `Float`, `Int`, `Str` |
//! | `Vector3` | `Vec3` |
//! | `Color3` | `Color`, channels as the script gave them, alpha 1 |
//! | `Enum.<Name>` | `Enum`, the item's name |
//! | `CFrame`, `Vector2`, `UDim`, `UDim2`, `Rect`, `NumberRange`, `PhysicalProperties`, `Font`, keypoints | the record of that name (`libraries/*.json`) |
//! | `NumberSequence`, `ColorSequence` | a list of keypoint records |
//!
//! Records are registered when the class registry is built, so build it
//! before converting record values.

use glam::Vec3;
use mlua::{Lua, Result, Value as LuaValue};
use re_reflect::{Color, RecordId, Value};

use super::{
    CFrame, Color3, ColorSequence, ColorSequenceKeypoint, Font, NumberRange, NumberSequence,
    NumberSequenceKeypoint, PhysicalProperties, Rect, UDim, UDim2, Vector2, bad_argument,
    type_name, ud_arg, vec_value,
};
use crate::enums::EnumItem;

fn bad_stored(ty: &str, v: &Value) -> mlua::Error {
    mlua::Error::runtime(format!(
        "[RE9003] a stored {ty} value has the wrong form: {}",
        v.describe()
    ))
}

/// Field values of record `name` by field name, in the schema's order.
fn record(name: &str, fields: &[(&str, Value)]) -> Result<Value> {
    let id = RecordId::by_name(name).ok_or_else(|| {
        mlua::Error::runtime(format!(
            "[RE9003] record type {name} is not registered: build the class registry first"
        ))
    })?;
    let schema = id.schema();
    let mut values = Vec::with_capacity(schema.fields.len());
    for f in &schema.fields {
        let v = fields
            .iter()
            .find(|(n, _)| *n == f.name)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| {
                mlua::Error::runtime(format!(
                    "[RE9003] record {name} has no field {:?} here",
                    f.name
                ))
            })?;
        values.push(v);
    }
    Ok(Value::Record(id, values))
}

/// The fields of a stored record of type `name`.
fn fields<'a>(name: &str, v: &'a Value) -> Result<Vec<(&'static str, &'a Value)>> {
    match v {
        Value::Record(id, values) if id.name() == name => Ok(id
            .schema()
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .zip(values)
            .collect()),
        other => Err(bad_stored(name, other)),
    }
}

fn get<'a>(fs: &[(&'static str, &'a Value)], name: &str) -> Option<&'a Value> {
    fs.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

fn f32_of(fs: &[(&'static str, &Value)], name: &str) -> f32 {
    match get(fs, name) {
        Some(Value::Float(x)) => *x as f32,
        Some(Value::Int(i)) => *i as f32,
        _ => 0.0,
    }
}

fn i32_of(fs: &[(&'static str, &Value)], name: &str) -> i32 {
    match get(fs, name) {
        Some(Value::Int(i)) => *i as i32,
        Some(Value::Float(x)) => *x as i32,
        _ => 0,
    }
}

fn v3_of(fs: &[(&'static str, &Value)], name: &str) -> [f32; 3] {
    match get(fs, name) {
        Some(Value::Vec3(v)) => v.to_array(),
        _ => [0.0; 3],
    }
}

fn color_of(fs: &[(&'static str, &Value)], name: &str) -> Color3 {
    match get(fs, name) {
        Some(Value::Color(c)) => Color3::new(c.r, c.g, c.b),
        _ => Color3::default(),
    }
}

fn enum_of(fs: &[(&'static str, &Value)], name: &str, ty: &str, default: &str) -> EnumItem {
    match get(fs, name) {
        Some(Value::Enum(s) | Value::Str(s)) => EnumItem::lookup(ty, s),
        _ => None,
    }
    .unwrap_or_else(|| EnumItem::named(ty, default))
}

fn f(x: f32) -> Value {
    Value::Float(x as f64)
}

fn v3(v: [f32; 3]) -> Value {
    Value::Vec3(Vec3::from_array(v))
}

fn color(c: Color3) -> Value {
    Value::Color(Color::rgba(c.r, c.g, c.b, 1.0))
}

fn ud(lua: &Lua, v: impl mlua::UserData + 'static) -> Result<LuaValue> {
    Ok(LuaValue::UserData(lua.create_userdata(v)?))
}

fn nsk_value(k: &NumberSequenceKeypoint) -> Result<Value> {
    record(
        "NumberSequenceKeypoint",
        &[
            ("Time", f(k.time)),
            ("Value", f(k.value)),
            ("Envelope", f(k.envelope)),
        ],
    )
}

fn csk_value(k: &ColorSequenceKeypoint) -> Result<Value> {
    record(
        "ColorSequenceKeypoint",
        &[("Time", f(k.time)), ("Value", color(k.value))],
    )
}

fn nsk_of(v: &Value) -> Result<NumberSequenceKeypoint> {
    let fs = fields("NumberSequenceKeypoint", v)?;
    Ok(NumberSequenceKeypoint {
        time: f32_of(&fs, "Time"),
        value: f32_of(&fs, "Value"),
        envelope: f32_of(&fs, "Envelope"),
    })
}

fn csk_of(v: &Value) -> Result<ColorSequenceKeypoint> {
    let fs = fields("ColorSequenceKeypoint", v)?;
    Ok(ColorSequenceKeypoint {
        time: f32_of(&fs, "Time"),
        value: color_of(&fs, "Value"),
    })
}

/// Converts the stored value of a property of data-file type `ty` to Luau.
pub fn to_lua(lua: &Lua, ty: &str, v: Value) -> Result<LuaValue> {
    if let Some(name) = ty.strip_prefix("Enum.") {
        return match &v {
            Value::Enum(s) | Value::Str(s) => match EnumItem::lookup(name, s) {
                Some(item) => item.to_lua(lua),
                None => Err(bad_stored(ty, &v)),
            },
            other => Err(bad_stored(ty, other)),
        };
    }
    Ok(match (ty, &v) {
        ("bool", Value::Bool(b)) => LuaValue::Boolean(*b),
        ("number" | "int", Value::Float(x)) => LuaValue::Number(*x),
        ("number" | "int", Value::Int(i)) => LuaValue::Number(*i as f64),
        ("string", Value::Str(s)) => LuaValue::String(lua.create_string(s)?),
        ("Vector3", Value::Vec3(v)) => vec_value(v.to_array()),
        ("Color3", Value::Color(c)) => ud(lua, Color3::new(c.r, c.g, c.b))?,
        ("CFrame", _) => {
            let fs = fields(ty, &v)?;
            let (x, y, z) = (
                v3_of(&fs, "XVector"),
                v3_of(&fs, "YVector"),
                v3_of(&fs, "ZVector"),
            );
            ud(
                lua,
                CFrame {
                    p: v3_of(&fs, "Position"),
                    r: [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]],
                },
            )?
        }
        ("Vector2", _) => {
            let fs = fields(ty, &v)?;
            ud(lua, Vector2::new(f32_of(&fs, "X"), f32_of(&fs, "Y")))?
        }
        ("UDim", _) => {
            let fs = fields(ty, &v)?;
            ud(lua, UDim::new(f32_of(&fs, "Scale"), i32_of(&fs, "Offset")))?
        }
        ("UDim2", _) => {
            let fs = fields(ty, &v)?;
            ud(
                lua,
                UDim2::new(
                    f32_of(&fs, "XScale"),
                    i32_of(&fs, "XOffset"),
                    f32_of(&fs, "YScale"),
                    i32_of(&fs, "YOffset"),
                ),
            )?
        }
        ("Rect", _) => {
            let fs = fields(ty, &v)?;
            ud(
                lua,
                Rect {
                    min: Vector2::new(f32_of(&fs, "MinX"), f32_of(&fs, "MinY")),
                    max: Vector2::new(f32_of(&fs, "MaxX"), f32_of(&fs, "MaxY")),
                },
            )?
        }
        ("NumberRange", _) => {
            let fs = fields(ty, &v)?;
            ud(
                lua,
                NumberRange {
                    min: f32_of(&fs, "Min"),
                    max: f32_of(&fs, "Max"),
                },
            )?
        }
        ("PhysicalProperties", _) => {
            let fs = fields(ty, &v)?;
            ud(
                lua,
                PhysicalProperties {
                    density: f32_of(&fs, "Density"),
                    friction: f32_of(&fs, "Friction"),
                    elasticity: f32_of(&fs, "Elasticity"),
                    friction_weight: f32_of(&fs, "FrictionWeight"),
                    elasticity_weight: f32_of(&fs, "ElasticityWeight"),
                    acoustic_absorption: f32_of(&fs, "AcousticAbsorption"),
                },
            )?
        }
        ("Font", _) => {
            let fs = fields(ty, &v)?;
            let family = match get(&fs, "Family") {
                Some(Value::Str(s)) => s.clone(),
                _ => String::new(),
            };
            ud(
                lua,
                Font {
                    family,
                    weight: enum_of(&fs, "Weight", "FontWeight", "Regular"),
                    style: enum_of(&fs, "Style", "FontStyle", "Normal"),
                },
            )?
        }
        ("NumberSequenceKeypoint", _) => ud(lua, nsk_of(&v)?)?,
        ("ColorSequenceKeypoint", _) => ud(lua, csk_of(&v)?)?,
        ("NumberSequence", Value::List(items)) => ud(
            lua,
            NumberSequence {
                keypoints: items.iter().map(nsk_of).collect::<Result<_>>()?,
            },
        )?,
        ("ColorSequence", Value::List(items)) => ud(
            lua,
            ColorSequence {
                keypoints: items.iter().map(csk_of).collect::<Result<_>>()?,
            },
        )?,
        _ => return Err(bad_stored(ty, &v)),
    })
}

/// Converts a Luau value written to a property of data-file type `ty` to its
/// stored form. A value of the wrong type raises `RE3006`; an enum property
/// accepts an item, its name or its number.
pub fn from_lua(_lua: &Lua, ty: &str, v: LuaValue) -> Result<Value> {
    let wrong = |v: &LuaValue| bad_argument(3, ty, format!("{ty} expected, got {}", type_name(v)));
    if let Some(name) = ty.strip_prefix("Enum.") {
        return Ok(Value::Enum(EnumItem::coerce(&v, name)?.name().to_string()));
    }
    Ok(match ty {
        "bool" => match v {
            LuaValue::Boolean(b) => Value::Bool(b),
            other => return Err(wrong(&other)),
        },
        "number" => Value::Float(super::num_arg(&v, 3, ty, None)?),
        "int" => Value::Int(super::num_arg(&v, 3, ty, None)? as i64),
        "string" => match v {
            LuaValue::String(s) => Value::Str(s.to_str()?.to_string()),
            LuaValue::Integer(_) | LuaValue::Number(_) => Value::Str(v.to_string()?),
            other => return Err(wrong(&other)),
        },
        "Vector3" => match v {
            LuaValue::Vector(x) => Value::Vec3(Vec3::new(x.x(), x.y(), x.z())),
            other => return Err(wrong(&other)),
        },
        "Color3" => color(ud_arg::<Color3>(&v, 3, ty, ty)?),
        "CFrame" => {
            let c = ud_arg::<CFrame>(&v, 3, ty, ty)?;
            record(
                ty,
                &[
                    ("Position", v3(c.p)),
                    ("XVector", v3(c.x_vector())),
                    ("YVector", v3(c.y_vector())),
                    ("ZVector", v3(c.z_vector())),
                ],
            )?
        }
        "Vector2" => {
            let x = ud_arg::<Vector2>(&v, 3, ty, ty)?;
            record(ty, &[("X", f(x.x)), ("Y", f(x.y))])?
        }
        "UDim" => {
            let u = ud_arg::<UDim>(&v, 3, ty, ty)?;
            record(
                ty,
                &[
                    ("Scale", f(u.scale)),
                    ("Offset", Value::Int(u.offset as i64)),
                ],
            )?
        }
        "UDim2" => {
            let u = ud_arg::<UDim2>(&v, 3, ty, ty)?;
            record(
                ty,
                &[
                    ("XScale", f(u.x.scale)),
                    ("XOffset", Value::Int(u.x.offset as i64)),
                    ("YScale", f(u.y.scale)),
                    ("YOffset", Value::Int(u.y.offset as i64)),
                ],
            )?
        }
        "Rect" => {
            let r = ud_arg::<Rect>(&v, 3, ty, ty)?;
            record(
                ty,
                &[
                    ("MinX", f(r.min.x)),
                    ("MinY", f(r.min.y)),
                    ("MaxX", f(r.max.x)),
                    ("MaxY", f(r.max.y)),
                ],
            )?
        }
        "NumberRange" => {
            let r = ud_arg::<NumberRange>(&v, 3, ty, ty)?;
            record(ty, &[("Min", f(r.min)), ("Max", f(r.max))])?
        }
        "PhysicalProperties" => {
            let p = ud_arg::<PhysicalProperties>(&v, 3, ty, ty)?;
            record(
                ty,
                &[
                    ("Density", f(p.density)),
                    ("Friction", f(p.friction)),
                    ("Elasticity", f(p.elasticity)),
                    ("FrictionWeight", f(p.friction_weight)),
                    ("ElasticityWeight", f(p.elasticity_weight)),
                    ("AcousticAbsorption", f(p.acoustic_absorption)),
                ],
            )?
        }
        "Font" => {
            let x = ud_arg::<Font>(&v, 3, ty, ty)?;
            record(
                ty,
                &[
                    ("Family", Value::Str(x.family.clone())),
                    ("Weight", Value::Enum(x.weight.name().into())),
                    ("Style", Value::Enum(x.style.name().into())),
                ],
            )?
        }
        "NumberSequenceKeypoint" => nsk_value(&ud_arg::<NumberSequenceKeypoint>(&v, 3, ty, ty)?)?,
        "ColorSequenceKeypoint" => csk_value(&ud_arg::<ColorSequenceKeypoint>(&v, 3, ty, ty)?)?,
        "NumberSequence" => {
            let s = ud_arg::<NumberSequence>(&v, 3, ty, ty)?;
            Value::List(s.keypoints.iter().map(nsk_value).collect::<Result<_>>()?)
        }
        "ColorSequence" => {
            let s = ud_arg::<ColorSequence>(&v, 3, ty, ty)?;
            Value::List(s.keypoints.iter().map(csk_value).collect::<Result<_>>()?)
        }
        _ => {
            return Err(mlua::Error::runtime(format!(
                "[RE9003] {ty} is not a datatype this layer stores"
            )));
        }
    })
}
