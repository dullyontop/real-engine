//! Attachments, joint and constraint state, and the constraints that are not
//! rigid: the joints (`BallSocketConstraint`, `HingeConstraint`,
//! `RopeConstraint`, `RodConstraint`), which become `re_physics` joints
//! between two assemblies, and the movers (`VectorForce`, `LinearVelocity`,
//! `AngularVelocity`, `AlignPosition`, `AlignOrientation`), which push their
//! first attachment's assembly every substep.
//!
//! A constraint acts on the assemblies of its attachments' parent parts, at
//! the attachments' frames. It is idle while it is disabled, an attachment is
//! missing or not on a simulated part, or (for the joints) both attachments
//! are on the same assembly.

use std::sync::Arc;

use glam::{Mat3, Quat, Vec3};
use re_physics::{HingeDrive, JointDesc, JointKind, Pose};
use re_reflect::Value;
use re_scene::{Entity, World};
use re_script::codes::BAD_ARGUMENT;
use re_types::EntityId;
use re_types::physics::BodyKind;

use super::layout::Field;
use super::pose::{cframe_value, from_orientation, orientation_of, pose_of, world_pose};
use super::read_sim;
use super::sim::{ConKind, Sim};
use crate::class::{Accessor, ClassMarker};
use crate::error::CompatError;
use crate::instance::tree;

fn bad(message: impl Into<String>) -> CompatError {
    CompatError::Runtime {
        code: BAD_ARGUMENT,
        message: message.into(),
    }
}

fn class_of(w: &World, e: Entity) -> &str {
    w.ecs
        .get::<ClassMarker>(e)
        .map(|m| m.class.as_str())
        .unwrap_or("")
}

/// The field `name` of `class`'s own component.
fn field(w: &World, class: &str, name: &str) -> Option<Field> {
    Field::find(w, class, name)
}

fn num(w: &World, e: Entity, class: &str, name: &str, default: f32) -> f32 {
    field(w, class, name).map_or(default, |f| f.f32(w, e, default))
}

fn flag(w: &World, e: Entity, class: &str, name: &str, default: bool) -> bool {
    field(w, class, name).map_or(default, |f| f.bool(w, e, default))
}

fn vec(w: &World, e: Entity, class: &str, name: &str) -> Vec3 {
    field(w, class, name).map_or(Vec3::ZERO, |f| f.vec3(w, e, Vec3::ZERO))
}

fn item(w: &World, e: Entity, class: &str, name: &str) -> String {
    field(w, class, name)
        .and_then(|f| f.str(w, e))
        .unwrap_or_default()
}

/// An attachment's `CFrame`, relative to its parent part.
pub(crate) fn attachment_cframe(w: &World, a: Entity) -> Pose {
    field(w, "Attachment", "CFrame").map_or(Pose::IDENTITY, |f| f.pose(w, a))
}

/// The world pose of an attachment's parent (identity when it has none with
/// a frame).
fn attachment_parent_pose(w: &World, a: Entity) -> Pose {
    tree::parent(w, a)
        .and_then(|p| world_pose(w, p))
        .unwrap_or(Pose::IDENTITY)
}

/// An attachment's frame in the world.
pub(crate) fn attachment_world(w: &World, a: Entity) -> Pose {
    attachment_parent_pose(w, a).mul(&attachment_cframe(w, a))
}

/// A constraint's two attachments (when set to attachments).
fn attachments(w: &World, e: Entity) -> (Option<Entity>, Option<Entity>) {
    let get = |n: &str| {
        field(w, "Constraint", n)
            .and_then(|f| f.entity(w, e))
            .filter(|a| class_of(w, *a) == "Attachment")
    };
    (get("Attachment0"), get("Attachment1"))
}

/// The parts a constraint's attachments are on.
pub(crate) fn constraint_parts(w: &World, e: Entity) -> (Option<Entity>, Option<Entity>) {
    let (a, b) = attachments(w, e);
    (
        a.and_then(|a| tree::parent(w, a)),
        b.and_then(|b| tree::parent(w, b)),
    )
}

/// Where an attachment sits on its assembly: the assembly's key and the
/// attachment's frame relative to the assembly's origin.
fn on_assembly(sim: &Sim, w: &World, a: Entity) -> Option<(EntityId, Pose)> {
    let part = tree::parent(w, a)?;
    let rec = sim.parts.get(&part)?;
    let key = rec.asm?;
    Some((key, rec.local.mul(&attachment_cframe(w, a))))
}

