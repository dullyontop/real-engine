//! [`Doc`]: the untyped value tree used at every text boundary (scene files,
//! agent operations, the transaction log). It is serde-generic; conversion to
//! typed [`Value`]s happens against the registry schema, which is where all
//! validation lives.
//!
//! Vectors are accepted as `[x, y, z]` or `{"x": .., "y": .., "z": ..}`,
//! colours as 3 or 4 numbers, and numbers for `f32` fields go through
//! [`f32_from_parsed_f64`].
//!
//! Containers (ADR-0032 amendment):
//! * a list is a JSON array of its items;
//! * a record is an object of its fields by name (`{"x": 1, "y": 2}`), and is
//!   also accepted as an array of its fields in order;
//! * a map is an object whose values carry their own type: `true`, a number
//!   or a string as themselves, anything else tagged, as in
//!   `{"$type": "vec3", "value": [1, 2, 3]}`, `{"$type": "list", "value":
//!   [...]}` or `{"$type": "record", "name": "UDim2", "value": {...}}`. Map numbers are always floats, so
//!   they are written with a decimal point (`5.0`).

use std::collections::BTreeMap;
use std::fmt;

use bevy_ecs::entity::Entity;
use glam::{Quat, Vec3};
use indexmap::IndexMap;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::record::RecordId;
use crate::value::{Color, FieldType, Value, ValueError, at};

/// An untyped value as written in a file or sent by an agent.
#[derive(Clone, Debug, PartialEq)]
pub enum Doc {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A number written with `f32` shortest round-trip formatting (`0.1`, not
    /// `0.10000000149011612`). Parsing never produces this variant.
    F32(f32),
    /// A number.
    F64(f64),
    /// A string.
    Str(String),
    /// A list.
    List(Vec<Doc>),
    /// A map with keys in insertion order.
    Map(IndexMap<String, Doc>),
}

impl Serialize for Doc {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Doc::Null => s.serialize_unit(),
            Doc::Bool(b) => s.serialize_bool(*b),
            Doc::Int(i) => s.serialize_i64(*i),
            Doc::F32(f) => s.serialize_f32(*f),
            Doc::F64(f) => s.serialize_f64(*f),
            Doc::Str(v) => s.serialize_str(v),
            Doc::List(items) => {
                let mut seq = s.serialize_seq(Some(items.len()))?;
                for item in items {
                    seq.serialize_element(item)?;
                }
                seq.end()
            }
            Doc::Map(map) => {
                let mut m = s.serialize_map(Some(map.len()))?;
                for (k, v) in map {
                    m.serialize_entry(k, v)?;
                }
                m.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Doc {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(DocVisitor)
    }
}

struct DocVisitor;

impl<'de> Visitor<'de> for DocVisitor {
    type Value = Doc;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a scene value (null, bool, number, string, list or map)")
    }
    fn visit_unit<E>(self) -> Result<Doc, E> {
        Ok(Doc::Null)
    }
    fn visit_none<E>(self) -> Result<Doc, E> {
        Ok(Doc::Null)
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Doc, D::Error> {
        Doc::deserialize(d)
    }
    fn visit_bool<E>(self, v: bool) -> Result<Doc, E> {
        Ok(Doc::Bool(v))
    }
    fn visit_i64<E>(self, v: i64) -> Result<Doc, E> {
        Ok(Doc::Int(v))
    }
    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Doc, E> {
        i64::try_from(v)
            .map(Doc::Int)
            .map_err(|_| E::custom(format!("integer {v} is too large")))
    }
    fn visit_f32<E>(self, v: f32) -> Result<Doc, E> {
        Ok(Doc::F64(v as f64))
    }
    fn visit_f64<E>(self, v: f64) -> Result<Doc, E> {
        Ok(Doc::F64(v))
    }
    fn visit_str<E>(self, v: &str) -> Result<Doc, E> {
        Ok(Doc::Str(v.to_string()))
    }
    fn visit_string<E>(self, v: String) -> Result<Doc, E> {
        Ok(Doc::Str(v))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Doc, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Doc::List(items))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Doc, A::Error> {
        let mut out = IndexMap::new();
        while let Some((k, v)) = map.next_entry::<String, Doc>()? {
            out.insert(k, v);
        }
        Ok(Doc::Map(out))
    }
}

impl Doc {
    /// A short description of the document's kind, for error messages
    /// (`"list of 2"`, `"string \"heavy\""`).
    pub fn describe(&self) -> String {
        match self {
            Doc::Null => "null".into(),
            Doc::Bool(_) => "bool".into(),
            Doc::Int(_) => "integer".into(),
            Doc::F32(_) | Doc::F64(_) => "number".into(),
            Doc::Str(s) => format!("string {s:?}"),
            Doc::List(l) => format!("list of {}", l.len()),
            Doc::Map(_) => "map".into(),
        }
    }

