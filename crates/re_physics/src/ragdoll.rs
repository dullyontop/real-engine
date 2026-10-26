//! Ragdolls (ADR-0040): oxijolt's ragdolls on an animation skeleton described as
//! plain data ([`RagdollDesc`]).
//!
//! The parts become an oxijolt `Skeleton` in animation joint order, so a part's
//! parent is the part on its nearest ancestor joint, and `RagdollSettings` built in
//! the skeleton's neutral pose (stabilized unless the description says not). Each
//! part's body origin is its joint; its shape sits in the joint's space through a
//! one-child compound. A `SkeletonMapper` with translation locks on every descendant
//! of the root turns the simulated parts back into a pose of the whole animation
//! skeleton.
//!
//! Poses cross the boundary in the skeleton's model space, placed in the world by a
//! frame that travels with `AddRagdoll` and `SetRagdollTarget`. Jolt gets them
//! relative to the frame, with the frame's position relative to the simulation
//! origin (ADR-0042), so a ragdoll is as precise far out as near zero. `re_physics`
//! adds what Jolt leaves to the caller:
//!
//! * animated ragdolls are driven to their target every step, and teleported when
//!   the target is too far for one step;
//! * powered ragdolls aim their motors at the target every step; a new strength
//!   rebuilds the ragdoll with scaled motor springs (Jolt's settings are
//!   immutable), carrying every part's pose and velocity;
//! * whether the ragdoll is calm is counted every step, in the saved state;
//! * parts answer queries and character collisions as the ragdoll's entity, with its
//!   groups; they report no contact events and no body poses;
//! * an entity may have a character and a ragdoll (a character that can fall over):
//!   the character never meets its own ragdoll's parts;
//! * a ragdoll whose target a move of the origin leaves out of reach stays where it
//!   is, undriven, until a move or a new target brings the target back in reach.

use std::collections::BTreeMap;

use glam::{Quat, Vec3};
use oxijolt::{
    Activation, BodyId, BodySettings, CompoundChild, HingeConstraintSettings, JointTransform,
    MappedSkeleton, MotionType, MotorSettings, RagdollId, RagdollJoint, RagdollPart,
    RagdollSettings, SettleDetector, Shape, SixDofAxis, SixDofConstraintAxis,
    SixDofConstraintSettings, Skeleton, SkeletonJoint as JSkeletonJoint, SkeletonMapper,
    SkeletonPose, SpringSettings, SwingTwistConstraintSettings, SwingType, TranslationLocks,
};
use re_core::{Diagnostic, Severity};
use re_types::EntityId;
use re_types::physics::{
    GroupMask, JointPose, RagdollConstraint, RagdollDesc, RagdollMode, RagdollPartDesc,
    RagdollState,
};

use crate::codes;
use crate::convert::{gq, grv, gv, jq, jv, rv, unit_rotation};
use crate::shapes;
use crate::world::{Physics, at, in_world, object_layer, unknown};

/// The ragdoll of one entity.
pub(crate) struct RagdollEntry {
    id: RagdollId,
    /// The part bodies, in part order.
    pub(crate) bodies: Vec<BodyId>,
    /// The animation joint of each part, in part order (ascending).
    joints: Vec<usize>,
    /// The description, its parts in part order.
    desc: RagdollDesc,
    /// Each part's shape, in part order, kept to rebuild the settings.
    shapes: Vec<Shape>,
    /// The ragdoll's skeleton: one joint per part.
    skeleton: Skeleton,
    mapper: SkeletonMapper,
    pub(crate) groups: GroupMask,
    mode: RagdollMode,
    /// The factor the motor springs of the live settings were built with.
    motor_scale: f32,
    /// The frame the target is given in: world position and rotation of model space.
    frame: (Vec3, Quat),
    /// The target pose, one model-space transform per animation joint.
    target: Vec<JointPose>,
    /// The frame is out of reach of the moved origin, and that was reported.
    far: bool,
}

/// The motion of one ragdoll's parts, in a [`MotionState`](crate::MotionState).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RagdollMotion {
    pub(crate) id: EntityId,
    pub(crate) mode: RagdollMode,
    /// Per part: world position, rotation, linear and angular velocity, awake.
    pub(crate) parts: Vec<(Vec3, Quat, Vec3, Vec3, bool)>,
}

pub(crate) fn ragdoll_invalid(id: EntityId, why: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::error(codes::RAGDOLL_INVALID, format!("ragdoll {id}: {why}"))
        .at(at(id))
        .with_hint(
            "every part names a different joint of the skeleton, one part is above all the \
             others, masses and motor frequencies are positive, limits ordered within ±180°, \
             and poses have one finite transform per skeleton joint",
        )
}

/// `local` turned by a frame's rotation `rot`: placed by the frame, relative to the
/// frame's position.
fn turned(rot: Quat, local: &JointPose) -> JointPose {
    JointPose::new(Vec3::ZERO, rot).mul(local)
}

/// A Jolt pose with its root offset at `root`, from transforms relative to it.
fn skeleton_pose(root: Vec3, relative: impl Iterator<Item = JointPose>) -> SkeletonPose {
    SkeletonPose {
        root_offset: rv(root),
        joints: relative
            .map(|r| JointTransform {
                translation: jv(r.pos),
                rotation: jq(r.rot),
            })
            .collect(),
    }
}