fn deg(x: f32) -> f32 {
    x.to_radians()
}

/// The `re_physics` joint a joint constraint asks for now, if it is active.
pub(crate) fn joint_desc(sim: &Sim, w: &World, e: Entity, kind: ConKind) -> Option<JointDesc> {
    if !flag(w, e, "Constraint", "Enabled", true) {
        return None;
    }
    let (a0, a1) = attachments(w, e);
    let (ka, fa) = on_assembly(sim, w, a0?)?;
    let (kb, fb) = on_assembly(sim, w, a1?)?;
    if ka == kb {
        return None;
    }
    let joint = match kind {
        ConKind::BallSocket => {
            let c = "BallSocketConstraint";
            JointKind::BallSocket {
                cone: flag(w, e, c, "LimitsEnabled", false)
                    .then(|| deg(num(w, e, c, "UpperAngle", 45.0).clamp(0.0, 180.0))),
                twist: flag(w, e, c, "TwistLimitsEnabled", false).then(|| {
                    let lo = deg(num(w, e, c, "TwistLowerAngle", -45.0).clamp(-180.0, 180.0));
                    let hi = deg(num(w, e, c, "TwistUpperAngle", 45.0).clamp(-180.0, 180.0));
                    (lo.min(hi), lo.max(hi))
                }),
            }
        }
        ConKind::Hinge => {
            let c = "HingeConstraint";
            let limits = flag(w, e, c, "LimitsEnabled", false).then(|| {
                let lo = deg(num(w, e, c, "LowerAngle", -45.0).clamp(-180.0, 180.0));
                let hi = deg(num(w, e, c, "UpperAngle", 45.0).clamp(-180.0, 180.0));
                (lo.min(hi), lo.max(hi))
            });
            let drive = match item(w, e, c, "ActuatorType").as_str() {
                "Motor" => HingeDrive::Motor {
                    velocity: num(w, e, c, "AngularVelocity", 0.0),
                    max_torque: num(w, e, c, "MotorMaxTorque", 0.0).max(0.0),
                },
                "Servo" => HingeDrive::Servo {
                    angle: deg(num(w, e, c, "TargetAngle", 0.0)),
                    speed: num(w, e, c, "AngularSpeed", 0.0).max(0.0),
                    max_torque: num(w, e, c, "ServoMaxTorque", 0.0).max(0.0),
                },
                _ => HingeDrive::Free,
            };
            JointKind::Hinge { limits, drive }
        }
        ConKind::Rope => JointKind::Rope {
            length: num(w, e, "RopeConstraint", "Length", 5.0).max(0.0),
        },
        ConKind::Rod => JointKind::Rod {
            length: num(w, e, "RodConstraint", "Length", 5.0).max(0.0),
        },
        _ => return None,
    };
    Some(JointDesc {
        a: ka,
        b: kb,
        frame_a: fa,
        frame_b: fb,
        kind: joint,
    })
}

/// The frame a mover's direction is given in.
#[derive(Clone, Copy, Debug)]
enum Rel {
    World,
    /// Attachment0's frame.
    A0,
    /// Attachment1's frame, on an assembly or fixed.
    A1(Anchor),
}

/// Where a target sits: on an assembly (it moves with it) or fixed.
#[derive(Clone, Copy, Debug)]
enum Anchor {
    On(EntityId, Pose),
    Fixed(Pose),
}

#[derive(Clone, Copy, Debug)]
enum MoverKind {
    Force {
        force: Vec3,
        rel: Rel,
        at_com: bool,
    },
    LinearVelocity {
        /// `Some(direction)` for `Line` mode.
        line: Option<Vec3>,
        velocity: Vec3,
        speed: f32,
        max_force: f32,
        rel: Rel,
    },
    AngularVelocity {
        velocity: Vec3,
        max_torque: f32,
        rel: Rel,
    },
    AlignPosition {
        target: Anchor,
        max_force: f32,
        max_velocity: f32,
        responsiveness: f32,
        rigid: bool,
        at_com: bool,
    },
    AlignOrientation {
        target: Anchor,
        max_torque: f32,
        max_angular: f32,
        responsiveness: f32,
        rigid: bool,
    },
}

/// One active mover for this frame.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Mover {
    key: EntityId,
    /// Attachment0's frame on the assembly.
    frame: Pose,
    kind: MoverKind,
}

