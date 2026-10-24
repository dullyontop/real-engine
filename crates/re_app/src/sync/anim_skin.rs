//! Skin sync (ADR-0039): each `SkinnedMesh` keeps a skeleton pose here, posed
//! in the PreAnimation phase of each fixed step and turned into a model-space
//! joint palette for the render snapshot.
//!
//! An `Animator` on the entity plays an animation graph (`re_anim::graph`)
//! onto the skeleton; otherwise the `AnimPlayer` on the entity (if any) plays
//! one clip. Graphs are bound once per graph file and skin and shared, and
//! the graphs of different entities are evaluated in parallel on the worker
//! pool. A graph's root motion is returned to the caller, which moves the
//! entity. `BoneAttachment` children follow their parent's joints
//! ([`attach`]).
//!
//! Joints are not entities: a clip animates a skin's joints by name. A clip
//! target that matches no joint is reported once per clip and skin
//! (`RE0522`); without a player or graph, or while its clip or graph cannot be
//! played, the skeleton holds its rest pose.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};
use re_anim::graph::{BoundGraph, GraphPlayer, RootMotion};
use re_anim::skin::{BoundClip, ModelPose, Pose, check_skin, palette_into};
use re_anim::{Playhead, Sampler};
use re_core::{Diagnostic, Location};
use re_scene::ecs::entity::Entity;
use re_scene::{
    AnimPlayer, Animator, BoneAttachment, GlobalTransform, SkinnedMesh, Transform, World,
    entity_id, hierarchy,
};
use re_types::{EntityId, SkinData};

use crate::codes;
use crate::media::Media;

/// A graph playing on one skin.
struct Run {
    path: String,
    player: GraphPlayer,
    playing: bool,
    speed: f32,
}

/// One posed skin.
struct Posed {
    entity: Entity,
    asset: String,
    skin: Arc<SkinData>,
    clip: String,
    bound: Option<BoundClip>,
    playhead: Playhead,
    /// The `Animator.graph` last seen; `run` is `None` when it is empty or
    /// cannot play.
    graph: String,
    run: Option<Run>,
    motion: RootMotion,
    /// The entity's world matrix at the start of the step.
    world: Mat4,
    pose: Pose,
    model: ModelPose,
    palette: Vec<Mat4>,
}

impl Posed {
    /// Steps the graph (if any) and rebuilds the model pose and palette.
    fn finish(&mut self, dt: f32) {
        self.motion = RootMotion::IDENTITY;
        if let Some(run) = &mut self.run
            && run.playing
        {
            self.motion = run.player.step(dt * run.speed.max(0.0), &mut self.pose);
        }
        self.model.compute(&self.skin, &self.pose);
        self.palette.clear();
        palette_into(&self.skin, &self.model, &mut self.palette);
    }
}

/// The skeleton pose and palette of every `SkinnedMesh` whose skin loaded.
#[derive(Default)]
pub struct SkinSync {
    posed: BTreeMap<EntityId, Posed>,
    /// `(clip, skin asset)` pairs already reported for unmatched targets.
    reported: HashSet<(String, String)>,
    /// Bound graphs by `(graph file, skin asset)`, shared between entities.
    graphs: HashMap<(String, String), Arc<BoundGraph>>,
    /// `(graph file, skin asset)` pairs that failed to bind (reported once).
    failed: HashSet<(String, String)>,
    /// Bone attachments already reported for a missing joint or skin.
    attach_reported: HashSet<(EntityId, String)>,
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

    /// The graph an entity's `Animator` plays, once it is bound.
    pub fn player(&self, id: EntityId) -> Option<&GraphPlayer> {
        self.posed.get(&id)?.run.as_ref().map(|r| &r.player)
    }

    /// The graph an entity's `Animator` plays, to set its parameters.
    pub fn player_mut(&mut self, id: EntityId) -> Option<&mut GraphPlayer> {
        self.posed.get_mut(&id)?.run.as_mut().map(|r| &mut r.player)
    }

    /// The root motion of an entity's graph in the last step.
    pub fn root_motion(&self, id: EntityId) -> Option<RootMotion> {
        self.posed.get(&id).map(|p| p.motion)
    }

    /// A joint's world-space position and rotation: the entity's world
    /// matrix at the start of the last step times the joint's model matrix
    /// after it. `None` when the entity has no posed skin or the skin has no
    /// joint of that name.
    pub fn joint_world(&self, id: EntityId, joint: &str) -> Option<(Vec3, Quat)> {
        let p = self.posed.get(&id)?;
        let m = p.world * p.model.joint(p.skin.joint(joint)?)?;
        let (_, rotation, translation) = m.to_scale_rotation_translation();
        Some((translation, rotation.normalize()))
    }

