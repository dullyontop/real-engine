//! Dynamic values: the one currency that scripts, the editor, scene files and
//! agents use to read and write component fields (ADR-0032).
//!
//! The set of field types is deliberately closed. Besides the scalars there
//! are three container types (ADR-0032 amendment): a list of one scalar or
//! record type, a map from strings to dynamically typed values, and a record
//! (a registered, fixed tuple of scalars; see [`crate::record`]). Containers
//! nest only that far: a list holds scalars or records, a map holds scalars,
//! records or such lists, and no container holds an entity reference. The
//! types map one-to-one onto Luau values, JSON values and Roblox-style typed
//! properties, which keeps every consumer simple.
//!
//! Coercion rules ([`FieldType::coerce`]):
//! * an integer is accepted for a float field;
//! * an integral float is accepted for an integer field;
//! * an `f64` is rounded to `f32` for an `f32` field, with a range check;
//! * non-finite floats are rejected;
//! * quaternions are normalised (and must be finite and non-zero);
//! * enum names are checked against the variants;
//! * list items and record fields are coerced to their types, and map values
//!   by the dynamic rules ([`coerce_dynamic`]): integers become floats.
//!
//! [`Value::diff`] reports what changed between two values: the indices of a
//! list, the keys of a map or the fields of a record.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Mutex, OnceLock};

use bevy_ecs::entity::Entity;
use glam::{Quat, Vec3};
use re_core::{Code, Diagnostic};

use crate::codes;
use crate::record::RecordId;

/// Linear RGBA colour.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Color {
    /// Red, linear.
    pub r: f32,
    /// Green, linear.
    pub g: f32,
    /// Blue, linear.
    pub b: f32,
    /// Alpha (1 is opaque).
    pub a: f32,
}

impl Color {
    /// Opaque white.
    pub const WHITE: Color = Color::rgba(1.0, 1.0, 1.0, 1.0);

    /// A colour from its four channels.
    pub const fn rgba(r: f32, g: f32, b: f32, a: f32) -> Self {
        Self { r, g, b, a }
    }
}

/// The type of a component field: the closed set of ADR-0032.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldType {
    /// `bool`.
    Bool,
    /// `i32`, stored in [`Value::Int`].
    I32,
    /// `i64`, stored in [`Value::Int`].
    I64,
    /// `f32`, stored in [`Value::Float`] (already rounded to `f32`).
    F32,
    /// `f64`, stored in [`Value::Float`].
    F64,
    /// A `glam::Vec3`.
    Vec3,
    /// A `glam::Quat` (normalised on write).
    Quat,
    /// A linear RGBA [`Color`].
    Color,
    /// A UTF-8 string.
    String,
    /// A nullable reference to another entity. Stored as a runtime `Entity`,
    /// written to files as the target's stable id.
    Entity,
    /// One of a fixed set of named variants (unit enums only).
    Enum(&'static [&'static str]),
    /// A list of values of one scalar or record type, stored in
    /// [`Value::List`].
    List(Box<FieldType>),
    /// A map from strings to dynamically typed values (scalars, records, or
    /// lists of those; no entity references), stored in [`Value::Map`].
    Map,
    /// A registered record type, stored in [`Value::Record`].
    Record(RecordId),
}