fn anchor(sim: &Sim, w: &World, a: Entity) -> Anchor {
    match on_assembly(sim, w, a) {
        Some((k, f)) => Anchor::On(k, f),
        None => Anchor::Fixed(attachment_world(w, a)),
    }
}

fn rel(sim: &Sim, w: &World, e: Entity, class: &str, a1: Option<Entity>) -> Rel {
    match item(w, e, class, "RelativeTo").as_str() {
        "Attachment0" => Rel::A0,
        "Attachment1" => match a1 {
            Some(a) => Rel::A1(anchor(sim, w, a)),
            None => Rel::World,
        },
        _ => Rel::World,
    }
}

fn limit(x: f32) -> f32 {
    if x.is_nan() { 0.0 } else { x.max(0.0) }
}

/// The movers that act this frame.
pub(crate) fn movers(sim: &Sim, w: &World) -> Vec<Mover> {
    let mut out = Vec::new();
    for (&e, c) in &sim.constraints {
        if c.kind.is_joint() || !flag(w, e, "Constraint", "Enabled", true) {
            continue;
        }
        let (a0, a1) = attachments(w, e);
        let Some((key, frame)) = a0.and_then(|a| on_assembly(sim, w, a)) else {
            continue;
        };
        let kind = match c.kind {
            ConKind::VectorForce => {
                let k = "VectorForce";
                MoverKind::Force {
                    force: vec(w, e, k, "Force"),
                    rel: rel(sim, w, e, k, a1),
                    at_com: flag(w, e, k, "ApplyAtCenterOfMass", false),
                }
            }
            ConKind::LinearVelocity => {
                let k = "LinearVelocity";
                let mode = item(w, e, k, "VelocityConstraintMode");
                if mode == "Plane" {
                    continue;
                }
                let max = if flag(w, e, k, "ForceLimitsEnabled", true) {
                    limit(num(w, e, k, "MaxForce", 0.0))
                } else {
                    f32::INFINITY
                };
                MoverKind::LinearVelocity {
                    line: (mode == "Line")
                        .then(|| vec(w, e, k, "LineDirection").normalize_or_zero()),
                    velocity: vec(w, e, k, "VectorVelocity"),
                    speed: num(w, e, k, "LineVelocity", 0.0),
                    max_force: max,
                    rel: rel(sim, w, e, k, a1),
                }
            }
            ConKind::AngularVelocity => {
                let k = "AngularVelocity";
                MoverKind::AngularVelocity {
                    velocity: vec(w, e, k, "AngularVelocity"),
                    max_torque: limit(num(w, e, k, "MaxTorque", 0.0)),
                    rel: rel(sim, w, e, k, a1),
                }
            }
            ConKind::AlignPosition => {
                let k = "AlignPosition";
                let target = if item(w, e, k, "Mode") == "OneAttachment" {
                    Anchor::Fixed(Pose::new(vec(w, e, k, "Position"), Quat::IDENTITY))
                } else {
                    match a1 {
                        Some(a) => anchor(sim, w, a),
                        None => continue,
                    }
                };
                MoverKind::AlignPosition {
                    target,
                    max_force: limit(num(w, e, k, "MaxForce", 0.0)),
                    max_velocity: limit(num(w, e, k, "MaxVelocity", f32::INFINITY)),
                    responsiveness: num(w, e, k, "Responsiveness", 10.0).clamp(0.0, 200.0),
                    rigid: flag(w, e, k, "RigidityEnabled", false),
                    at_com: flag(w, e, k, "ApplyAtCenterOfMass", false),
                }
            }
            ConKind::AlignOrientation => {
                let k = "AlignOrientation";
                let target = if item(w, e, k, "Mode") == "OneAttachment" {
                    let cf = field(w, k, "CFrame").map_or(Pose::IDENTITY, |f| f.pose(w, e));
                    Anchor::Fixed(Pose::new(Vec3::ZERO, cf.rot))
                } else {
                    match a1 {
                        Some(a) => anchor(sim, w, a),
                        None => continue,
                    }
                };
                MoverKind::AlignOrientation {
                    target,
                    max_torque: limit(num(w, e, k, "MaxTorque", 0.0)),
                    max_angular: limit(num(w, e, k, "MaxAngularVelocity", f32::INFINITY)),
                    responsiveness: num(w, e, k, "Responsiveness", 10.0).clamp(0.0, 200.0),
                    rigid: flag(w, e, k, "RigidityEnabled", false),
                }
            }
            _ => continue,
        };
        out.push(Mover { key, frame, kind });
    }
    out
}

