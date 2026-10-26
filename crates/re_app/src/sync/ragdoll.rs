//! Ragdoll sync (ADR-0040): each `Ragdoll` on a posed `SkinnedMesh` gets a
//! physics ragdoll generated on the skin's skeleton, follows the animation, and
//! shows the simulated pose blended in while it is simulated or powered.
//!
//! [`push`] runs before the physics step: it adds a ragdoll once the skin is
//! posed (and rebuilds it when the skin or the component's parts change; one
//! physics refused waits for such a change), sends
//! the component's mode when that field is edited, and sets this step's target
//! from the animated pose, placed by the entity's world transform. Scripts switch
//! modes with `physics.ragdoll`, which holds until the next edit of the field.
//! An entity with a joint out of reach of a moved simulation origin (ADR-0042)
//! gets its ragdoll, its rebuild and its targets once the origin comes within
//! reach of all of them; until then physics keeps its ragdoll where it is.
//!
//! [`pull`] runs after `PostSimulation`: it moves each ragdoll's blend weight
//! toward 1 while physics simulates it and toward 0 while it is animated, and
//! blends the skin's joint matrices from the animated pose toward the simulated
//! one (`blend_pose`), keeping each joint's animated scale. Physics never blends,
//! so blending cannot change a replay.
//!
//! The skeleton is the skin's joints under one added root joint at the model's
//! origin, so skins with several roots work. Model space is in metres after the
//! entity's uniform world scale; a non-uniform scale is refused.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use glam::{Mat4, Vec3};
use re_anim::skin::{ModelPose, Pose};
use re_core::{Diagnostic, Location};
use re_physics::Physics;
use re_scene::ecs::entity::Entity;
use re_scene::{Ragdoll, RagdollControl, SkinnedMesh, World, entity_id};
use re_types::physics::{
    JointPose, PhysicsCmd, RagdollBlend, RagdollDesc, RagdollMode, RagdollSkeleton, SkeletonJoint,
    blend_pose,
};
use re_types::{EntityId, SkinData};

use crate::codes;
use crate::sync::anim_skin::SkinSync;
use crate::sync::physics::{world_affine, world_scale};

/// The name of the root joint added above the skin's joints.
const ROOT: &str = "<skin root>";

/// What a linked ragdoll was built from.
#[derive(Clone, Debug)]
struct Built {
    skin: Arc<SkinData>,
    joints: Vec<String>,
    total_mass: f32,
    scale: f32,
}

/// The same skin, and the same numbers bit for bit, so a NaN matches itself and an
/// unchanged component is never rebuilt.
impl PartialEq for Built {
    fn eq(&self, other: &Built) -> bool {
        Arc::ptr_eq(&self.skin, &other.skin)
            && self.joints == other.joints
            && self.total_mass.to_bits() == other.total_mass.to_bits()
            && self.scale.to_bits() == other.scale.to_bits()
    }
}

/// The same mode, a strength compared by its bits, so an invalid (NaN) strength is
/// sent, and refused, once rather than on every step.
fn same_mode(a: RagdollMode, b: RagdollMode) -> bool {
    match (a, b) {
        (RagdollMode::Powered { strength: x }, RagdollMode::Powered { strength: y }) => {
            x.to_bits() == y.to_bits()
        }
        _ => a == b,
    }
}

struct Linked {
    built: Built,
    /// The component's mode and strength last seen.
    mode: RagdollMode,
    blend: RagdollBlend,
}

/// The ragdolls `re_app` has added to physics.
#[derive(Default)]
pub struct RagdollSync {
    linked: BTreeMap<EntityId, Linked>,
    /// Entities whose ragdoll could not be built or physics refused, with what
    /// it was built from and, for a refusal, physics' simulation origin then,
    /// so each problem is reported once (a ragdoll refused while the origin
    /// is moved is tried again when the origin moves).
    refused: BTreeMap<EntityId, (Built, Option<Vec3>)>,
}

impl RagdollSync {
    /// No ragdolls yet.
    pub fn new() -> RagdollSync {
        RagdollSync::default()
    }

    /// True when `id` has a linked ragdoll.
    pub fn has(&self, id: EntityId) -> bool {
        self.linked.contains_key(&id)
    }

    /// The blend weight of `id`'s ragdoll: 0 shows the animation, 1 the ragdoll.
    pub fn weight(&self, id: EntityId) -> Option<f32> {
        self.linked.get(&id).map(|l| l.blend.weight)
    }
}

fn mode_of(r: &Ragdoll) -> RagdollMode {
    match r.mode {
        RagdollControl::Animated => RagdollMode::Animated,
        RagdollControl::Simulated => RagdollMode::Simulated,
        RagdollControl::Powered => RagdollMode::Powered {
            strength: r.strength,
        },
    }
}

