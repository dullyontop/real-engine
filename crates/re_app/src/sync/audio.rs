//! Audio sync (ADR-0006): `AudioSource` components and the listener become
//! [`AudioCmd`]s.
//!
//! - A source with `autoplay` starts when its entity appears (once per entity,
//!   whether or not it loops), and stops when the entity or the component goes.
//! - A source with a `bus` plays on it (`SetSourceBus` before `StartSource`). A
//!   change of clip or bus stops the sound, and autoplay applies again.
//! - A spatial source's `rolloff`, `min_distance`, `max_distance` and `doppler`
//!   go to the mixer with `SetSourceSpatial` when they differ from what it holds
//!   for the entity: before the source starts, and while it plays.
//! - A playing spatial source follows its entity; a volume change is sent as is,
//!   and so is a rate (`SetSourceRate`, also right after `StartSource` when the
//!   rate is not 1), except in a frame whose commands already fade that field
//!   (a script's `SourceControl` `volume` or `rate`, ADR-0057 §8).
//! - A script's `SourceControl` `start` becomes the source's start, in its place
//!   among the frame's commands, as autoplay sends it.
//! - The listener is the first active `AudioListener`, in entity order, or
//!   without one the first active camera.
//!
//! One-shot sounds requested by scripts (`HostServices::play_sound`) are queued
//! by the app directly.

use std::collections::{BTreeMap, BTreeSet};

use glam::{Quat, Vec3};
use re_scene::ecs::entity::Entity;
use re_scene::{
    AudioListener, AudioRolloff, AudioSource, Camera, GlobalTransform, World, entity_id,
};
use re_types::{AudioCmd, EntityId, Rolloff, SoundOp, Spatial};

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
    /// The spatial settings sent for each entity, which the mixer holds until
    /// the entity's next `StopSource`.
    spatial: BTreeMap<EntityId, Spatial>,
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

fn spatial_of(src: &AudioSource) -> Spatial {
    Spatial {
        rolloff: match src.rolloff {
            AudioRolloff::Natural => Rolloff::Natural,
            AudioRolloff::Inverse => Rolloff::Inverse,
            AudioRolloff::Linear => Rolloff::Linear,
            AudioRolloff::Off => Rolloff::Off,
        },
        min_distance: src.min_distance,
        max_distance: src.max_distance,
        doppler: src.doppler,
    }
}

/// Compared by bits, so invalid (NaN) settings are sent, and refused, once
/// rather than every frame.
fn same(a: &Spatial, b: &Spatial) -> bool {
    a.rolloff == b.rolloff
        && a.min_distance.to_bits() == b.min_distance.to_bits()
        && a.max_distance.to_bits() == b.max_distance.to_bits()
        && a.doppler.to_bits() == b.doppler.to_bits()
}

/// Sends a spatial source's settings when they differ from what the mixer holds.
fn send_spatial(sync: &mut AudioSync, id: EntityId, src: &AudioSource, cmds: &mut Vec<AudioCmd>) {
    let want = spatial_of(src);
    if !same(&want, &sync.spatial.get(&id).copied().unwrap_or_default()) {
        sync.spatial.insert(id, want);
        cmds.push(AudioCmd::SetSourceSpatial {
            entity: id,
            spatial: want,
        });
    }
}

/// A change of clip or bus stops the old sound (the mixer forgets the entity's
/// bus and settings), and autoplay applies again.
fn stop_if_changed(
    sync: &mut AudioSync,
    id: EntityId,
    src: &AudioSource,
    cmds: &mut Vec<AudioCmd>,
) {
    let started = sync.started.get(&id);
    if started.is_some_and(|(clip, bus)| *clip != src.clip || *bus != src.bus) {
        if sync.playing.remove(&id).is_some() {
            sync.spatial.remove(&id);
            cmds.push(AudioCmd::StopSource { entity: id });
        }
        sync.started.remove(&id);
    }
}