impl FieldType {
    /// Short name used in error messages and in the schema (`"f32"`, `"enum"`, ...).
    pub fn name(&self) -> &'static str {
        match self {
            FieldType::Bool => "bool",
            FieldType::I32 => "i32",
            FieldType::I64 => "i64",
            FieldType::F32 => "f32",
            FieldType::F64 => "f64",
            FieldType::Vec3 => "vec3",
            FieldType::Quat => "quat",
            FieldType::Color => "color",
            FieldType::String => "string",
            FieldType::Entity => "entity",
            FieldType::Enum(_) => "enum",
            FieldType::List(_) => "list",
            FieldType::Map => "map",
            FieldType::Record(_) => "record",
        }
    }

    /// True for the types a record field or list item may have without being
    /// a record: everything but `entity` and the containers.
    pub fn is_scalar(&self) -> bool {
        !matches!(
            self,
            FieldType::Entity | FieldType::List(_) | FieldType::Map | FieldType::Record(_)
        )
    }

    /// Checks the nesting rules: a list's items are a scalar or a record type.
    pub fn validate(&self) -> Result<(), String> {
        match self {
            FieldType::List(item)
                if !(item.is_scalar() || matches!(**item, FieldType::Record(_))) =>
            {
                Err(format!(
                    "a list holds scalars or records, not {}",
                    item.describe()
                ))
            }
            FieldType::Enum([]) => Err("an enum needs at least one variant".into()),
            _ => Ok(()),
        }
    }

    /// The zero value of the type: `false`, `0`, the zero vector, the identity
    /// quaternion, white, the empty string, null, the first variant, an empty
    /// container, or a record of its fields' zero values. Used when a schema
    /// gives no default.
    pub fn default_value(&self) -> Value {
        match self {
            FieldType::Bool => Value::Bool(false),
            FieldType::I32 | FieldType::I64 => Value::Int(0),
            FieldType::F32 | FieldType::F64 => Value::Float(0.0),
            FieldType::Vec3 => Value::Vec3(Vec3::ZERO),
            FieldType::Quat => Value::Quat(Quat::IDENTITY),
            FieldType::Color => Value::Color(Color::WHITE),
            FieldType::String => Value::Str(String::new()),
            FieldType::Entity => Value::Entity(None),
            FieldType::Enum(v) => Value::Enum(v.first().copied().unwrap_or_default().to_string()),
            FieldType::List(_) => Value::List(Vec::new()),
            FieldType::Map => Value::Map(BTreeMap::new()),
            FieldType::Record(id) => Value::Record(*id, id.schema().default_values()),
        }
    }

    /// The type named `kind` (the inverse of [`FieldType::name`]); `variants` is
    /// used only for `"enum"`. Returns `None` for an unknown kind, an enum
    /// without variants, and `"list"` and `"record"`, which need more than a
    /// name ([`FieldTypeDoc::to_field_type`](crate::FieldTypeDoc::to_field_type)).
    pub fn from_name(kind: &str, variants: &[String]) -> Option<FieldType> {
        Some(match kind {
            "bool" => FieldType::Bool,
            "i32" => FieldType::I32,
            "i64" => FieldType::I64,
            "f32" => FieldType::F32,
            "f64" => FieldType::F64,
            "vec3" => FieldType::Vec3,
            "quat" => FieldType::Quat,
            "color" => FieldType::Color,
            "string" => FieldType::String,
            "entity" => FieldType::Entity,
            "enum" if !variants.is_empty() => FieldType::Enum(intern_variants(variants)),
            "map" => FieldType::Map,
            _ => return None,
        })
    }

    /// Coerce a value into the exact representation this field type stores.
    ///
    /// Numbers are lenient the way Luau and JSON need them to be: an integer is
    /// accepted for a float field, and an integral float for an integer field.
    pub fn coerce(&self, value: Value) -> Result<Value, ValueError> {
        let mismatch = |v: &Value| ValueError::TypeMismatch {
            expected: self.describe(),
            found: v.describe(),
        };
        Ok(match (self, value) {
            (FieldType::Bool, v @ Value::Bool(_)) => v,
            (FieldType::I32, Value::Int(i)) => {
                i32::try_from(i).map_err(|_| ValueError::OutOfRange {
                    ty: "i32",
                    value: i.to_string(),
                })?;
                Value::Int(i)
            }
            (FieldType::I64, v @ Value::Int(_)) => v,
            (FieldType::I32 | FieldType::I64, Value::Float(f)) => {
                if f.fract() != 0.0 || !f.is_finite() || f.abs() > 9.007_199_254_740_992e15 {
                    return Err(ValueError::NotAnInteger(f));
                }
                return self.coerce(Value::Int(f as i64));
            }
            (FieldType::F32, Value::Float(f)) => {
                if !f.is_finite() {
                    return Err(ValueError::NotFinite(f));
                }
                // Values that round to a finite f32 (up to just below
                // f32::MAX + half an ulp) are accepted.
                if (f as f32).is_infinite() {
                    return Err(ValueError::OutOfRange {
                        ty: "f32",
                        value: f.to_string(),
                    });
                }
                Value::Float(f as f32 as f64)
            }
            (FieldType::F64, Value::Float(f)) => {
                if !f.is_finite() {
                    return Err(ValueError::NotFinite(f));
                }
                Value::Float(f)
            }
            // `i64 as f32` is correctly rounded; going through f64 would round
            // twice (and can land one f32 ulp off above 2^53).
            (FieldType::F32, Value::Int(i)) => Value::Float(i as f32 as f64),
            (FieldType::F64, Value::Int(i)) => {
                return self.coerce(Value::Float(i as f64));
            }
            (FieldType::Vec3, Value::Vec3(v)) => {
                if !v.is_finite() {
                    return Err(ValueError::Invalid("vec3 must be finite".into()));
                }
                Value::Vec3(v)
            }
            (FieldType::Quat, Value::Quat(q)) => {
                if !q.is_finite() || q.length_squared() < 1e-12 {
                    return Err(ValueError::Invalid(
                        "quaternion must be finite and non-zero".into(),
                    ));
                }
                Value::Quat(q.normalize())
            }
            (FieldType::Color, Value::Color(c)) => {
                if ![c.r, c.g, c.b, c.a].iter().all(|x| x.is_finite()) {
                    return Err(ValueError::Invalid("color must be finite".into()));
                }
                Value::Color(c)
            }
            (FieldType::String, v @ Value::Str(_)) => v,
            (FieldType::Entity, v @ Value::Entity(_)) => v,
            (FieldType::List(item), Value::List(items)) => Value::List(
                items
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| item.coerce(v).map_err(|e| at(format!("[{i}]"), e)))
                    .collect::<Result<_, _>>()?,
            ),
            (FieldType::Map, Value::Map(map)) => Value::Map(
                map.into_iter()
                    .map(|(k, v)| {
                        let v = coerce_dynamic(v, false).map_err(|e| at(format!("[{k:?}]"), e))?;
                        Ok((k, v))
                    })
                    .collect::<Result<_, ValueError>>()?,
            ),
            (FieldType::Record(id), Value::Record(found, values)) => {
                let schema = id.schema();
                if found != *id {
                    return Err(ValueError::TypeMismatch {
                        expected: self.describe(),
                        found: format!("record {}", found.name()),
                    });
                }
                if values.len() != schema.fields.len() {
                    return Err(ValueError::Invalid(format!(
                        "record {} has {} fields, got {} values",
                        schema.name,
                        schema.fields.len(),
                        values.len()
                    )));
                }
                Value::Record(
                    *id,
                    schema
                        .fields
                        .iter()
                        .zip(values)
                        .map(|(f, v)| f.ty.coerce(v).map_err(|e| at(format!(".{}", f.name), e)))
                        .collect::<Result<_, _>>()?,
                )
            }
            (FieldType::Enum(variants), Value::Enum(name))
            | (FieldType::Enum(variants), Value::Str(name)) => {
                if !variants.contains(&name.as_str()) {
                    return Err(ValueError::UnknownVariant {
                        found: name,
                        expected: variants,
                    });
                }
                Value::Enum(name)
            }
            (_, v) => return Err(mismatch(&v)),
        })
    }

    /// A description for messages: the name, the variants of an enum, the
    /// item type of a list or the name of a record (`"list of f32"`).
    pub fn describe(&self) -> String {
        match self {
            FieldType::Enum(v) => format!("one of {}", v.join(" | ")),
            FieldType::List(item) => format!("list of {}", item.describe()),
            FieldType::Record(id) => format!("record {}", id.name()),
            other => other.name().to_string(),
        }
    }
}

