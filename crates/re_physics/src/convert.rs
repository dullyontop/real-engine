//! Conversions between glam (the engine's maths) and oxijolt's plain math types.
//!
//! oxijolt's `Vec3`, `RVec3` and `Quat` have no operators, and its `glam032` feature
//! targets an older glam, so the engine converts at this one boundary by copying fields.

use oxijolt::{Quat as JQuat, RVec3, Real, Vec3 as JVec3};

pub(crate) fn jv(v: glam::Vec3) -> JVec3 {
    JVec3::new(v.x, v.y, v.z)
}

pub(crate) fn rv(v: glam::Vec3) -> RVec3 {
    RVec3::new(Real::from(v.x), Real::from(v.y), Real::from(v.z))
}

pub(crate) fn jq(q: glam::Quat) -> JQuat {
    JQuat::from_xyzw(q.x, q.y, q.z, q.w)
}

pub(crate) fn gv(v: JVec3) -> glam::Vec3 {
    glam::Vec3::new(v.x, v.y, v.z)
}

// `Real` is f32 unless oxijolt's `double-precision` feature is on (not used in M1).
#[allow(
    clippy::unnecessary_cast,
    reason = "`Real` is f64 when oxijolt's `double-precision` feature is on"
)]
pub(crate) fn grv(v: RVec3) -> glam::Vec3 {
    glam::Vec3::new(v.x as f32, v.y as f32, v.z as f32)
}

pub(crate) fn gq(q: JQuat) -> glam::Quat {
    glam::Quat::from_xyzw(q.x, q.y, q.z, q.w)
}

/// A rotation Jolt accepts: finite, normalised. `None` for a zero or non-finite quaternion.
pub(crate) fn unit_rotation(q: glam::Quat) -> Option<glam::Quat> {
    let len = q.length();
    (q.is_finite() && len > 1e-6).then(|| q / len)
}
