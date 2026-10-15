//! Joints between assemblies: ball sockets, hinges (free, motor or servo),
//! ropes and rods.
//!
//! A joint names its two assemblies and a frame on each, relative to the
//! assembly's origin. Jolt cannot reshape a body a constraint holds, so
//! [`Physics::set_assembly_parts`] takes the assembly's joints out, reshapes
//! and puts them back from their descriptions; removing an assembly removes its
//! joints for good and reports them through [`Physics::take_dropped_joints`].

use std::collections::BTreeMap;

use glam::Vec3;
use oxijolt::{
    AnyConstraintId, ConstraintId, DistanceConstraintSettings, DistanceRange, HingeConstraint,
    HingeConstraintSettings, MotorSettings, MotorState, PointConstraintSettings, SpringSettings,
    SwingTwistConstraintSettings,
};
use re_core::Diagnostic;
use re_types::EntityId;

use super::{Pose, at};
use crate::codes;
use crate::convert::{jv, rv};
use crate::world::Physics;

/// What a hinge does about its angle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum HingeDrive {
    /// It turns freely.
    Free,
    /// It turns at `velocity` (rad/s) using at most `max_torque`.
    Motor {
        /// Target angular velocity, rad/s.
        velocity: f32,
        /// The most torque the motor uses.
        max_torque: f32,
    },
    /// It turns to `angle` (rad) at up to `speed` (rad/s) using at most
    /// `max_torque`.
    Servo {
        /// Target angle, rad.
        angle: f32,
        /// Turning speed, rad/s.
        speed: f32,
        /// The most torque the servo uses.
        max_torque: f32,
    },
}

/// The kind of a joint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JointKind {
    /// The frames' origins stay together; with `cone` the angle between the
    /// frames' X axes stays below it, and with `twist` the turn about X stays in
    /// range (radians).
    BallSocket {
        /// Largest angle between the two X axes.
        cone: Option<f32>,
        /// Twist range about X, `min <= max`.
        twist: Option<(f32, f32)>,
    },
    /// The frames' origins stay together and their X axes stay aligned.
    Hinge {
        /// Angle range, `min <= max`, within ±π.
        limits: Option<(f32, f32)>,
        /// Motor or servo.
        drive: HingeDrive,
    },
    /// The origins stay at most `length` apart.
    Rope {
        /// The longest distance.
        length: f32,
    },
    /// The origins stay exactly `length` apart.
    Rod {
        /// The distance.
        length: f32,
    },
}

/// A joint between two assemblies.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct JointDesc {
    /// The first assembly.
    pub a: EntityId,
    /// The second assembly.
    pub b: EntityId,
    /// The joint's frame on `a`, relative to `a`'s origin.
    pub frame_a: Pose,
    /// The joint's frame on `b`, relative to `b`'s origin.
    pub frame_b: Pose,
    /// What the joint keeps.
    pub kind: JointKind,
}

pub(crate) struct JointRec {
    desc: JointDesc,
    id: AnyConstraintId,
    /// A servo's current target, walked toward its angle at its speed.
    servo_at: f32,
}

#[derive(Default)]
pub(crate) struct Joints {
    pub(crate) table: BTreeMap<EntityId, JointRec>,
    dropped: Vec<EntityId>,
}

fn joint_invalid(id: EntityId, why: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::error(codes::CONSTRAINT_INVALID, format!("joint {id}: {why}"))
        .at(at(id))
        .with_hint("join two different assemblies with finite frames, lengths and limits")
}

fn torque(max: f32) -> f32 {
    if max.is_nan() {
        0.0
    } else {
        max.clamp(0.0, f32::MAX)
    }
}

fn ang_speed(v: f32) -> f32 {
    let m = oxijolt::limits::MAX_ANGULAR_VELOCITY;
    if v.is_finite() { v.clamp(-m, m) } else { 0.0 }
}

fn wrap_angle(a: f32) -> f32 {
    let pi = std::f32::consts::PI;
    (a + pi).rem_euclid(2.0 * pi) - pi
}

