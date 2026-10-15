//! The runtime side of [`HostServices`]: what scripts may ask of the engine.
//!
//! [`Host`] borrows the parts of the [`App`](crate::App) that scripts reach while
//! a script callback runs (the World itself goes to the script runtime
//! separately). Calls that change something are queued and take effect at a
//! fixed point of the frame:
//! - impulses and character moves are physics commands for the next physics
//!   phase (the current fixed step's, when called from a fixed-step callback);
//! - sounds go to the frame's audio commands;
//! - HUD text, screenshots, regeneration and restart are applied in EndFrame.

use glam::Vec3;
use re_physics::{Physics, QueryFilter};
use re_types::host::{HostError, HostResult, HostServices};
use re_types::{
    ActionFrame, AudioCmd, BodyKind, CharacterState, EntityId, GroupMask, PhysicsCmd, RayHit,
};

use crate::codes::{HOST_INVALID_ARGUMENT_STR, HOST_NO_SUCH_THING_STR};
use crate::sync::physics::{LinkKind, PhysicsSync};

/// Requests queued by scripts (and commands) for the end of the frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pending {
    /// Reload the entry scene.
    pub restart: bool,
    /// Screenshots to write, relative to the project.
    pub screenshots: Vec<String>,
    /// Procedural sources to regenerate with a seed: `(source id, seed)`.
    pub regenerate: Vec<(String, i64)>,
    /// HUD text writes: `(entity, text)`.
    pub hud: Vec<(EntityId, String)>,
}

/// The [`HostServices`] implementation handed to the script runtime.
pub struct Host<'a> {
    /// The physics world (queries, character and body state).
    pub physics: &'a Physics,
    /// Which entity has which body.
    pub links: &'a PhysicsSync,
    /// This frame's actions.
    pub actions: &'a ActionFrame,
    /// Physics commands for the next physics phase.
    pub physics_cmds: &'a mut Vec<PhysicsCmd>,
    /// Audio commands for this frame.
    pub audio_cmds: &'a mut Vec<AudioCmd>,
    /// End-of-frame requests.
    pub pending: &'a mut Pending,
}

fn no_such(msg: String) -> HostError {
    HostError::new(HOST_NO_SUCH_THING_STR, msg)
}

fn invalid(msg: String) -> HostError {
    HostError::new(HOST_INVALID_ARGUMENT_STR, msg)
}

fn finite(v: Vec3, what: &str) -> HostResult<()> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(invalid(format!("{what} {v} is not finite")))
    }
}

impl Host<'_> {
    fn require_character(&self, entity: EntityId) -> HostResult<()> {
        match self.links.kind(entity) {
            Some(LinkKind::Character) => Ok(()),
            _ => Err(no_such(format!(
                "entity {} has no character (add a CharacterController)",
                entity.0
            ))),
        }
    }
}

impl HostServices for Host<'_> {
    fn raycast(&self, origin: Vec3, dir: Vec3, max_dist: f32, mask: GroupMask) -> Option<RayHit> {
        let filter = QueryFilter {
            groups: mask,
            ..QueryFilter::default()
        };
        // An invalid ray hits nothing.
        self.physics
            .raycast(origin, dir, max_dist, &filter)
            .ok()
            .flatten()
    }

    fn apply_impulse(&mut self, entity: EntityId, impulse: Vec3) -> HostResult<()> {
        finite(impulse, "impulse")?;
        match self.links.kind(entity) {
            Some(LinkKind::Body(BodyKind::Dynamic)) => {
                self.physics_cmds.push(PhysicsCmd::ApplyImpulse {
                    id: entity,
                    impulse,
                });
                Ok(())
            }
            Some(_) => Err(invalid(format!(
                "entity {} is not a dynamic body; impulses move dynamic bodies only",
                entity.0
            ))),
            None => Err(no_such(format!(
                "entity {} has no physics body (add a Collider and a dynamic RigidBody)",
                entity.0
            ))),
        }
    }

    fn character_move(
        &mut self,
        entity: EntityId,
        desired_velocity: Vec3,
        jump: bool,
    ) -> HostResult<()> {
        finite(desired_velocity, "velocity")?;
        self.require_character(entity)?;
        self.physics_cmds.push(PhysicsCmd::MoveCharacter {
            id: entity,
            desired_velocity,
            jump,
        });
        Ok(())
    }

    fn character_state(&self, entity: EntityId) -> HostResult<CharacterState> {
        self.require_character(entity)?;
        self.physics
            .character(entity)
            .ok_or_else(|| no_such(format!("entity {} has no character yet", entity.0)))
    }

    fn body_velocity(&self, entity: EntityId) -> HostResult<(Vec3, Vec3)> {
        self.physics
            .velocity(entity)
            .ok_or_else(|| no_such(format!("entity {} has no physics body", entity.0)))
    }

    fn play_sound(&mut self, clip: &str, at: Option<Vec3>, volume: f32) -> HostResult<()> {
        if clip.is_empty() {
            return Err(invalid("the clip path is empty".into()));
        }
        if !(volume.is_finite() && volume >= 0.0) {
            return Err(invalid(format!("volume {volume} must be 0 or more")));
        }
        if let Some(p) = at {
            finite(p, "position")?;
        }
        self.audio_cmds.push(AudioCmd::PlayOneShot {
            clip: clip.to_string(),
            position: at,
            volume,
        });
        Ok(())
    }

    fn actions(&self) -> &ActionFrame {
        self.actions
    }

    fn set_hud_text(&mut self, entity: EntityId, text: &str) -> HostResult<()> {
        self.pending.hud.push((entity, text.to_string()));
        Ok(())
    }

    fn request_screenshot(&mut self, path: &str) -> HostResult<()> {
        if path.is_empty() || path.starts_with('/') || path.split(['/', '\\']).any(|s| s == "..") {
            return Err(invalid(format!(
                "screenshot path {path:?} must be relative to the project and stay inside it"
            )));
        }
        self.pending.screenshots.push(path.to_string());
        Ok(())
    }

    fn regenerate(&mut self, source_id: &str, seed: i64) -> HostResult<()> {
        if source_id.is_empty() {
            return Err(invalid("the source id is empty".into()));
        }
        self.pending.regenerate.push((source_id.to_string(), seed));
        Ok(())
    }

    fn request_restart(&mut self) {
        self.pending.restart = true;
    }
}
