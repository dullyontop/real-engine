//! What scripts may ask of the runtime.
//!
//! `re_script` holds a `&mut dyn HostServices` while it runs script callbacks, and
//! the native Luau API calls through it. `re_app` implements the trait and owns its
//! error codes (`RE05xx`); `re_script` turns a [`HostError`] into a Luau error that
//! carries the code. Scripts never reach a subsystem directly.

use std::fmt;

use glam::{Quat, Vec3};
use serde::Serialize;

use crate::anim::AnimValue;
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

    /// Sets the linear (m/s) and angular (rad/s) velocity of the dynamic body on
    /// `entity` at the next physics phase, in place of what it had. An entity
    /// that has no body yet is accepted: one spawned this frame gets its body in
    /// that phase, before the velocity is set.
    fn set_body_velocity(
        &mut self,
        entity: EntityId,
        linear: Vec3,
        angular: Vec3,
    ) -> HostResult<()>;

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

    /// Queues a mixer or sound command for this frame (ADR-0041, ADR-0057 §8):
    /// one of [`AudioCmd::PlayOneShot`] and [`AudioCmd::PlayOneShotOnBus`]
    /// (with or without an id), [`AudioCmd::SoundControl`],
    /// [`AudioCmd::SourceControl`], [`AudioCmd::SetBusVolume`],
    /// [`AudioCmd::SetEffectParam`] and [`AudioCmd::ApplySnapshot`]. The
    /// implementation refuses any other command, and values out of range.
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

    /// Sets how fast simulation time runs against frame time, from the next
    /// fixed steps the runtime counts: 1 is normal, 0.5 half speed, and 0 runs
    /// no fixed steps (frame callbacks still run); 0 to 10. The step itself
    /// never changes, only how many run per frame. A restart sets it back to 1.
    fn set_time_scale(&mut self, scale: f64) -> HostResult<()>;

    /// The time scale in effect (1 unless [`set_time_scale`](Self::set_time_scale)
    /// changed it).
    fn time_scale(&self) -> f64;

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

    /// Sets a float or bool parameter of the animation graph the entity's
    /// `Animator` plays (ADR-0039). An unknown or mistyped parameter is an
    /// error whose message lists the graph's parameters. Implemented by
    /// `re_app`'s host (WP21).
    fn anim_set(&mut self, entity: EntityId, param: &str, value: AnimValue) -> HostResult<()>;

    /// A parameter of the entity's animation graph; a trigger reads as a bool
    /// (whether it is set).
    fn anim_get(&self, entity: EntityId, param: &str) -> HostResult<AnimValue>;

    /// Sets a trigger of the entity's animation graph, which stays set until
    /// a transition consumes it.
    fn anim_trigger(&mut self, entity: EntityId, param: &str) -> HostResult<()>;

    /// The state a layer of the entity's graph is in (or fading into): the
    /// first layer when `layer` is `None`.
    fn anim_state(&self, entity: EntityId, layer: Option<&str>) -> HostResult<String>;

    /// A joint of the entity's skin in world space: the last step's pose,
    /// placed by the entity's transform from the start of that step (so it
    /// trails that step's root motion). `(position, rotation)`.
    fn anim_joint(&self, entity: EntityId, joint: &str) -> HostResult<(Vec3, Quat)>;
}
