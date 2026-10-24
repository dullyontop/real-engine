//! Skin sync (ADR-0039): each `SkinnedMesh` keeps a skeleton pose here, posed
//! by the `AnimPlayer` on the same entity (if any) in the PreAnimation phase of
//! each fixed step, and turned into a model-space joint palette for the render
//! snapshot.
//!
//! Joints are not entities: a clip animates a skin's joints by name. A clip
//! target that matches no joint is reported once per clip and skin
//! (`RE0522`); without a player, or while its clip cannot be played, the
//! skeleton holds its rest pose.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use glam::Mat4;
use re_anim::skin::{BoundClip, ModelPose, Pose, check_skin, palette_into};
use re_anim::{Playhead, Sampler};
use re_core::{Diagnostic, Location};
use re_scene::ecs::entity::Entity;
use re_scene::{AnimPlayer, SkinnedMesh, World, entity_id};
use re_types::{EntityId, SkinData};

use crate::codes;
use crate::media::Media;

/// One posed skin.
struct Posed {
    entity: Entity,
    asset: String,
    skin: Arc<SkinData>,
    clip: String,
    bound: Option<BoundClip>,
    playhead: Playhead,
    pose: Pose,
    model: ModelPose,
    palette: Vec<Mat4>,
}

/// The skeleton pose and palette of every `SkinnedMesh` whose skin loaded.
#[derive(Default)]
pub struct SkinSync {
    posed: BTreeMap<EntityId, Posed>,
    /// `(clip, skin asset)` pairs already reported for unmatched targets.
    reported: HashSet<(String, String)>,
}

impl SkinSync {
    /// No skins yet.
    pub fn new() -> SkinSync {
        SkinSync::default()
    }

    /// The number of posed skins.
    pub fn len(&self) -> usize {
        self.posed.len()
    }

    /// True when no skin is posed.
    pub fn is_empty(&self) -> bool {
        self.posed.is_empty()
    }

    /// An entity's model-space joint palette (joint matrix times inverse bind,
    /// one per joint), from the last step.
    pub fn palette(&self, id: EntityId) -> Option<&[Mat4]> {
        self.posed.get(&id).map(|p| p.palette.as_slice())
    }

    /// The skin an entity is posed with.
    pub fn skin(&self, id: EntityId) -> Option<&SkinData> {
        self.posed.get(&id).map(|p| &*p.skin)
    }

    /// The shared skin an entity is posed with.
    pub fn skin_arc(&self, id: EntityId) -> Option<Arc<SkinData>> {
        self.posed.get(&id).map(|p| p.skin.clone())
    }

    /// Replaces an entity's joint model-space matrices for this step (a ragdoll's
    /// blended pose, ADR-0040) and rebuilds its palette. The next step poses the
    /// skeleton from its animation again.
    pub fn override_joints(&mut self, id: EntityId, f: impl FnOnce(&mut [Mat4])) {
        if let Some(p) = self.posed.get_mut(&id) {
            f(&mut p.model.joints);
            p.palette.clear();
            palette_into(&p.skin, &p.model, &mut p.palette);
        }
    }

    /// An entity's joint model-space matrices, from the last step.
    pub fn joint_matrices(&self, id: EntityId) -> Option<&[Mat4]> {
        self.posed.get(&id).map(|p| p.model.joints.as_slice())
    }

    /// Forgets skins and clips built from `stale` assets (their files changed).
    pub fn invalidate(&mut self, stale: &[String]) {
        self.posed
            .retain(|_, p| !stale.contains(&p.asset) && !stale.contains(&p.clip));
        self.reported.clear();
    }
}

/// Loads, poses and builds the palette of every `SkinnedMesh`, advancing the
/// `AnimPlayer` on its entity by `dt` seconds.
pub fn step(
    world: &mut World,
    sync: &mut SkinSync,
    media: &mut Media,
    dt: f32,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut found: Vec<(Entity, String, Option<AnimPlayer>)> = world
        .ecs
        .query::<(Entity, &SkinnedMesh, Option<&AnimPlayer>)>()
        .iter(&world.ecs)
        .filter(|(_, s, _)| !s.asset.is_empty())
        .map(|(e, s, p)| (e, s.asset.clone(), p.cloned()))
        .collect();
    found.sort_by_key(|(e, _, _)| e.to_bits());
    let ids: Vec<EntityId> = found.iter().map(|(e, _, _)| entity_id(*e)).collect();
    sync.posed.retain(|id, _| ids.contains(id));

    for (entity, asset, player) in found {
        let id = entity_id(entity);
        if sync
            .posed
            .get(&id)
            .is_none_or(|p| p.asset != asset || p.entity != entity)
        {
            sync.posed.remove(&id);
            let Some(skin) = load(media, &asset, diagnostics) else {
                continue;
            };
            sync.posed.insert(
                id,
                Posed {
                    entity,
                    asset: asset.clone(),
                    pose: Pose::rest(&skin),
                    skin,
                    clip: String::new(),
                    bound: None,
                    playhead: Playhead::default(),
                    model: ModelPose::default(),
                    palette: Vec::new(),
                },
            );
        }
        let posed = sync.posed.get_mut(&id).expect("inserted above");
        let clip = player.as_ref().map_or(String::new(), |p| p.clip.clone());
        if clip != posed.clip {
            posed.clip = clip.clone();
            posed.playhead = Playhead::default();
            posed.pose.reset(&posed.skin);
            posed.bound = if clip.is_empty() {
                None
            } else {
                bind(media, &clip, posed, &mut sync.reported, diagnostics)
            };
        }
        if let (Some(bound), Some(player)) = (&posed.bound, &player) {
            posed.playhead.speed = player.speed;
            posed.playhead.looping = player.looping;
            posed.playhead.playing = player.playing;
            let duration = bound.duration();
            posed.playhead.advance(dt, duration);
            bound.sample(posed.playhead.clip_time(duration), &mut posed.pose);
        }
        posed.model.compute(&posed.skin, &posed.pose);
        posed.palette.clear();
        palette_into(&posed.skin, &posed.model, &mut posed.palette);
    }
}

fn load(
    media: &mut Media,
    asset: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Arc<SkinData>> {
    let loaded = media.skin(asset).and_then(|l| {
        check_skin(&l.skin)?;
        Ok(l.skin)
    });
    match loaded {
        Ok(skin) => Some(skin),
        Err(d) => {
            if media.first_report(asset) {
                diagnostics.push(
                    Diagnostic::error(
                        codes::ASSET_UNAVAILABLE,
                        format!("the skinned mesh `{asset}` cannot be posed"),
                    )
                    .at(Location::file(asset))
                    .with_related(d),
                );
            }
            None
        }
    }
}

fn bind(
    media: &mut Media,
    clip: &str,
    posed: &Posed,
    reported: &mut HashSet<(String, String)>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<BoundClip> {
    let bound = media
        .clip(clip)
        .and_then(|c| Sampler::new((*c).clone()))
        .and_then(|s| BoundClip::bind_shared(Arc::new(s), &posed.skin));
    match bound {
        Ok(b) => {
            if !b.unmatched().is_empty() && reported.insert((clip.to_string(), posed.asset.clone()))
            {
                diagnostics.push(Diagnostic::warning(
                    codes::ANIM_TARGET_MISSING,
                    format!(
                        "clip `{clip}` animates {}, but skin `{}` has no joint of that name",
                        b.unmatched()
                            .iter()
                            .map(|t| format!("`{t}`"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        posed.asset
                    ),
                ));
            }
            Some(b)
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
