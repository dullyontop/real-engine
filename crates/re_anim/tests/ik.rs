//! Two-bone IK and aiming.

use glam::{Quat, Vec3};
use re_anim::ik::{look_at_delta, two_bone};

/// Applies a solution as `two_bone` documents it.
fn apply(root: Vec3, mid: Vec3, end: Vec3, (r1, r2): (Quat, Quat)) -> (Vec3, Vec3) {
    let new_mid = root + r1 * (mid - root);
    let new_end = new_mid + r2 * r1 * (end - mid);
    (new_mid, new_end)
}

fn finite(q: Quat) -> bool {
    q.is_finite() && (q.length() - 1.0).abs() < 1e-3
}

#[test]
fn reachable_targets_are_reached_with_lengths_kept() {
    let root = Vec3::new(0.0, 2.0, 0.0);
    let mid = Vec3::new(0.0, 1.0, 0.1);
    let end = Vec3::new(0.0, 0.0, 0.0);
    let a = (mid - root).length();
    let b = (end - mid).length();
    let targets = [
        Vec3::new(0.5, 0.8, 0.3),
        Vec3::new(-0.7, 1.5, 0.9),
        Vec3::new(0.0, 0.5, 0.0),
        Vec3::new(1.2, 2.0, -0.4),
        Vec3::new(0.0, 2.3, 0.0),
    ];
    for target in targets {
        for pole in [
            None,
            Some(Vec3::new(0.0, 1.0, 5.0)),
            Some(Vec3::new(3.0, 1.0, 0.0)),
        ] {
            let sol = two_bone(root, mid, end, target, pole);
            let (m, e) = apply(root, mid, end, sol);
            assert!(
                (e - target).length() < 1e-4,
                "{target} {pole:?}: end at {e}"
            );
            assert!(((m - root).length() - a).abs() < 1e-4);
            assert!(((e - m).length() - b).abs() < 1e-4);
        }
    }
}

#[test]
fn unreachable_targets_give_a_straight_chain_pointing_at_them() {
    let root = Vec3::ZERO;
    let mid = Vec3::new(1.0, 0.0, 0.0);
    let end = Vec3::new(1.0, -1.0, 0.0);
    let target = Vec3::new(0.0, 10.0, 10.0);
    let sol = two_bone(root, mid, end, target, None);
    let (m, e) = apply(root, mid, end, sol);
    let dir = target.normalize();
    assert!((m - dir).length() < 1e-4, "mid {m}");
    assert!((e - dir * 2.0).length() < 1e-4, "end {e}");
}

#[test]
fn the_pole_picks_the_bend_side() {
    let root = Vec3::ZERO;
    let mid = Vec3::new(0.0, -1.0, 0.0);
    let end = Vec3::new(0.0, -2.0, 0.0); // straight
    let target = Vec3::new(0.0, -1.2, 0.0);
    for pole in [
        Vec3::new(0.0, -1.0, 4.0),
        Vec3::new(0.0, -1.0, -4.0),
        Vec3::new(3.0, 0.0, 0.0),
    ] {
        let sol = two_bone(root, mid, end, target, Some(pole));
        let (m, e) = apply(root, mid, end, sol);
        assert!((e - target).length() < 1e-4);
        // The mid joint sits on the pole's side of the root-target line.
        let axis = target.normalize();
        let off = |p: Vec3| p - axis * p.dot(axis);
        assert!(off(m).dot(off(pole)) > 0.0, "pole {pole}: mid {m}");
        assert!(off(m).normalize().dot(off(pole).normalize()) > 0.999);
    }
}

#[test]
fn without_a_pole_the_current_bend_plane_is_kept() {
    let root = Vec3::ZERO;
    let mid = Vec3::new(0.0, -1.0, 0.3);
    let end = Vec3::new(0.0, -2.0, 0.0);
    let target = Vec3::new(0.0, -1.0, 0.0);
    let (m, _) = apply(root, mid, end, two_bone(root, mid, end, target, None));
    assert!(m.x.abs() < 1e-4 && m.z > 0.0, "mid {m}");
}

#[test]
fn straight_chains_without_a_pole_bend_the_same_way_every_time() {
    let root = Vec3::ZERO;
    let mid = Vec3::new(0.0, -1.0, 0.0);
    let end = Vec3::new(0.0, -2.0, 0.0);
    let target = Vec3::new(0.0, -1.5, 0.0);
    let s1 = two_bone(root, mid, end, target, None);
    let s2 = two_bone(root, mid, end, target, None);
    assert_eq!(s1, s2);
    let (m, e) = apply(root, mid, end, s1);
    assert!((e - target).length() < 1e-4);
    assert!(m.x.abs() > 1e-3 || m.z.abs() > 1e-3, "it bends");
}

#[test]
fn degenerate_inputs_give_finite_rotations() {
    let p = Vec3::new(1.0, 2.0, 3.0);
    let cases = [
        // zero-length upper bone
        (p, p, p + Vec3::X, p + Vec3::Y, None),
        // zero-length lower bone
        (p, p + Vec3::X, p + Vec3::X, p + Vec3::Y, None),
        // everything at one point
        (p, p, p, p, None),
        // target at the root, equal bones
        (p, p + Vec3::X, p + Vec3::X * 2.0, p, None),
        // target at the root, pole on the line
        (p, p + Vec3::X, p + Vec3::X * 2.0, p, Some(p)),
        // non-finite target
        (p, p + Vec3::X, p + Vec3::X * 2.0, Vec3::NAN, None),
    ];
    for (root, mid, end, target, pole) in cases {
        let (r1, r2) = two_bone(root, mid, end, target, pole);
        assert!(finite(r1) && finite(r2), "{root} {mid} {end} {target}");
    }
    // Target at the root with unequal bones: folds as far as it can.
    let root = Vec3::ZERO;
    let mid = Vec3::new(2.0, 0.0, 0.0);
    let end = Vec3::new(3.0, 0.0, 0.0);
    let sol = two_bone(root, mid, end, root, None);
    let (m, e) = apply(root, mid, end, sol);
    assert!(((m - root).length() - 2.0).abs() < 1e-4);
    assert!(
        ((e - root).length() - 1.0).abs() < 1e-4,
        "fully folded: {e}"
    );
}

#[test]
fn look_at_turns_forward_toward_the_target() {
    let from = Vec3::new(1.0, 0.0, 0.0);
    let target = Vec3::new(1.0, 5.0, 0.0);
    let q = look_at_delta(Vec3::NEG_Z, from, target);
    assert!((q * Vec3::NEG_Z - Vec3::Y).length() < 1e-5);
    // Opposite direction: a half turn.
    let q = look_at_delta(Vec3::X, Vec3::ZERO, Vec3::NEG_X * 2.0);
    assert!((q * Vec3::X - Vec3::NEG_X).length() < 1e-5);
    // Degenerate: identity.
    assert_eq!(look_at_delta(Vec3::ZERO, from, target), Quat::IDENTITY);
    assert_eq!(look_at_delta(Vec3::X, from, from), Quat::IDENTITY);
}
