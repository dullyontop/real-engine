//! Dynamic values: the one currency that scripts, the editor, scene files and
//! agents use to read and write component fields (ADR-0032).
//!
//! The set of field types is deliberately closed and flat (no nested structs).
//! It maps one-to-one onto Luau values, JSON values and Roblox-style typed
//! properties, which keeps every consumer simple.
//!
//! Coercion rules ([`FieldType::coerce`]):
//! * an integer is accepted for a float field;
//! * an integral float is accepted for an integer field;
//! * an `f64` is rounded to `f32` for an `f32` field, with a range check;
//! * non-finite floats are rejected;
//! * quaternions are normalised (and must be finite and non-zero);
//! * enum names are checked against the variants.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use bevy_ecs::entity::Entity;
use glam::{Quat, Vec3};
use re_core::{Code, Diagnostic};

use crate::codes;

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
        }
    }

    /// The type named `kind` (the inverse of [`FieldType::name`]); `variants` is
    /// used only for `"enum"`. Returns `None` for an unknown kind or an enum
    /// without variants.
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

    fn describe(&self) -> String {
        match self {
            FieldType::Enum(v) => format!("one of {}", v.join(" | ")),
            other => other.name().to_string(),
        }
    }
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
        }
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
        #[allow(missing_docs)]
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