    fn number(&self) -> Option<f64> {
        match self {
            Doc::Int(i) => Some(*i as f64),
            Doc::F32(f) => Some(*f as f64),
            Doc::F64(f) => Some(*f),
            _ => None,
        }
    }

    fn f32(&self) -> Option<f32> {
        match self {
            Doc::F64(x) => Some(f32_from_parsed_f64(*x)),
            // Correctly rounded, unlike `i as f64 as f32`.
            Doc::Int(i) => Some(*i as f32),
            other => other.number().map(|n| n as f32),
        }
    }

    /// Reads `n` numbers from `[a, b, c]` or `{x: a, y: b, z: c}`.
    fn numbers<const N: usize>(&self, keys: [&str; N]) -> Option<[f32; N]> {
        let mut out = [0.0; N];
        match self {
            Doc::List(items) if items.len() == N => {
                for (o, item) in out.iter_mut().zip(items) {
                    *o = item.f32()?;
                }
            }
            Doc::Map(map) if map.len() == N => {
                for (o, k) in out.iter_mut().zip(keys) {
                    *o = map.get(k)?.f32()?;
                }
            }
            _ => return None,
        }
        out.iter().all(|f| f.is_finite()).then_some(out)
    }

    /// Converts to a typed value of type `ty`, coerced. Entity references
    /// (stable id strings) are resolved by `resolve`.
    pub fn to_value(
        &self,
        ty: &FieldType,
        resolve: &dyn Fn(&str) -> Option<Entity>,
    ) -> Result<Value, ValueError> {
        let mismatch = || ValueError::TypeMismatch {
            expected: match ty {
                FieldType::Vec3 => "vec3 as [x, y, z]".to_string(),
                FieldType::Quat => "quat as [x, y, z, w]".to_string(),
                FieldType::Color => "color as [r, g, b] or [r, g, b, a]".to_string(),
                FieldType::Entity => "entity id string or null".to_string(),
                FieldType::Map => "map as an object".to_string(),
                FieldType::Record(id) => format!(
                    "record {} as an object of {} or a list of their values",
                    id.name(),
                    id.schema()
                        .fields
                        .iter()
                        .map(|f| f.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                other => other.describe(),
            },
            found: self.describe(),
        };
        let raw = match (ty, self) {
            (FieldType::Bool, Doc::Bool(b)) => Value::Bool(*b),
            (FieldType::I32 | FieldType::I64, Doc::Int(i)) => Value::Int(*i),
            (FieldType::I32 | FieldType::I64 | FieldType::F32 | FieldType::F64, d)
                if d.number().is_some() =>
            {
                match d {
                    Doc::Int(i) => Value::Int(*i),
                    // Out-of-range values fall through to `coerce`, which
                    // reports them as out of range rather than non-finite.
                    Doc::F64(x) if *ty == FieldType::F32 && f32_from_parsed_f64(*x).is_finite() => {
                        Value::Float(f32_from_parsed_f64(*x) as f64)
                    }
                    _ => Value::Float(d.number().unwrap_or_default()),
                }
            }
            (FieldType::Vec3, d) => Value::Vec3(Vec3::from_array(
                d.numbers(["x", "y", "z"]).ok_or_else(mismatch)?,
            )),
            (FieldType::Quat, d) => Value::Quat(Quat::from_array(
                d.numbers(["x", "y", "z", "w"]).ok_or_else(mismatch)?,
            )),
            (FieldType::Color, d) => {
                let [r, g, b, a] = match d.numbers(["r", "g", "b", "a"]) {
                    Some(c) => c,
                    None => {
                        let [r, g, b] = d.numbers(["r", "g", "b"]).ok_or_else(mismatch)?;
                        [r, g, b, 1.0]
                    }
                };
                Value::Color(Color::rgba(r, g, b, a))
            }
            (FieldType::String, Doc::Str(s)) => Value::Str(s.clone()),
            (FieldType::Enum(_), Doc::Str(s)) => Value::Enum(s.clone()),
            (FieldType::Entity, Doc::Null) => Value::Entity(None),
            (FieldType::Entity, Doc::Str(id)) => {
                Value::Entity(Some(resolve(id).ok_or_else(|| {
                    ValueError::Invalid(format!("no entity with id {id:?}"))
                })?))
            }
            (FieldType::List(item), Doc::List(items)) => Value::List(
                items
                    .iter()
                    .enumerate()
                    .map(|(i, d)| {
                        d.to_value(item, resolve)
                            .map_err(|e| at(format!("[{i}]"), e))
                    })
                    .collect::<Result<_, _>>()?,
            ),
            (FieldType::Map, Doc::Map(map)) => Value::Map(
                map.iter()
                    .map(|(k, d)| {
                        let v = d
                            .dynamic_value(false)
                            .map_err(|e| at(format!("[{k:?}]"), e))?;
                        Ok((k.clone(), v))
                    })
                    .collect::<Result<BTreeMap<_, _>, ValueError>>()?,
            ),
            (FieldType::Record(id), Doc::Map(map)) => {
                let schema = id.schema();
                if let Some(unknown) = map.keys().find(|k| schema.field_index(k).is_none()) {
                    return Err(ValueError::Invalid(format!(
                        "record {} has no field {unknown:?}{}",
                        schema.name,
                        crate::error::suggest(
                            unknown,
                            schema.fields.iter().map(|f| f.name.as_str())
                        )
                        .map(|s| format!(" (did you mean {s:?}?)"))
                        .unwrap_or_default()
                    )));
                }
                let mut values = Vec::with_capacity(schema.fields.len());
                for f in &schema.fields {
                    let d = map.get(&f.name).ok_or_else(|| {
                        ValueError::Invalid(format!(
                            "record {} needs field {:?}",
                            schema.name, f.name
                        ))
                    })?;
                    values.push(
                        d.to_value(&f.ty, resolve)
                            .map_err(|e| at(format!(".{}", f.name), e))?,
                    );
                }
                Value::Record(*id, values)
            }
            (FieldType::Record(id), Doc::List(items))
                if items.len() == id.schema().fields.len() =>
            {
                Value::Record(
                    *id,
                    id.schema()
                        .fields
                        .iter()
                        .zip(items)
                        .map(|(f, d)| {
                            d.to_value(&f.ty, resolve)
                                .map_err(|e| at(format!(".{}", f.name), e))
                        })
                        .collect::<Result<_, _>>()?,
                )
            }
            _ => return Err(mismatch()),
        };
        ty.coerce(raw)
    }

    /// Reads a map value, which carries its own type (see the module docs).
    fn dynamic_value(&self, in_list: bool) -> Result<Value, ValueError> {
        let no_entities = |_: &str| None;
        let raw = match self {
            Doc::Bool(b) => Value::Bool(*b),
            Doc::Str(s) => Value::Str(s.clone()),
            d @ (Doc::Int(_) | Doc::F32(_) | Doc::F64(_)) => {
                d.to_value(&FieldType::F64, &no_entities)?
            }
            Doc::Map(m) if m.contains_key("$type") => {
                let tag = match m.get("$type") {
                    Some(Doc::Str(t)) => t.as_str(),
                    _ => return Err(ValueError::Invalid("`$type` must be a string".into())),
                };
                let allowed: &[&str] = if tag == "record" {
                    &["$type", "name", "value"]
                } else {
                    &["$type", "value"]
                };
                if let Some(extra) = m.keys().find(|k| !allowed.contains(&k.as_str())) {
                    return Err(ValueError::Invalid(format!(
                        "unexpected key {extra:?} in a {tag:?} map value"
                    )));
                }
                let value = m.get("value").ok_or_else(|| {
                    ValueError::Invalid(format!("a {tag:?} map value needs `value`"))
                })?;
                match tag {
                    "vec3" => value.to_value(&FieldType::Vec3, &no_entities)?,
                    "quat" => value.to_value(&FieldType::Quat, &no_entities)?,
                    "color" => value.to_value(&FieldType::Color, &no_entities)?,
                    "list" if !in_list => match value {
                        Doc::List(items) => Value::List(
                            items
                                .iter()
                                .enumerate()
                                .map(|(i, d)| {
                                    d.dynamic_value(true).map_err(|e| at(format!("[{i}]"), e))
                                })
                                .collect::<Result<_, _>>()?,
                        ),
                        other => {
                            return Err(ValueError::TypeMismatch {
                                expected: "list".into(),
                                found: other.describe(),
                            });
                        }
                    },
                    "record" => {
                        let name = match m.get("name") {
                            Some(Doc::Str(n)) => n,
                            _ => {
                                return Err(ValueError::Invalid(
                                    "a \"record\" map value needs `name`".into(),
                                ));
                            }
                        };
                        let id = RecordId::by_name(name).ok_or_else(|| {
                            ValueError::Invalid(format!("no record type named {name:?}"))
                        })?;
                        value.to_value(&FieldType::Record(id), &no_entities)?
                    }
                    other => {
                        return Err(ValueError::Invalid(format!(
                            "unknown map value type {other:?} (types: vec3, quat, color, list, record{})",
                            if in_list { "; lists cannot nest" } else { "" }
                        )));
                    }
                }
            }
            other => {
                return Err(ValueError::TypeMismatch {
                    expected: "bool, number, string or an object with `$type`".into(),
                    found: other.describe(),
                });
            }
        };
        crate::value::coerce_dynamic(raw, in_list)
    }

    /// Writes a map value with its type (see the module docs).
    fn from_dynamic(value: &Value) -> Doc {
        let tagged = |tag: &str, v: Doc| {
            Doc::Map(IndexMap::from([
                ("$type".to_string(), Doc::Str(tag.to_string())),
                ("value".to_string(), v),
            ]))
        };
        let no_names = |_| None;
        match value {
            Value::Bool(b) => Doc::Bool(*b),
            Value::Int(i) => Doc::F64(*i as f64),
            Value::Float(x) => Doc::F64(*x),
            Value::Str(s) => Doc::Str(s.clone()),
            Value::Vec3(_) => tagged("vec3", Doc::from_value(value, &FieldType::Vec3, &no_names)),
            Value::Quat(_) => tagged("quat", Doc::from_value(value, &FieldType::Quat, &no_names)),
            Value::Color(_) => tagged(
                "color",
                Doc::from_value(value, &FieldType::Color, &no_names),
            ),
            Value::List(items) => tagged(
                "list",
                Doc::List(items.iter().map(Doc::from_dynamic).collect()),
            ),
            Value::Record(id, _) => Doc::Map(IndexMap::from([
                ("$type".to_string(), Doc::Str("record".into())),
                ("name".to_string(), Doc::Str(id.name().to_string())),
                (
                    "value".to_string(),
                    Doc::from_value(value, &FieldType::Record(*id), &no_names),
                ),
            ])),
            // Not storable in a map (refused by coercion).
            Value::Map(_) | Value::Entity(_) | Value::Enum(_) => Doc::Null,
        }
    }

    /// Converts a typed value for writing. Entity references are named by
    /// `name_of`; an entity without a stable id is written as null.
    pub fn from_value(
        value: &Value,
        ty: &FieldType,
        name_of: &dyn Fn(Entity) -> Option<String>,
    ) -> Doc {
        let f = Doc::F32;
        match value {
            Value::Bool(b) => Doc::Bool(*b),
            Value::Int(i) => Doc::Int(*i),
            Value::Float(x) if *ty == FieldType::F32 => Doc::F32(*x as f32),
            Value::Float(x) => Doc::F64(*x),
            Value::Vec3(v) => Doc::List(v.to_array().map(f).to_vec()),
            Value::Quat(q) => Doc::List(q.to_array().map(f).to_vec()),
            Value::Color(c) => Doc::List([c.r, c.g, c.b, c.a].map(f).to_vec()),
            Value::Str(s) => Doc::Str(s.clone()),
            Value::Enum(s) => Doc::Str(s.clone()),
            Value::Entity(e) => e.and_then(name_of).map(Doc::Str).unwrap_or(Doc::Null),
            Value::List(items) => {
                let item = match ty {
                    FieldType::List(item) => Some(&**item),
                    _ => None,
                };
                Doc::List(
                    items
                        .iter()
                        .map(|v| match item {
                            Some(t) => Doc::from_value(v, t, name_of),
                            None => Doc::from_dynamic(v),
                        })
                        .collect(),
                )
            }
            Value::Map(map) => Doc::Map(
                map.iter()
                    .map(|(k, v)| (k.clone(), Doc::from_dynamic(v)))
                    .collect(),
            ),
            Value::Record(id, values) => Doc::Map(
                id.schema()
                    .fields
                    .iter()
                    .zip(values)
                    .map(|(f, v)| (f.name.clone(), Doc::from_value(v, &f.ty, name_of)))
                    .collect(),
            ),
        }
    }
}

impl From<&serde_json::Value> for Doc {
    fn from(v: &serde_json::Value) -> Doc {
        match v {
            serde_json::Value::Null => Doc::Null,
            serde_json::Value::Bool(b) => Doc::Bool(*b),
            serde_json::Value::Number(n) => match (n.as_i64(), n.as_f64()) {
                (Some(i), _) => Doc::Int(i),
                (None, Some(f)) => Doc::F64(f),
                (None, None) => Doc::Null,
            },
            serde_json::Value::String(s) => Doc::Str(s.clone()),
            serde_json::Value::Array(a) => Doc::List(a.iter().map(Doc::from).collect()),
            serde_json::Value::Object(o) => {
                Doc::Map(o.iter().map(|(k, v)| (k.clone(), Doc::from(v))).collect())
            }
        }
    }
}

impl From<&Doc> for serde_json::Value {
    fn from(d: &Doc) -> serde_json::Value {
        serde_json::to_value(d).unwrap_or(serde_json::Value::Null)
    }
}

/// Recovers the `f32` a text number meant after the parser read it as `f64`.
///
/// `text -> f64 -> f32` double-rounds: of all 4,278,190,080 finite `f32`
/// values, exactly two (±7.038531e-26, bits `0x15ae43fd` and `0x95ae43fd`) come
/// back one ulp off. Re-reading the `f64`'s shortest decimal form as `f32` is
/// exact for any text of up to 15 significant digits, which covers every
/// shortest-form `f32`.
pub fn f32_from_parsed_f64(x: f64) -> f32 {
    let direct = x as f32;
    if !x.is_finite() || direct as f64 == x {
        return direct;
    }
    format!("{x}").parse::<f32>().unwrap_or(direct)
}