/// The first joint of `pose` (the root first) beyond Jolt's limits, in Jolt's frame.
fn beyond(pose: &SkeletonPose) -> Option<Vec3> {
    let root = grv(pose.root_offset);
    std::iter::once(root)
        .chain(pose.joints.iter().map(|t| root + gv(t.translation)))
        .find(|&p| !in_world(p))
}

/// Whether every joint of `pose` is within Jolt's limits.
fn reachable(pose: &SkeletonPose) -> bool {
    beyond(pose).is_none()
}

/// The joint between a part at `at` (its neutral world pose) and its parent part, with
/// motor springs `scale` times the part's frequency.
fn constraint(p: &RagdollPartDesc, at: JointPose, scale: f32) -> RagdollJoint {
    let twist = (at.rot * p.twist_axis).normalize();
    let plane = at.rot * p.plane_axis;
    let plane = (plane - twist * plane.dot(twist)).normalize();
    let pos = rv(at.pos);
    let motor = MotorSettings::default().spring(SpringSettings::FrequencyAndDamping {
        frequency: p.motor_hz * scale,
        damping: 1.0,
    });
    let rad = f32::to_radians;
    match p.constraint {
        RagdollConstraint::SwingTwist {
            cone_deg,
            twist_min_deg,
            twist_max_deg,
        } => RagdollJoint::SwingTwist(
            SwingTwistConstraintSettings::new(pos, jv(twist), jv(plane))
                .half_cone_angles(rad(cone_deg), rad(cone_deg))
                .twist_limits(rad(twist_min_deg), rad(twist_max_deg))
                .max_friction_torque(p.joint_friction)
                .swing_motor(motor)
                .twist_motor(motor),
        ),
        RagdollConstraint::Hinge { min_deg, max_deg } => RagdollJoint::Hinge(
            HingeConstraintSettings::new(pos, jv(plane), jv(twist))
                .limits(rad(min_deg), rad(max_deg))
                .max_friction_torque(p.joint_friction)
                .motor(motor),
        ),
        RagdollConstraint::SixDof {
            twist_min_deg,
            twist_max_deg,
            swing_y_min_deg,
            swing_y_max_deg,
            swing_z_min_deg,
            swing_z_max_deg,
        } => {
            let limited = |min: f32, max: f32| SixDofAxis::Limited {
                min: rad(min),
                max: rad(max),
            };
            let mut s = SixDofConstraintSettings::new(pos, jv(twist), jv(plane))
                .swing_type(SwingType::Pyramid)
                .axis(
                    SixDofConstraintAxis::RotationX,
                    limited(twist_min_deg, twist_max_deg),
                )
                .axis(
                    SixDofConstraintAxis::RotationY,
                    limited(swing_y_min_deg, swing_y_max_deg),
                )
                .axis(
                    SixDofConstraintAxis::RotationZ,
                    limited(swing_z_min_deg, swing_z_max_deg),
                );
            for axis in [
                SixDofConstraintAxis::TranslationX,
                SixDofConstraintAxis::TranslationY,
                SixDofConstraintAxis::TranslationZ,
            ] {
                s = s.axis(axis, SixDofAxis::Fixed);
            }
            for axis in [
                SixDofConstraintAxis::RotationX,
                SixDofConstraintAxis::RotationY,
                SixDofConstraintAxis::RotationZ,
            ] {
                s = s.max_friction(axis, p.joint_friction).motor(axis, motor);
            }
            RagdollJoint::SixDof(s)
        }
    }
}

/// Why a part's numbers cannot build a body and joint, or `None`.
fn part_problem(p: &RagdollPartDesc) -> Option<String> {
    let positive = |v: f32| v.is_finite() && v > 0.0;
    let non_negative = |v: f32| v.is_finite() && v >= 0.0;
    let axes_ok = p.twist_axis.is_finite()
        && p.plane_axis.is_finite()
        && p.twist_axis.length() > 1e-6
        && p.twist_axis.normalize().cross(p.plane_axis).length() > 1e-3 * p.plane_axis.length()
        && p.plane_axis.length() > 1e-6;
    if !positive(p.mass) {
        Some(format!("part `{}` has mass {} kg", p.joint, p.mass))
    } else if !(non_negative(p.friction) && non_negative(p.joint_friction)) {
        Some(format!(
            "part `{}` has a negative or non-finite friction",
            p.joint
        ))
    } else if !positive(p.motor_hz) {
        Some(format!(
            "part `{}` has motor frequency {} Hz",
            p.joint, p.motor_hz
        ))
    } else if !axes_ok {
        Some(format!(
            "part `{}`: the twist and plane axes must be non-zero and not parallel",
            p.joint
        ))
    } else if !p.constraint.is_valid() {
        Some(format!(
            "part `{}` has limits out of order or beyond ±180°",
            p.joint
        ))
    } else if !(p.shape_pos.is_finite() && unit_rotation(p.shape_rot).is_some()) {
        Some(format!(
            "part `{}` has a shape offset that is not finite",
            p.joint
        ))
    } else if shapes::is_static_only(&p.shape) {
        Some(format!(
            "part `{}` has a {} shape, which only static bodies may use",
            p.joint,
            shapes::kind_name(&p.shape)
        ))
    } else {
        None
    }
}

