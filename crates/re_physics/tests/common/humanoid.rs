//! A 19-joint humanoid animation skeleton and its generated 11-part ragdoll, shared by the
//! ragdoll tests and the ragdoll bench.
//!
//! The humanoid stands along +Y facing +Z, feet at the origin, every joint unrotated in
//! the neutral pose. Joint 0 (`root`) and `chest`, `neck` and the hands and feet have no
//! part, so the mapping has joints above the ragdoll root, chains and leaves.

#![allow(dead_code, reason = "test helpers, not all of them used by every test")]

use re_types::glam::{Quat, Vec3};
use re_types::physics::{JointPose, RagdollDesc, RagdollSkeleton, SkeletonJoint};

pub const JOINTS: [(&str, Option<u16>, [f32; 3]); 19] = [
    ("root", None, [0.0, 0.0, 0.0]),
    ("hips", Some(0), [0.0, 1.0, 0.0]),
    ("spine", Some(1), [0.0, 1.15, 0.0]),
    ("chest", Some(2), [0.0, 1.35, 0.0]),
    ("neck", Some(3), [0.0, 1.55, 0.0]),
    ("head", Some(4), [0.0, 1.65, 0.0]),
    ("head_end", Some(5), [0.0, 1.85, 0.0]),
    ("upper_arm_l", Some(3), [0.2, 1.5, 0.0]),
    ("forearm_l", Some(7), [0.48, 1.5, 0.0]),
    ("hand_l", Some(8), [0.74, 1.5, 0.0]),
    ("upper_arm_r", Some(3), [-0.2, 1.5, 0.0]),
    ("forearm_r", Some(10), [-0.48, 1.5, 0.0]),
    ("hand_r", Some(11), [-0.74, 1.5, 0.0]),
    ("thigh_l", Some(1), [0.1, 0.95, 0.0]),
    ("shin_l", Some(13), [0.1, 0.52, 0.0]),
    ("foot_l", Some(14), [0.1, 0.08, 0.0]),
    ("thigh_r", Some(1), [-0.1, 0.95, 0.0]),
    ("shin_r", Some(16), [-0.1, 0.52, 0.0]),
    ("foot_r", Some(17), [-0.1, 0.08, 0.0]),
];

pub const PARTS: [&str; 11] = [
    "hips",
    "spine",
    "head",
    "upper_arm_l",
    "forearm_l",
    "upper_arm_r",
    "forearm_r",
    "thigh_l",
    "shin_l",
    "thigh_r",
    "shin_r",
];

pub const MASS: f32 = 70.0;

pub fn skeleton() -> RagdollSkeleton {
    RagdollSkeleton {
        joints: JOINTS
            .iter()
            .map(|&(name, parent, pos)| SkeletonJoint {
                name: name.into(),
                parent,
                neutral: JointPose::new(Vec3::from(pos), Quat::IDENTITY),
            })
            .collect(),
    }
}

pub fn ragdoll() -> RagdollDesc {
    RagdollDesc::from_skeleton(&skeleton(), &PARTS, MASS).expect("humanoid ragdoll")
}

pub fn joint(name: &str) -> usize {
    JOINTS
        .iter()
        .position(|j| j.0 == name)
        .unwrap_or_else(|| panic!("no joint {name}"))
}

/// The model-space pose with each joint turned by `local[joint]` relative to its parent
/// (the neutral offsets kept).
pub fn posed(local: &[(&str, Quat)]) -> Vec<JointPose> {
    let s = skeleton();
    let mut out: Vec<JointPose> = Vec::with_capacity(s.joints.len());
    for joint in &s.joints {
        let turn = local
            .iter()
            .find(|(n, _)| *n == joint.name)
            .map_or(Quat::IDENTITY, |(_, q)| *q);
        let pose = match joint.parent {
            None => JointPose::new(joint.neutral.pos, turn),
            Some(p) => {
                let p = usize::from(p);
                let offset = s.joints[p].neutral.inverse().mul(&joint.neutral);
                out[p].mul(&JointPose::new(offset.pos, (offset.rot * turn).normalize()))
            }
        };
        out.push(pose);
    }
    out
}