/// Prefixes an error with where in a container it happened (`[2]`, `["k"]`,
/// `.x`), joining nested locations into one path.
pub(crate) fn at(path: String, e: ValueError) -> ValueError {
    match e {
        ValueError::At { path: inner, error } => ValueError::At {
            path: path + &inner,
            error,
        },
        e => ValueError::At {
            path,
            error: Box::new(e),
        },
    }
}

/// Coerces a value stored in a map (or in a list inside a map), where no
/// field type says what it should be:
/// * integers become floats (Luau numbers are doubles, so a map written from a
///   script and one read from a file compare equal);
/// * floats must be finite, vectors and colours finite, quaternions are
///   normalised, records are coerced to their schema;
/// * a list's items follow these rules but may not be lists themselves;
/// * maps, enum variants (which Luau sees as strings) and entity references
///   are refused.
pub fn coerce_dynamic(value: Value, in_list: bool) -> Result<Value, ValueError> {
    Ok(match value {
        v @ (Value::Bool(_) | Value::Str(_)) => v,
        // Luau sees an enum variant as a plain string, so a map holding one
        // would not survive a script round trip; store the name as a string.
        Value::Enum(_) => {
            return Err(ValueError::Invalid(
                "a map cannot hold an enum variant; store its name as a string".into(),
            ));
        }
        Value::Int(i) => Value::Float(i as f64),
        Value::Float(f) => FieldType::F64.coerce(Value::Float(f))?,
        v @ Value::Vec3(_) => FieldType::Vec3.coerce(v)?,
        v @ Value::Quat(_) => FieldType::Quat.coerce(v)?,
        v @ Value::Color(_) => FieldType::Color.coerce(v)?,
        Value::Record(id, values) => FieldType::Record(id).coerce(Value::Record(id, values))?,
        Value::List(items) if !in_list => Value::List(
            items
                .into_iter()
                .enumerate()
                .map(|(i, v)| coerce_dynamic(v, true).map_err(|e| at(format!("[{i}]"), e)))
                .collect::<Result<_, _>>()?,
        ),
        Value::List(_) => {
            return Err(ValueError::Invalid(
                "a list inside a map cannot hold another list".into(),
            ));
        }
        Value::Map(_) => {
            return Err(ValueError::Invalid("a map cannot hold another map".into()));
        }
        Value::Entity(_) => {
            return Err(ValueError::Invalid(
                "a map cannot hold an entity reference".into(),
            ));
        }
    })
}