/// A model matrix as a rigid pose in physics units (`scale` metres per model unit),
/// and its own scale.
fn rigid(m: &Mat4, scale: f32) -> (JointPose, Vec3) {
    let (s, r, t) = m.to_scale_rotation_translation();
    (JointPose::new(t * scale, r.normalize()), s)
}

/// The skeleton of `skin` under an added root, in its rest pose.
fn skeleton(skin: &SkinData, scale: f32) -> RagdollSkeleton {
    let mut rest = ModelPose::default();
    rest.compute(skin, &Pose::rest(skin));
    let mut joints = Vec::with_capacity(skin.joints.len() + 1);
    joints.push(SkeletonJoint {
        name: ROOT.into(),
        parent: None,
        neutral: JointPose::IDENTITY,
    });
    for (j, m) in skin.joints.iter().zip(&rest.joints) {
        joints.push(SkeletonJoint {
            name: j.name.clone(),
            parent: Some(j.parent.map_or(0, |p| p.saturating_add(1))),
            neutral: rigid(m, scale).0,
        });
    }
    RagdollSkeleton { joints }
}

/// The animated pose of every skeleton joint (the added root first).
fn animated(matrices: &[Mat4], scale: f32) -> Vec<JointPose> {
    std::iter::once(JointPose::IDENTITY)
        .chain(matrices.iter().map(|m| rigid(m, scale).0))
        .collect()
}

fn unavailable(entity: Entity, world: &World, why: String) -> Diagnostic {
    Diagnostic::error(codes::RAGDOLL_UNAVAILABLE, why)
        .at(Location::entity(world.id_of(entity).unwrap_or("?")))
        .with_hint(
            "list the joints that get parts in `Ragdoll.joints`, or run `real explain RE0529`",
        )
}