/// The settings of a ragdoll whose parts (in part order) follow `joints` and have
/// `shapes`, with motor springs `scale` times the parts' frequencies.
fn build_settings(
    desc: &RagdollDesc,
    joints: &[usize],
    shapes: &[Shape],
    skeleton: &Skeleton,
    scale: f32,
) -> Result<RagdollSettings, String> {
    let layer = object_layer(desc.layer, true);
    let parts: Vec<RagdollPart<'_>> = desc
        .parts
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let at = desc.skeleton.joints[joints[i]].neutral;
            RagdollPart {
                shape: &shapes[i],
                body: BodySettings::new_dynamic()
                    .object_layer(layer)
                    .position(rv(at.pos))
                    .rotation(jq(at.rot))
                    .mass(p.mass)
                    .friction(p.friction)
                    .linear_damping(desc.linear_damping)
                    .angular_damping(desc.angular_damping),
                joint: (i > 0).then(|| constraint(p, at, scale)),
            }
        })
        .collect();
    let built = if desc.stabilize {
        RagdollSettings::new_stabilized(skeleton, &parts)
    } else {
        RagdollSettings::new(skeleton, &parts)
    };
    built.map_err(|e| e.to_string())
}

impl RagdollEntry {
    /// The settings with motor springs `scale` times the parts' frequencies.
    fn settings(&self, scale: f32) -> Result<RagdollSettings, String> {
        build_settings(
            &self.desc,
            &self.joints,
            &self.shapes,
            &self.skeleton,
            scale,
        )
    }

    /// The target as a Jolt pose of the parts, for a simulation origin at `origin`.
    fn target_pose(&self, origin: Vec3) -> SkeletonPose {
        let (pos, rot) = self.frame;
        skeleton_pose(
            pos - origin,
            self.joints.iter().map(|&j| turned(rot, &self.target[j])),
        )
    }

    /// The part on `joint` or on its nearest ancestor with one.
    fn part_of(&self, joint: usize) -> Option<usize> {
        let mut at = Some(joint);
        while let Some(j) = at {
            if let Ok(i) = self.joints.binary_search(&j) {
                return Some(i);
            }
            at = self.desc.skeleton.joints[j].parent.map(usize::from);
        }
        None
    }
}

