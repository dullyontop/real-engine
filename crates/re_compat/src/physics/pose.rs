//! Poses: a part's frame in the world, read and written through `Transform`,
//! and the conversions between stored `CFrame` records, Euler angles and
//! `re_physics` poses.
//!
//! A part's `Transform` is relative to its nearest ancestor with a
//! `Transform` (parts under models and folders have none, so theirs is the
//! world frame). Reading a part's `CFrame` walks that chain; writing it sets
//! the `Transform` that puts the part at the frame in the world.

use glam::{EulerRot, Mat3, Quat, Vec3};
use re_physics::Pose;
use re_reflect::{RecordId, Value};
use re_scene::ecs::hierarchy::ChildOf;
use re_scene::{Entity, Transform, World};

use crate::error::CompatError;

/// The world pose of `e`: its `Transform` composed with its ancestors'
/// (scale ignored). `None` if `e` has no `Transform`.
pub fn world_pose(w: &World, e: Entity) -> Option<Pose> {
    let t = w.ecs.get::<Transform>(e)?;
    let mut pose = Pose::new(t.translation, t.rotation);
    let mut cur = e;
    while let Some(c) = w.ecs.get::<ChildOf>(cur) {
        cur = c.parent();
        if let Some(pt) = w.ecs.get::<Transform>(cur) {
            pose = Pose::new(pt.translation, pt.rotation).mul(&pose);
        }
    }
    Some(pose)
}

/// The world pose of the nearest ancestor of `e` with a `Transform`
/// (identity when there is none).
pub fn parent_pose(w: &World, e: Entity) -> Pose {
    let mut cur = e;
    while let Some(c) = w.ecs.get::<ChildOf>(cur) {
        cur = c.parent();
        if w.ecs.get::<Transform>(cur).is_some() {
            return world_pose(w, cur).unwrap_or(Pose::IDENTITY);
        }
    }
    Pose::IDENTITY
}

/// Sets `e`'s `Transform` so that it sits at `pose` in the world (a script
/// write: change detection sees it).
pub fn set_world_pose(w: &mut World, e: Entity, pose: Pose) -> Result<(), CompatError> {
    if !(pose.pos.is_finite() && pose.rot.is_finite()) {
        return Err(CompatError::Runtime {
            code: re_script::codes::BAD_ARGUMENT,
            message: "the frame is not finite".into(),
        });
    }
    if w.ecs.get::<Transform>(e).is_none() {
        return Err(CompatError::Scene("the instance has no Transform".into()));
    }
    // Children with a frame keep theirs: a part's CFrame is in the world.
    let kids: Vec<(Entity, Pose)> = re_scene::hierarchy::children(&w.ecs, e)
        .to_vec()
        .into_iter()
        .filter_map(|k| world_pose(w, k).map(|p| (k, p)))
        .collect();
    let local = parent_pose(w, e).inverse().mul(&pose);
    if let Some(mut t) = w.ecs.get_mut::<Transform>(e) {
        t.translation = local.pos;
        t.rotation = local.rot.normalize();
    }
    if !kids.is_empty() {
        super::sim::write_world_poses(w, &kids);
    }
    if let Some(mut f) = w.ecs.get_resource_mut::<super::sim::PhysicsFeed>() {
        f.moved.push(e);
    }
    Ok(())
}

/// The stored `CFrame` record of a pose.
pub fn cframe_value(p: &Pose) -> Value {
    let m = Mat3::from_quat(p.rot.normalize());
    let id = RecordId::by_name("CFrame").expect("the class registry registers CFrame");
    Value::Record(
        id,
        vec![
            Value::Vec3(p.pos),
            Value::Vec3(m.x_axis),
            Value::Vec3(m.y_axis),
            Value::Vec3(m.z_axis),
        ],
    )
}

/// The pose of a stored `CFrame` record.
pub fn pose_of(v: &Value) -> Option<Pose> {
    let Value::Record(id, fields) = v else {
        return None;
    };
    if id.name() != "CFrame" {
        return None;
    }
    let schema = id.schema();
    let mut pos = Vec3::ZERO;
    let mut axes = [Vec3::X, Vec3::Y, Vec3::Z];
    for (f, v) in schema.fields.iter().zip(fields) {
        let Value::Vec3(x) = v else { continue };
        match f.name.as_str() {
            "Position" => pos = *x,
            "XVector" => axes[0] = *x,
            "YVector" => axes[1] = *x,
            "ZVector" => axes[2] = *x,
            _ => {}
        }
    }
    let m = Mat3::from_cols(axes[0], axes[1], axes[2]);
    let rot = if m.determinant().abs() > 1e-6 {
        Quat::from_mat3(&m).normalize()
    } else {
        Quat::IDENTITY
    };
    Some(Pose::new(pos, rot))
}

/// Degrees about X, Y and Z, applied in Y, X, Z order (`Orientation`).
pub fn orientation_of(q: Quat) -> Vec3 {
    let (y, x, z) = q.to_euler(EulerRot::YXZ);
    Vec3::new(x, y, z) * (180.0 / std::f32::consts::PI)
}

/// The rotation of an `Orientation`.
pub fn from_orientation(deg: Vec3) -> Quat {
    let r = deg * (std::f32::consts::PI / 180.0);
    Quat::from_euler(EulerRot::YXZ, r.y, r.x, r.z)
}

/// Degrees about X, Y and Z, applied in X, Y, Z order (`Rotation`).
pub fn rotation_of(q: Quat) -> Vec3 {
    let (x, y, z) = q.to_euler(EulerRot::XYZ);
    Vec3::new(x, y, z) * (180.0 / std::f32::consts::PI)
}

/// The rotation of a `Rotation`.
pub fn from_rotation(deg: Vec3) -> Quat {
    let r = deg * (std::f32::consts::PI / 180.0);
    Quat::from_euler(EulerRot::XYZ, r.x, r.y, r.z)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_round_trips() {
        let deg = Vec3::new(30.0, -60.0, 15.0);
        let back = orientation_of(from_orientation(deg));
        assert!((back - deg).length() < 1e-3, "{back}");
        let back = rotation_of(from_rotation(deg));
        assert!((back - deg).length() < 1e-3, "{back}");
    }
}