/// Interns a list of enum variant names, so that a script-defined enum field
/// can use [`FieldType::Enum`]'s `&'static` form. Each distinct list is
/// allocated once per process.
pub fn intern_variants(variants: &[String]) -> &'static [&'static str] {
    static TABLE: OnceLock<Mutex<HashSet<&'static [&'static str]>>> = OnceLock::new();
    let table = TABLE.get_or_init(|| Mutex::new(HashSet::new()));
    let mut table = table.lock().unwrap_or_else(|e| e.into_inner());
    let wanted: Vec<&str> = variants.iter().map(String::as_str).collect();
    if let Some(found) = table.iter().find(|v| **v == wanted.as_slice()) {
        return found;
    }
    let leaked: Vec<&'static str> = variants
        .iter()
        .map(|v| &*Box::leak(v.clone().into_boxed_str()))
        .collect();
    let leaked: &'static [&'static str] = Box::leak(leaked.into_boxed_slice());
    table.insert(leaked);
    leaked
}

/// A dynamically typed field value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// A `bool` field.
    Bool(bool),
    /// An `i32` or `i64` field.
    Int(i64),
    /// An `f32` or `f64` field. Every float travels as `f64`; `f32` fields round
    /// on the way in.
    Float(f64),
    /// A `vec3` field.
    Vec3(Vec3),
    /// A `quat` field.
    Quat(Quat),
    /// A `color` field.
    Color(Color),
    /// A `string` field.
    Str(String),
    /// An `entity` field (`None` is null).
    Entity(Option<Entity>),
    /// An enum field, by variant name.
    Enum(String),
    /// A `list` field.
    List(Vec<Value>),
    /// A `map` field, ordered by key.
    Map(BTreeMap<String, Value>),
    /// A `record` field: the record type and one value per record field.
    Record(RecordId, Vec<Value>),
}

/// What changed between two values ([`Value::diff`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueChange {
    /// The value changed as a whole (a scalar, or a different kind or record type).
    Whole,
    /// List indices whose item changed, was added or was removed, ascending.
    Indices(Vec<usize>),
    /// Map keys whose value changed, was added or was removed, sorted.
    Keys(Vec<String>),
    /// Record field indices whose value changed, ascending.
    Fields(Vec<usize>),
}

