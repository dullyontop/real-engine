//! The runtime side of [`HostServices`]: what scripts may ask of the engine.
//!
//! [`Host`] borrows the parts of the [`App`](crate::App) that scripts reach while
//! a script callback runs (the World itself goes to the script runtime
//! separately). Calls that change something are queued and take effect at a
//! fixed point of the frame:
//! - impulses, velocities, character moves and vehicle input are physics commands for the
//!   next physics phase (the current fixed step's, when called from a fixed-step callback);
//!   an impulse or velocity for an entity spawned this frame waits in the queue for the
//!   body that phase creates;
//! - the time scale applies to the fixed steps the next frame counts (this frame's, when
//!   called from a frame callback);
//! - sounds go to the frame's audio commands;
//! - HUD text, screenshots, regeneration and restart are applied in EndFrame.

use glam::{Quat, Vec3};
use re_anim::graph::{GraphPlayer, Param};
use re_physics::{Physics, QueryFilter};
use re_types::anim::AnimValue;
use re_types::host::{HostError, HostResult, HostServices};
use re_types::{
    ActionFrame, AudioCmd, BodyKind, BusLevel, CharacterState, EntityId, GroupMask, PhysicsCmd,
    RagdollMode, RagdollState, RayHit, SoundOp, VehicleInput, VehicleState,
};

use crate::codes::{HOST_INVALID_ARGUMENT_STR, HOST_NO_SUCH_THING_STR};
use crate::sync::anim_skin::SkinSync;
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
    /// Every mixer bus's level over the last frame, as scripts read it
    /// (silence on the headless backend).
    pub bus_levels: &'a [BusLevel],
    /// The mixer graph the audio commands are checked against.
    pub audio_graph: &'a re_types::AudioGraphDesc,
    /// End-of-frame requests.
    pub pending: &'a mut Pending,
    /// Skeleton poses and animation graphs (`anim.*` read and set them at
    /// once).
    pub skins: &'a mut SkinSync,
    /// How fast simulation time runs against frame time (`time.set_scale`).
    pub time_scale: &'a mut f64,
}

/// The largest time scale `time.set_scale` takes.
pub const MAX_TIME_SCALE: f64 = 10.0;

fn no_such(msg: String) -> HostError {
    HostError::new(HOST_NO_SUCH_THING_STR, msg)
}

fn no_graph(entity: EntityId) -> HostError {
    no_such(format!(
        "entity {} plays no animation graph (add a SkinnedMesh and an Animator whose graph \
         loads; it starts in the first fixed step)",
        entity.0
    ))
}

fn invalid(msg: String) -> HostError {
    HostError::new(HOST_INVALID_ARGUMENT_STR, msg)
}

/// The checks `audio_mix` makes of a sound control (ADR-0057 §8): a fade is
/// from 0 to [`re_audio::MAX_FADE`] seconds, a seek and a volume 0 or more, and
/// a rate above 0 and at most [`re_audio::MAX_RATE`].
fn sound_op(op: &SoundOp) -> HostResult<()> {
    let at_least_0 = |v: f64, what: &str| {
        if v.is_finite() && v >= 0.0 {
            Ok(())
        } else {
            Err(invalid(format!("{what} {v} must be 0 or more")))
        }
    };
    let fade = |fade: f32| {
        if (0.0..=re_audio::MAX_FADE).contains(&fade) {
            Ok(())
        } else {
            Err(invalid(format!(
                "fade {fade} must be from 0 to {} seconds",
                re_audio::MAX_FADE
            )))
        }
    };
    match *op {
        SoundOp::Start => Ok(()),
        SoundOp::Stop { fade: f } | SoundOp::Pause { fade: f } | SoundOp::Resume { fade: f } => {
            fade(f)
        }
        SoundOp::Seek { seconds } => at_least_0(seconds, "seek"),
        SoundOp::Volume { volume, fade: f } => {
            at_least_0(volume.into(), "volume")?;
            fade(f)
        }
        SoundOp::Rate { rate, fade: f } => {
            if !(rate.is_finite() && rate > 0.0 && rate <= re_audio::MAX_RATE) {
                return Err(invalid(format!(
                    "rate {rate} must be above 0 and at most {}",
                    re_audio::MAX_RATE
                )));
            }
            fade(f)
        }
    }
}

