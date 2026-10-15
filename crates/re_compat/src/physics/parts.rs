//! The members scripts use on parts and models: `BasePart`'s frame, mass and
//! assembly properties, its impulses and joint helpers, `PVInstance`'s pivot,
//! and the `Model` helpers.
//!
//! Property reads see the simulation as of the last step (plus velocities
//! and impulses scripts asked for since); methods about assemblies sync the
//! simulation first, so they see welds and parts made this frame.

use std::collections::BTreeSet;
use std::sync::Arc;

use glam::{Quat, Vec3};
use mlua::{Lua, MultiValue, Value as LuaValue, Vector};
use re_physics::Pose;
use re_reflect::Value;
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::component::Component;
use re_scene::{Entity, World};
use re_script::codes::BAD_ARGUMENT;
use re_types::physics::BodyKind;

use super::layout::Layout;
use super::pose::{
    cframe_value, from_orientation, from_rotation, orientation_of, pose_of, rotation_of,
    set_world_pose, world_pose,
};
use super::sim::{CustomPhysics, Impulse, PartSnap, Sim};
use super::{read_sim, with_sim};
use crate::class::{Accessor, ClassMarker};
use crate::datatypes::{CFrame, PhysicalProperties};
use crate::error::CompatError;
use crate::instance::methods_api::{args, ret};
use crate::instance::{push, push_list, tree};
use crate::runtime::{Bindings, compat_error, lua_error, with_world};

/// A model's pivot when it has no `PrimaryPart` (`Model.WorldPivot`).
#[derive(Component, Clone, Copy, Debug)]
pub struct ModelPivot(pub Pose);

fn bad(message: impl Into<String>) -> CompatError {
    CompatError::Runtime {
        code: BAD_ARGUMENT,
        message: message.into(),
    }
}

fn is_part(w: &World, e: Entity) -> bool {
    Layout::find(w).is_some_and(|l| l.is_part(w, e))
}

fn snap(w: &World, e: Entity) -> Option<PartSnap> {
    let l = Layout::find(w)?;
    l.is_part(w, e).then(|| PartSnap::read(w, &l, e))
}

/// A Luau `Vector3` as a vector.
fn vec_of(v: Vector) -> Vec3 {
    Vec3::new(v.x(), v.y(), v.z())
}

fn lua_vec(v: Vec3) -> LuaValue {
    LuaValue::Vector(Vector::new(v.x, v.y, v.z))
}

/// A `CFrame` as a pose.
pub(crate) fn pose_of_cframe(c: &CFrame) -> Pose {
    let q = c.quaternion();
    Pose::new(
        Vec3::from_array(c.p),
        Quat::from_xyzw(q[0], q[1], q[2], q[3]).normalize(),
    )
}

/// A pose as a `CFrame`.
pub(crate) fn cframe_of(p: &Pose) -> CFrame {
    let r = p.rot.normalize();
    CFrame::from_quaternion(p.pos.to_array(), [r.x, r.y, r.z, r.w])
}

fn finite(p: &Pose) -> bool {
    p.pos.is_finite() && p.rot.is_finite()
}

// ---------------------------------------------------------------- accessors

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
            "{what} must be a finite Vector3, got {}",
            other.describe()
        ))),
    }
}

fn want_pose(v: Value, what: &str) -> Result<Pose, CompatError> {
    pose_of(&v)
        .filter(finite)
        .ok_or_else(|| bad(format!("{what} must be a finite CFrame")))
}

fn pose_or_err(w: &World, e: Entity) -> Result<Pose, CompatError> {
    world_pose(w, e).ok_or_else(|| CompatError::Scene("the part has no Transform".into()))
}

/// The velocities of the assembly `part` is in: what a script set this
/// frame, else the simulation's, else zero.
fn velocities(w: &World, part: Entity) -> (Vec3, Vec3) {
    read_sim(w, |s| {
        let info = s.assembly_key(part).and_then(|k| s.physics.assembly(k));
        let (mut lin, mut ang) = info
            .filter(|i| i.kind != BodyKind::Static)
            .map_or((Vec3::ZERO, Vec3::ZERO), |i| (i.linear, i.angular));
        // A pending write to any member counts.
        for m in s.assembly_members(part) {
            if let Some(v) = s.pending.linear.get(&m) {
                lin = *v;
            }
            if let Some(v) = s.pending.angular.get(&m) {
                ang = *v;
            }
        }
        (lin, ang)
    })
    .unwrap_or((Vec3::ZERO, Vec3::ZERO))
}

