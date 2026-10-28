//! `re_reflect`: the component registry of Real Engine (ADR-0032).
//!
//! Commands, the inspector, Luau, agents and scene files all read and write
//! component fields by name. This crate gives every registered bevy_ecs
//! component a [`ComponentSchema`] (name, doc, origin and typed fields with
//! docs and defaults), and reads and writes fields through it:
//!
//! * [`value`]: the closed set of [`FieldType`]s, the dynamic [`Value`], the
//!   coercion rules, [`Value::diff`], [`Color`], [`FieldValue`] and the
//!   [`field_enum!`] macro;
//! * [`record`]: [`RecordSchema`] and [`RecordId`], the registered record
//!   types behind [`FieldType::Record`];
//! * [`registry`]: [`Registry`] (native components through [`component!`],
//!   script components through [`Registry::register_script`], by-name access,
//!   equality-checked writes, [`Registry::references_to`]);
//! * [`doc`]: [`Doc`], the untyped tree used at every text boundary, and
//!   [`f32_from_parsed_f64`], the exact `f32` text round trip;
//! * [`schema`]: [`Registry::describe`], [`Registry::scene_json_schema`] and
//!   the file form of script component schemas;
//! * [`error`]: [`ReflectError`], whose variants carry the stable codes
//!   `RE1000`–`RE1199` and convert to [`re_core::Diagnostic`].
//!
//! This is a frozen contract from WP0 PR "entity core" (docs/M1_PLAN.md).

#![warn(missing_docs)]

pub mod codes;
pub mod doc;
pub mod error;
pub mod record;
pub mod registry;
pub mod schema;
pub mod value;

pub use doc::{Doc, f32_from_parsed_f64};
pub use error::{ReflectError, suggest};
/// `#[derive(Reflect)]`, the derive form of [`component!`]: field docs from
/// `///`, defaults from the type's `Default`, `#[reflect(skip)]` for fields that
/// are not data.
pub use re_reflect_derive::Reflect;
pub use record::{RecordField, RecordId, RecordSchema};
pub use registry::{
    ChangeHook, ComponentKey, ComponentOrigin, ComponentSchema, FieldHint, FieldSchema, Reflect,
    Registry, WriteMode, is_identifier,
};
pub use schema::{ComponentSchemaDoc, FieldSchemaDoc, FieldTypeDoc};
pub use value::{Color, FieldType, FieldValue, Value, ValueChange, ValueError, coerce_dynamic};

use re_core::DiagInfo;

/// Every diagnostic code `re_reflect` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::UNKNOWN_COMPONENT,
        title: "Unknown component",
        page: include_str!("../explain/RE1001.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_FIELD,
        title: "Unknown field",
        page: include_str!("../explain/RE1002.md"),
    },
    DiagInfo {
        code: codes::TYPE_MISMATCH,
        title: "Value has the wrong type for its field",
        page: include_str!("../explain/RE1003.md"),
    },
    DiagInfo {
        code: codes::OUT_OF_RANGE,
        title: "Number out of range for its field",
        page: include_str!("../explain/RE1004.md"),
    },
    DiagInfo {
        code: codes::NOT_AN_INTEGER,
        title: "Fractional number for an integer field",
        page: include_str!("../explain/RE1005.md"),
    },
    DiagInfo {
        code: codes::NON_FINITE,
        title: "Non-finite number",
        page: include_str!("../explain/RE1006.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_VARIANT,
        title: "Unknown enum variant",
        page: include_str!("../explain/RE1007.md"),
    },
    DiagInfo {
        code: codes::DUPLICATE_COMPONENT,
        title: "Component name already registered",
        page: include_str!("../explain/RE1008.md"),
    },
    DiagInfo {
        code: codes::INVALID_VALUE,
        title: "Invalid value",
        page: include_str!("../explain/RE1009.md"),
    },
    DiagInfo {
        code: codes::MISSING_COMPONENT,
        title: "Entity lacks the component",
        page: include_str!("../explain/RE1010.md"),
    },
    DiagInfo {
        code: codes::INVALID_SCHEMA,
        title: "Malformed component schema",
        page: include_str!("../explain/RE1011.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 1000..=1199;

#[doc(hidden)]
pub mod __private {
    //! Re-exports used by [`component!`](crate::component); not public API.
    pub use bevy_ecs::component::Component;
}