fn finite(v: Vec3, what: &str) -> HostResult<()> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(invalid(format!("{what} {v} is not finite")))
    }
}

fn graph_err(d: re_core::Diagnostic) -> HostError {
    HostError::new(re_anim::codes::GRAPH_PARAM_UNKNOWN_STR, d.message)
}

impl Host<'_> {
    fn player(&self, entity: EntityId) -> HostResult<&GraphPlayer> {
        self.skins.player(entity).ok_or_else(|| no_graph(entity))
    }

    fn player_mut(&mut self, entity: EntityId) -> HostResult<&mut GraphPlayer> {
        self.skins
            .player_mut(entity)
            .ok_or_else(|| no_graph(entity))
    }

    fn require_ragdoll(&self, entity: EntityId) -> HostResult<()> {
        if self.physics.has_ragdoll(entity) {
            Ok(())
        } else {
            Err(no_such(format!(
                "entity {} has no ragdoll (add a Ragdoll to a SkinnedMesh; it is built once \
                 the skin is posed)",
                entity.0
            )))
        }
    }

    /// Refuses an entity whose body is not dynamic. One without a body yet is
    /// let through: an entity spawned this frame gets its body at the next
    /// physics phase, which applies its commands after creating it (the
    /// script library checks that it describes a dynamic body), and a command
    /// for an entity that still has none then is reported there.
    fn require_dynamic(&self, entity: EntityId, what: &str) -> HostResult<()> {
        match self.links.kind(entity) {
            Some(LinkKind::Body(BodyKind::Dynamic)) | None => Ok(()),
            Some(_) => Err(invalid(format!(
                "entity {} is not a dynamic body; {what} dynamic bodies only",
                entity.0
            ))),
        }
    }

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
        self.require_dynamic(entity, "impulses move")?;
        self.physics_cmds.push(PhysicsCmd::ApplyImpulse {
            id: entity,
            impulse,
        });
        Ok(())
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

    fn set_body_velocity(
        &mut self,
        entity: EntityId,
        linear: Vec3,
        angular: Vec3,
    ) -> HostResult<()> {
        finite(linear, "linear velocity")?;
        finite(angular, "angular velocity")?;
        self.require_dynamic(entity, "set_velocity moves")?;
        self.physics_cmds.push(PhysicsCmd::SetVelocity {
            id: entity,
            linear,
            angular,
        });
        Ok(())
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

    fn ragdoll_mode(&mut self, entity: EntityId, mode: RagdollMode) -> HostResult<()> {
        self.require_ragdoll(entity)?;
        if !mode.is_valid() {
            return Err(invalid(format!(
                "ragdoll mode {mode:?} is out of range: a powered strength is 0 to 1"
            )));
        }
        self.physics_cmds
            .push(PhysicsCmd::SetRagdollMode { id: entity, mode });
        Ok(())
    }

    fn ragdoll_state(&self, entity: EntityId) -> HostResult<RagdollState> {
        self.require_ragdoll(entity)?;
        self.physics
            .ragdoll(entity)
            .ok_or_else(|| no_such(format!("entity {} has no ragdoll", entity.0)))
    }

    fn ragdoll_impulse(&mut self, entity: EntityId, joint: &str, impulse: Vec3) -> HostResult<()> {
        self.require_ragdoll(entity)?;
        finite(impulse, "impulse")?;
        if joint.is_empty() {
            return Err(invalid("the joint name is empty".into()));
        }
        self.physics_cmds.push(PhysicsCmd::ApplyRagdollImpulse {
            id: entity,
            joint: joint.to_string(),
            impulse,
        });
        Ok(())
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
            id: None,
        });
        Ok(())
    }

    fn audio_mix(&mut self, cmd: AudioCmd) -> HostResult<()> {
        let named = |bus: &str, what: &str| {
            if bus.is_empty() {
                Err(invalid(format!("the {what} name is empty")))
            } else {
                Ok(())
            }
        };
        let tween = |seconds: f32| {
            if seconds.is_finite() && seconds >= 0.0 {
                Ok(())
            } else {
                Err(invalid(format!("seconds {seconds} must be 0 or more")))
            }
        };
        let graph = self.audio_graph;
        let bus_only = |bus: &str| {
            named(bus, "bus")?;
            if graph.buses.contains_key(bus) {
                Ok(())
            } else {
                Err(no_such(format!("the mixer has no bus `{bus}`")))
            }
        };
        let bus_or_send = |bus: &str| {
            named(bus, "bus")?;
            if graph.buses.contains_key(bus) || graph.sends.contains_key(bus) {
                Ok(())
            } else {
                Err(no_such(format!("the mixer has no bus or send `{bus}`")))
            }
        };
        match &cmd {
            AudioCmd::PlayOneShot {
                clip,
                position,
                volume,
                ..
            } => {
                // The same checks as `play_sound`, keeping the id.
                return self.play_sound(clip, *position, *volume).map(|()| {
                    if let Some(last) = self.audio_cmds.last_mut() {
                        *last = cmd;
                    }
                });
            }
            AudioCmd::PlayOneShotOnBus {
                clip,
                position,
                volume,
                bus,
                ..
            } => {
                bus_only(bus)?;
                // The same checks as an unrouted one-shot.
                return self.play_sound(clip, *position, *volume).map(|()| {
                    if let Some(last) = self.audio_cmds.last_mut() {
                        *last = cmd;
                    }
                });
            }
            AudioCmd::SoundControl { op, .. } | AudioCmd::SourceControl { op, .. } => {
                sound_op(op)?;
            }
            AudioCmd::SetBusVolume {
                bus,
                volume_db,
                seconds,
            } => {
                bus_or_send(bus)?;
                if !volume_db.is_finite() {
                    return Err(invalid(format!("volume {volume_db} dB is not finite")));
                }
                tween(*seconds)?;
            }
            AudioCmd::SetEffectParam {
                bus,
                effect,
                param,
                value,
                seconds,
            } => {
                bus_or_send(bus)?;
                let effects = graph
                    .buses
                    .get(bus)
                    .map(|b| b.effects.len())
                    .or_else(|| graph.sends.get(bus).map(|s| s.effects.len()))
                    .unwrap_or(0);
                if *effect as usize >= effects {
                    return Err(no_such(format!(
                        "`{bus}` has {effects} effects, so there is no effect {}",
                        u64::from(*effect) + 1
                    )));
                }
                named(param, "parameter")?;
                if let re_types::EffectParam::Number(v) = value
                    && !v.is_finite()
                {
                    return Err(invalid(format!("{param} {v} is not finite")));
                }
                tween(*seconds)?;
            }
            AudioCmd::ApplySnapshot { name, seconds } => {
                named(name, "snapshot")?;
                if name != re_audio::DEFAULT_SNAPSHOT && !graph.snapshots.contains_key(name) {
                    return Err(no_such(format!("the mixer has no snapshot `{name}`")));
                }
                tween(*seconds)?;
            }
            other => {
                return Err(invalid(format!(
                    "{other:?} is not a mixer command scripts can send"
                )));
            }
        }
        self.audio_cmds.push(cmd);
        Ok(())
    }

    fn audio_level(&self, bus: &str) -> HostResult<BusLevel> {
        self.bus_levels
            .iter()
            .find(|l| l.bus == bus)
            .cloned()
            .ok_or_else(|| no_such(format!("the mixer has no bus or send `{bus}`")))
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

    fn set_time_scale(&mut self, scale: f64) -> HostResult<()> {
        if !(scale.is_finite() && (0.0..=MAX_TIME_SCALE).contains(&scale)) {
            return Err(invalid(format!(
                "time scale {scale} is not a number from 0 to {MAX_TIME_SCALE}"
            )));
        }
        *self.time_scale = scale;
        Ok(())
    }

    fn time_scale(&self) -> f64 {
        *self.time_scale
    }

    fn anim_set(&mut self, entity: EntityId, param: &str, value: AnimValue) -> HostResult<()> {
        let value = match value {
            AnimValue::Float(v) => Param::Float(v),
            AnimValue::Bool(v) => Param::Bool(v),
        };
        self.player_mut(entity)?
            .set(param, value)
            .map_err(graph_err)
    }

    fn anim_get(&self, entity: EntityId, param: &str) -> HostResult<AnimValue> {
        Ok(match self.player(entity)?.get(param).map_err(graph_err)? {
            Param::Float(v) => AnimValue::Float(v),
            Param::Bool(v) | Param::Trigger(v) => AnimValue::Bool(v),
        })
    }

    fn anim_trigger(&mut self, entity: EntityId, param: &str) -> HostResult<()> {
        self.player_mut(entity)?.trigger(param).map_err(graph_err)
    }

    fn anim_state(&self, entity: EntityId, layer: Option<&str>) -> HostResult<String> {
        let player = self.player(entity)?;
        player.state(layer).map(str::to_string).ok_or_else(|| {
            let layers: Vec<String> = player.graph().layers().map(|l| format!("`{l}`")).collect();
            invalid(format!(
                "the animation graph of entity {} has no layer `{}`; its layers: {}",
                entity.0,
                layer.unwrap_or("(first)"),
                if layers.is_empty() {
                    "none".to_string()
                } else {
                    layers.join(", ")
                }
            ))
        })
    }

    fn anim_joint(&self, entity: EntityId, joint: &str) -> HostResult<(Vec3, Quat)> {
        match self.skins.joint_world(entity, joint) {
            Some(j) => Ok(j),
            None if self.skins.skin(entity).is_none() => Err(no_such(format!(
                "entity {} has no posed skin (add a SkinnedMesh)",
                entity.0
            ))),
            None => Err(invalid(format!(
                "the skin of entity {} has no joint `{joint}`",
                entity.0
            ))),
        }
    }
}

