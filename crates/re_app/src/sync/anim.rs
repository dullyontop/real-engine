//! Animation sync: `AnimPlayer` components play glTF node clips onto the
//! transforms of the entities named after the clip's nodes.
//!
//! Each clip target is matched by `name` among the player entity and its
//! descendants (the first in depth-first order); a clip with a single target
//! that matches no name animates the player entity itself. Playback time is
//! runtime state kept here, advanced in the PreAnimation phase of each fixed
//! step.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use re_anim::{Playhead, Sampler, TargetPose};
use re_core::{Diagnostic, Location};
use re_scene::ecs::entity::Entity;
use re_scene::{AnimPlayer, Name, Transform, World, entity_id, hierarchy};
use re_types::EntityId;

use crate::codes;
use crate::media::Media;

struct Player {
    entity: Entity,
    clip: String,
    playhead: Playhead,
    sampler: Option<Arc<Sampler>>,
    targets: Vec<Option<Entity>>,
}

/// Playback state of every `AnimPlayer`.
#[derive(Default)]
pub struct AnimSync {
    players: BTreeMap<EntityId, Player>,
    samplers: HashMap<String, Arc<Sampler>>,
    poses: Vec<TargetPose>,
}

impl AnimSync {
    /// No players yet.
    pub fn new() -> AnimSync {
        AnimSync::default()
    }

    /// The number of players being tracked.
    pub fn len(&self) -> usize {
        self.players.len()
    }

    /// True when no player is tracked.
    pub fn is_empty(&self) -> bool {
        self.players.is_empty()
    }

    /// The playback time of an entity's clip, in clip seconds.
    pub fn time(&self, id: EntityId) -> Option<f32> {
        let p = self.players.get(&id)?;
        let duration = p.sampler.as_ref().map_or(0.0, |s| s.duration());
        Some(p.playhead.clip_time(duration))
    }

    /// Forgets cached samplers of `clips` (their files changed).
    pub fn invalidate(&mut self, clips: &[String]) {
        for c in clips {
            self.samplers.remove(c);
        }
        for p in self.players.values_mut() {
            if clips.contains(&p.clip) {
                p.sampler = None;
                p.clip.clear();
            }
        }
    }
}

fn sampler(
    sync: &mut AnimSync,
    media: &mut Media,
    clip: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Arc<Sampler>> {
    if let Some(s) = sync.samplers.get(clip) {
        return Some(s.clone());
    }
    let made = media
        .clip(clip)
        .and_then(|c| Sampler::new((*c).clone()))
        .map(Arc::new);
    match made {
        Ok(s) => {
            sync.samplers.insert(clip.to_string(), s.clone());
            Some(s)
        }
        Err(d) => {
            if media.first_report(clip) {
                diagnostics.push(
                    Diagnostic::error(
                        codes::ASSET_UNAVAILABLE,
                        format!("the animation clip `{clip}` cannot be played"),
                    )
                    .at(Location::file(clip))
                    .with_related(d),
                );
            }
            None
        }
    }
}

fn resolve_targets(
    world: &World,
    root: Entity,
    clip: &str,
    sampler: &Sampler,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<Option<Entity>> {
    let subtree = hierarchy::subtree(&world.ecs, root);
    let named = |name: &str| {
        subtree
            .iter()
            .copied()
            .find(|&e| world.ecs.get::<Name>(e).is_some_and(|n| n.0 == name))
    };
    let targets = &sampler.clip().targets;
    targets
        .iter()
        .map(|t| {
            let found = named(&t.target).or((targets.len() == 1).then_some(root));
            if found.is_none() {
                let root_name = world
                    .ecs
                    .get::<Name>(root)
                    .map_or_else(|| format!("{}", entity_id(root).0), |n| n.0.clone());
                diagnostics.push(Diagnostic::warning(
                    codes::ANIM_TARGET_MISSING,
                    format!(
                        "clip `{clip}` animates `{}`, but no entity under `{root_name}` is named `{}`",
                        t.target, t.target
                    ),
                ));
            }
            found
        })
        .collect()
}

/// Advances every player by `dt` seconds and writes the sampled poses into the
/// targets' `Transform`s.
pub fn step(
    world: &mut World,
    sync: &mut AnimSync,
    media: &mut Media,
    dt: f32,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut found: Vec<(Entity, AnimPlayer)> = world
        .ecs
        .query::<(Entity, &AnimPlayer)>()
        .iter(&world.ecs)
        .filter(|(_, p)| !p.clip.is_empty())
        .map(|(e, p)| (e, p.clone()))
        .collect();
    found.sort_by_key(|(e, _)| e.to_bits());
    let ids: Vec<EntityId> = found.iter().map(|(e, _)| entity_id(*e)).collect();
    sync.players.retain(|id, _| ids.contains(id));

    for (entity, component) in found {
        let id = entity_id(entity);
        let fresh = sync
            .players
            .get(&id)
            .is_none_or(|p| p.clip != component.clip || p.entity != entity);
        if fresh {
            let s = sampler(sync, media, &component.clip, diagnostics);
            let targets = s.as_ref().map_or_else(Vec::new, |s| {
                resolve_targets(world, entity, &component.clip, s, diagnostics)
            });
            sync.players.insert(
                id,
                Player {
                    entity,
                    clip: component.clip.clone(),
                    playhead: Playhead::default(),
                    sampler: s,
                    targets,
                },
            );
        }
        let player = sync.players.get_mut(&id).expect("inserted above");
        let Some(sampler) = player.sampler.clone() else {
            continue;
        };
        player.playhead.speed = component.speed;
        player.playhead.looping = component.looping;
        player.playhead.playing = component.playing;
        let duration = sampler.duration();
        player.playhead.advance(dt, duration);
        sampler.sample_into(player.playhead.clip_time(duration), &mut sync.poses);
        for pose in &sync.poses {
            let Some(Some(target)) = player.targets.get(pose.index) else {
                continue;
            };
            let Some(mut tr) = world.ecs.get_mut::<Transform>(*target) else {
                continue;
            };
            let mut next = tr.clone();
            if let Some(t) = pose.translation {
                next.translation = t;
            }
            if let Some(r) = pose.rotation {
                next.rotation = r;
            }
            if let Some(s) = pose.scale {
                next.scale = s;
            }
            if *tr != next {
                *tr = next;
            }
        }
    }
}
