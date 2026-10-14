//! The diagnostic codes `re_reflect` owns (range `RE1000`–`RE1199`).
//!
//! Each code has an explain page in `crates/re_reflect/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// A component name is not registered.
pub const UNKNOWN_COMPONENT: Code = Code::new("RE1001");

/// A component has no field of the given name.
pub const UNKNOWN_FIELD: Code = Code::new("RE1002");

/// A value has the wrong type for its field (for example a string for an `f32`).
pub const TYPE_MISMATCH: Code = Code::new("RE1003");

/// A number does not fit its field type (an `f32` beyond `f32::MAX`, an `i32` beyond its range).
pub const OUT_OF_RANGE: Code = Code::new("RE1004");

/// A float with a fractional part was given for an integer field.
pub const NOT_AN_INTEGER: Code = Code::new("RE1005");

/// A NaN or infinite float was given for a float field.
pub const NON_FINITE: Code = Code::new("RE1006");

/// An enum field was given a name that is not one of its variants.
pub const UNKNOWN_VARIANT: Code = Code::new("RE1007");

/// A component type with this name is already registered.
pub const DUPLICATE_COMPONENT: Code = Code::new("RE1008");

/// A value is the right type but not valid (a zero quaternion, an unknown entity id).
pub const INVALID_VALUE: Code = Code::new("RE1009");

/// The entity does not have the component that was read or written.
pub const MISSING_COMPONENT: Code = Code::new("RE1010");

/// A component schema is malformed (a name that is not an identifier, a wrong
/// number of field values, an unknown field type).
pub const INVALID_SCHEMA: Code = Code::new("RE1011");
