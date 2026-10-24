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
//! | [`render`] | [`RenderSnapshot`], [`Camera`], [`Light`], [`DrawItem`], [`SkinnedDrawItem`], [`DrawFlags`], [`DebugLine`], [`MeshHandle`], [`MaterialHandle`], [`TextureHandle`], [`TextureRef`], [`MaterialDesc`], [`TextureData`] |
//! | [`mesh`] | [`MeshData`], [`Vertex`], [`Aabb`], [`SkinWeights`] |
//! | [`anim`] | [`ClipData`], [`TargetChannels`], [`Channel`], [`SkinData`], [`JointDesc`], [`Trs`] |
//! | [`physics`] | [`PhysicsCmd`], [`BodyDesc`], [`BodyKind`], [`ShapeDesc`], [`Layer`], [`GroupMask`], [`BodyPose`], [`ContactEvent`], [`RayHit`], [`CharacterDesc`], [`CharacterState`], [`ConstraintDesc`], [`ConstraintKind`], [`VehicleDesc`], [`VehicleKind`], [`WheelDesc`], [`WheelTester`], [`EngineDesc`], [`TransmissionDesc`], [`VehicleInput`], [`VehicleState`], [`WheelState`], [`WheelPose`] |
//! | [`audio`] | [`AudioCmd`], [`AudioGraphDesc`], [`EffectDesc`], [`EffectParam`], [`BusLevel`] |
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

pub use anim::{Channel, ClipData, Interpolation, JointDesc, SkinData, TargetChannels, Trs};
pub use audio::{AudioCmd, AudioGraphDesc, BusLevel, EffectDesc, EffectParam};
pub use graph::{EdgeDoc, GraphDoc, GraphDocError, NodeDoc};
pub use host::{HostError, HostResult, HostServices};
pub use ids::EntityId;
pub use input::{
    ActionFrame, ButtonState, InputEvent, InputKind, InputPhase, MouseLock, RawInputFrame,
};
pub use mesh::{Aabb, MeshData, SkinWeights, Vertex};
pub use physics::{
    BodyDesc, BodyKind, BodyPose, CharacterDesc, CharacterState, ConstraintDesc, ConstraintKind,
    ContactEvent, EngineDesc, GroundState, GroupMask, JointPose, Layer, PhysicsCmd, RagdollBlend,
    RagdollConstraint, RagdollDesc, RagdollMode, RagdollPartDesc, RagdollSkeleton, RagdollState,
    RayHit, ShapeDesc, SkeletonJoint, TransmissionDesc, VehicleDesc, VehicleInput, VehicleKind,
    VehicleState, WheelDesc, WheelPose, WheelState, WheelTester,
};
pub use render::{
    AlphaMode, Camera, DebugLine, DrawFlags, DrawItem, Light, LightKind, MaterialDesc,
    MaterialHandle, MeshHandle, RenderSnapshot, SkinnedDrawItem, TextureData, TextureFormat,
    TextureHandle, TextureRef,
};
pub use replay::{RawReplayInput, ReplayCheck, ReplayCommand, ReplayDoc, ReplayInput};

/// Re-export of the glam version used in every contract type (glam 0.34, ADR-0028).
pub use glam;