impl Physics {
    pub(crate) fn add_ragdoll(
        &mut self,
        id: EntityId,
        desc: &RagdollDesc,
        pos: Vec3,
        rot: Quat,
        pose: Option<&[JointPose]>,
    ) -> Result<(), Diagnostic> {
        if self.bodies.contains_key(&id)
            || self.waiting.contains_key(&id)
            || self.ragdolls.contains_key(&id)
        {
            return Err(Diagnostic::error(
                codes::DUPLICATE_ENTITY,
                format!("entity {id} already has a body or ragdoll"),
            )
            .at(at(id))
            .with_hint("an entity has a ragdoll instead of a body; remove the body first"));
        }
        self.check_layer(id, desc.layer)?;
        let skeleton = &desc.skeleton;
        if let Some(why) = skeleton.problem() {
            return Err(ragdoll_invalid(
                id,
                format!("its skeleton is malformed: {why}"),
            ));
        }
        if desc.parts.is_empty() {
            return Err(ragdoll_invalid(id, "it has no parts"));
        }
        if !(desc.linear_damping.is_finite()
            && desc.linear_damping >= 0.0
            && desc.angular_damping.is_finite()
            && desc.angular_damping >= 0.0)
        {
            return Err(ragdoll_invalid(
                id,
                "damping must be finite and not negative",
            ));
        }
        let mut order: Vec<(usize, &RagdollPartDesc)> = Vec::with_capacity(desc.parts.len());
        for p in &desc.parts {
            let j = skeleton.joint(&p.joint).ok_or_else(|| {
                ragdoll_invalid(
                    id,
                    format!("part `{}` names no joint of the skeleton", p.joint),
                )
            })?;
            if order.iter().any(|(k, _)| *k == j) {
                return Err(ragdoll_invalid(
                    id,
                    format!("joint `{}` has two parts", p.joint),
                ));
            }
            if let Some(why) = part_problem(p) {
                return Err(ragdoll_invalid(id, why));
            }
            order.push((j, p));
        }
        order.sort_by_key(|(j, _)| *j);
        let joints: Vec<usize> = order.iter().map(|(j, _)| *j).collect();
        let parent_part = |j: usize| -> Option<u32> {
            let mut at = skeleton.joints[j].parent.map(usize::from);
            while let Some(a) = at {
                if let Ok(i) = joints.binary_search(&a) {
                    return Some(i as u32);
                }
                at = skeleton.joints[a].parent.map(usize::from);
            }
            None
        };
        let parents: Vec<Option<u32>> = joints.iter().map(|&j| parent_part(j)).collect();
        if parents.iter().skip(1).any(Option::is_none) {
            return Err(ragdoll_invalid(
                id,
                format!(
                    "no part is above all the others: `{}` and `{}` have no part above them",
                    order[0].1.joint,
                    order[parents
                        .iter()
                        .skip(1)
                        .position(Option::is_none)
                        .unwrap_or(0)
                        + 1]
                    .1
                    .joint
                ),
            ));
        }
        if !self.in_reach(pos) {
            return Err(self.far_ragdoll(id, "ragdoll", pos));
        }
        let rot = unit_rotation(rot).ok_or_else(|| ragdoll_invalid(id, "the rotation is zero"))?;
        let target = match pose {
            Some(p) => self.check_pose(id, desc, p)?,
            None => skeleton.neutral_pose(),
        };

        let mut part_shapes = Vec::with_capacity(order.len());
        for (_, p) in &order {
            let built = shapes::build(&p.shape).map_err(|d| d.at(at(id)))?;
            if let Some(w) = built.warning {
                self.push_diagnostic(w.at(at(id)));
            }
            let shape = if p.shape_pos == Vec3::ZERO && p.shape_rot == Quat::IDENTITY {
                built.shape
            } else {
                Shape::new_compound(&[CompoundChild {
                    shape: &built.shape,
                    position: jv(p.shape_pos),
                    rotation: jq(unit_rotation(p.shape_rot).unwrap_or(Quat::IDENTITY)),
                    user_data: 0,
                }])
                .map_err(|e| ragdoll_invalid(id, format!("part `{}`: {e}", p.joint)))?
            };
            part_shapes.push(shape);
        }
        let names: Vec<JSkeletonJoint<'_>> = order
            .iter()
            .zip(&parents)
            .map(|((_, p), &parent)| JSkeletonJoint {
                name: &p.joint,
                parent,
            })
            .collect();
        let ragdoll_skeleton = Skeleton::new(&names).map_err(|e| ragdoll_invalid(id, e))?;
        let anim_names: Vec<JSkeletonJoint<'_>> = skeleton
            .joints
            .iter()
            .map(|j| JSkeletonJoint {
                name: &j.name,
                parent: j.parent.map(u32::from),
            })
            .collect();
        let anim_skeleton = Skeleton::new(&anim_names).map_err(|e| ragdoll_invalid(id, e))?;
        let ragdoll_neutral = skeleton_pose(
            Vec3::ZERO,
            joints.iter().map(|&j| skeleton.joints[j].neutral),
        );
        let anim_neutral = skeleton_pose(Vec3::ZERO, skeleton.joints.iter().map(|j| j.neutral));
        let mapper = SkeletonMapper::new(
            MappedSkeleton {
                skeleton: &ragdoll_skeleton,
                neutral_pose: &ragdoll_neutral,
            },
            MappedSkeleton {
                skeleton: &anim_skeleton,
                neutral_pose: &anim_neutral,
            },
            TranslationLocks::All,
        )
        .map_err(|e| ragdoll_invalid(id, e))?;
        let mut sorted = desc.clone();
        sorted.parts = order.into_iter().map(|(_, p)| p.clone()).collect();
        let settings = build_settings(&sorted, &joints, &part_shapes, &ragdoll_skeleton, 1.0)
            .map_err(|e| ragdoll_invalid(id, e))?;
        let frame = (pos, rot);
        let start = skeleton_pose(
            pos - self.parts.origin,
            joints.iter().map(|&j| turned(rot, &target[j])),
        );
        if let Some(part) = beyond(&start) {
            return Err(self.far_ragdoll(id, "ragdoll part", part + self.parts.origin));
        }
        let rid = self
            .world
            .create_ragdoll(&settings, Some(&start), Activation::Activate)
            .map_err(|e| ragdoll_invalid(id, e))?;
        // A new ragdoll is animated: kinematic parts.
        let _ = self
            .world
            .ragdoll_mut(rid)
            .and_then(|mut r| r.set_motion_type(MotionType::Kinematic, Activation::Activate));
        let mut entry = RagdollEntry {
            id: rid,
            bodies: Vec::new(),
            joints,
            desc: sorted,
            shapes: part_shapes,
            skeleton: ragdoll_skeleton,
            mapper,
            groups: desc.groups,
            mode: RagdollMode::Animated,
            motor_scale: 1.0,
            frame,
            target,
            far: false,
        };
        self.register_parts(id, &mut entry);
        self.ragdolls.insert(id, entry);
        self.state
            .settle
            .insert(id, (SettleDetector::default(), false));
        Ok(())
    }

    /// `RE5006` for a ragdoll added with its `what` at `pos`, out of reach: an error
    /// at the world origin, a warning while the origin is moved (content left
    /// behind, ADR-0042).
    fn far_ragdoll(&self, id: EntityId, what: &str, pos: Vec3) -> Diagnostic {
        let mut d = self.out_of_reach(id, what, pos);
        if self.parts.origin != Vec3::ZERO && pos.is_finite() {
            d.severity = Severity::Warning;
            d.message = format!(
                "{}; the ragdoll is not added: add it again once the origin comes within reach",
                d.message
            );
        }
        d
    }

    /// Checks a pose for `desc`'s skeleton and returns it with normalised rotations.
    fn check_pose(
        &self,
        id: EntityId,
        desc: &RagdollDesc,
        pose: &[JointPose],
    ) -> Result<Vec<JointPose>, Diagnostic> {
        let n = desc.skeleton.joints.len();
        if pose.len() != n {
            return Err(ragdoll_invalid(
                id,
                format!("the pose has {} joints; the skeleton has {n}", pose.len()),
            ));
        }
        pose.iter()
            .map(|p| match unit_rotation(p.rot) {
                Some(rot) if in_world(p.pos) => Ok(JointPose::new(p.pos, rot)),
                _ => Err(ragdoll_invalid(
                    id,
                    "the pose has a joint that is not finite or beyond the world limits",
                )),
            })
            .collect()
    }