fn servo_spring() -> SpringSettings {
    SpringSettings::FrequencyAndDamping {
        frequency: 8.0,
        damping: 1.0,
    }
}

fn length(l: f32) -> f32 {
    if l.is_finite() {
        l.clamp(0.0, oxijolt::limits::MAX_SHAPE_EXTENT)
    } else {
        oxijolt::limits::MAX_SHAPE_EXTENT
    }
}

impl Physics {
    /// Adds (or replaces) the joint `id` between two assemblies. Fails with
    /// `RE5008` when an assembly is missing or the numbers are out of range.
    pub fn set_joint(&mut self, id: EntityId, desc: &JointDesc) -> Result<(), Diagnostic> {
        if let Some(old) = self.parts.joints.table.remove(&id) {
            let _ = self.world.remove_constraint(old.id);
        }
        self.create_joint(id, desc, None)
    }

    fn create_joint(
        &mut self,
        id: EntityId,
        desc: &JointDesc,
        servo_at: Option<f32>,
    ) -> Result<(), Diagnostic> {
        if desc.a == desc.b {
            return Err(joint_invalid(id, "it joins an assembly to itself"));
        }
        let finite = |p: &Pose| p.pos.is_finite() && p.rot.is_finite();
        if !(finite(&desc.frame_a) && finite(&desc.frame_b)) {
            return Err(joint_invalid(id, "a frame is not finite"));
        }
        let body = |k: EntityId| {
            self.parts
                .assemblies
                .get(&k)
                .map(|a| a.body)
                .ok_or_else(|| joint_invalid(id, format!("entity {k} has no assembly")))
        };
        let (ba, bb) = (body(desc.a)?, body(desc.b)?);
        let pa = self.assembly_pose_raw(ba).mul(&desc.frame_a);
        let pb = self.assembly_pose_raw(bb).mul(&desc.frame_b);
        let pa = Pose::new(pa.pos, pa.rot.normalize());
        let pb = Pose::new(pb.pos, pb.rot.normalize());
        let (xa, ya) = (pa.rot * Vec3::X, pa.rot * Vec3::Y);
        let (xb, yb) = (pb.rot * Vec3::X, pb.rot * Vec3::Y);
        let mut servo_target = 0.0;
        let created: Result<AnyConstraintId, oxijolt::ConstraintError> = match desc.kind {
            JointKind::BallSocket { cone, twist } => {
                if cone.is_none() && twist.is_none() {
                    self.world
                        .create_constraint(
                            ba,
                            bb,
                            &PointConstraintSettings::new(rv(pa.pos)).point2(rv(pb.pos)),
                        )
                        .map(Into::into)
                } else {
                    let pi = std::f32::consts::PI;
                    let c = cone.map_or(pi, |c| c.clamp(0.0, pi));
                    let (lo, hi) = twist.map_or((-pi, pi), |(lo, hi)| {
                        (lo.clamp(-pi, pi), hi.clamp(-pi, pi).max(lo.clamp(-pi, pi)))
                    });
                    let s = SwingTwistConstraintSettings::new(rv(pa.pos), jv(xa), jv(ya))
                        .frame1(rv(pa.pos), jv(xa), jv(ya))
                        .frame2(rv(pb.pos), jv(xb), jv(yb))
                        .half_cone_angles(c, c)
                        .twist_limits(lo, hi);
                    self.world.create_constraint(ba, bb, &s).map(Into::into)
                }
            }
            JointKind::Hinge { limits, drive } => {
                let mut s = HingeConstraintSettings::new(rv(pa.pos), jv(xa), jv(ya))
                    .frame1(rv(pa.pos), jv(xa), jv(ya))
                    .frame2(rv(pb.pos), jv(xb), jv(yb));
                if let Some((lo, hi)) = limits {
                    let pi = std::f32::consts::PI;
                    let lo = lo.clamp(-pi, pi);
                    s = s.limits(lo, hi.clamp(-pi, pi).max(lo));
                }
                match drive {
                    HingeDrive::Free => {}
                    HingeDrive::Motor { max_torque, .. } => {
                        let t = torque(max_torque);
                        s = s.motor(MotorSettings::default().torque_limits(-t, t));
                    }
                    HingeDrive::Servo { max_torque, .. } => {
                        let t = torque(max_torque);
                        s = s.motor(
                            MotorSettings::default()
                                .spring(servo_spring())
                                .torque_limits(-t, t),
                        );
                    }
                }
                let made = self.world.create_constraint(ba, bb, &s);
                if let Ok(cid) = made {
                    let current = self
                        .world
                        .constraint(cid)
                        .map(|c| c.current_angle())
                        .unwrap_or(0.0);
                    servo_target = servo_at.unwrap_or(current);
                    self.apply_drive(cid, drive, servo_target);
                }
                made.map(Into::into)
            }
            JointKind::Rope { length: l } => self
                .world
                .create_constraint(
                    ba,
                    bb,
                    &DistanceConstraintSettings::new(rv(pa.pos), rv(pb.pos)).range(
                        DistanceRange::Range {
                            min: 0.0,
                            max: length(l),
                        },
                    ),
                )
                .map(Into::into),
            JointKind::Rod { length: l } => {
                let l = length(l);
                self.world
                    .create_constraint(
                        ba,
                        bb,
                        &DistanceConstraintSettings::new(rv(pa.pos), rv(pb.pos))
                            .range(DistanceRange::Range { min: l, max: l }),
                    )
                    .map(Into::into)
            }
        };
        let cid = created.map_err(|e| joint_invalid(id, e))?;
        for b in [ba, bb] {
            if let Ok(mut body) = self.world.body_mut(b) {
                body.activate();
            }
        }
        self.parts.joints.table.insert(
            id,
            JointRec {
                desc: *desc,
                id: cid,
                servo_at: servo_target,
            },
        );
        Ok(())
    }