fn set_velocity(w: &mut World, e: Entity, v: Value, linear: bool) -> Result<(), CompatError> {
    let v = want_vec(v, "the velocity")?;
    with_sim(w, |sim, _| {
        if linear {
            sim.pending.linear.insert(e, v);
        } else {
            sim.pending.angular.insert(e, v);
        }
    });
    Ok(())
}

fn assembly_mass(w: &World, e: Entity) -> f32 {
    let part_mass = || snap(w, e).map_or(0.0, |s| s.mass());
    read_sim(w, |s| {
        let members = s.assembly_members(e);
        match s.assembly_key(e).and_then(|k| s.physics.assembly(k)) {
            Some(i) if i.kind == BodyKind::Dynamic => i.mass,
            // Anchored: the members' masses (massless ones count for nothing).
            _ => members
                .iter()
                .filter_map(|m| s.parts.get(m))
                .filter(|p| !p.snap.massless)
                .map(|p| p.snap.mass())
                .sum(),
        }
    })
    .unwrap_or_else(part_mass)
}

fn center_of_mass(w: &World, e: Entity) -> Vec3 {
    let own = || world_pose(w, e).map_or(Vec3::ZERO, |p| p.pos);
    read_sim(w, |s| {
        let info = s.assembly_key(e).and_then(|k| s.physics.assembly(k));
        match info {
            Some(i) if i.kind == BodyKind::Dynamic => Some(i.center_of_mass),
            _ => {
                // Anchored: the mass-weighted centre of the members.
                let mut sum = Vec3::ZERO;
                let mut m = 0.0;
                for p in s.assembly_members(e) {
                    if let (Some(rec), Some(pose)) = (s.parts.get(&p), world_pose(w, p)) {
                        let pm = rec.snap.mass();
                        sum += pose.point(rec.snap.geom.centroid()) * pm;
                        m += pm;
                    }
                }
                (m > 0.0).then(|| sum / m)
            }
        }
    })
    .flatten()
    .unwrap_or_else(own)
}

fn props_value(p: &PhysicalProperties) -> Value {
    let id = re_reflect::RecordId::by_name("PhysicalProperties")
        .expect("the class registry registers PhysicalProperties");
    let f = |x: f32| Value::Float(f64::from(x));
    let schema = id.schema();
    let fields = schema
        .fields
        .iter()
        .map(|fd| match fd.name.as_str() {
            "Density" => f(p.density),
            "Friction" => f(p.friction),
            "Elasticity" => f(p.elasticity),
            "FrictionWeight" => f(p.friction_weight),
            "ElasticityWeight" => f(p.elasticity_weight),
            "AcousticAbsorption" => f(p.acoustic_absorption),
            _ => Value::Float(0.0),
        })
        .collect();
    Value::Record(id, fields)
}

fn props_of(v: &Value) -> Option<PhysicalProperties> {
    let Value::Record(id, fields) = v else {
        return None;
    };
    if id.name() != "PhysicalProperties" {
        return None;
    }
    let mut p = PhysicalProperties::of_material("Plastic");
    for (fd, v) in id.schema().fields.iter().zip(fields) {
        let x = match v {
            Value::Float(x) => *x as f32,
            Value::Int(i) => *i as f32,
            _ => continue,
        };
        match fd.name.as_str() {
            "Density" => p.density = x,
            "Friction" => p.friction = x,
            "Elasticity" => p.elasticity = x,
            "FrictionWeight" => p.friction_weight = x,
            "ElasticityWeight" => p.elasticity_weight = x,
            "AcousticAbsorption" => p.acoustic_absorption = x,
            _ => {}
        }
    }
    Some(p)
}