/// The [`ScriptHost`](re_script::ScriptHost) of a running game: the World, the
/// [`Host`] services and the game's scenes. Scripts get no command registry
/// while the game runs.
pub struct PlayHost<'a> {
    /// The world scripts read and write.
    pub world: &'a mut re_scene::World,
    /// The services.
    pub services: Host<'a>,
    /// The game's scenes, for the `scene` library (ADR-0059); `None` for a
    /// compat game's contexts, which keep their own model.
    pub game: Option<&'a mut crate::scenes::Scenes>,
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
    fn game(&mut self) -> Option<&mut dyn re_script::GameAccess> {
        self.game
            .as_deref_mut()
            .map(|g| g as &mut dyn re_script::GameAccess)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_controls_are_checked_as_scripts_check_them() {
        for op in [
            SoundOp::Start,
            SoundOp::Stop { fade: 0.0 },
            SoundOp::Pause { fade: 1.0 },
            SoundOp::Resume { fade: 0.5 },
            SoundOp::Seek { seconds: 0.0 },
            SoundOp::Volume {
                volume: 2.0,
                fade: 0.0,
            },
            SoundOp::Rate {
                rate: 16.0,
                fade: 3.0,
            },
            SoundOp::Stop { fade: 3600.0 },
        ] {
            assert!(sound_op(&op).is_ok(), "{op:?}");
        }
        for op in [
            SoundOp::Stop { fade: -1.0 },
            SoundOp::Pause { fade: f32::NAN },
            SoundOp::Resume {
                fade: f32::INFINITY,
            },
            SoundOp::Seek { seconds: -1.0 },
            SoundOp::Volume {
                volume: -0.5,
                fade: 0.0,
            },
            SoundOp::Volume {
                volume: 1.0,
                fade: -2.0,
            },
            SoundOp::Rate {
                rate: 0.0,
                fade: 0.0,
            },
            SoundOp::Rate {
                rate: 16.5,
                fade: 0.0,
            },
            SoundOp::Rate {
                rate: 1.0,
                fade: f32::NAN,
            },
            SoundOp::Pause { fade: 3600.5 },
            SoundOp::Volume {
                volume: 1.0,
                fade: 1e30,
            },
        ] {
            let e = sound_op(&op).unwrap_err();
            assert_eq!(e.code, HOST_INVALID_ARGUMENT_STR, "{op:?}");
        }
    }
}
