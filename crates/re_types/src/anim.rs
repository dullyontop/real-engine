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