impl Value {
    /// A short description of the value's kind, for error messages.
    pub fn describe(&self) -> String {
        match self {
            Value::Bool(_) => "bool".into(),
            Value::Int(_) => "integer".into(),
            Value::Float(_) => "number".into(),
            Value::Vec3(_) => "vec3".into(),
            Value::Quat(_) => "quat".into(),
            Value::Color(_) => "color".into(),
            Value::Str(_) => "string".into(),
            Value::Entity(_) => "entity".into(),
            Value::Enum(v) => format!("enum variant {v:?}"),
            Value::List(items) => format!("list of {}", items.len()),
            Value::Map(_) => "map".into(),
            Value::Record(id, _) => format!("record {}", id.name()),
        }
    }

    /// What changed from `self` to `new`, or `None` if they are equal. Lists
    /// report indices, maps keys and records fields; anything else changes
    /// as a whole.
    pub fn diff(&self, new: &Value) -> Option<ValueChange> {
        if self == new {
            return None;
        }
        Some(match (self, new) {
            (Value::List(a), Value::List(b)) => ValueChange::Indices(
                (0..a.len().max(b.len()))
                    .filter(|&i| a.get(i) != b.get(i))
                    .collect(),
            ),
            (Value::Map(a), Value::Map(b)) => {
                let mut keys: Vec<String> = a
                    .iter()
                    .filter(|(k, v)| b.get(*k) != Some(v))
                    .map(|(k, _)| k.clone())
                    .chain(b.keys().filter(|k| !a.contains_key(*k)).cloned())
                    .collect();
                keys.sort();
                ValueChange::Keys(keys)
            }
            (Value::Record(ia, a), Value::Record(ib, b)) if ia == ib && a.len() == b.len() => {
                ValueChange::Fields((0..a.len()).filter(|&i| a[i] != b[i]).collect())
            }
            _ => ValueChange::Whole,
        })
    }
}

/// Why a value does not fit a field type. Wrapped in
/// [`ReflectError::Value`](crate::ReflectError::Value) with the field's path.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ValueError {
    /// The value has the wrong kind.
    #[error("expected {expected}, found {found}")]
    TypeMismatch {
        /// What the field accepts.
        expected: String,
        /// What was given.
        found: String,
    },
    /// The number does not fit the field type.
    #[error("{value} is out of range for {ty}")]
    OutOfRange {
        /// The field type (`"f32"` or `"i32"`).
        ty: &'static str,
        /// The number, as text.
        value: String,
    },
    /// A non-integral float for an integer field.
    #[error("expected an integer, found {0}")]
    NotAnInteger(f64),
    /// NaN or an infinity.
    #[error("expected a finite number, found {0}")]
    NotFinite(f64),
    /// An enum name that is not a variant.
    #[error("unknown variant {found:?}, expected one of {}", expected.join(", "))]
    UnknownVariant {
        /// The name given.
        found: String,
        /// The valid variants.
        expected: &'static [&'static str],
    },
    /// The value is the right kind but not valid.
    #[error("{0}")]
    Invalid(String),
    /// An error inside a container value, at `path` (`[2]`, `["key"]`, `.x`,
    /// or a chain of them).
    #[error("at {path}: {error}")]
    At {
        /// Where in the container.
        path: String,
        /// What is wrong there.
        error: Box<ValueError>,
    },
}

impl ValueError {
    /// The diagnostic code of this kind of error.
    pub fn code(&self) -> Code {
        match self {
            ValueError::TypeMismatch { .. } => codes::TYPE_MISMATCH,
            ValueError::OutOfRange { .. } => codes::OUT_OF_RANGE,
            ValueError::NotAnInteger(_) => codes::NOT_AN_INTEGER,
            ValueError::NotFinite(_) => codes::NON_FINITE,
            ValueError::UnknownVariant { .. } => codes::UNKNOWN_VARIANT,
            ValueError::Invalid(_) => codes::INVALID_VALUE,
            ValueError::At { error, .. } => error.code(),
        }
    }

    /// This error as a [`Diagnostic`] (without a field path).
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::error(self.code(), self.to_string())
    }
}

/// Conversion between a Rust field type and [`Value`]; implemented for every
/// type a [`component!`](crate::component) field may have.
///
/// `from_value` receives a value already coerced by [`FieldType::coerce`], so
/// implementations only need to unwrap the matching variant.
pub trait FieldValue: Sized {
    /// The field type this Rust type maps to.
    fn field_type() -> FieldType;
    /// The value of `self`.
    fn to_value(&self) -> Value;
    /// The Rust value of an already-coerced `value`; `None` if the variant does not match.
    fn from_value(value: &Value) -> Option<Self>;
}

