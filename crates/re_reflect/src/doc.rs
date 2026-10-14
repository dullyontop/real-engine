//! [`Doc`]: the untyped value tree used at every text boundary (scene files,
//! agent operations, the transaction log). It is serde-generic; conversion to
//! typed [`Value`]s happens against the registry schema, which is where all
//! validation lives.
//!
//! Vectors are accepted as `[x, y, z]` or `{"x": .., "y": .., "z": ..}`,
//! colours as 3 or 4 numbers, and numbers for `f32` fields go through
//! [`f32_from_parsed_f64`].

use std::fmt;

use bevy_ecs::entity::Entity;
use glam::{Quat, Vec3};
use indexmap::IndexMap;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::value::{Color, FieldType, Value, ValueError};

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
                FieldType::Enum(v) => format!("one of {}", v.join(" | ")),
                other => other.name().to_string(),
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
            _ => return Err(mismatch()),
        };
        ty.coerce(raw)
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
