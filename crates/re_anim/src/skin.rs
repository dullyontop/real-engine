//! Skeletal poses for skins (ADR-0039): sampling clips onto a skin's joints,
//! the joints' model matrices, the joint palette the renderer skins with, and
//! conservative skinned bounds.
//!
//! Joints are addressed by index, not by name: a [`BoundClip`] resolves a
//! [`ClipData`]'s targets against a [`SkinData`] once, so sampling a pose does
//! no string lookups. Everything is plain computation; the caller decides when
//! to evaluate (each fixed step) and what to do with the result.
//!
//! ```
//! use re_anim::skin::{BoundClip, ModelPose, Pose, palette_into};
//! # use re_types::anim::{SkinData, JointDesc, Trs, ClipData};
//! # let skin = SkinData { name: "s".into(), joints: vec![JointDesc { name: "root".into(),
//! #     parent: None, rest: Trs::IDENTITY, parent_offset: None }],
//! #     inverse_bind: vec![glam::Mat4::IDENTITY], joint_bounds: vec![None] };
//! # let clip = ClipData::default();
//! let walk = BoundClip::bind(&clip, &skin)?;
//! let mut pose = Pose::rest(&skin);
//! walk.sample(0.25, &mut pose);
//! let mut model = ModelPose::default();
//! model.compute(&skin, &pose);
//! let mut palette = Vec::new();
//! palette_into(&skin, &model, &mut palette);
//! # Ok::<(), re_core::Diagnostic>(())
//! ```

use std::sync::Arc;

use glam::{Mat4, Quat, Vec3};
use re_core::Diagnostic;
use re_types::anim::{ClipData, SkinData, Trs};
use re_types::mesh::Aabb;

use crate::codes;
use crate::sampler::{Sampler, sample_quat, sample_vec3};

/// Checks that `skin` can be evaluated: one inverse bind matrix and one bound
/// per joint, parents before children, at most 65,535 joints.
///
/// # Errors
///
/// [`codes::SKIN_MALFORMED`] naming what is wrong.
pub fn check_skin(skin: &SkinData) -> Result<(), Diagnostic> {
    if skin.is_well_formed() {
        return Ok(());
    }
    let n = skin.joints.len();
    let detail = if n > u16::MAX as usize {
        format!("{n} joints (at most 65,535)")
    } else if skin.inverse_bind.len() != n || skin.joint_bounds.len() != n {
        format!(
            "{n} joints, {} inverse bind matrices and {} joint bounds",
            skin.inverse_bind.len(),
            skin.joint_bounds.len()
        )
    } else {
        let (i, j) = skin
            .joints
            .iter()
            .enumerate()
            .find(|(i, j)| j.parent.is_some_and(|p| p as usize >= *i))
            .expect("some parent is out of order");
        format!(
            "joint {i} (`{}`) has parent {}, which does not come before it",
            j.name,
            j.parent.expect("found above")
        )
    };
    Err(Diagnostic::error(
        codes::SKIN_MALFORMED,
        format!("skin `{}`: {detail}", skin.name),
    )
    .with_hint("re-import the model, or run `real explain RE7606`"))
}

/// A local pose: one translation, rotation and scale per joint of a skin.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pose {
    /// Per joint, relative to its parent (after the joint's `parent_offset`).
    pub joints: Vec<Trs>,
}

impl Pose {
    /// The skin's rest pose.
    pub fn rest(skin: &SkinData) -> Pose {
        Pose {
            joints: skin.joints.iter().map(|j| j.rest).collect(),
        }
    }

    /// Resets every joint to the skin's rest pose, reusing the allocation.
    pub fn reset(&mut self, skin: &SkinData) {
        self.joints.clear();
        self.joints.extend(skin.joints.iter().map(|j| j.rest));
    }

    /// Moves every joint `weight` of the way toward `other` (0 keeps `self`, 1
    /// gives `other`): translation and scale linearly, rotation by normalised
    /// lerp along the shorter arc. Joints past the shorter pose are left alone.
    pub fn blend_toward(&mut self, other: &Pose, weight: f32) {
        for (a, b) in self.joints.iter_mut().zip(&other.joints) {
            *a = blend(a, b, weight);
        }
    }
}

/// `a` moved `w` of the way toward `b` (nlerp for the rotation).
pub fn blend(a: &Trs, b: &Trs, w: f32) -> Trs {
    Trs {
        translation: a.translation.lerp(b.translation, w),
        rotation: nlerp(a.rotation, b.rotation, w),
        scale: a.scale.lerp(b.scale, w),
    }
}

/// Normalised lerp along the shorter arc. Unlike slerp it needs no
/// trigonometry, so blends use no platform maths function (ADR-0036).
pub fn nlerp(a: Quat, b: Quat, w: f32) -> Quat {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    let q = a * (1.0 - w) + b * w;
    let len2 = q.length_squared();
    if len2 > 0.0 && len2.is_finite() {
        q * (1.0 / len2.sqrt())
    } else {
        a
    }
}

/// A clip resolved against a skin: each channel's joint index, found once by
/// joint name. Targets the skin has no joint for are listed in
/// [`BoundClip::unmatched`] and ignored.
#[derive(Clone, Debug)]
pub struct BoundClip {
    sampler: Arc<Sampler>,
    /// (joint index, clip target index), in clip target order.
    channels: Vec<(u16, usize)>,
    unmatched: Vec<String>,
}