/// Adds, rebuilds and removes ragdolls, sends edited modes and sets this step's
/// targets.
pub fn push(
    world: &mut World,
    physics: &Physics,
    skins: &SkinSync,
    sync: &mut RagdollSync,
    cmds: &mut Vec<PhysicsCmd>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut found: Vec<(Entity, Ragdoll)> = world
        .ecs
        .query::<(Entity, &Ragdoll, &SkinnedMesh)>()
        .iter(&world.ecs)
        .map(|(e, r, _)| (e, r.clone()))
        .collect();
    found.sort_by_key(|(e, _)| e.to_bits());
    let world = &*world;
    let present: BTreeSet<EntityId> = found.iter().map(|(e, _)| entity_id(*e)).collect();
    let gone: Vec<EntityId> = sync
        .linked
        .keys()
        .filter(|id| !present.contains(id))
        .copied()
        .collect();
    for id in gone {
        sync.linked.remove(&id);
        cmds.push(PhysicsCmd::RemoveRagdoll { id });
    }
    sync.refused.retain(|id, _| present.contains(id));
    // A ragdoll physics refused to add (it reported why) is not retried until what
    // it was built from changes.
    let lost: Vec<EntityId> = sync
        .linked
        .keys()
        .filter(|&&id| !physics.has_ragdoll(id))
        .copied()
        .collect();
    for id in lost {
        if let Some(linked) = sync.linked.remove(&id) {
            sync.refused
                .insert(id, (linked.built, Some(physics.origin())));
        }
    }

    for (entity, component) in found {
        let id = entity_id(entity);
        let (Some(skin), Some(matrices)) = (skins.skin_arc(id), skins.joint_matrices(id)) else {
            // Not posed (yet): a ragdoll whose skin went away goes too.
            if sync.linked.remove(&id).is_some() {
                cmds.push(PhysicsCmd::RemoveRagdoll { id });
            }
            continue;
        };
        let (_, rot, pos) = world_affine(world, entity).to_scale_rotation_translation();
        let rot = rot.normalize();
        // The product of the `Transform` scales, which a rotation leaves exact, so
        // a turning entity is not rebuilt.
        let scale3 = world_scale(world, entity);
        let scale = scale3.x.abs();
        let built = Built {
            skin: Arc::clone(&skin),
            joints: component.joints.clone(),
            total_mass: component.total_mass,
            scale,
        };
        let pose = animated(matrices, scale);
        // Left behind by a moved origin, or a joint of it is: physics would refuse
        // it, so it is added or rebuilt once the origin comes within reach of every
        // joint (the added root is the entity's position), and gets no target until
        // then (physics keeps it where it is, and a waiting rebuild's pose may not
        // fit the old ragdoll). A position that is not finite goes to physics, which
        // reports it.
        let far = physics.origin() != Vec3::ZERO
            && pose.iter().any(|j| {
                let at = pos + rot * j.pos;
                at.is_finite() && !physics.in_reach(at)
            });
        // A rebuild keeps the mode physics has (a script may have set it), the
        // component's mode last seen and the blend.
        let mut kept = None;
        if !far
            && sync.linked.get(&id).is_some_and(|l| l.built != built)
            && let Some(old) = sync.linked.remove(&id)
        {
            let now = physics.ragdoll(id).map_or(old.mode, |s| s.mode);
            kept = Some((now, old.mode, old.blend.weight));
            cmds.push(PhysicsCmd::RemoveRagdoll { id });
        }
        if !sync.linked.contains_key(&id) {
            if far
                || sync
                    .refused
                    .get(&id)
                    .is_some_and(|(b, o)| *b == built && o.is_none_or(|o| o == physics.origin()))
            {
                continue;
            }
            let desc = if (scale3.abs() - Vec3::splat(scale)).abs().max_element() > 1e-3 * scale {
                Err(format!(
                    "the ragdoll needs a uniform world scale, but the entity's is {scale3}"
                ))
            } else {
                describe(&skin, &component, scale)
            };
            match desc {
                Ok(desc) => {
                    let seen = mode_of(&component);
                    cmds.push(PhysicsCmd::AddRagdoll {
                        id,
                        desc,
                        pos,
                        rot,
                        pose: Some(pose.clone()),
                    });
                    // A copied world's ragdoll gets the mode it was copied with, and
                    // shows it at once; a rebuilt one keeps its mode and blend.
                    let copied = physics.pending_ragdoll_mode(id);
                    let (mode, seen, weight) = match (kept, copied) {
                        (Some(k), _) => k,
                        (None, Some(m)) => (m, seen, if m.is_dynamic() { 1.0 } else { 0.0 }),
                        (None, None) => (seen, seen, 0.0),
                    };
                    if mode != RagdollMode::Animated && copied.is_none() {
                        cmds.push(PhysicsCmd::SetRagdollMode { id, mode });
                    }
                    let seconds = component.blend_seconds;
                    sync.linked.insert(
                        id,
                        Linked {
                            built,
                            mode: seen,
                            blend: RagdollBlend { weight, seconds },
                        },
                    );
                    sync.refused.remove(&id);
                    // `AddRagdoll` carries this step's pose.
                    continue;
                }
                Err(why) => {
                    diagnostics.push(unavailable(
                        entity,
                        world,
                        format!("the ragdoll on skin `{}` cannot be built: {why}", skin.name),
                    ));
                    sync.refused.insert(id, (built, None));
                    continue;
                }
            }
        }
        let linked = sync.linked.get_mut(&id).expect("linked above");
        linked.blend.seconds = component.blend_seconds;
        let mode = mode_of(&component);
        if !same_mode(mode, linked.mode) {
            linked.mode = mode;
            cmds.push(PhysicsCmd::SetRagdollMode { id, mode });
        }
        if !far {
            cmds.push(PhysicsCmd::SetRagdollTarget { id, pos, rot, pose });
        }
    }
}

/// The ragdoll the component asks for on `skin`.
fn describe(skin: &SkinData, component: &Ragdoll, scale: f32) -> Result<RagdollDesc, String> {
    let skeleton = skeleton(skin, scale);
    if let Some(why) = skeleton.problem() {
        return Err(why);
    }
    let names: Vec<&str> = if component.joints.is_empty() {
        let found = skeleton.humanoid_parts();
        if found.is_empty() {
            return Err("no joint has a humanoid name (hips, spine, head, arms, legs)".into());
        }
        found
    } else {
        component.joints.iter().map(String::as_str).collect()
    };
    RagdollDesc::from_skeleton(&skeleton, &names, component.total_mass)
}

/// Blends each ragdoll's simulated pose into its skin's joint matrices, by its
/// weight, moved `dt` seconds toward the physics mode's.
pub fn pull(physics: &Physics, skins: &mut SkinSync, sync: &mut RagdollSync, dt: f32) {
    for (&id, linked) in &mut sync.linked {
        let Some(state) = physics.ragdoll(id) else {
            continue;
        };
        let target = if state.mode.is_dynamic() { 1.0 } else { 0.0 };
        linked.blend.advance(target, dt);
        let weight = linked.blend.weight;
        if weight <= 0.0 {
            continue;
        }
        let scale = linked.built.scale;
        skins.override_joints(id, |joints| {
            let shown = animated(joints, scale);
            let blended = blend_pose(&shown, &state.pose, weight);
            for (m, b) in joints.iter_mut().zip(blended.iter().skip(1)) {
                let (_, s) = rigid(m, scale);
                let pos = if scale > 0.0 { b.pos / scale } else { b.pos };
                *m = Mat4::from_scale_rotation_translation(s, b.rot, pos);
            }
        });
    }
}
