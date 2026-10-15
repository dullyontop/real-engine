//! Audio commands: what `re_app` asks `re_audio` to do each frame.
//!
//! Entity-driven sounds (the scene's `AudioSource` component) are keyed by
//! [`EntityId`]; one-shot sounds requested by scripts are fire-and-forget. Clip
//! names are project asset paths such as `assets/sfx/pickup.wav`.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::ids::EntityId;

/// One audio command.
///
/// Serialized with a `cmd` tag, for example `{"cmd": "stop_source", "entity": 7}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum AudioCmd {
    /// Play a clip once, not tied to an entity.
    PlayOneShot {
        /// The clip's asset path.
        clip: String,
        /// Where it plays in the world; `None` for non-spatial (UI) sounds.
        position: Option<Vec3>,
        /// Volume, linear, 1 is unchanged.
        volume: f32,
    },
    /// Start (or restart) an entity's sound.
    StartSource {
        /// The entity.
        entity: EntityId,
        /// The clip's asset path.
        clip: String,
        /// Volume, linear.
        volume: f32,
        /// Loop until stopped.
        looping: bool,
        /// Attenuate and pan by distance to the listener.
        spatial: bool,
        /// World position.
        position: Vec3,
    },
    /// Stop an entity's sound.
    StopSource {
        /// The entity.
        entity: EntityId,
    },
    /// Change an entity's volume.
    SetSourceVolume {
        /// The entity.
        entity: EntityId,
        /// Volume, linear.
        volume: f32,
    },
    /// Move an entity's sound.
    SetSourcePosition {
        /// The entity.
        entity: EntityId,
        /// World position.
        position: Vec3,
    },
    /// Move the listener (usually the active camera).
    SetListener {
        /// World position.
        position: Vec3,
        /// World rotation (the listener faces −Z).
        rotation: Quat,
    },
    /// Change an entity's playback rate (1 is normal speed; it shifts the
    /// pitch with the speed).
    SetSourceRate {
        /// The entity.
        entity: EntityId,
        /// The rate, positive.
        rate: f32,
    },
    /// Jump to a time in an entity's sound.
    SeekSource {
        /// The entity.
        entity: EntityId,
        /// Seconds from the start of the clip.
        seconds: f64,
    },
    /// Turn distance attenuation of an entity's spatial sound on or off (on by
    /// default). Sent before `StartSource`, it applies to that sound and the
    /// later ones of the entity. Off, the sound still pans by direction and
    /// its volume is only what `volume` says, so the sender can apply its own
    /// distance model.
    SetSourceAttenuation {
        /// The entity.
        entity: EntityId,
        /// True to attenuate by distance.
        enabled: bool,
    },
    /// Set the master volume.
    SetMasterVolume {
        /// Volume, linear.
        volume: f32,
    },
    /// Stop every sound (for a restart).
    StopAll,
}