fn clamp_len(v: Vec3, max: f32) -> Vec3 {
    if !v.is_finite() {
        return Vec3::ZERO;
    }
    let l = v.length();
    if l > max { v * (max / l) } else { v }
}

/// Pushes the movers' assemblies for one substep of `h` seconds.
pub(crate) fn apply_movers(sim: &mut Sim, movers: &[Mover], h: f32) {
    if movers.is_empty() || h <= 0.0 {
        return;
    }
    let g = sim.physics.gravity();
    let pose_of_anchor = |sim: &Sim, a: &Anchor| -> Pose {
        match a {
            Anchor::Fixed(p) => *p,
            Anchor::On(k, f) => sim.physics.assembly(*k).map_or(*f, |i| i.pose.mul(f)),
        }
    };
    for m in movers {
        let Some(info) = sim.physics.assembly(m.key) else {
            continue;
        };
        if info.kind != BodyKind::Dynamic {
            continue;
        }
        let at = info.pose.mul(&m.frame);
        let rel_rot = |r: &Rel| match r {
            Rel::World => Quat::IDENTITY,
            Rel::A0 => at.rot,
            Rel::A1(a) => pose_of_anchor(sim, a).rot,
        };
        let mass = info.mass;
        let inertia: Mat3 = info.inertia;
        let mut force = None;
        let mut torque = None;
        match m.kind {
            MoverKind::Force {
                force: f,
                rel,
                at_com,
            } => {
                let f = rel_rot(&rel) * f;
                if f != Vec3::ZERO {
                    force = Some((f, (!at_com).then_some(at.pos)));
                }
            }
            MoverKind::LinearVelocity {
                line,
                velocity,
                speed,
                max_force,
                rel,
            } => {
                let r = rel_rot(&rel);
                let f = match line {
                    Some(d) => {
                        let d = r * d;
                        let along = mass * (speed - info.linear.dot(d)) / h - mass * g.dot(d);
                        d * along
                    }
                    None => mass * (r * velocity - info.linear) / h - mass * g,
                };
                force = Some((clamp_len(f, max_force), None));
            }
            MoverKind::AngularVelocity {
                velocity,
                max_torque,
                rel,
            } => {
                let t = inertia * ((rel_rot(&rel) * velocity - info.angular) / h);
                torque = Some(clamp_len(t, max_torque));
            }
            MoverKind::AlignPosition {
                target,
                max_force,
                max_velocity,
                responsiveness,
                rigid,
                at_com,
            } => {
                let err = pose_of_anchor(sim, &target).pos - at.pos;
                let want = if rigid {
                    err / h
                } else {
                    clamp_len(err * responsiveness, max_velocity)
                };
                let f = mass * (want - info.linear) / h - mass * g;
                let f = if rigid { f } else { clamp_len(f, max_force) };
                force = Some((f, (!at_com).then_some(at.pos)));
            }
            MoverKind::AlignOrientation {
                target,
                max_torque,
                max_angular,
                responsiveness,
                rigid,
            } => {
                let goal = pose_of_anchor(sim, &target).rot;
                let mut q = (goal * at.rot.inverse()).normalize();
                if q.w < 0.0 {
                    q = -q;
                }
                let (axis, angle) = q.to_axis_angle();
                let err = if angle.is_finite() {
                    axis * angle
                } else {
                    Vec3::ZERO
                };
                let want = if rigid {
                    err / h
                } else {
                    clamp_len(err * responsiveness, max_angular)
                };
                let t = inertia * ((want - info.angular) / h);
                torque = Some(if rigid { t } else { clamp_len(t, max_torque) });
            }
        }
        if let Some((f, p)) = force
            && f.is_finite()
            && f != Vec3::ZERO
        {
            let _ = sim.physics.add_assembly_force(m.key, f, p);
        }
        if let Some(t) = torque
            && t.is_finite()
            && t != Vec3::ZERO
        {
            let _ = sim.physics.add_assembly_torque(m.key, t);
        }
    }
}

/// Whether a joint instance holds: enabled, with both parts set and in the
/// workspace.
fn joint_active(w: &World, e: Entity, class: &str) -> bool {
    let Some(workspace) = workspace(w) else {
        return false;
    };
    let get = |n: &str| field(w, class, n).and_then(|f| f.entity(w, e));
    flag(w, e, class, "Enabled", true)
        && [get("Part0"), get("Part1")]
            .into_iter()
            .all(|p| p.is_some_and(|p| in_workspace(w, p, workspace)))
}