/// The accessors of `BasePart` and `Model`.
pub(crate) fn accessors() -> Vec<(&'static str, &'static str, Accessor)> {
    vec![
        (
            "BasePart",
            "CFrame",
            accessor(
                |w, e| Ok(cframe_value(&pose_or_err(w, e)?)),
                |w, e, v| set_world_pose(w, e, want_pose(v, "CFrame")?),
            ),
        ),
        (
            "BasePart",
            "Position",
            accessor(
                |w, e| Ok(Value::Vec3(pose_or_err(w, e)?.pos)),
                |w, e, v| {
                    let p = want_vec(v, "Position")?;
                    let now = pose_or_err(w, e)?;
                    set_world_pose(w, e, Pose::new(p, now.rot))
                },
            ),
        ),
        (
            "BasePart",
            "Orientation",
            accessor(
                |w, e| Ok(Value::Vec3(orientation_of(pose_or_err(w, e)?.rot))),
                |w, e, v| {
                    let o = want_vec(v, "Orientation")?;
                    let now = pose_or_err(w, e)?;
                    set_world_pose(w, e, Pose::new(now.pos, from_orientation(o)))
                },
            ),
        ),
        (
            "BasePart",
            "Rotation",
            accessor(
                |w, e| Ok(Value::Vec3(rotation_of(pose_or_err(w, e)?.rot))),
                |w, e, v| {
                    let o = want_vec(v, "Rotation")?;
                    let now = pose_or_err(w, e)?;
                    set_world_pose(w, e, Pose::new(now.pos, from_rotation(o)))
                },
            ),
        ),
        (
            "BasePart",
            "CustomPhysicalProperties",
            accessor(
                |w, e| {
                    Ok(match w.ecs.get::<CustomPhysics>(e) {
                        Some(c) => props_value(&c.0),
                        None => Value::Entity(None),
                    })
                },
                |w, e, v| {
                    match v {
                        Value::Entity(None) => {
                            w.ecs.entity_mut(e).remove::<CustomPhysics>();
                        }
                        other => {
                            let p = props_of(&other).ok_or_else(|| {
                                bad("CustomPhysicalProperties must be PhysicalProperties or nil")
                            })?;
                            w.ecs.entity_mut(e).insert(CustomPhysics(p));
                        }
                    }
                    if let Some(mut f) = w.ecs.get_resource_mut::<super::sim::PhysicsFeed>() {
                        f.dirty.push(e);
                    }
                    Ok(())
                },
            ),
        ),
        (
            "BasePart",
            "CurrentPhysicalProperties",
            getter(|w, e| {
                let s = snap(w, e).ok_or_else(|| bad("not a part"))?;
                Ok(props_value(&s.material))
            }),
        ),
        (
            "BasePart",
            "Mass",
            getter(|w, e| {
                Ok(Value::Float(f64::from(
                    snap(w, e).map_or(0.0, |s| s.mass()),
                )))
            }),
        ),
        (
            "BasePart",
            "AssemblyLinearVelocity",
            accessor(
                |w, e| Ok(Value::Vec3(velocities(w, e).0)),
                |w, e, v| set_velocity(w, e, v, true),
            ),
        ),
        (
            "BasePart",
            "AssemblyAngularVelocity",
            accessor(
                |w, e| Ok(Value::Vec3(velocities(w, e).1)),
                |w, e, v| set_velocity(w, e, v, false),
            ),
        ),
        (
            "BasePart",
            "Velocity",
            accessor(
                |w, e| Ok(Value::Vec3(velocities(w, e).0)),
                |w, e, v| set_velocity(w, e, v, true),
            ),
        ),
        (
            "BasePart",
            "RotVelocity",
            accessor(
                |w, e| Ok(Value::Vec3(velocities(w, e).1)),
                |w, e, v| set_velocity(w, e, v, false),
            ),
        ),
        (
            "BasePart",
            "AssemblyMass",
            getter(|w, e| Ok(Value::Float(f64::from(assembly_mass(w, e))))),
        ),
        (
            "BasePart",
            "AssemblyCenterOfMass",
            getter(|w, e| Ok(Value::Vec3(center_of_mass(w, e)))),
        ),
        (
            "BasePart",
            "AssemblyRootPart",
            getter(|w, e| {
                Ok(Value::Entity(Some(
                    read_sim(w, |s| s.root_of(e)).unwrap_or(e),
                )))
            }),
        ),
        (
            "Model",
            "WorldPivot",
            accessor(
                |w, e| Ok(cframe_value(&model_pivot(w, e))),
                |w, e, v| {
                    let p = want_pose(v, "WorldPivot")?;
                    match primary_part(w, e) {
                        // With a primary part, the pivot is its frame.
                        Some(pp) => set_world_pose(w, pp, p),
                        None => {
                            w.ecs.entity_mut(e).insert(ModelPivot(p));
                            Ok(())
                        }
                    }
                },
            ),
        ),
    ]
}

