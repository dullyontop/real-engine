//! `re_compat`: the Roblox-style compatibility layer (ADR-0012, ARCHITECTURE
//! §4, docs/M2_PLAN.md).
//!
//! This crate maps Roblox-style classes, enums and libraries onto the native
//! entity model. It holds:
//! * the class layer ([`class`]): [`ClassRegistry`], built from data, with
//!   inheritance, `IsA`, member lookup with did-you-mean, and property access
//!   through native fields, the class's own component, or Rust accessors;
//! * the data formats ([`data`]): class, enum and library files, built into
//!   the crate from `classes/`, `enums/` and `libraries/`, and the generated
//!   coverage table `coverage.json` that the gap report reads;
//! * the scene conventions ([`scene`]): the `game` root, services, the class
//!   marker, script contexts and the `[compat]` section of `real.toml`;
//! * the datatypes ([`datatypes`]) and enums ([`enums`]): `Vector3`,
//!   `CFrame`, `Color3` and the rest, and the `Enum` tables, which
//!   [`datatypes::install`] puts into a compat VM.
//!
//! The behaviour lives in the runtime ([`runtime`]): [`runtime::CompatVm`] is
//! one `server` or `client` script context bound to one world, with the
//! instance handles ([`instance`]), signals ([`signal`]) and services
//! ([`services`]) that scripts reach through `game`. Each area registers its
//! accessors, methods, globals and value conversions in
//! [`runtime::Bindings`].

pub mod class;
pub mod codes;
pub mod data;
pub mod datatypes;
pub mod enums;
pub mod error;
pub mod instance;
pub mod runtime;
pub mod scene;
pub mod services;
pub mod signal;
pub mod sound;
pub mod tween;

pub use class::{
    Accessor, ClassDef, ClassMarker, ClassRegistry, ClassRegistryBuilder, Converter, EventDef,
    FromFieldFn, GetFn, Member, MemberRef, MethodDef, PropertyDef, SetFn, Storage, ToFieldFn,
    own_component_name,
};
pub use data::{COVERAGE_JSON, CoverageFile, DataSet, Status};
pub use enums::EnumItem;
pub use error::CompatError;
pub use runtime::{Bindings, CompatSetup, CompatVm, ValueCodec};
pub use scene::{CompatApi, CompatConfig, GAME_ID, ROOT_CLASS, ServiceTag, script_context};
pub use signal::SignalBehavior;

use re_core::DiagInfo;

/// Every diagnostic code `re_compat` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::NOT_AVAILABLE_OFFLINE,
        title: "Not available offline",
        page: include_str!("../explain/RE9002.md"),
    },
    DiagInfo {
        code: codes::INVALID_DATA,
        title: "Invalid compat data file",
        page: include_str!("../explain/RE9003.md"),
    },
    DiagInfo {
        code: codes::SCENE_CONVENTION,
        title: "Compat scene breaks the scene conventions",
        page: include_str!("../explain/RE9004.md"),
    },
    DiagInfo {
        code: codes::READ_ONLY,
        title: "Read-only property",
        page: include_str!("../explain/RE9005.md"),
    },
    DiagInfo {
        code: codes::INVALID_ATTRIBUTE,
        title: "Attribute refused",
        page: include_str!("../explain/RE9007.md"),
    },
    DiagInfo {
        code: codes::INVALID_PARENT,
        title: "Parent change refused",
        page: include_str!("../explain/RE9008.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_ASSET,
        title: "Asset id not in the substitute table",
        page: include_str!("../explain/RE9350.md"),
    },
    DiagInfo {
        code: codes::EFFECT_BUDGET,
        title: "Effect budget exceeded",
        page: include_str!("../explain/RE9351.md"),
    },
    DiagInfo {
        code: codes::NOT_TWEENABLE,
        title: "Property cannot be tweened",
        page: include_str!("../explain/RE9352.md"),
    },
    DiagInfo {
        code: codes::DEPRECATED_MEMBER,
        title: "Deprecated member used",
        page: include_str!("../explain/RE9101.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 9002..=9799;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_is_valid() {
        let problems = re_core::validate_catalog(DIAGNOSTICS, CODE_RANGE);
        assert!(problems.is_empty(), "{problems:?}");
    }
}