fn workspace(w: &World) -> Option<Entity> {
    let game = w.entity(crate::GAME_ID).ok()?;
    crate::services::core::find_service(w, game, "Workspace")
}

fn in_workspace(w: &World, e: Entity, workspace: Entity) -> bool {
    tree::ancestors(w, e).contains(&workspace)
}

fn constraint_active(w: &World, e: Entity) -> bool {
    let Some(ws) = workspace(w) else {
        return false;
    };
    let (a0, a1) = attachments(w, e);
    let ok = |a: Option<Entity>| {
        a.and_then(|a| tree::parent(w, a))
            .is_some_and(|p| in_workspace(w, p, ws))
    };
    let movers = matches!(
        class_of(w, e),
        "VectorForce" | "AngularVelocity" | "LinearVelocity"
    ) || (class_of(w, e) == "AlignPosition"
        && item(w, e, "AlignPosition", "Mode") == "OneAttachment")
        || (class_of(w, e) == "AlignOrientation"
            && item(w, e, "AlignOrientation", "Mode") == "OneAttachment");
    flag(w, e, "Constraint", "Enabled", true) && ok(a0) && (movers || ok(a1))
}

fn getter(
    f: impl Fn(&World, Entity) -> Result<Value, CompatError> + Send + Sync + 'static,
) -> Accessor {
    Accessor {
        get: Arc::new(f),
        set: None,
    }
}

fn accessor(
    get: impl Fn(&World, Entity) -> Result<Value, CompatError> + Send + Sync + 'static,
    set: impl Fn(&mut World, Entity, Value) -> Result<(), CompatError> + Send + Sync + 'static,
) -> Accessor {
    Accessor {
        get: Arc::new(get),
        set: Some(Arc::new(set)),
    }
}

fn want_vec(v: Value, what: &str) -> Result<Vec3, CompatError> {
    match v {
        Value::Vec3(v) if v.is_finite() => Ok(v),
        other => Err(bad(format!(
            "{what} must be a finite Vector3, got {other:?}"
        ))),
    }
}

fn want_pose(v: Value, what: &str) -> Result<Pose, CompatError> {
    pose_of(&v)
        .filter(|p| p.pos.is_finite() && p.rot.is_finite())
        .ok_or_else(|| bad(format!("{what} must be a CFrame")))
}

fn set_attachment_cframe(w: &mut World, a: Entity, p: Pose) -> Result<(), CompatError> {
    let f = field(w, "Attachment", "CFrame")
        .ok_or_else(|| CompatError::Scene("Attachment has no CFrame field".into()))?;
    f.set(w, a, cframe_value(&p))
}

/// A rotation whose X axis is `x` and whose Y axis is as close to `y` as
/// possible.
fn axes_rotation(x: Vec3, y: Vec3) -> Quat {
    let x = x.normalize_or_zero();
    if x == Vec3::ZERO {
        return Quat::IDENTITY;
    }
    let mut y = y - x * y.dot(x);
    if y.length_squared() < 1e-8 {
        y = x.any_orthonormal_vector();
    }
    let y = y.normalize();
    let z = x.cross(y);
    Quat::from_mat3(&Mat3::from_cols(x, y, z)).normalize()
}