impl BoundClip {
    /// Binds `clip` to `skin`'s joints by name.
    ///
    /// # Errors
    ///
    /// [`codes::CLIP_MALFORMED`] for a channel that cannot be sampled and
    /// [`codes::SKIN_MALFORMED`] for a skin that fails [`check_skin`].
    pub fn bind(clip: &ClipData, skin: &SkinData) -> Result<BoundClip, Diagnostic> {
        BoundClip::bind_shared(Arc::new(Sampler::new(clip.clone())?), skin)
    }

    /// Binds an already checked clip, shared between skins and characters.
    ///
    /// # Errors
    ///
    /// [`codes::SKIN_MALFORMED`] for a skin that fails [`check_skin`].
    pub fn bind_shared(sampler: Arc<Sampler>, skin: &SkinData) -> Result<BoundClip, Diagnostic> {
        check_skin(skin)?;
        let mut channels = Vec::new();
        let mut unmatched = Vec::new();
        for (t, target) in sampler.clip().targets.iter().enumerate() {
            match skin.joint(&target.target) {
                Some(j) => channels.push((j as u16, t)),
                None => unmatched.push(target.target.clone()),
            }
        }
        Ok(BoundClip {
            sampler,
            channels,
            unmatched,
        })
    }

    /// The clip.
    pub fn clip(&self) -> &ClipData {
        self.sampler.clip()
    }

    /// The clip length in seconds.
    pub fn duration(&self) -> f32 {
        self.sampler.duration()
    }

    /// `time` wrapped into the clip (looping) or clamped to it.
    pub fn wrap_time(&self, time: f32, looping: bool) -> f32 {
        self.sampler.wrap_time(time, looping)
    }

    /// The clip targets no joint matched, in clip order.
    pub fn unmatched(&self) -> &[String] {
        &self.unmatched
    }

    /// The number of joints the clip animates.
    pub fn animated_joints(&self) -> usize {
        self.channels.len()
    }

    /// Writes the clip's value at `time` (clip seconds) into the joints it
    /// animates. Joints and properties without a channel keep `pose`'s value.
    pub fn sample(&self, time: f32, pose: &mut Pose) {
        let targets = &self.sampler.clip().targets;
        for &(j, t) in &self.channels {
            let Some(joint) = pose.joints.get_mut(j as usize) else {
                continue;
            };
            let target = &targets[t];
            if let Some(c) = &target.translation {
                joint.translation = sample_vec3(c, time);
            }
            if let Some(c) = &target.rotation {
                joint.rotation = sample_quat(c, time);
            }
            if let Some(c) = &target.scale {
                joint.scale = sample_vec3(c, time);
            }
        }
    }
}

/// Every joint's model matrix: `model(parent) * parent_offset * local`, in the
/// skin's space. WP22's ragdolls read and overwrite it before the palette is
/// built.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelPose {
    /// One matrix per joint.
    pub joints: Vec<Mat4>,
}

impl ModelPose {
    /// Computes the model matrices of `pose` on `skin`, reusing the allocation.
    /// A pose with fewer joints than the skin uses the rest pose for the rest.
    pub fn compute(&mut self, skin: &SkinData, pose: &Pose) {
        self.joints.clear();
        for (i, joint) in skin.joints.iter().enumerate() {
            let local = pose.joints.get(i).unwrap_or(&joint.rest).to_mat4();
            let local = match joint.parent_offset {
                Some(offset) => offset * local,
                None => local,
            };
            let m = match joint.parent {
                // `check_skin` guarantees the parent is already computed; a
                // malformed skin falls back to the skin's space.
                Some(p) => match self.joints.get(p as usize) {
                    Some(&parent) => parent * local,
                    None => local,
                },
                None => local,
            };
            self.joints.push(m);
        }
    }

    /// The model matrix of joint `index`.
    pub fn joint(&self, index: usize) -> Option<Mat4> {
        self.joints.get(index).copied()
    }

    /// The position of joint `index` in the skin's space.
    pub fn position(&self, index: usize) -> Option<Vec3> {
        self.joint(index).map(|m| m.w_axis.truncate())
    }
}

/// Appends the joint palette of `model` to `out`: `model * inverse_bind` per
/// joint, the matrices a `SkinnedDrawItem` points at.
pub fn palette_into(skin: &SkinData, model: &ModelPose, out: &mut Vec<Mat4>) {
    out.extend(
        model
            .joints
            .iter()
            .zip(&skin.inverse_bind)
            .map(|(m, ibm)| *m * *ibm),
    );
}

/// A conservative world-space bound of the skinned mesh: each joint's
/// bind-space vertex bound moved by its palette matrix and then by `world`.
/// `None` when no joint influences a vertex.
pub fn skinned_bounds(skin: &SkinData, palette: &[Mat4], world: &Mat4) -> Option<Aabb> {
    skin.joint_bounds
        .iter()
        .zip(palette)
        .filter_map(|(b, m)| b.map(|b| b.transformed(&(*world * *m))))
        .reduce(|a, b| a.union(&b))
}