    fn apply_drive(
        &mut self,
        cid: ConstraintId<HingeConstraint>,
        drive: HingeDrive,
        at_angle: f32,
    ) {
        let Ok(mut c) = self.world.constraint_mut(cid) else {
            return;
        };
        match drive {
            HingeDrive::Free => c.set_motor_state(MotorState::Off),
            HingeDrive::Motor { velocity, .. } => {
                let _ = c.set_target_angular_velocity(ang_speed(velocity));
                c.set_motor_state(MotorState::Velocity);
            }
            HingeDrive::Servo { .. } => {
                let _ = c.set_target_angle(wrap_angle(at_angle));
                c.set_motor_state(MotorState::Position);
            }
        }
    }

    /// Changes a hinge's drive without rebuilding it. Fails with `RE5008` for
    /// a joint that is not a hinge.
    pub fn set_hinge_drive(&mut self, id: EntityId, drive: HingeDrive) -> Result<(), Diagnostic> {
        let rec = self
            .parts
            .joints
            .table
            .get(&id)
            .ok_or_else(|| joint_invalid(id, "there is no such joint"))?;
        let JointKind::Hinge { limits, drive: old } = rec.desc.kind else {
            return Err(joint_invalid(id, "it is not a hinge"));
        };
        let same_kind = std::mem::discriminant(&old) == std::mem::discriminant(&drive);
        let same_torque = match (old, drive) {
            (HingeDrive::Motor { max_torque: a, .. }, HingeDrive::Motor { max_torque: b, .. })
            | (HingeDrive::Servo { max_torque: a, .. }, HingeDrive::Servo { max_torque: b, .. }) => {
                a == b
            }
            _ => true,
        };
        if !(same_kind && same_torque) {
            let mut desc = rec.desc;
            desc.kind = JointKind::Hinge { limits, drive };
            let at_angle = rec.servo_at;
            let old = self
                .parts
                .joints
                .table
                .remove(&id)
                .expect("looked up above");
            let _ = self.world.remove_constraint(old.id);
            return self.create_joint(id, &desc, Some(at_angle));
        }
        let cid = rec
            .id
            .downcast::<HingeConstraint>()
            .expect("hinge joints are hinges");
        let at_angle = rec.servo_at;
        if let Some(rec) = self.parts.joints.table.get_mut(&id) {
            rec.desc.kind = JointKind::Hinge { limits, drive };
        }
        self.apply_drive(cid, drive, at_angle);
        for b in self
            .world
            .constraint(cid)
            .map(|c| c.bodies().to_vec())
            .unwrap_or_default()
        {
            if let Ok(mut body) = self.world.body_mut(b) {
                body.activate();
            }
        }
        Ok(())
    }

