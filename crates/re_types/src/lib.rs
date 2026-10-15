//! `re_types`: the plain-data contracts between Real Engine's app and its subsystems.
//!
//! Subsystems (render, physics, audio, input, assets, animation) never see the
//! scene. They take snapshots and command buffers made of the types in this crate,
//! and give back plain results (ADR-0006). Scripts reach the runtime only through
//! the [`HostServices`] trait.
//!
//! Every data type here is `Send + Clone + Serialize + Deserialize`, owns its data
//! and has no behaviour beyond small helpers, so it can cross a thread or be
//! written to a file. The crate depends only on `glam`, `serde` and `serde_json`
//! (plus `schemars` behind the optional `schemars` feature, which derives
//! `JsonSchema` for the file formats [`GraphDoc`] and [`ReplayDoc`]); in particular it does not depend on `re_core`: errors cross [`HostServices`] as
//! the plain [`HostError`], and callers turn them into diagnostics.
//!
//! One module per area, all re-exported at the crate root:
//!
//! | Module | Types |
//! |---|---|
//! | [`ids`] | [`EntityId`] |
//! | [`render`] | [`RenderSnapshot`], [`Camera`], [`Light`], [`DrawItem`], [`DrawFlags`], [`DebugLine`], [`MeshHandle`], [`MaterialHandle`], [`TextureHandle`], [`TextureRef`], [`MaterialDesc`], [`TextureData`] |
//! | [`mesh`] | [`MeshData`], [`Vertex`], [`Aabb`] |
//! | [`anim`] | [`ClipData`], [`TargetChannels`], [`Channel`] |
//! | [`physics`] | [`PhysicsCmd`], [`BodyDesc`], [`BodyKind`], [`ShapeDesc`], [`Layer`], [`GroupMask`], [`BodyPose`], [`ContactEvent`], [`RayHit`], [`CharacterDesc`], [`CharacterState`], [`ConstraintDesc`], [`ConstraintKind`] |
//! | [`audio`] | [`AudioCmd`] |
//! | [`input`] | [`ActionFrame`], [`ButtonState`], [`RawInputFrame`], [`InputEvent`], [`InputKind`], [`InputPhase`], [`MouseLock`] |
//! | [`graph`] | [`GraphDoc`], [`NodeDoc`], [`EdgeDoc`] |
//! | [`replay`] | [`ReplayDoc`] |
//! | [`host`] | [`HostServices`], [`HostError`], [`HostResult`] |
//!
//! This crate is a frozen contract (docs/M1_PLAN.md, "Cross-WP contracts"): a
//! change needs review by every work package that consumes it.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod anim;
pub mod audio;
pub mod graph;
pub mod host;
pub mod ids;
pub mod input;
pub mod mesh;
pub mod physics;
pub mod render;
pub mod replay;

pub use anim::{Channel, ClipData, Interpolation, TargetChannels};
pub use audio::AudioCmd;
pub use graph::{EdgeDoc, GraphDoc, GraphDocError, NodeDoc};
pub use host::{HostError, HostResult, HostServices};
pub use ids::EntityId;
pub use input::{
    ActionFrame, ButtonState, InputEvent, InputKind, InputPhase, MouseLock, RawInputFrame,
};
pub use mesh::{Aabb, MeshData, Vertex};
pub use physics::{
    BodyDesc, BodyKind, BodyPose, CharacterDesc, CharacterState, ConstraintDesc, ConstraintKind,
    ContactEvent, GroundState, GroupMask, Layer, PhysicsCmd, RayHit, ShapeDesc,
};
pub use render::{
    AlphaMode, Camera, DebugLine, DrawFlags, DrawItem, Light, LightKind, MaterialDesc,
    MaterialHandle, MeshHandle, RenderSnapshot, TextureData, TextureFormat, TextureHandle,
    TextureRef,
};
pub use replay::{RawReplayInput, ReplayCheck, ReplayCommand, ReplayDoc, ReplayInput};

/// Re-export of the glam version used in every contract type (glam 0.34, ADR-0028).
pub use glam;
