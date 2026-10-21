//! The runtime side of [`HostServices`]: what scripts may ask of the engine.
//!
//! [`Host`] borrows the parts of the [`App`](crate::App) that scripts reach while
//! a script callback runs (the World itself goes to the script runtime
//! separately). Calls that change something are queued and take effect at a
//! fixed point of the frame:
//! - impulses, character moves and vehicle input are physics commands for the next physics
//!   phase (the current fixed step's, when called from a fixed-step callback);
//! - sounds go to the frame's audio commands;
//! - HUD text, screenshots, regeneration and restart are applied in EndFrame.

use glam::Vec3;
use re_physics::{Physics, QueryFilter};
use re_types::host::{HostError, HostResult, HostServices};
use re_types::{
    ActionFrame, AudioCmd, BodyKind, CharacterState, EntityId, GroupMask, PhysicsCmd, RayHit,
    VehicleInput, VehicleState,
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
    /// The cursor settings scripts asked for last (`MouseBehavior`,
    /// `MouseIconEnabled`), for the window.
    pub mouse: Option<(re_types::input::MouseLock, bool)>,
}

/// The [`HostServices`] implementation handed to the script runtime.
pub struct Host<'a> {
    /// The physics world (queries, character and body state).
    pub physics: &'a Physics,
    /// Which entity has which body.
    pub links: &'a PhysicsSync,
    /// This frame's actions.
    pub actions: &'a ActionFrame,
    /// This frame's raw input (from the window, or a replay's raw frames).
    pub raw: &'a re_types::input::RawInputFrame,
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

    fn vehicle_drive(&mut self, entity: EntityId, input: VehicleInput) -> HostResult<()> {
        if !self.physics.has_vehicle(entity) {
            return Err(no_such(format!(
                "entity {} has no vehicle (add a Vehicle to a dynamic body, with VehicleWheel children)",
                entity.0
            )));
        }
        if !input.is_valid() {
            return Err(invalid(format!(
                "vehicle input {input:?} is out of range: throttle and steer are -1 to 1, \
                 brake and handbrake 0 to 1"
            )));
        }
        self.physics_cmds
            .push(PhysicsCmd::SetVehicleInput { id: entity, input });
        Ok(())
    }

    fn vehicle_state(&self, entity: EntityId) -> HostResult<VehicleState> {
        self.physics
            .vehicle(entity)
            .ok_or_else(|| no_such(format!("entity {} has no vehicle", entity.0)))
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

    fn raw_input(&self) -> Option<&re_types::input::RawInputFrame> {
        Some(self.raw)
    }

    fn set_mouse_behavior(
        &mut self,
        lock: re_types::input::MouseLock,
        icon_visible: bool,
    ) -> HostResult<()> {
        self.pending.mouse = Some((lock, icon_visible));
        Ok(())
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

/// The [`ScriptHost`](re_script::ScriptHost) of a running game: the World and
/// the [`Host`] services. Scripts get no command registry while the game runs.
pub struct PlayHost<'a> {
    /// The world scripts read and write.
    pub world: &'a mut re_scene::World,
    /// The services.
    pub services: Host<'a>,
}

impl re_script::ScriptHost for PlayHost<'_> {
    fn world(&mut self) -> &mut re_scene::World {
        self.world
    }
    fn services(&mut self) -> &mut dyn HostServices {
        &mut self.services
    }
    fn stream(&mut self) -> Option<&'static dyn re_script::StreamAccess> {
        // Every App world has its streaming manager (`ensure_streamer`).
        self.world
            .ecs
            .contains_resource::<re_proc::stream::Streamer>()
            .then_some(&WorldStream as &'static dyn re_script::StreamAccess)
    }
}

/// The `stream` library over `re_proc::stream`'s world resources.
struct WorldStream;

impl re_script::StreamAccess for WorldStream {
    fn events(
        &self,
        world: &re_scene::World,
    ) -> Option<(u64, Vec<re_scene::StreamCell>, Vec<re_scene::StreamCell>)> {
        let e = world.ecs.get_resource::<re_proc::stream::StreamEvents>()?;
        Some((e.tick, e.unloaded.clone(), e.loaded.clone()))
    }

    fn cells(&self, world: &re_scene::World) -> Vec<re_scene::StreamCell> {
        world
            .ecs
            .get_resource::<re_proc::stream::Streamer>()
            .map(|s| s.cells().cloned().collect())
            .unwrap_or_default()
    }

    fn is_loaded(&self, world: &re_scene::World, key: &str) -> bool {
        world
            .ecs
            .get_resource::<re_proc::stream::Streamer>()
            .is_some_and(|s| s.is_loaded(key))
    }

    fn pin(&self, world: &mut re_scene::World, key: &str, on: bool) {
        if let Some(mut s) = world.ecs.get_resource_mut::<re_proc::stream::Streamer>() {
            if on {
                s.pin(key);
            } else {
                s.unpin(key);
            }
        }
    }
}