/// Starts a source from its component, as autoplay does.
fn start(
    sync: &mut AudioSync,
    id: EntityId,
    src: &AudioSource,
    position: Vec3,
    cmds: &mut Vec<AudioCmd>,
) {
    sync.started.insert(id, (src.clip.clone(), src.bus.clone()));
    if src.spatial {
        send_spatial(sync, id, src, cmds);
    }
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

/// Appends the audio commands for the world's changes since the last call, and
/// resolves the scripts' `start` controls among the frame's commands.
pub fn build(world: &mut World, sync: &mut AudioSync, cmds: &mut Vec<AudioCmd>) {
    let mut sources: Vec<(Entity, AudioSource)> = world
        .ecs
        .query::<(Entity, &AudioSource)>()
        .iter(&world.ecs)
        .map(|(e, s)| (e, s.clone()))
        .collect();
    sources.sort_by_key(|(e, _)| e.to_bits());
    let index: BTreeMap<EntityId, usize> = sources
        .iter()
        .enumerate()
        .map(|(i, (e, _))| (entity_id(*e), i))
        .collect();

    // The scripts' controls of sources in this frame (ADR-0057 §8).
    let mut faded_volume = BTreeSet::new();
    let mut faded_rate = BTreeSet::new();
    let mut i = 0;
    while i < cmds.len() {
        let AudioCmd::SourceControl { entity: id, op } = cmds[i] else {
            i += 1;
            continue;
        };
        match op {
            SoundOp::Volume { .. } => {
                faded_volume.insert(id);
            }
            SoundOp::Rate { .. } => {
                faded_rate.insert(id);
            }
            SoundOp::Start => {
                // Without a source and a clip, the mixer ignores it.
                if let Some(&k) = index.get(&id)
                    && !sources[k].1.clip.is_empty()
                {
                    let (entity, src) = &sources[k];
                    let mut started = Vec::new();
                    stop_if_changed(sync, id, src, &mut started);
                    start(sync, id, src, pose(world, *entity).0, &mut started);
                    let n = started.len();
                    cmds.splice(i..=i, started);
                    i += n;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }

    let mut present = BTreeSet::new();
    for (entity, src) in &sources {
        let id = entity_id(*entity);
        present.insert(id);
        let (position, _) = pose(world, *entity);
        stop_if_changed(sync, id, src, cmds);
        if let Some(p) = sync.playing.get_mut(&id) {
            if p.volume != src.volume {
                p.volume = src.volume;
                if !faded_volume.contains(&id) {
                    cmds.push(AudioCmd::SetSourceVolume {
                        entity: id,
                        volume: src.volume,
                    });
                }
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
                if !faded_rate.contains(&id) {
                    cmds.push(AudioCmd::SetSourceRate {
                        entity: id,
                        rate: src.rate,
                    });
                }
            }
            if src.spatial {
                send_spatial(sync, id, src, cmds);
            }
            continue;
        }
        if src.autoplay && !src.clip.is_empty() && !sync.started.contains_key(&id) {
            start(sync, id, src, position, cmds);
        }
    }
    let gone: Vec<EntityId> = sync
        .playing
        .keys()
        .filter(|id| !present.contains(*id))
        .copied()
        .collect();
    for id in gone {
        sync.playing.remove(&id);
        sync.spatial.remove(&id);
        cmds.push(AudioCmd::StopSource { entity: id });
    }
    sync.started.retain(|id, _| present.contains(id));
    sync.spatial.retain(|id, _| present.contains(id));

    let listener = world
        .ecs
        .query::<(Entity, &AudioListener)>()
        .iter(&world.ecs)
        .filter(|(_, l)| l.active)
        .map(|(e, _)| e)
        .min_by_key(|e| e.to_bits())
        .or_else(|| {
            world
                .ecs
                .query::<(Entity, &Camera)>()
                .iter(&world.ecs)
                .filter(|(_, c)| c.active)
                .map(|(e, _)| e)
                .min_by_key(|e| e.to_bits())
        });
    if let Some(listener) = listener {
        let (position, rotation) = pose(world, listener);
        if sync.listener != Some((position, rotation)) {
            sync.listener = Some((position, rotation));
            cmds.push(AudioCmd::SetListener { position, rotation });
        }
    }
}
