//! Animation clips: produced by `re_assets` from glTF animations and sampled by
//! `re_anim`.
//!
//! A clip animates named targets (glTF nodes, matched to entities by name below
//! the animated root). Each target has optional translation, rotation and scale
//! channels.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

/// An animation clip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ClipData {
    /// The clip's name (the glTF animation name, or `animation_<index>`).
    pub name: String,
    /// The clip length in seconds (the latest keyframe time over all channels).
    pub duration: f32,
    /// The animated targets, in source order.
    pub targets: Vec<TargetChannels>,
}

/// The channels that animate one target.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TargetChannels {
    /// The target's name (the glTF node name).
    pub target: String,
    /// Translation keyframes.
    pub translation: Option<Channel<Vec3>>,
    /// Rotation keyframes (unit quaternions).
    pub rotation: Option<Channel<Quat>>,
    /// Scale keyframes.
    pub scale: Option<Channel<Vec3>>,
}

/// How values between two keyframes are computed (glTF sampler interpolation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Interpolation {
    /// Hold the previous keyframe's value.
    Step,
    /// Linear interpolation (spherical for rotations).
    #[default]
    Linear,
    /// Cubic Hermite spline. `values` holds three entries per keyframe:
    /// in-tangent, value, out-tangent.
    CubicSpline,
}

/// Keyframes of one property.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Channel<T> {
    /// How to interpolate between keyframes.
    pub interpolation: Interpolation,
    /// Keyframe times in seconds, strictly increasing.
    pub times: Vec<f32>,
    /// Keyframe values: one per time, or three per time for
    /// [`Interpolation::CubicSpline`].
    pub values: Vec<T>,
}

impl<T> Channel<T> {
    /// True when `values` has the length `times` and `interpolation` require.
    pub fn is_well_formed(&self) -> bool {
        let per_key = match self.interpolation {
            Interpolation::CubicSpline => 3,
            Interpolation::Step | Interpolation::Linear => 1,
        };
        self.values.len() == self.times.len() * per_key
            && self.times.windows(2).all(|w| w[0] < w[1])
    }
}

/// A translation, rotation and scale: a joint's local pose.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Trs {
    /// Translation.
    pub translation: Vec3,
    /// Rotation (a unit quaternion).
    pub rotation: Quat,
    /// Scale.
    pub scale: Vec3,
}

impl Trs {
    /// No translation or rotation, unit scale.
    pub const IDENTITY: Trs = Trs {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        scale: Vec3::ONE,
    };

    /// The matrix `translation * rotation * scale`.
    pub fn to_mat4(&self) -> glam::Mat4 {
        glam::Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

impl Default for Trs {
    fn default() -> Self {
        Trs::IDENTITY
    }
}

/// One joint of a [`SkinData`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct JointDesc {
    /// The joint's name: the glTF node name that clip targets use.
    pub name: String,
    /// The parent joint, always an earlier index; `None` for a root joint.
    pub parent: Option<u16>,
    /// The rest pose, relative to the parent joint (or to the skin's space for a
    /// root joint) after `parent_offset`.
    pub rest: Trs,
    /// The transform of the glTF nodes between the parent joint and this one
    /// that are not joints (for a root joint: of every ancestor node up to the
    /// scene root). `None` means identity, the usual case.
    #[serde(default)]
    pub parent_offset: Option<glam::Mat4>,
}

/// A skin: a skeleton of joints with their inverse bind matrices (ADR-0039).
///
/// A joint's model matrix is `model(parent) * parent_offset * rest` (with an
/// animated pose in place of `rest`), and its palette matrix is
/// `model * inverse_bind`. The skin's space is the glTF scene's root space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SkinData {
    /// The glTF skin name, or `skin_<index>`.
    pub name: String,
    /// The joints, every parent before its children; at most 65,535.
    pub joints: Vec<JointDesc>,
    /// One inverse bind matrix per joint.
    pub inverse_bind: Vec<glam::Mat4>,
    /// Per joint: the bounds, in the mesh's bind space, of the vertices the joint
    /// influences, or `None` when it influences none.
    pub joint_bounds: Vec<Option<crate::mesh::Aabb>>,
}

impl SkinData {
    /// True when the joint, inverse bind and bounds lists have one entry per
    /// joint, every parent comes before its child and there are at most 65,535
    /// joints.
    pub fn is_well_formed(&self) -> bool {
        let n = self.joints.len();
        n <= u16::MAX as usize
            && self.inverse_bind.len() == n
            && self.joint_bounds.len() == n
            && self
                .joints
                .iter()
                .enumerate()
                .all(|(i, j)| j.parent.is_none_or(|p| (p as usize) < i))
    }

    /// The index of the joint called `name`.
    pub fn joint(&self, name: &str) -> Option<usize> {
        self.joints.iter().position(|j| j.name == name)
    }
}