    /// Records the parts of `entry` (now in the world) as the entity's.
    fn register_parts(&mut self, id: EntityId, entry: &mut RagdollEntry) {
        entry.bodies = self
            .world
            .ragdoll(entry.id)
            .map(|r| r.body_ids().to_vec())
            .unwrap_or_default();
        for &b in &entry.bodies {
            self.entity_of.insert(b, id);
            self.gate.set(b, entry.groups);
            self.gate.own(b, id);
        }
    }

    /// Forgets the parts of `entry` (about to leave the world); their late events are
    /// dropped by the next step.
    fn unregister_parts(&mut self, entry: &RagdollEntry) {
        for &b in &entry.bodies {
            self.gate.forget(b);
            self.removed.push(b);
        }
    }

    pub(crate) fn remove_ragdoll(&mut self, id: EntityId) -> Result<(), Diagnostic> {
        let entry = self
            .ragdolls
            .remove(&id)
            .ok_or_else(|| unknown(id, "ragdoll"))?;
        self.unregister_parts(&entry);
        self.state.settle.remove(&id);
        self.world
            .remove_ragdoll(entry.id)
            .map_err(|e| ragdoll_invalid(id, e))
    }

    pub(crate) fn set_ragdoll_mode(
        &mut self,
        id: EntityId,
        mode: RagdollMode,
    ) -> Result<(), Diagnostic> {
        if !mode.is_valid() {
            return Err(ragdoll_invalid(
                id,
                format!("mode {mode:?}: a powered strength is from 0 to 1"),
            ));
        }
        let entry = self
            .ragdolls
            .get(&id)
            .ok_or_else(|| unknown(id, "ragdoll"))?;
        let (rid, old, built) = (entry.id, entry.mode, entry.motor_scale);
        if old == mode {
            return Ok(());
        }
        let motion = if mode.is_dynamic() {
            MotionType::Dynamic
        } else {
            MotionType::Kinematic
        };
        let mut r = self
            .world
            .ragdoll_mut(rid)
            .map_err(|e| ragdoll_invalid(id, e))?;
        r.stop_motors();
        if old.is_dynamic() != mode.is_dynamic() {
            // The parts keep their velocities: a falling character carries its momentum.
            r.set_motion_type(motion, Activation::Activate)
                .map_err(|e| ragdoll_invalid(id, e))?;
        }
        if let RagdollMode::Powered { strength } = mode
            && strength > 0.0
            && strength != built
        {
            self.rebuild_ragdoll(id, strength)?;
        }
        if let Some(entry) = self.ragdolls.get_mut(&id) {
            entry.mode = mode;
        }
        if let Some(s) = self.state.settle.get_mut(&id) {
            s.0.reset();
            s.1 = false;
        }
        Ok(())
    }

    /// Replaces a dynamic ragdoll with one whose motor springs are `scale` times the
    /// parts' frequencies, in the same pose with the same velocities.
    fn rebuild_ragdoll(&mut self, id: EntityId, scale: f32) -> Result<(), Diagnostic> {
        let Some(mut entry) = self.ragdolls.remove(&id) else {
            return Err(unknown(id, "ragdoll"));
        };
        let settings = match entry.settings(scale) {
            Ok(s) => s,
            Err(e) => {
                self.ragdolls.insert(id, entry);
                return Err(ragdoll_invalid(id, e));
            }
        };
        let (pose, motion) = match self.world.ragdoll(entry.id) {
            Ok(r) => {
                let motion: Vec<_> = r
                    .body_ids()
                    .iter()
                    .filter_map(|&b| self.world.body(b).ok())
                    .map(|b| (b.linear_velocity(), b.angular_velocity(), b.is_active()))
                    .collect();
                (r.pose(), motion)
            }
            Err(e) => {
                self.ragdolls.insert(id, entry);
                return Err(ragdoll_invalid(id, e));
            }
        };
        let created = self
            .world
            .create_ragdoll(&settings, Some(&pose), Activation::Activate);
        let rid = match created {
            Ok(rid) => rid,
            Err(e) => {
                self.ragdolls.insert(id, entry);
                return Err(ragdoll_invalid(id, e));
            }
        };
        self.unregister_parts(&entry);
        let _ = self.world.remove_ragdoll(entry.id);
        entry.id = rid;
        entry.motor_scale = scale;
        self.register_parts(id, &mut entry);
        for (&b, &(linear, angular, awake)) in entry.bodies.iter().zip(&motion) {
            if let Ok(mut body) = self.world.body_mut(b) {
                let _ = body.set_linear_velocity(linear);
                let _ = body.set_angular_velocity(angular);
                if !awake {
                    let _ = body.deactivate();
                }
            }
        }
        self.ragdolls.insert(id, entry);
        Ok(())
    }

