//! Audio sync (ADR-0006): `AudioSource` components and the active camera become
//! [`AudioCmd`]s.
//!
//! - A source with `autoplay` starts when its entity appears (once per entity,
//!   whether or not it loops), and stops when the entity or the component goes.
//! - A source with a `bus` plays on it (`SetSourceBus` before `StartSource`). A
//!   change of clip or bus stops the sound, and autoplay applies again.
//! - A playing spatial source follows its entity; a volume change is sent as is,
//!   and so is a rate (`SetSourceRate`, also right after `StartSource` when the
//!   rate is not 1).
//! - The listener is the active camera.
//!
//! One-shot sounds requested by scripts (`HostServices::play_sound`) are queued
//! by the app directly.

use std::collections::BTreeMap;

use glam::{Quat, Vec3};
use re_scene::ecs::entity::Entity;
use re_scene::{AudioSource, Camera, GlobalTransform, World, entity_id};
use re_types::{AudioCmd, EntityId};

#[derive(Clone, Debug, PartialEq)]
struct Playing {
    volume: f32,
    position: Vec3,
    rate: f32,
}

/// Which sources are playing, and where the listener is.
#[derive(Clone, Debug, Default)]
pub struct AudioSync {
    playing: BTreeMap<EntityId, Playing>,
    /// Entities whose autoplay already started, with the clip and bus (a
    /// one-shot that ended is not started again).
    started: BTreeMap<EntityId, (String, String)>,
    listener: Option<(Vec3, Quat)>,
}

impl AudioSync {
    /// An empty state.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of sources started and still present.
    pub fn playing(&self) -> usize {
        self.playing.len()
    }
}

fn pose(world: &World, e: Entity) -> (Vec3, Quat) {
    world
        .ecs
        .get::<GlobalTransform>(e)
        .map_or((Vec3::ZERO, Quat::IDENTITY), |g| {
            let (_, r, t) = g.0.to_scale_rotation_translation();
            (t, r)
        })
}

/// Appends the audio commands for the world's changes since the last call.
pub fn build(world: &mut World, sync: &mut AudioSync, cmds: &mut Vec<AudioCmd>) {
    let mut sources: Vec<(Entity, AudioSource)> = world
        .ecs
        .query::<(Entity, &AudioSource)>()
        .iter(&world.ecs)
        .map(|(e, s)| (e, s.clone()))
        .collect();
    sources.sort_by_key(|(e, _)| e.to_bits());

    let mut present = BTreeMap::new();
    for (entity, src) in sources {
        let id = entity_id(entity);
        present.insert(id, ());
        let (position, _) = pose(world, entity);
        let started = sync.started.get(&id);
        if started.is_some_and(|(clip, bus)| *clip != src.clip || *bus != src.bus) {
            // The clip or bus changed: the old sound stops, and autoplay applies again.
            if sync.playing.remove(&id).is_some() {
                cmds.push(AudioCmd::StopSource { entity: id });
            }
            sync.started.remove(&id);
        }
        if let Some(p) = sync.playing.get_mut(&id) {
            if p.volume != src.volume {
                p.volume = src.volume;
                cmds.push(AudioCmd::SetSourceVolume {
                    entity: id,
                    volume: src.volume,
                });
            }
            if src.spatial && p.position != position {
                p.position = position;
                cmds.push(AudioCmd::SetSourcePosition {
                    entity: id,
                    position,
                });
            }
            // Compared by bits, so an invalid (NaN) rate is sent, and refused,
            // once rather than every frame.
            if p.rate.to_bits() != src.rate.to_bits() {
                p.rate = src.rate;
                cmds.push(AudioCmd::SetSourceRate {
                    entity: id,
                    rate: src.rate,
                });
            }
            continue;
        }
        if src.autoplay && !src.clip.is_empty() && !sync.started.contains_key(&id) {
            sync.started.insert(id, (src.clip.clone(), src.bus.clone()));
            if !src.bus.is_empty() {
                cmds.push(AudioCmd::SetSourceBus {
                    entity: id,
                    bus: src.bus.clone(),
                });
            }
            cmds.push(AudioCmd::StartSource {
                entity: id,
                clip: src.clip.clone(),
                volume: src.volume,
                looping: src.looping,
                spatial: src.spatial,
                position,
            });
            // The rate applies to a playing sound, so it follows the start.
            if src.rate.to_bits() != 1.0f32.to_bits() {
                cmds.push(AudioCmd::SetSourceRate {
                    entity: id,
                    rate: src.rate,
                });
            }
            sync.playing.insert(
                id,
                Playing {
                    volume: src.volume,
                    position,
                    rate: src.rate,
                },
            );
        }
    }
    let gone: Vec<EntityId> = sync
        .playing
        .keys()
        .filter(|id| !present.contains_key(id))
        .copied()
        .collect();
    for id in gone {
        sync.playing.remove(&id);
        cmds.push(AudioCmd::StopSource { entity: id });
    }
    sync.started.retain(|id, _| present.contains_key(id));

    let mut cams: Vec<Entity> = world
        .ecs
        .query::<(Entity, &Camera)>()
        .iter(&world.ecs)
        .filter(|(_, c)| c.active)
        .map(|(e, _)| e)
        .collect();
    cams.sort_by_key(|e| e.to_bits());
    if let Some(&cam) = cams.first() {
        let (position, rotation) = pose(world, cam);
        if sync.listener != Some((position, rotation)) {
            sync.listener = Some((position, rotation));
            cmds.push(AudioCmd::SetListener { position, rotation });
        }
    }
}
