//! Inverse kinematics helpers: aiming one joint, and bending a two-bone chain
//! (an arm or a leg) so its end reaches a target.
//!
//! Everything works on world-space positions and returns world-space rotation
//! deltas; the caller turns them into joint rotations.

use glam::Quat;
use glam::Vec3;

/// Below this, a length or distance counts as zero.
const EPSILON: f32 = 1e-6;

/// The rotation that turns direction `forward` to point from `from` at
/// `target`, taking the shortest arc.
///
/// Identity when `forward` is zero or `target` is at `from`. When `forward`
/// points exactly away from the target, the half turn is about some axis
/// perpendicular to it.
pub fn look_at_delta(forward: Vec3, from: Vec3, target: Vec3) -> Quat {
    let (Some(a), Some(b)) = (forward.try_normalize(), (target - from).try_normalize()) else {
        return Quat::IDENTITY;
    };
    if !a.is_finite() || !b.is_finite() {
        return Quat::IDENTITY;
    }
    Quat::from_rotation_arc(a, b)
}

/// Solves a two-bone chain `root -> mid -> end` so that `end` reaches
/// `target`, keeping both bone lengths.
///
/// Returns `(root_delta, mid_delta)`, world-space rotations applied in this
/// order: `root_delta` turns the whole chain about `root`, then `mid_delta`
/// turns the lower bone about the moved mid joint. The new positions are
///
/// ```text
/// mid'  = root + root_delta * (mid - root)
/// end'  = mid' + mid_delta * root_delta * (end - mid)
/// ```
///
/// In a joint hierarchy this means: pre-multiply the root's world rotation by
/// `root_delta`, and the mid joint's world rotation (after it inherits the
/// root's change) by `mid_delta`.
///
/// When the target is farther than the chain can reach, the chain points
/// straight at it; when it is closer than the difference of the bone lengths,
/// the chain folds as far as it can toward it. The bend happens in the plane
/// through `root`, the target and `pole` when a pole is given (the mid joint
/// goes toward the pole's side); otherwise in the plane the chain bends in now,
/// or, for a straight chain, a fixed plane chosen from the world axes.
///
/// Degenerate input (a zero-length bone, the target at the root, non-finite
/// values) gives finite rotations, at worst the identity.
pub fn two_bone(
    root: Vec3,
    mid: Vec3,
    end: Vec3,
    target: Vec3,
    pole: Option<Vec3>,
) -> (Quat, Quat) {
    if !(root.is_finite() && mid.is_finite() && end.is_finite() && target.is_finite()) {
        return (Quat::IDENTITY, Quat::IDENTITY);
    }
    let pole = pole.filter(|p| p.is_finite());
    let upper = mid - root;
    let lower = end - mid;
    let a = upper.length();
    let b = lower.length();
    let to_target = target - root;

    // A zero-length bone: only aim the whole chain.
    if a < EPSILON || b < EPSILON {
        return (look_at_delta(end - root, root, target), Quat::IDENTITY);
    }

    // The direction toward the target, with fallbacks when it is at the root.
    let dir = to_target
        .try_normalize()
        .or_else(|| (end - root).try_normalize())
        .or_else(|| upper.try_normalize())
        .unwrap_or(Vec3::Y);
    let d = to_target.length().clamp((a - b).abs(), a + b);

    // Which way the mid joint bends, perpendicular to `dir`.
    let perpendicular = |v: Vec3| (v - dir * v.dot(dir)).try_normalize();
    let bend = pole
        .and_then(|p| perpendicular(p - root))
        .or_else(|| perpendicular(upper))
        .unwrap_or_else(|| stable_perpendicular(dir));

    // The mid joint by the law of cosines.
    let (along, across) = if d < EPSILON {
        (0.0, a)
    } else {
        let x = (a * a - b * b + d * d) / (2.0 * d);
        (x, (a * a - x * x).max(0.0).sqrt())
    };
    let new_mid = root + dir * along + bend * across;
    let new_end = root + dir * d;

    let root_delta = arc(upper, new_mid - root);
    let mid_delta = arc(root_delta * lower, new_end - new_mid);
    (root_delta, mid_delta)
}

/// The shortest rotation from direction `from` to direction `to`; identity
/// when either is zero.
fn arc(from: Vec3, to: Vec3) -> Quat {
    match (from.try_normalize(), to.try_normalize()) {
        (Some(a), Some(b)) => Quat::from_rotation_arc(a, b),
        _ => Quat::IDENTITY,
    }
}

/// A unit vector perpendicular to the unit vector `dir`, the same for the same
/// `dir`: toward world -Z when possible (so a straight limb pointing sideways
/// or down bends in a consistent plane), else toward +X.
fn stable_perpendicular(dir: Vec3) -> Vec3 {
    let reference = if dir.z.abs() < 0.99 {
        Vec3::NEG_Z
    } else {
        Vec3::X
    };
    (reference - dir * reference.dot(dir))
        .try_normalize()
        .unwrap_or(Vec3::X)
}