    pub(crate) fn set_ragdoll_target(
        &mut self,
        id: EntityId,
        pos: Vec3,
        rot: Quat,
        pose: &[JointPose],
    ) -> Result<(), Diagnostic> {
        let entry = self
            .ragdolls
            .get(&id)
            .ok_or_else(|| unknown(id, "ragdoll"))?;
        let target = self.check_pose(id, &entry.desc, pose)?;
        let reported = entry.far;
        let far = !self.in_reach(pos);
        if far && (self.parts.origin == Vec3::ZERO || !pos.is_finite()) {
            return Err(self.out_of_reach(id, "target", pos));
        }
        let rot = unit_rotation(rot).ok_or_else(|| ragdoll_invalid(id, "the rotation is zero"))?;
        // A moved origin left it behind (ADR-0042): it keeps the target and follows it
        // once in reach, reported once until then.
        if far && !reported {
            let d = self.out_of_reach(id, "target", pos);
            self.push_diagnostic(Diagnostic {
                severity: Severity::Warning,
                message: format!(
                    "{}; the ragdoll stays where it is and follows its target once in reach",
                    d.message
                ),
                ..d
            });
        }
        let entry = self.ragdolls.get_mut(&id).expect("checked above");
        entry.frame = (pos, rot);
        entry.target = target;
        entry.far = far;
        Ok(())
    }

    pub(crate) fn apply_ragdoll_impulse(
        &mut self,
        id: EntityId,
        joint: &str,
        impulse: Vec3,
    ) -> Result<(), Diagnostic> {
        let entry = self
            .ragdolls
            .get(&id)
            .ok_or_else(|| unknown(id, "ragdoll"))?;
        if !entry.mode.is_dynamic() {
            return Err(ragdoll_invalid(
                id,
                "an animated ragdoll takes no impulses; make it simulated or powered first",
            ));
        }
        if !impulse.is_finite() {
            return Err(ragdoll_invalid(
                id,
                format!("impulse {impulse} is not finite"),
            ));
        }
        let part = entry
            .desc
            .skeleton
            .joint(joint)
            .and_then(|j| entry.part_of(j))
            .ok_or_else(|| {
                ragdoll_invalid(
                    id,
                    format!("`{joint}` is not a joint with a part on it or above it"),
                )
            })?;
        let body = entry.bodies[part];
        self.world
            .body_mut(body)
            .and_then(|mut b| b.add_impulse(jv(impulse)))
            .map_err(|e| ragdoll_invalid(id, e))
    }

    /// Drives animated ragdolls to their targets and aims powered ragdolls' motors.
    /// A target out of reach of the origin drives nothing until it is back in reach:
    /// animated parts stay where they are, and powered motors keep their last aim.
    pub(crate) fn drive_ragdolls(&mut self, dt: f32) {
        let origin = self.parts.origin;
        let mut failed = Vec::new();
        for (&id, r) in &self.ragdolls {
            let motors = match r.mode {
                RagdollMode::Animated => false,
                RagdollMode::Powered { strength } if strength > 0.0 => true,
                _ => continue,
            };
            let pose = r.target_pose(origin);
            if !reachable(&pose) {
                // Animated parts stop where they are: a kinematic body keeps the
                // velocity of its last drive.
                if !motors && let Ok(mut m) = self.world.ragdoll_mut(r.id) {
                    let _ = m.set_linear_and_angular_velocity(jv(Vec3::ZERO), jv(Vec3::ZERO));
                }
                continue;
            }
            let driven = if motors {
                self.world
                    .ragdoll_mut(r.id)
                    .and_then(|mut m| m.drive_to_pose_using_motors(&pose))
            } else {
                let Ok(mut m) = self.world.ragdoll_mut(r.id) else {
                    continue;
                };
                // A target too far for one step (a teleport) is jumped to.
                m.drive_to_pose_using_kinematics(&pose, dt).or_else(|_| {
                    m.set_pose(&pose)?;
                    m.set_linear_and_angular_velocity(jv(Vec3::ZERO), jv(Vec3::ZERO))
                })
            };
            if let Err(e) = driven {
                failed.push(ragdoll_invalid(id, e));
            }
        }
        for d in failed {
            self.push_diagnostic(d);
        }
    }

    /// Counts calm steps after a step.
    pub(crate) fn settle_ragdolls(&mut self) {
        for (id, r) in &self.ragdolls {
            if let (Ok(rr), Some(s)) = (self.world.ragdoll(r.id), self.state.settle.get_mut(id)) {
                s.1 = s.0.update(&rr);
            }
        }
    }

    /// The parts of an entity's ragdoll: each part's joint name and world pose.
    pub fn ragdoll_parts(&self, id: EntityId) -> Option<Vec<(String, JointPose)>> {
        let r = self.ragdolls.get(&id)?;
        Some(
            r.desc
                .parts
                .iter()
                .zip(self.jolt_parts(r))
                .map(|(p, at)| {
                    (
                        p.joint.clone(),
                        JointPose::new(at.pos + self.parts.origin, at.rot),
                    )
                })
                .collect(),
        )
    }

    /// Each part's pose in Jolt's frame (relative to the simulation origin), in part
    /// order.
    fn jolt_parts(&self, r: &RagdollEntry) -> Vec<JointPose> {
        r.bodies
            .iter()
            .filter_map(|&b| self.world.body(b).ok())
            .map(|b| JointPose::new(grv(b.position()), gq(b.rotation())))
            .collect()
    }

