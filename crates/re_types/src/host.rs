//! What scripts may ask of the runtime.
//!
//! `re_script` holds a `&mut dyn HostServices` while it runs script callbacks, and
//! the native Luau API calls through it. `re_app` implements the trait and owns its
//! error codes (`RE05xx`); `re_script` turns a [`HostError`] into a Luau error that
//! carries the code. Scripts never reach a subsystem directly.

use std::fmt;

use glam::Vec3;
use serde::Serialize;

use crate::audio::{AudioCmd, BusLevel};
use crate::ids::EntityId;
use crate::input::{ActionFrame, MouseLock, RawInputFrame};
use crate::physics::{
    CharacterState, GroupMask, RagdollMode, RagdollState, RayHit, VehicleInput, VehicleState,
};

/// Why a [`HostServices`] call failed.
///
/// It is not a diagnostic (this crate does not depend on `re_core`); callers build
/// one from `code` and `message`. It can be serialized (for logs) but not
/// deserialized, because `code` is a `&'static str`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct HostError {
    /// An `RE####` code of the implementing crate.
    pub code: &'static str,
    /// What went wrong, in one sentence.
    pub message: String,
}

impl HostError {
    /// An error with this code and message.
    pub fn new(code: &'static str, message: impl Into<String>) -> HostError {
        HostError {
            code,
            message: message.into(),
        }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for HostError {}

/// The result of a [`HostServices`] call.
pub type HostResult<T> = Result<T, HostError>;

/// The runtime services available to scripts.
///
/// Calls that change the world take effect at a defined point of the frame chosen
/// by the implementation (for example, impulses and character moves are queued as
/// physics commands for the next fixed step).
pub trait HostServices {
    /// Casts a ray from `origin` along `dir` (normalised by the implementation) up to
    /// `max_dist`, against bodies whose groups intersect `mask`. Returns the closest hit.
    fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32, mask: GroupMask) -> Option<RayHit>;

    /// Applies an impulse (N·s) to a dynamic body at its centre of mass.
    fn apply_impulse(&mut self, entity: EntityId, impulse: Vec3) -> HostResult<()>;

    /// Sets a character's desired velocity for the next step, and asks it to jump
    /// when `jump` is true and it is on the ground.
    fn character_move(
        &mut self,
        entity: EntityId,
        desired_velocity: Vec3,
        jump: bool,
    ) -> HostResult<()>;

    /// A character's state after the last step.
    fn character_state(&self, entity: EntityId) -> HostResult<CharacterState>;

    /// A body's velocities after the last step: `(linear, angular)`.
    fn body_velocity(&self, entity: EntityId) -> HostResult<(Vec3, Vec3)>;

    /// Sets the driver input of the vehicle on `entity` from the next step
    /// until the next call (ADR-0040).
    fn vehicle_drive(&mut self, entity: EntityId, input: VehicleInput) -> HostResult<()>;

    /// The state of the vehicle on `entity` after the last step.
    fn vehicle_state(&self, entity: EntityId) -> HostResult<VehicleState>;

    /// Switches the ragdoll on `entity` to `mode` from the next step (ADR-0040); it
    /// holds until the next call or an edit of the entity's ragdoll settings.
    fn ragdoll_mode(&mut self, entity: EntityId, mode: RagdollMode) -> HostResult<()>;

    /// The state of the ragdoll on `entity` after the last step.
    fn ragdoll_state(&self, entity: EntityId) -> HostResult<RagdollState>;

    /// Applies an impulse (N·s) at the next step to the part of the ragdoll on
    /// `entity` that follows `joint`, or the joint's nearest ancestor with a part.
    fn ragdoll_impulse(&mut self, entity: EntityId, joint: &str, impulse: Vec3) -> HostResult<()>;

    /// Plays a sound clip (an asset path) once, at a world position or non-spatially.
    fn play_sound(&mut self, clip: &str, at: Option<Vec3>, volume: f32) -> HostResult<()>;

    /// Queues a mixer command for this frame (ADR-0041): one of
    /// [`AudioCmd::PlayOneShotOnBus`], [`AudioCmd::SetBusVolume`],
    /// [`AudioCmd::SetEffectParam`] and [`AudioCmd::ApplySnapshot`]. The
    /// implementation refuses any other command.
    fn audio_mix(&mut self, cmd: AudioCmd) -> HostResult<()>;

    /// A bus's or send's level over the last frame.
    fn audio_level(&self, bus: &str) -> HostResult<BusLevel>;

    /// The state of every input action this frame.
    fn actions(&self) -> &ActionFrame;

    /// Sets the text of an entity's `HudText` component.
    fn set_hud_text(&mut self, entity: EntityId, text: &str) -> HostResult<()>;

    /// Asks for a screenshot of the current frame, written to `path` (relative to
    /// the project) at the end of the frame.
    fn request_screenshot(&mut self, path: &str) -> HostResult<()>;

    /// Regenerates the procedural source with stable id `source_id` using `seed`.
    fn regenerate(&mut self, source_id: &str, seed: i64) -> HostResult<()>;

    /// Asks the runtime to restart the game (reload the entry scene) at the end of
    /// the frame.
    fn request_restart(&mut self);

    /// This frame's raw device state and events, for layers that read keys
    /// rather than actions. `None` when the host has no raw input (the
    /// default). Implemented by `re_app`'s host (WP16) from `re_input`.
    fn raw_input(&self) -> Option<&RawInputFrame> {
        None
    }

    /// Asks the window to hold or free the cursor and to show or hide it.
    /// The default ignores it: a host without a window has no cursor.
    /// Implemented by `re_app`'s host (WP16).
    fn set_mouse_behavior(&mut self, lock: MouseLock, icon_visible: bool) -> HostResult<()> {
        let _ = (lock, icon_visible);
        Ok(())
    }
}