/// The accessors of `Attachment`, `JointInstance`, `WeldConstraint`,
/// `Constraint`, `HingeConstraint`, `RopeConstraint` and `RodConstraint`.
pub(crate) fn accessors() -> Vec<(&'static str, &'static str, Accessor)> {
    let local = |w: &World, e: Entity| attachment_cframe(w, e);
    vec![
        (
            "Attachment",
            "Position",
            accessor(
                move |w, e| Ok(Value::Vec3(local(w, e).pos)),
                move |w, e, v| {
                    let p = want_vec(v, "Position")?;
                    let c = local(w, e);
                    set_attachment_cframe(w, e, Pose::new(p, c.rot))
                },
            ),
        ),
        (
            "Attachment",
            "Orientation",
            accessor(
                move |w, e| Ok(Value::Vec3(orientation_of(local(w, e).rot))),
                move |w, e, v| {
                    let o = want_vec(v, "Orientation")?;
                    let c = local(w, e);
                    set_attachment_cframe(w, e, Pose::new(c.pos, from_orientation(o)))
                },
            ),
        ),
        (
            "Attachment",
            "Axis",
            accessor(
                move |w, e| Ok(Value::Vec3(local(w, e).rot * Vec3::X)),
                move |w, e, v| {
                    let x = want_vec(v, "Axis")?;
                    let c = local(w, e);
                    set_attachment_cframe(w, e, Pose::new(c.pos, axes_rotation(x, c.rot * Vec3::Y)))
                },
            ),
        ),
        (
            "Attachment",
            "SecondaryAxis",
            accessor(
                move |w, e| Ok(Value::Vec3(local(w, e).rot * Vec3::Y)),
                move |w, e, v| {
                    let y = want_vec(v, "SecondaryAxis")?;
                    let c = local(w, e);
                    set_attachment_cframe(w, e, Pose::new(c.pos, axes_rotation(c.rot * Vec3::X, y)))
                },
            ),
        ),
        (
            "Attachment",
            "WorldCFrame",
            accessor(
                |w, e| Ok(cframe_value(&attachment_world(w, e))),
                |w, e, v| {
                    let p = want_pose(v, "WorldCFrame")?;
                    let local = attachment_parent_pose(w, e).inverse().mul(&p);
                    set_attachment_cframe(w, e, local)
                },
            ),
        ),
        (
            "Attachment",
            "WorldPosition",
            accessor(
                |w, e| Ok(Value::Vec3(attachment_world(w, e).pos)),
                |w, e, v| {
                    let p = want_vec(v, "WorldPosition")?;
                    let c = attachment_cframe(w, e);
                    let pos = attachment_parent_pose(w, e).inv_point(p);
                    set_attachment_cframe(w, e, Pose::new(pos, c.rot))
                },
            ),
        ),
        (
            "Attachment",
            "WorldOrientation",
            getter(|w, e| Ok(Value::Vec3(orientation_of(attachment_world(w, e).rot)))),
        ),
        (
            "Attachment",
            "WorldAxis",
            getter(|w, e| Ok(Value::Vec3(attachment_world(w, e).rot * Vec3::X))),
        ),
        (
            "Attachment",
            "WorldSecondaryAxis",
            getter(|w, e| Ok(Value::Vec3(attachment_world(w, e).rot * Vec3::Y))),
        ),
        (
            "JointInstance",
            "Active",
            getter(|w, e| Ok(Value::Bool(joint_active(w, e, "JointInstance")))),
        ),
        (
            "WeldConstraint",
            "Active",
            getter(|w, e| Ok(Value::Bool(joint_active(w, e, "WeldConstraint")))),
        ),
        (
            "Constraint",
            "Active",
            getter(|w, e| Ok(Value::Bool(constraint_active(w, e)))),
        ),
        (
            "HingeConstraint",
            "CurrentAngle",
            getter(|w, e| {
                let rad = read_sim(w, |s| s.physics.hinge_angle(super::sim::key_of(e)))
                    .flatten()
                    .unwrap_or_else(|| hinge_angle_from_poses(w, e));
                Ok(Value::Float(f64::from(rad.to_degrees())))
            }),
        ),
        (
            "RopeConstraint",
            "CurrentDistance",
            getter(|w, e| Ok(Value::Float(f64::from(current_distance(w, e))))),
        ),
        (
            "RodConstraint",
            "CurrentDistance",
            getter(|w, e| Ok(Value::Float(f64::from(current_distance(w, e))))),
        ),
    ]
}

/// The distance between a constraint's attachments (0 when one is missing).
fn current_distance(w: &World, e: Entity) -> f32 {
    match attachments(w, e) {
        (Some(a), Some(b)) => attachment_world(w, a)
            .pos
            .distance(attachment_world(w, b).pos),
        _ => 0.0,
    }
}

/// A hinge's angle from its attachments' frames: the turn of attachment1's
/// secondary axis about attachment0's axis.
fn hinge_angle_from_poses(w: &World, e: Entity) -> f32 {
    let (Some(a), Some(b)) = attachments(w, e) else {
        return 0.0;
    };
    let (fa, fb) = (attachment_world(w, a), attachment_world(w, b));
    let axis = fa.rot * Vec3::X;
    let ya = fa.rot * Vec3::Y;
    let yb = fb.rot * Vec3::Y;
    let yb = yb - axis * yb.dot(axis);
    if yb.length_squared() < 1e-12 {
        return 0.0;
    }
    ya.cross(yb).dot(axis).atan2(ya.dot(yb))
}