    /// True when some entity plays a graph.
    pub fn has_graphs(&self) -> bool {
        self.posed.values().any(|p| p.run.is_some())
    }

    /// Appends the state determinism checks hash: for each entity playing a
    /// graph, in id order, its id and the graph's parameters, states and
    /// times (`GraphPlayer::write_state`). Poses are not included.
    pub fn write_state(&self, out: &mut Vec<u8>) {
        for (id, p) in &self.posed {
            if let Some(run) = &p.run {
                out.extend_from_slice(&id.0.to_le_bytes());
                out.push(run.playing as u8);
                out.extend_from_slice(&run.speed.to_bits().to_le_bytes());
                run.player.write_state(out);
            }
        }
    }

    /// Forgets skins, clips and graphs built from `stale` assets (their files
    /// changed).
    pub fn invalidate(&mut self, stale: &[String]) {
        self.posed.retain(|_, p| {
            !stale.contains(&p.asset) && !stale.contains(&p.clip) && !stale.contains(&p.graph)
        });
        // A clip a graph plays may have changed too: rebind every graph.
        self.graphs.clear();
        for p in self.posed.values_mut() {
            p.graph.clear();
            p.run = None;
        }
        self.reported.clear();
        self.failed.clear();
        self.attach_reported.clear();
    }

    /// The bound graph `path` for `posed`'s skin, binding (and reporting a
    /// failure) once.
    fn bind_graph(
        &mut self,
        media: &mut Media,
        path: &str,
        asset: &str,
        skin: &SkinData,
        diagnostics: &mut Vec<Diagnostic>,
    ) -> Option<Arc<BoundGraph>> {
        let key = (path.to_string(), asset.to_string());
        if let Some(g) = self.graphs.get(&key) {
            return Some(g.clone());
        }
        if self.failed.contains(&key) {
            return None;
        }
        let bound = media.anim_graph(path).and_then(|graph| {
            BoundGraph::bind(&graph, skin, &mut |clip| {
                let clip = media.clip(clip)?;
                Ok(Arc::new(Sampler::new((*clip).clone())?))
            })
        });
        match bound {
            Ok((graph, warnings)) => {
                diagnostics.extend(warnings.into_iter().map(|w| w.at(Location::file(path))));
                let graph = Arc::new(graph);
                self.graphs.insert(key, graph.clone());
                Some(graph)
            }
            Err(d) => {
                diagnostics.push(
                    Diagnostic::error(
                        codes::ASSET_UNAVAILABLE,
                        format!("the animation graph `{path}` cannot play on `{asset}`"),
                    )
                    .at(Location::file(path))
                    .with_related(d),
                );
                self.failed.insert(key);
                None
            }
        }
    }
}