    /// The state of an entity's ragdoll after the last step, or `None` if it has none.
    ///
    /// Its pose covers the whole animation skeleton in model space, relative to the
    /// frame of the last `AddRagdoll` or `SetRagdollTarget`: joints with a part follow
    /// it, the joints between turn so that their chain, laid out by the target's local
    /// pose, points at the part below (oxijolt's `SkeletonMapper`), and the rest keep
    /// the target's local pose.
    pub fn ragdoll(&self, id: EntityId) -> Option<RagdollState> {
        let r = self.ragdolls.get(&id)?;
        let (fpos, frot) = r.frame;
        // The parts relative to the frame's position, from Jolt's frame: as precise far
        // from the world origin as near it, and within Jolt's limits for the mapper
        // wherever the ragdoll is.
        let jolt = self.jolt_parts(r);
        let at = fpos - self.parts.origin;
        let parts: Vec<JointPose> = jolt
            .iter()
            .map(|p| JointPose::new(p.pos - at, p.rot))
            .collect();
        let to_model = JointPose::new(Vec3::ZERO, frot).inverse();
        let ragdoll = skeleton_pose(Vec3::ZERO, parts.iter().copied());
        let target = &r.target;
        let skeleton = &r.desc.skeleton.joints;
        let local: Vec<JointTransform> = skeleton
            .iter()
            .enumerate()
            .map(|(j, joint)| {
                let l = match joint.parent {
                    None => {
                        JointPose::new(frot * target[j].pos, (frot * target[j].rot).normalize())
                    }
                    Some(p) => target[usize::from(p)].inverse().mul(&target[j]),
                };
                JointTransform {
                    translation: jv(l.pos),
                    rotation: jq(l.rot),
                }
            })
            .collect();
        let pose = match r.mapper.map(&ragdoll, &local) {
            Ok(mapped) => {
                let root = grv(mapped.root_offset);
                mapped
                    .joints
                    .iter()
                    .map(|t| {
                        to_model.mul(&JointPose::new(
                            root + gv(t.translation),
                            gq(t.rotation).normalize(),
                        ))
                    })
                    .collect()
            }
            // A chain too short to turn: parts and the target's local pose only.
            Err(_) => {
                let mut out: Vec<JointPose> = Vec::with_capacity(skeleton.len());
                for (j, joint) in skeleton.iter().enumerate() {
                    let p = match r.joints.binary_search(&j) {
                        Ok(i) => to_model.mul(&parts[i]),
                        Err(_) => match joint.parent {
                            None => target[j],
                            Some(p) => out[usize::from(p)]
                                .mul(&target[usize::from(p)].inverse().mul(&target[j])),
                        },
                    };
                    out.push(p);
                }
                out
            }
        };
        let root = jolt.first().map_or(JointPose::IDENTITY, |p| *p);
        Some(RagdollState {
            mode: r.mode,
            pos: fpos,
            rot: frot,
            root_pos: root.pos + self.parts.origin,
            root_rot: root.rot,
            calm: self.state.settle.get(&id).is_some_and(|s| s.1),
            pose,
        })
    }

    /// Whether the entity has a ragdoll.
    pub fn has_ragdoll(&self, id: EntityId) -> bool {
        self.ragdolls.contains_key(&id)
    }

    /// The number of ragdolls.
    pub fn ragdoll_count(&self) -> usize {
        self.ragdolls.len()
    }

    /// The motion of every ragdoll's parts, for [`MotionState`](crate::MotionState).
    pub(crate) fn ragdoll_motion(&self) -> Vec<RagdollMotion> {
        self.ragdolls
            .iter()
            .map(|(&id, r)| RagdollMotion {
                id,
                mode: r.mode,
                parts: r
                    .bodies
                    .iter()
                    .filter_map(|&b| self.world.body(b).ok())
                    .map(|b| {
                        (
                            self.world_point(b.position()),
                            gq(b.rotation()),
                            gv(b.linear_velocity()),
                            gv(b.angular_velocity()),
                            b.is_active(),
                        )
                    })
                    .collect(),
            })
            .collect()
    }

    /// Gives each ragdoll of `motion` that this world also has, with as many parts, the
    /// mode and the parts' poses, velocities and sleep states. A ragdoll this world
    /// does not have yet gets them when it is added (a copied world adds its ragdolls
    /// once its skins are posed).
    pub(crate) fn apply_ragdoll_motion(&mut self, motion: &[RagdollMotion]) {
        self.pending_ragdolls.clear();
        for m in motion {
            if self.ragdolls.contains_key(&m.id) {
                self.apply_one_ragdoll_motion(m);
            } else {
                self.pending_ragdolls.insert(m.id, m.clone());
            }
        }
    }

    /// Applies the motion held for `id` since [`Physics::apply_motion_state`], if any.
    pub(crate) fn apply_pending_ragdoll(&mut self, id: EntityId) {
        if let Some(m) = self.pending_ragdolls.remove(&id) {
            self.apply_one_ragdoll_motion(&m);
        }
    }

    /// The mode of the motion from [`Physics::apply_motion_state`] waiting for `id`'s
    /// ragdoll, which gets that mode when it is added.
    pub fn pending_ragdoll_mode(&self, id: EntityId) -> Option<RagdollMode> {
        self.pending_ragdolls.get(&id).map(|m| m.mode)
    }

