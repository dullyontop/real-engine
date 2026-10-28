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
//! * WAV files and Ogg Vorbis files of 10 s or less are decoded whole and
//!   kept; a longer Ogg file streams from its file through a ring of frames
//!   (ADR-0066), decoded on one stream thread on the desktop and in
//!   [`Audio::advance`] in the browser and headless runs, so a 3-minute track
//!   takes well under a megabyte while it plays. Seeks land on the exact
//!   frame, and a looping stream repeats without a gap.
//! * Sounds play on named buses (ADR-0041): a tree of kira sub-tracks with
//!   kira's built-in effects, plus send tracks, built from an
//!   [`AudioGraphDesc`] (the `[audio]` section of `real.toml`) by
//!   [`Audio::set_graph`]. Commands tween bus volumes and effect parameters, or
//!   apply a mixer snapshot; [`Audio::bus_levels`] meters every bus. The
//!   headless backend runs the same graph, so tests check levels without a device.
//! * Spatial sounds play on kira spatial tracks under their bus: they pan by
//!   direction, and their volume falls with distance from the listener on the
//!   curve their entity's `SetSourceSpatial` chooses (`natural`, `inverse`,
//!   `linear` or `off`), which can change while they play. Doppler multiplies
//!   a source's rate by a factor from its and the listener's motion, which
//!   [`Audio::advance`]'s clock times. The listener faces −Z.
//! * Commands stop, pause, resume, seek and fade an entity's sound
//!   (`SourceControl`) or a one-shot started with an id (`SoundControl`),
//!   ADR-0057 §8. [`Audio::set_user_volume`] sets the player's volumes.
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
mod graph;
mod meter;
mod mixer;
mod spatial;
mod stream;
mod vorbis;
mod wav;

pub use capture::{CaptureBackend, CaptureSettings};
pub use graph::{DEFAULT_SNAPSHOT, MAX_SENDS};
pub use kira::sound::static_sound::StaticSoundData;
pub use mixer::{Audio, MAX_FADE, MAX_RATE, linear_to_decibels, rms, sine};
pub use re_types::audio::{AudioCmd, AudioGraphDesc, BusLevel, Rolloff, SoundOp, Spatial};
pub use stream::check_ogg;

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
    DiagInfo {
        code: codes::INVALID_EFFECT,
        title: "Invalid audio effect",
        page: include_str!("../explain/RE7406.md"),
    },
    DiagInfo {
        code: codes::INVALID_GRAPH,
        title: "Invalid audio bus graph",
        page: include_str!("../explain/RE7407.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_BUS,
        title: "Unknown audio bus or snapshot",
        page: include_str!("../explain/RE7408.md"),
    },
    DiagInfo {
        code: codes::UNSUPPORTED_AUDIO,
        title: "Unsupported audio file",
        page: include_str!("../explain/RE7409.md"),
    },
    DiagInfo {
        code: codes::STREAM_FAILED,
        title: "Audio stream failed",
        page: include_str!("../explain/RE7410.md"),
    },
    DiagInfo {
        code: codes::INVALID_SOUND_CONTROL,
        title: "Invalid sound control",
        page: include_str!("../explain/RE7411.md"),
    },
    DiagInfo {
        code: codes::INVALID_SPATIAL,
        title: "Invalid spatial sound settings",
        page: include_str!("../explain/RE7412.md"),
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