    /// A hinge's current angle, radians.
    pub fn hinge_angle(&self, id: EntityId) -> Option<f32> {
        let rec = self.parts.joints.table.get(&id)?;
        let cid = rec.id.downcast::<HingeConstraint>()?;
        self.world.constraint(cid).ok().map(|c| c.current_angle())
    }

    /// Removes a joint. Unknown ids are ignored.
    pub fn remove_joint(&mut self, id: EntityId) {
        if let Some(rec) = self.parts.joints.table.remove(&id) {
            let _ = self.world.remove_constraint(rec.id);
        }
    }

    /// Whether `id` names a joint.
    pub fn has_joint(&self, id: EntityId) -> bool {
        self.parts.joints.table.contains_key(&id)
    }

    /// The joints removed because one of their assemblies was removed, since
    /// the last call.
    pub fn take_dropped_joints(&mut self) -> Vec<EntityId> {
        std::mem::take(&mut self.parts.joints.dropped)
    }

    /// Removes and reports the joints of an assembly that is going away.
    pub(crate) fn drop_joints_of(&mut self, key: EntityId) {
        let ids: Vec<EntityId> = self
            .parts
            .joints
            .table
            .iter()
            .filter(|(_, j)| j.desc.a == key || j.desc.b == key)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            self.remove_joint(id);
            self.parts.joints.dropped.push(id);
        }
    }

    /// Takes out the joints of an assembly about to be reshaped.
    pub(crate) fn suspend_joints_of(&mut self, key: EntityId) -> Vec<(EntityId, JointDesc, f32)> {
        let ids: Vec<EntityId> = self
            .parts
            .joints
            .table
            .iter()
            .filter(|(_, j)| j.desc.a == key || j.desc.b == key)
            .map(|(id, _)| *id)
            .collect();
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(rec) = self.parts.joints.table.remove(&id) {
                let _ = self.world.remove_constraint(rec.id);
                out.push((id, rec.desc, rec.servo_at));
            }
        }
        out
    }

    /// Puts back joints taken out by [`suspend_joints_of`](Self::suspend_joints_of).
    pub(crate) fn resume_joints(&mut self, joints: Vec<(EntityId, JointDesc, f32)>) {
        for (id, desc, servo_at) in joints {
            if let Err(d) = self.create_joint(id, &desc, Some(servo_at)) {
                self.parts.joints.dropped.push(id);
                self.push_diagnostic(d);
            }
        }
    }

    /// Before a step: servos walk their targets toward their angles.
    pub(crate) fn drive_joints(&mut self, dt: f32) {
        let mut moves = Vec::new();
        for (id, rec) in &mut self.parts.joints.table {
            if let JointKind::Hinge {
                drive: HingeDrive::Servo { angle, speed, .. },
                ..
            } = rec.desc.kind
            {
                let goal = wrap_angle(if angle.is_finite() { angle } else { 0.0 });
                let diff = wrap_angle(goal - rec.servo_at);
                let step = speed.abs().min(1e6) * dt;
                if diff.abs() < 1e-6 {
                    continue;
                }
                let next = if diff.abs() <= step {
                    goal
                } else {
                    wrap_angle(rec.servo_at + step * diff.signum())
                };
                rec.servo_at = next;
                if let Some(cid) = rec.id.downcast::<HingeConstraint>() {
                    moves.push((*id, cid, next));
                }
            }
        }
        for (_, cid, next) in moves {
            if let Ok(mut c) = self.world.constraint_mut(cid) {
                let _ = c.set_target_angle(next);
            }
            for b in self
                .world
                .constraint(cid)
                .map(|c| c.bodies().to_vec())
                .unwrap_or_default()
            {
                if let Ok(mut body) = self.world.body_mut(b) {
                    body.activate();
                }
            }
        }
    }
}