// ------------------------------------------------------------------ models

fn primary_part(w: &World, model: Entity) -> Option<Entity> {
    crate::physics::layout::Field::find(w, "Model", "PrimaryPart")
        .and_then(|f| f.entity(w, model))
        .filter(|p| tree::alive(w, *p) && tree::ancestors(w, *p).contains(&model))
}

/// The parts under `e` (with `e` itself when it is a part).
fn parts_under(w: &World, e: Entity) -> Vec<Entity> {
    let l = Layout::find(w);
    let is = |x: Entity| l.as_ref().is_some_and(|l| l.is_part(w, x));
    let mut v: Vec<Entity> = Vec::new();
    if is(e) {
        v.push(e);
    }
    v.extend(tree::descendants(w, e).into_iter().filter(|x| is(*x)));
    v
}

/// The box around `parts`, in the axes of `rot`: its centre frame and size.
fn bounding_box(w: &World, parts: &[Entity], rot: Quat) -> Option<(Pose, Vec3)> {
    let inv = rot.inverse();
    let mut lo = Vec3::splat(f32::INFINITY);
    let mut hi = Vec3::splat(f32::NEG_INFINITY);
    let l = Layout::find(w)?;
    for &p in parts {
        let Some(pose) = world_pose(w, p) else {
            continue;
        };
        let half = l.size.vec3(w, p, Vec3::ONE) * 0.5;
        for i in 0..8 {
            let c = Vec3::new(
                if i & 1 == 0 { -half.x } else { half.x },
                if i & 2 == 0 { -half.y } else { half.y },
                if i & 4 == 0 { -half.z } else { half.z },
            );
            let q = inv * pose.point(c);
            lo = lo.min(q);
            hi = hi.max(q);
        }
    }
    if !lo.is_finite() {
        return None;
    }
    Some((Pose::new(rot * ((lo + hi) * 0.5), rot), hi - lo))
}

/// A model's pivot: its primary part's frame, else the pivot set on it, else
/// the centre of its parts' box.
fn model_pivot(w: &World, model: Entity) -> Pose {
    if let Some(pp) = primary_part(w, model) {
        return world_pose(w, pp).unwrap_or(Pose::IDENTITY);
    }
    if let Some(p) = w.ecs.get::<ModelPivot>(model) {
        return p.0;
    }
    bounding_box(w, &parts_under(w, model), Quat::IDENTITY)
        .map_or(Pose::IDENTITY, |(p, _)| Pose::new(p.pos, Quat::IDENTITY))
}

/// The pivot of a part or model.
fn pivot(w: &World, e: Entity) -> Result<Pose, CompatError> {
    match w.ecs.get::<ClassMarker>(e).map(|m| m.class.as_str()) {
        Some("Model" | "WorldModel" | "Workspace") => Ok(model_pivot(w, e)),
        _ if is_part(w, e) => pose_or_err(w, e),
        _ => Ok(model_pivot(w, e)),
    }
}

/// Moves every part under `e` by `delta` (applied in the world:
/// `new = delta * old`), parents before children.
fn move_parts(w: &mut World, e: Entity, delta: Pose) -> Result<(), CompatError> {
    let parts = parts_under(w, e);
    let targets: Vec<(Entity, Pose)> = parts
        .iter()
        .filter_map(|p| world_pose(w, *p).map(|o| (*p, delta.mul(&o))))
        .collect();
    for (p, t) in targets {
        set_world_pose(w, p, Pose::new(t.pos, t.rot.normalize()))?;
    }
    Ok(())
}