/// Loads and poses every `SkinnedMesh`, playing the `Animator`'s graph or the
/// `AnimPlayer`'s clip on its entity for `dt` seconds, and builds its
/// palette. Returns the root motion of each entity whose graph moved it, in
/// entity id order.
pub fn step(
    world: &mut World,
    sync: &mut SkinSync,
    media: &mut Media,
    dt: f32,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<(Entity, RootMotion)> {
    type Found = (
        Entity,
        String,
        Option<AnimPlayer>,
        Option<Animator>,
        Option<Mat4>,
    );
    let mut found: Vec<Found> = world
        .ecs
        .query::<(
            Entity,
            &SkinnedMesh,
            Option<&AnimPlayer>,
            Option<&Animator>,
            Option<&GlobalTransform>,
        )>()
        .iter(&world.ecs)
        .filter(|(_, s, _, _, _)| !s.asset.is_empty())
        .map(|(e, s, p, a, g)| {
            (
                e,
                s.asset.clone(),
                p.cloned(),
                a.cloned(),
                g.map(|g| Mat4::from(g.0)),
            )
        })
        .collect();
    found.sort_by_key(|(e, ..)| e.to_bits());
    let ids: Vec<EntityId> = found.iter().map(|(e, ..)| entity_id(*e)).collect();
    sync.posed.retain(|id, _| ids.contains(id));

    for (entity, asset, player, animator, global) in found {
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
                    graph: String::new(),
                    run: None,
                    motion: RootMotion::IDENTITY,
                    world: Mat4::IDENTITY,
                    model: ModelPose::default(),
                    palette: Vec::new(),
                },
            );
        }
        let graph = animator.as_ref().map_or(String::new(), |a| a.graph.clone());
        let (changed, skin) = {
            let posed = sync.posed.get_mut(&id).expect("inserted above");
            posed.world = global.unwrap_or(Mat4::IDENTITY);
            (graph != posed.graph, posed.skin.clone())
        };
        if changed {
            let bound = if graph.is_empty() {
                None
            } else {
                sync.bind_graph(media, &graph, &asset, &skin, diagnostics)
            };
            let posed = sync.posed.get_mut(&id).expect("inserted above");
            posed.graph = graph.clone();
            posed.run = bound.map(|g| Run {
                path: graph.clone(),
                player: GraphPlayer::new(g),
                playing: true,
                speed: 1.0,
            });
            posed.pose.reset(&posed.skin);
            // A clip starts again from the beginning if the graph goes away.
            posed.clip.clear();
            posed.bound = None;
        }
        let posed = sync.posed.get_mut(&id).expect("inserted above");
        if !posed.graph.is_empty() {
            // An Animator with a graph replaces the AnimPlayer.
            if let (Some(run), Some(a)) = (&mut posed.run, &animator) {
                debug_assert_eq!(run.path, posed.graph);
                run.playing = a.playing;
                run.speed = a.speed;
            }
            continue;
        }
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
    }

    // Graphs, model poses and palettes; each skin on its own, so in parallel.
    let mut posed: Vec<&mut Posed> = sync.posed.values_mut().collect();
    let pool = re_core::task::TaskPool::global();
    let chunk = posed.len().div_ceil(pool.threads().max(1)).max(1);
    if posed.iter().filter(|p| p.run.is_some()).count() > 1 {
        pool.scope(|s| {
            for part in posed.chunks_mut(chunk) {
                s.spawn(move |_| part.iter_mut().for_each(|p| p.finish(dt)));
            }
        });
    } else {
        posed.iter_mut().for_each(|p| p.finish(dt));
    }
    sync.posed
        .values()
        .filter(|p| !p.motion.is_identity())
        .map(|p| (p.entity, p.motion))
        .collect()
}

/// Moves each entity by its graph's root motion: the translation in the
/// entity's frame (rotated and scaled by its `Transform`), then the yaw. A
/// character (`is_character`) is moved through `move_character` instead, by
/// the velocity that covers the translation in `dt`; its rotation is set
/// directly. Returns those characters' `(entity, velocity)`.
pub fn apply_root_motion(
    world: &mut World,
    motions: &[(Entity, RootMotion)],
    dt: f32,
    is_character: &dyn Fn(EntityId) -> bool,
) -> Vec<(EntityId, Vec3)> {
    let mut moves = Vec::new();
    for &(entity, m) in motions {
        let Some(mut tr) = world.ecs.get_mut::<Transform>(entity) else {
            continue;
        };
        let delta = tr.rotation * (tr.scale * m.translation);
        tr.rotation = (tr.rotation * m.rotation).normalize();
        let id = entity_id(entity);
        if is_character(id) && dt > 0.0 {
            moves.push((id, delta / dt));
        } else {
            tr.translation += delta;
        }
    }
    moves
}

/// Puts every `BoneAttachment` entity at its joint of its parent's skin: its
/// `Transform` becomes the joint's model matrix from the last step. A joint
/// or skin that is missing is reported once (`RE0522`) and the entity is left
/// where it is.
pub fn attach(world: &mut World, sync: &mut SkinSync, diagnostics: &mut Vec<Diagnostic>) {
    let attached: Vec<(Entity, String)> = world
        .ecs
        .query::<(Entity, &BoneAttachment)>()
        .iter(&world.ecs)
        .filter(|(_, b)| !b.joint.is_empty())
        .map(|(e, b)| (e, b.joint.clone()))
        .collect();
    for (entity, joint) in attached {
        let parent = hierarchy::parent(&world.ecs, entity).map(entity_id);
        let found = parent.and_then(|p| {
            let posed = sync.posed.get(&p)?;
            let index = posed.skin.joint(&joint)?;
            posed.model.joint(index)
        });
        let Some(m) = found else {
            let id = entity_id(entity);
            if sync.attach_reported.insert((id, joint.clone())) {
                diagnostics.push(
                    Diagnostic::warning(
                        codes::ANIM_TARGET_MISSING,
                        format!(
                            "BoneAttachment: joint `{joint}` is not in the skin of this entity's parent"
                        ),
                    )
                    .at(Location::entity(world.id_of(entity).unwrap_or_default())),
                );
            }
            continue;
        };
        let (scale, rotation, translation) = m.to_scale_rotation_translation();
        if let Some(mut tr) = world.ecs.get_mut::<Transform>(entity) {
            tr.translation = translation;
            tr.rotation = rotation.normalize();
            tr.scale = scale;
        }
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