    fn apply_one_ragdoll_motion(&mut self, m: &RagdollMotion) {
        let warn = |mut d: Diagnostic| {
            d.severity = re_core::Severity::Warning;
            d
        };
        let parts = self.ragdolls.get(&m.id).map_or(0, |r| r.bodies.len());
        if parts != m.parts.len() {
            let d = ragdoll_invalid(
                m.id,
                format!(
                    "its motion has {} parts; this world's ragdoll has {parts}",
                    m.parts.len()
                ),
            );
            self.push_diagnostic(warn(d));
            return;
        }
        if let Err(d) = self.set_ragdoll_mode(m.id, m.mode) {
            self.push_diagnostic(warn(d));
        }
        // A rebuild for the mode's strength replaced the bodies.
        let Some(r) = self.ragdolls.get(&m.id) else {
            return;
        };
        // The motion holds world positions.
        let origin = self.parts.origin;
        for (&b, &(pos, rot, linear, angular, awake)) in r.bodies.iter().zip(&m.parts) {
            let Ok(mut body) = self.world.body_mut(b) else {
                continue;
            };
            let _ =
                body.set_position_and_rotation(rv(pos - origin), jq(rot), Activation::DontActivate);
            if awake {
                let _ = body.set_linear_velocity(jv(linear));
                let _ = body.set_angular_velocity(jv(angular));
                body.activate();
            } else {
                let _ = body.deactivate();
            }
        }
    }
}

/// What a [`PhysicsSnapshot`](crate::PhysicsSnapshot) keeps of a ragdoll besides its
/// bodies, which the Jolt state carries.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RagdollSaved {
    mode: RagdollMode,
    motor_scale: f32,
    frame: (Vec3, Quat),
    target: Vec<JointPose>,
    far: bool,
}

impl Physics {
    pub(crate) fn save_ragdolls(&self) -> BTreeMap<EntityId, RagdollSaved> {
        self.ragdolls
            .iter()
            .map(|(&id, r)| {
                (
                    id,
                    RagdollSaved {
                        mode: r.mode,
                        motor_scale: r.motor_scale,
                        frame: r.frame,
                        target: r.target.clone(),
                        far: r.far,
                    },
                )
            })
            .collect()
    }

    /// Why `saved` cannot be restored into this world: other ragdolls, or ones rebuilt
    /// since (a rebuild also changes the Jolt structure, so this only names it).
    pub(crate) fn ragdolls_unrestorable(
        &self,
        saved: &BTreeMap<EntityId, RagdollSaved>,
    ) -> Option<String> {
        let same = saved.len() == self.ragdolls.len()
            && saved.iter().zip(&self.ragdolls).all(|((a, s), (b, r))| {
                a == b && s.motor_scale.to_bits() == r.motor_scale.to_bits()
            });
        (!same).then(|| "ragdolls were added, removed or rebuilt since it was saved".into())
    }

    /// Puts back each ragdoll's mode, motor scale, frame and target, and whether its
    /// target out of reach was reported; check first with
    /// [`ragdolls_unrestorable`](Self::ragdolls_unrestorable).
    pub(crate) fn restore_ragdolls(&mut self, saved: &BTreeMap<EntityId, RagdollSaved>) {
        for (id, s) in saved {
            if let Some(r) = self.ragdolls.get_mut(id) {
                r.mode = s.mode;
                r.motor_scale = s.motor_scale;
                r.frame = s.frame;
                r.target.clone_from(&s.target);
                r.far = s.far;
            }
        }
    }

    /// The ragdolls' state for a state hash, in entity order: each one's entity, mode
    /// and strength, then each part's position, rotation and velocities, as the bits of
    /// their floats (every NaN as one). Empty when there are no ragdolls.
    pub fn ragdoll_state_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let floats = |out: &mut Vec<u8>, v: &[f32]| {
            for x in v {
                let bits = if x.is_nan() {
                    f32::NAN.to_bits()
                } else {
                    x.to_bits()
                };
                out.extend_from_slice(&bits.to_le_bytes());
            }
        };
        for (&id, r) in &self.ragdolls {
            out.extend_from_slice(&id.0.to_le_bytes());
            let (tag, strength) = match r.mode {
                RagdollMode::Animated => (0u8, 0.0),
                RagdollMode::Simulated => (1, 0.0),
                RagdollMode::Powered { strength } => (2, strength),
            };
            out.push(tag);
            floats(&mut out, &[strength]);
            out.extend_from_slice(&(r.bodies.len() as u64).to_le_bytes());
            for &b in &r.bodies {
                let Ok(b) = self.world.body(b) else {
                    out.push(0);
                    continue;
                };
                out.push(1);
                floats(&mut out, &self.world_point(b.position()).to_array());
                floats(&mut out, &gq(b.rotation()).to_array());
                floats(&mut out, &gv(b.linear_velocity()).to_array());
                floats(&mut out, &gv(b.angular_velocity()).to_array());
            }
        }
        out
    }
}

/// Settle detectors by entity, in the step state.
pub(crate) type Settle = BTreeMap<EntityId, (SettleDetector, bool)>;
