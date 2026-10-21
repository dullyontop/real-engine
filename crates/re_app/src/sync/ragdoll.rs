//! Ragdoll sync (ADR-0040): each `Ragdoll` on a posed `SkinnedMesh` gets a
//! physics ragdoll generated on the skin's skeleton, follows the animation, and
//! shows the simulated pose blended in while it is simulated or powered.
//!
//! [`push`] runs before the physics step: it adds a ragdoll once the skin is
//! posed (and rebuilds it when the skin or the component's parts change), sends
//! the component's mode when that field is edited, and sets this step's target
//! from the animated pose, placed by the entity's world transform. Scripts switch
//! modes with `physics.ragdoll`, which holds until the next edit of the field.
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
use re_scene::{Ragdoll, RagdollMode as SceneMode, SkinnedMesh, World, entity_id};
use re_types::physics::{
    JointPose, PhysicsCmd, RagdollBlend, RagdollDesc, RagdollMode, RagdollSkeleton, SkeletonJoint,
    blend_pose,
};
use re_types::{EntityId, SkinData};

use crate::codes;
use crate::sync::anim_skin::SkinSync;
use crate::sync::physics::world_affine;

/// The name of the root joint added above the skin's joints.
const ROOT: &str = "<skin root>";

/// What a linked ragdoll was built from.
#[derive(Clone, Debug, PartialEq)]
struct Built {
    skin: usize,
    joints: Vec<String>,
    total_mass: f32,
    scale: f32,
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
    /// Entities whose ragdoll could not be built, with what it was built from, so
    /// each problem is reported once.
    refused: BTreeMap<EntityId, Built>,
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
        SceneMode::Animated => RagdollMode::Animated,
        SceneMode::Simulated => RagdollMode::Simulated,
        SceneMode::Powered => RagdollMode::Powered {
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

    for (entity, component) in found {
        let id = entity_id(entity);
        let (Some(skin), Some(matrices)) = (skins.skin_arc(id), skins.joint_matrices(id)) else {
            // Not posed (yet): a ragdoll whose skin went away goes too.
            if sync.linked.remove(&id).is_some() {
                cmds.push(PhysicsCmd::RemoveRagdoll { id });
            }
            continue;
        };
        let (scale3, rot, pos) = world_affine(world, entity).to_scale_rotation_translation();
        let rot = rot.normalize();
        let scale = scale3.x.abs();
        let built = Built {
            skin: Arc::as_ptr(&skin) as usize,
            joints: component.joints.clone(),
            total_mass: component.total_mass,
            scale,
        };
        if sync.linked.get(&id).is_some_and(|l| l.built != built) {
            sync.linked.remove(&id);
            cmds.push(PhysicsCmd::RemoveRagdoll { id });
        }
        let pose = animated(matrices, scale);
        if !sync.linked.contains_key(&id) {
            if sync.refused.get(&id) == Some(&built) {
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
                    let mode = mode_of(&component);
                    cmds.push(PhysicsCmd::AddRagdoll {
                        id,
                        desc,
                        pos,
                        rot,
                        pose: Some(pose.clone()),
                    });
                    if mode != RagdollMode::Animated {
                        cmds.push(PhysicsCmd::SetRagdollMode { id, mode });
                    }
                    let seconds = component.blend_seconds;
                    sync.linked.insert(
                        id,
                        Linked {
                            built,
                            mode,
                            blend: RagdollBlend {
                                weight: 0.0,
                                seconds,
                            },
                        },
                    );
                    sync.refused.remove(&id);
                }
                Err(why) => {
                    diagnostics.push(unavailable(
                        entity,
                        world,
                        format!("the ragdoll on skin `{}` cannot be built: {why}", skin.name),
                    ));
                    sync.refused.insert(id, built);
                    continue;
                }
            }
        }
        let linked = sync.linked.get_mut(&id).expect("linked above");
        linked.blend.seconds = component.blend_seconds;
        let mode = mode_of(&component);
        if mode != linked.mode {
            linked.mode = mode;
            cmds.push(PhysicsCmd::SetRagdollMode { id, mode });
        }
        cmds.push(PhysicsCmd::SetRagdollTarget { id, pos, rot, pose });
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