/// `PivotTo` on a part or model.
fn pivot_to(w: &mut World, e: Entity, target: Pose) -> Result<(), CompatError> {
    let now = pivot(w, e)?;
    let delta = target.mul(&now.inverse());
    move_parts(w, e, delta)?;
    if w.ecs.get::<ModelPivot>(e).is_some() {
        w.ecs.entity_mut(e).insert(ModelPivot(target));
    }
    Ok(())
}

// ----------------------------------------------------------------- methods

type Imp = fn(&Lua, Entity, MultiValue) -> mlua::Result<MultiValue>;

/// Registers the `PVInstance`, `Model` and `BasePart` methods.
pub(crate) fn register_methods(b: &mut Bindings) {
    let methods: &[(&str, &str, Imp)] = &[
        ("PVInstance", "GetPivot", get_pivot),
        ("PVInstance", "PivotTo", pivot_to_method),
        ("Model", "GetBoundingBox", get_bounding_box),
        ("Model", "GetExtentsSize", get_extents_size),
        ("Model", "MoveTo", move_to),
        ("Model", "TranslateBy", translate_by),
        ("Model", "SetPrimaryPartCFrame", set_primary_part_cframe),
        ("Model", "GetPrimaryPartCFrame", get_primary_part_cframe),
        ("BasePart", "ApplyImpulse", apply_impulse),
        ("BasePart", "ApplyImpulseAtPosition", apply_impulse_at),
        ("BasePart", "ApplyAngularImpulse", apply_angular_impulse),
        ("BasePart", "GetMass", get_mass),
        ("BasePart", "GetRootPart", get_root_part),
        ("BasePart", "GetConnectedParts", get_connected_parts),
        ("BasePart", "GetJoints", get_joints),
        ("BasePart", "IsGrounded", is_grounded),
        ("BasePart", "GetTouchingParts", get_touching_parts),
        (
            "BasePart",
            "GetVelocityAtPosition",
            get_velocity_at_position,
        ),
        ("BasePart", "CanCollideWith", can_collide_with),
        ("BasePart", "BreakJoints", break_joints),
    ];
    for (class, name, f) in methods {
        b.method(class, name, Arc::new(*f));
    }
}

fn world_result<R>(
    lua: &Lua,
    f: impl FnOnce(&mut World) -> Result<R, CompatError>,
) -> mlua::Result<R> {
    with_world(lua, f)?.map_err(compat_error)
}

fn cframe_ret(lua: &Lua, p: Pose) -> mlua::Result<MultiValue> {
    ret(lua, cframe_of(&p))
}

fn get_pivot(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "GetPivot")?;
    let p = world_result(lua, |w| pivot(w, e))?;
    cframe_ret(lua, p)
}

fn pivot_to_method(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (cf,): (CFrame,) = args(lua, a, "PivotTo")?;
    let target = pose_of_cframe(&cf);
    if !finite(&target) {
        return Err(lua_error(BAD_ARGUMENT, "PivotTo: the CFrame is not finite"));
    }
    world_result(lua, |w| pivot_to(w, e, target))?;
    ret(lua, ())
}

fn get_bounding_box(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "GetBoundingBox")?;
    let (p, size) = with_world(lua, |w| {
        let rot = model_pivot(w, e).rot;
        bounding_box(w, &parts_under(w, e), rot)
            .unwrap_or((Pose::new(model_pivot(w, e).pos, rot), Vec3::ZERO))
    })?;
    ret(lua, (cframe_of(&p), lua_vec(size)))
}

fn get_extents_size(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "GetExtentsSize")?;
    let size = with_world(lua, |w| {
        let rot = model_pivot(w, e).rot;
        bounding_box(w, &parts_under(w, e), rot).map_or(Vec3::ZERO, |(_, s)| s)
    })?;
    ret(lua, lua_vec(size))
}

fn move_to(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (pos,): (Vector,) = args(lua, a, "MoveTo")?;
    let pos = vec_of(pos);
    world_result(lua, |w| {
        let now = model_pivot(w, e);
        pivot_to(w, e, Pose::new(pos, now.rot))
    })?;
    ret(lua, ())
}

