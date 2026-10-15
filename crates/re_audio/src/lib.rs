//! `re_audio`: audio playback for Real Engine, on kira.
//!
//! `re_app` turns the scene's `AudioSource` components and scripts' one-shot
//! sounds into [`AudioCmd`]s each frame and hands them to [`Audio::apply`]. The
//! audio side never sees the scene.
//!
//! * [`Audio::new_or_headless`] opens the default output device (cpal) and falls
//!   back to the silent [`CaptureBackend`] when there is none, so games and CI run
//!   without a sound card. [`Audio::headless`] never touches a device.
//! * Clips are named by project asset path (`assets/sfx/pickup.wav`). They are
//!   loaded from the asset root on first use, or registered up front with
//!   [`Audio::insert_clip`] (generated sounds such as [`sine`] need no file).
//! * Spatial sounds play on kira spatial tracks: their volume falls with distance
//!   from the listener and they pan by direction. The listener faces −Z.
//!
//! kira 0.12.5 uses glam 0.33 while the engine uses glam 0.34, so positions and
//! rotations cross into kira only as plain arrays.
//!
//! Errors are [`re_core::Diagnostic`]s with codes RE7400–RE7499; each has an
//! explain page listed in [`DIAGNOSTICS`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod capture;
pub mod codes;
mod mixer;
mod wav;

pub use capture::{CaptureBackend, CaptureSettings};
pub use kira::sound::static_sound::StaticSoundData;
pub use mixer::{Audio, linear_to_decibels, rms, sine};
pub use re_types::audio::AudioCmd;

use re_core::DiagInfo;

/// Every diagnostic code this crate uses, with its explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::NO_OUTPUT_DEVICE,
        title: "No audio output device",
        page: include_str!("../explain/RE7400.md"),
    },
    DiagInfo {
        code: codes::CLIP_NOT_FOUND,
        title: "Audio clip not found",
        page: include_str!("../explain/RE7401.md"),
    },
    DiagInfo {
        code: codes::CLIP_DECODE_FAILED,
        title: "Audio clip cannot be decoded",
        page: include_str!("../explain/RE7402.md"),
    },
    DiagInfo {
        code: codes::PLAYBACK_LIMIT,
        title: "Too many sounds playing",
        page: include_str!("../explain/RE7403.md"),
    },
    DiagInfo {
        code: codes::INVALID_VOLUME,
        title: "Invalid volume",
        page: include_str!("../explain/RE7404.md"),
    },
    DiagInfo {
        code: codes::INVALID_RATE,
        title: "Invalid playback rate",
        page: include_str!("../explain/RE7405.md"),
    },
];

#[cfg(test)]
mod tests {
    #[test]
    fn catalogue_is_valid() {
        let problems = re_core::diag::validate_catalog(super::DIAGNOSTICS, 7400..=7499);
        assert!(problems.is_empty(), "{problems:#?}");
    }
}
