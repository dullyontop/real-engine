//! `re_anim`: animation-lite for Real Engine.
//!
//! Two pieces, both plain computation with no knowledge of the scene:
//!
//! * [`Sampler`] evaluates a [`ClipData`] (imported from glTF node animations by
//!   `re_assets`) at a time, giving the translation, rotation and scale of each
//!   animated target. Step, linear (spherical for rotations) and cubic-spline
//!   interpolation are supported, as in glTF. [`Playhead`] advances a clip's time
//!   the way the scene's `AnimPlayer` component describes it (speed, looping,
//!   playing).
//! * [`Tween`] moves a value from one end to another over a duration with an
//!   [`Easing`] curve. Scripts use it for doors, camera moves and UI.
//! * [`keyframes`] animates jointed rigs from keyframe poses: a
//!   [`KeyframeClip`] is played by [`Track`]s (fades, speed, looping, events)
//!   and a [`Mixer`] blends the tracks of one rig by [`Priority`].
//! * [`skin`] poses skins (ADR-0039): [`skin::BoundClip`] samples a clip onto
//!   a skin's joints by index, [`skin::ModelPose`] gives the joints' model
//!   matrices, and [`skin::palette_into`] the joint palette the renderer skins
//!   with.
//! * [`ik`] aims joints ([`ik::look_at_delta`]) and bends two-bone chains to
//!   reach a target ([`ik::two_bone`]).
//!
//! `re_app` matches a clip's targets to entities by name below the animated root
//! and writes the sampled values into their transforms.
//!
//! Errors are [`re_core::Diagnostic`]s with codes RE7600–RE7699; each has an
//! explain page listed in [`DIAGNOSTICS`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod codes;
pub mod ik;
pub mod keyframes;
mod sampler;
pub mod skin;
mod tween;

pub use keyframes::{
    DEFAULT_FADE_TIME, DEFAULT_SPEED, DEFAULT_WEIGHT, JointKey, Keyframe, KeyframeClip, Marker,
    Mixer, PoseDirection, PoseEasing, PoseStyle, Priority, SampledPoses, Track, TrackEvent,
    TrackId,
};
pub use sampler::{Playhead, Sampler, TargetPose};
pub use tween::{Easing, Tween, Tweenable};

use re_core::DiagInfo;
pub use re_types::{ClipData, Interpolation};

/// Every diagnostic code this crate uses, with its explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::CLIP_MALFORMED,
        title: "Malformed animation clip",
        page: include_str!("../explain/RE7600.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_EASING,
        title: "Unknown easing name",
        page: include_str!("../explain/RE7601.md"),
    },
    DiagInfo {
        code: codes::TWEEN_INVALID,
        title: "Invalid tween duration",
        page: include_str!("../explain/RE7602.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_POSE_EASING,
        title: "Unknown pose easing name",
        page: include_str!("../explain/RE7603.md"),
    },
    DiagInfo {
        code: codes::KEYFRAME_CLIP_MALFORMED,
        title: "Malformed keyframe clip",
        page: include_str!("../explain/RE7604.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_PRIORITY,
        title: "Unknown animation priority",
        page: include_str!("../explain/RE7605.md"),
    },
    DiagInfo {
        code: codes::SKIN_MALFORMED,
        title: "Malformed skin",
        page: include_str!("../explain/RE7606.md"),
    },
];

#[cfg(test)]
mod tests {
    #[test]
    fn catalogue_is_valid() {
        let problems = re_core::diag::validate_catalog(super::DIAGNOSTICS, 7600..=7699);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