fn translate_by(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (d,): (Vector,) = args(lua, a, "TranslateBy")?;
    let d = vec_of(d);
    world_result(lua, |w| {
        let now = model_pivot(w, e);
        pivot_to(w, e, Pose::new(now.pos + d, now.rot))
    })?;
    ret(lua, ())
}

fn need_primary(w: &World, e: Entity, what: &str) -> Result<Entity, CompatError> {
    primary_part(w, e).ok_or_else(|| CompatError::Runtime {
        code: crate::codes::PHYSICS_SETUP,
        message: format!("{what}: Model has no PrimaryPart"),
    })
}

fn set_primary_part_cframe(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (cf,): (CFrame,) = args(lua, a, "SetPrimaryPartCFrame")?;
    let target = pose_of_cframe(&cf);
    world_result(lua, |w| {
        let pp = need_primary(w, e, "SetPrimaryPartCFrame")?;
        let now = pose_or_err(w, pp)?;
        move_parts(w, e, target.mul(&now.inverse()))
    })?;
    ret(lua, ())
}

fn get_primary_part_cframe(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "GetPrimaryPartCFrame")?;
    let p = world_result(lua, |w| {
        let pp = need_primary(w, e, "GetPrimaryPartCFrame")?;
        pose_or_err(w, pp)
    })?;
    cframe_ret(lua, p)
}

fn queue_impulse(lua: &Lua, e: Entity, imp: Impulse) -> mlua::Result<MultiValue> {
    let ok = match imp {
        Impulse::Linear(v) | Impulse::Angular(v) => v.is_finite(),
        Impulse::AtPoint(v, p) => v.is_finite() && p.is_finite(),
    };
    if !ok {
        return Err(lua_error(BAD_ARGUMENT, "the impulse is not finite"));
    }
    with_world(lua, |w| {
        with_sim(w, |sim, _| sim.pending.impulses.push((e, imp)));
    })?;
    ret(lua, ())
}

fn apply_impulse(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (v,): (Vector,) = args(lua, a, "ApplyImpulse")?;
    queue_impulse(lua, e, Impulse::Linear(vec_of(v)))
}

fn apply_impulse_at(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (v, p): (Vector, Vector) = args(lua, a, "ApplyImpulseAtPosition")?;
    queue_impulse(lua, e, Impulse::AtPoint(vec_of(v), vec_of(p)))
}

fn apply_angular_impulse(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (v,): (Vector,) = args(lua, a, "ApplyAngularImpulse")?;
    queue_impulse(lua, e, Impulse::Angular(vec_of(v)))
}

fn get_mass(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "GetMass")?;
    let m = with_world(lua, |w| snap(w, e).map_or(0.0, |s| s.mass()))?;
    ret(lua, f64::from(m))
}

/// Runs `f` on the simulation after syncing it with the world.
fn synced<R>(lua: &Lua, f: impl FnOnce(&mut Sim, &mut World) -> R) -> mlua::Result<Option<R>> {
    with_world(lua, |w| {
        with_sim(w, |sim, w| {
            sim.sync(w, false);
            f(sim, w)
        })
    })
}

fn get_root_part(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "GetRootPart")?;
    let root = synced(lua, |s, _| s.root_of(e))?.unwrap_or(e);
    ret(lua, push(lua, root)?)
}