macro_rules! simple_field {
    ($ty:ty, $ft:expr, $variant:ident, |$s:ident| $to:expr, |$v:ident| $from:expr) => {
        impl FieldValue for $ty {
            fn field_type() -> FieldType {
                $ft
            }
            fn to_value(&self) -> Value {
                let $s = self;
                Value::$variant($to)
            }
            fn from_value(value: &Value) -> Option<Self> {
                match value {
                    Value::$variant($v) => Some($from),
                    _ => None,
                }
            }
        }
    };
}

simple_field!(bool, FieldType::Bool, Bool, |s| *s, |v| *v);
simple_field!(i32, FieldType::I32, Int, |s| *s as i64, |v| *v as i32);
simple_field!(i64, FieldType::I64, Int, |s| *s, |v| *v);
simple_field!(f32, FieldType::F32, Float, |s| *s as f64, |v| *v as f32);
simple_field!(f64, FieldType::F64, Float, |s| *s, |v| *v);
simple_field!(Vec3, FieldType::Vec3, Vec3, |s| *s, |v| *v);
simple_field!(Quat, FieldType::Quat, Quat, |s| *s, |v| *v);
simple_field!(Color, FieldType::Color, Color, |s| *s, |v| *v);
simple_field!(String, FieldType::String, Str, |s| s.clone(), |v| v.clone());
simple_field!(Option<Entity>, FieldType::Entity, Entity, |s| *s, |v| *v);
simple_field!(
    BTreeMap<String, Value>,
    FieldType::Map,
    Map,
    |s| s.clone(),
    |v| v.clone()
);

/// A list field. The item type must be a scalar or record type
/// ([`FieldType::validate`]); registration checks it.
impl<T: FieldValue> FieldValue for Vec<T> {
    fn field_type() -> FieldType {
        FieldType::List(Box::new(T::field_type()))
    }
    fn to_value(&self) -> Value {
        Value::List(self.iter().map(FieldValue::to_value).collect())
    }
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::List(items) => items.iter().map(T::from_value).collect(),
            _ => None,
        }
    }
}

/// Declares a unit-only enum usable as a component field.
///
/// ```
/// re_reflect::field_enum! {
///     /// How a physics body moves.
///     pub enum BodyKind { Static, Dynamic, Kinematic }
/// }
/// assert_eq!(BodyKind::VARIANTS, ["Static", "Dynamic", "Kinematic"]);
/// assert_eq!(BodyKind::Dynamic.as_str(), "Dynamic");
/// ```
///
/// The first variant is the Rust type's `Default`; a component field still
/// names its own default explicitly.
#[macro_export]
macro_rules! field_enum {
    ($(#[doc = $doc:literal])* $vis:vis enum $name:ident { $first:ident $(, $rest:ident)* $(,)? }) => {
        $(#[doc = $doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
        #[allow(missing_docs, reason = "the caller names the variants")]
        $vis enum $name { #[default] $first, $($rest),* }

        impl $name {
            /// The variant names, in declaration order.
            pub const VARIANTS: &'static [&'static str] = &[stringify!($first) $(, stringify!($rest))*];

            /// The variant's name.
            pub fn as_str(&self) -> &'static str {
                match self { $name::$first => stringify!($first), $($name::$rest => stringify!($rest)),* }
            }
        }

        impl $crate::value::FieldValue for $name {
            fn field_type() -> $crate::value::FieldType {
                $crate::value::FieldType::Enum(Self::VARIANTS)
            }
            fn to_value(&self) -> $crate::value::Value {
                $crate::value::Value::Enum(self.as_str().to_string())
            }
            fn from_value(value: &$crate::value::Value) -> Option<Self> {
                match value {
                    $crate::value::Value::Enum(s) => match s.as_str() {
                        stringify!($first) => Some($name::$first),
                        $(stringify!($rest) => Some($name::$rest),)*
                        _ => None,
                    },
                    _ => None,
                }
            }
        }
    };
}
