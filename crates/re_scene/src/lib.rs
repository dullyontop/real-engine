//! `re_scene`: the World facade of Real Engine over bevy_ecs 0.20 (ADR-0005).
//!
//! * [`world`]: [`World`] `{ ecs, registry, generators }` with by-name access
//!   ([`World::get`], [`World::set`]), [`World::find`], regeneration,
//!   propagation, [`World::describe`], [`World::scene_json_schema`],
//!   [`World::suppress`], [`World::despawn_runtime`], [`World::state_hash`]
//!   and [`World::fork`];
//! * [`ids`]: [`StableId`] (hook-maintained [`IdIndex`]) and [`IdGen`];
//! * [`hierarchy`]: helpers over bevy's `ChildOf` / `Children`, cycle checks
//!   and transform propagation;
//! * [`components`]: the built-in components of ARCHITECTURE §4;
//! * [`attributes`]: [`Attributes`] and the [`AttributeChanged`] events;
//! * [`journal`]: the [`ChangeJournal`] of what changed, for readers that
//!   keep state per entity;
//! * [`tags`]: [`Tags`], the hook-maintained [`TagIndex`] and the
//!   [`TagChanged`] events;
//! * [`scene`]: [`SceneDoc`] load and save, JSON text, `migrate`;
//! * [`prefab`]: [`PrefabFile`] and its instantiation into a fragment;
//! * [`docs`]: open documents (project files edited in memory by `set_doc`),
//!   their revisions, dirty flags and validators (ADR-0044);
//! * [`procedural`]: procedural sources, generated entities, overrides,
//!   suppression and the regeneration diff (ADR-0017);
//! * [`error`]: [`SceneError`], with codes `RE1200`–`RE1999`.
//!
//! This is a frozen contract from WP0 PR "entity core" (docs/M1_PLAN.md).
//!
//! # Using bevy from other crates
//!
//! Only `re_reflect`, `re_scene` and `re_command` depend on bevy_ecs
//! (`cargo xtask check-graph` enforces it). Other crates reach it through
//! [`ecs`], a re-export of `bevy_ecs`, and write typed queries against
//! [`World::ecs`].
//!
//! bevy's `#[derive(Component)]` and `#[derive(Resource)]` expand to a bare
//! `bevy_ecs::...` path when the deriving crate does not depend on bevy_ecs.
//! Such a crate therefore puts
//!
//! ```ignore
//! use re_scene::ecs as bevy_ecs;
//! ```
//!
//! at its crate root, and in every module that uses the derives (a `use` is
//! visible only in its own module). `tools/derive_alias_check` proves this
//! compiles. The same applies to [`re_reflect::component!`], which derives
//! `Component`.

#![warn(missing_docs)]

pub mod attributes;
pub mod codes;
pub mod components;
pub mod docs;
pub mod error;
mod hash;
pub mod hierarchy;
pub mod ids;
pub mod journal;
pub mod prefab;
pub mod procedural;
pub mod scene;
pub mod stream;
pub mod structure;
pub mod tags;
pub mod world;

/// bevy_ecs 0.20.0, re-exported for typed access from crates that must not
/// depend on it directly.
pub use bevy_ecs as ecs;
/// The runtime entity handle (index plus generation). Never written down;
/// `re_types::EntityId` carries its `to_bits()`.
pub use bevy_ecs::entity::Entity;

pub use attributes::{AttributeChanged, AttributeFeed, Attributes};
pub use components::{
    AnimPlayer, AudioSource, BodyKind, Camera, CharacterController, Collider, ColliderShape,
    GlobalTransform, HudAnchor, HudText, Light, LightKind, MaterialRef, MeshRef, Name, Ragdoll,
    RagdollMode, RigidBody, ScriptContext, ScriptRef, SkinnedMesh, Transform, Vehicle, VehicleKind,
    VehicleWheel, Visible, WheelTester, register_builtins,
};
pub use docs::{DocMarks, DocValidator, OpenDoc, OpenDocs, doc_path};
pub use error::SceneError;
pub use ids::{IdGen, IdIndex, StableId, validate_id};
pub use journal::{ChangeJournal, note_moved};
pub use prefab::{PREFAB_FORMAT, PrefabFile};
pub use procedural::{
    FingerprintFn, GenContext, GenNode, Generated, GeneratorFn, GeneratorInfo, Generators,
    GraphSource, Orphan, OverrideDoc, Overrides, Procedural, RegenReport, SceneFragment,
    Suppressions,
};
pub use scene::{EntityDoc, GeneratedDoc, SaveOptions, SceneDoc, Snapshot};
pub use stream::{
    LoadedCell, Stash, StashedRoot, StreamCell, StreamFocus, Streamed, TerrainChunk, cell_key,
    chunk_key, parse_cell_key, parse_chunk_key,
};
pub use tags::{TagChanged, TagFeed, TagIndex, Tags};
pub use world::World;

/// Converts a runtime entity to the contract-layer id (`Entity::to_bits`).
pub fn entity_id(entity: Entity) -> re_types::EntityId {
    re_types::EntityId(entity.to_bits())
}

/// Converts a contract-layer id back to an entity, if the bits are valid.
pub fn entity_from_id(id: re_types::EntityId) -> Option<Entity> {
    Entity::try_from_bits(id.0)
}

use re_core::DiagInfo;

/// Every diagnostic code `re_scene` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::UNKNOWN_ID,
        title: "Unknown entity id",
        page: include_str!("../explain/RE1200.md"),
    },
    DiagInfo {
        code: codes::DUPLICATE_ID,
        title: "Duplicate entity id",
        page: include_str!("../explain/RE1201.md"),
    },
    DiagInfo {
        code: codes::INVALID_ID,
        title: "Invalid entity id",
        page: include_str!("../explain/RE1202.md"),
    },
    DiagInfo {
        code: codes::HIERARCHY_CYCLE,
        title: "Hierarchy cycle",
        page: include_str!("../explain/RE1203.md"),
    },
    DiagInfo {
        code: codes::PARSE_ERROR,
        title: "Scene or prefab file does not parse",
        page: include_str!("../explain/RE1204.md"),
    },
    DiagInfo {
        code: codes::FORWARD_PARENT,
        title: "Parent not defined before its child",
        page: include_str!("../explain/RE1205.md"),
    },
    DiagInfo {
        code: codes::UNSUPPORTED_FORMAT,
        title: "Unsupported file format or version",
        page: include_str!("../explain/RE1206.md"),
    },
    DiagInfo {
        code: codes::GENERATOR_FAILED,
        title: "Generator failed",
        page: include_str!("../explain/RE1207.md"),
    },
    DiagInfo {
        code: codes::SCRIPT_COMPONENT_MISMATCH,
        title: "Script component schema mismatch",
        page: include_str!("../explain/RE1208.md"),
    },
    DiagInfo {
        code: codes::OVERRIDE_NOT_APPLIED,
        title: "Override not applied",
        page: include_str!("../explain/RE1209.md"),
    },
    DiagInfo {
        code: codes::NOT_GENERATED,
        title: "Not a generated entity",
        page: include_str!("../explain/RE1210.md"),
    },
    DiagInfo {
        code: codes::INVALID_CONTENT,
        title: "Unsupported scene content",
        page: include_str!("../explain/RE1211.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 1200..=1999;