/// The parts joined to `e`: directly (rigid joints and joint constraints),
/// or through any chain of them with `recursive`.
fn connected(sim: &Sim, w: &World, e: Entity, recursive: bool) -> Vec<Entity> {
    let mut links: Vec<(Entity, Entity)> = sim
        .rigid_links()
        .into_iter()
        .map(|(_, a, b)| (a, b))
        .collect();
    for (&c, rec) in &sim.constraints {
        if rec.joint.is_none() {
            continue;
        }
        let parts = super::constraints::constraint_parts(w, c);
        if let (Some(a), Some(b)) = parts {
            links.push((a, b));
        }
    }
    let neighbours = |x: Entity| {
        links
            .iter()
            .filter_map(move |&(a, b)| {
                if a == x {
                    Some(b)
                } else if b == x {
                    Some(a)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
    };
    if !recursive {
        let mut v: Vec<Entity> = neighbours(e);
        v.sort_unstable();
        v.dedup();
        v.retain(|x| *x != e);
        return v;
    }
    let mut seen = BTreeSet::from([e]);
    let mut stack = vec![e];
    while let Some(x) = stack.pop() {
        for n in neighbours(x) {
            if seen.insert(n) {
                stack.push(n);
            }
        }
    }
    seen.into_iter().collect()
}

fn get_connected_parts(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (recursive,): (Option<bool>,) = args(lua, a, "GetConnectedParts")?;
    let list =
        synced(lua, |s, w| connected(s, w, e, recursive.unwrap_or(false)))?.unwrap_or_default();
    ret(lua, push_list(lua, &list)?)
}

fn get_joints(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "GetJoints")?;
    let list = synced(lua, |s, w| {
        let mut v: Vec<Entity> = s
            .rigid
            .iter()
            .filter(|(_, r)| r.part0 == Some(e) || r.part1 == Some(e))
            .map(|(j, _)| *j)
            .collect();
        for &c in s.constraints.keys() {
            let (a, b) = super::constraints::constraint_parts(w, c);
            if a == Some(e) || b == Some(e) {
                v.push(c);
            }
        }
        v.sort_unstable();
        v
    })?
    .unwrap_or_default();
    ret(lua, push_list(lua, &list)?)
}

fn is_grounded(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "IsGrounded")?;
    let grounded = synced(lua, |s, w| {
        connected(s, w, e, true)
            .into_iter()
            .any(|p| s.parts.get(&p).is_some_and(|r| r.snap.anchored))
    })?
    .unwrap_or(false);
    let grounded = grounded || with_world(lua, |w| snap(w, e).is_some_and(|s| s.anchored))?;
    ret(lua, grounded)
}

fn get_touching_parts(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "GetTouchingParts")?;
    let list = with_world(lua, |w| {
        read_sim(w, |s| {
            s.touching
                .get(&e)
                .map(|t| t.iter().copied().collect::<Vec<_>>())
                .unwrap_or_default()
        })
        .unwrap_or_default()
    })?;
    ret(lua, push_list(lua, &list)?)
}

fn get_velocity_at_position(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (p,): (Vector,) = args(lua, a, "GetVelocityAtPosition")?;
    let p = vec_of(p);
    let v = with_world(lua, |w| {
        let (lin, ang) = velocities(w, e);
        lin + ang.cross(p - center_of_mass(w, e))
    })?;
    ret(lua, lua_vec(v))
}

fn can_collide_with(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (other,): (LuaValue,) = args(lua, a, "CanCollideWith")?;
    let other = crate::instance::arg_instance(&other, "CanCollideWith")?;
    let ok = with_world(lua, |w| {
        let (Some(sa), Some(sb)) = (snap(w, e), snap(w, other)) else {
            return Ok(false);
        };
        if !(sa.can_collide && sb.can_collide) || e == other {
            return Ok(false);
        }
        Ok(with_sim(w, |sim, _| {
            let (ga, gb) = (sim.groups.id(&sa.group), sim.groups.id(&sb.group));
            sim.groups.table.collides(ga, gb)
        })
        .unwrap_or(true))
    })?
    .map_err(compat_error)?;
    ret(lua, ok)
}

fn break_joints(lua: &Lua, e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let () = args(lua, a, "BreakJoints")?;
    let joints = synced(lua, |s, w| {
        let mut v: Vec<Entity> = s
            .rigid
            .iter()
            .filter(|(_, r)| r.part0 == Some(e) || r.part1 == Some(e))
            .map(|(j, _)| *j)
            .collect();
        for &c in s.constraints.keys() {
            let (a, b) = super::constraints::constraint_parts(w, c);
            if a == Some(e) || b == Some(e) {
                v.push(c);
            }
        }
        v.sort_unstable();
        v
    })?
    .unwrap_or_default();
    for j in joints {
        crate::instance::destroy(lua, j)?;
    }
    ret(lua, ())
}
