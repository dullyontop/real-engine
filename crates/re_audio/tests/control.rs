//! Sound control (ADR-0057 §8), distance curves, doppler and the player's
//! volumes, on the headless backend. No test opens an audio device.

use std::time::Duration;

use glam::{Quat, Vec3};
use re_audio::{Audio, AudioCmd, CaptureSettings, Rolloff, SoundOp, Spatial, rms, sine};
use re_types::EntityId;

const RATE: u32 = 48_000;
/// A step of the engine's 60 Hz clock.
const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 60);

fn audio() -> Audio {
    let mut a = Audio::headless_with(CaptureSettings { sample_rate: RATE });
    a.insert_clip("tone", sine(440.0, 2.0, RATE, 0.5));
    a
}

fn codes(problems: &[re_core::Diagnostic]) -> Vec<String> {
    problems.iter().map(|d| d.code.to_string()).collect()
}

fn listener() -> AudioCmd {
    AudioCmd::SetListener {
        position: Vec3::ZERO,
        rotation: Quat::IDENTITY,
    }
}

fn start(e: EntityId, clip: &str, looping: bool, position: Option<Vec3>) -> AudioCmd {
    AudioCmd::StartSource {
        entity: e,
        clip: clip.into(),
        volume: 1.0,
        looping,
        spatial: position.is_some(),
        position: position.unwrap_or(Vec3::ZERO),
    }
}

fn spatial(e: EntityId, rolloff: Rolloff, min: f32, max: f32, doppler: f32) -> AudioCmd {
    AudioCmd::SetSourceSpatial {
        entity: e,
        spatial: Spatial {
            rolloff,
            min_distance: min,
            max_distance: max,
            doppler,
        },
    }
}

fn control(e: EntityId, op: SoundOp) -> AudioCmd {
    AudioCmd::SourceControl { entity: e, op }
}

/// The left channel's RMS over 0.1 s, after 0.05 s to settle.
fn level(a: &mut Audio) -> f32 {
    a.render_headless(2400);
    rms(&a.render_headless(4800).unwrap(), 0)
}

#[test]
fn each_rolloff_sets_the_level_at_a_distance() {
    let mut a = audio();
    let e = EntityId(1);
    // Straight ahead, so both ears hear the same, and the curve is the only
    // difference from `off` at the same place.
    let at = |d: f32| Some(Vec3::new(0.0, 0.0, -d));
    for (rolloff, min, max, d, gain) in [
        (Rolloff::Linear, 2.0, 12.0, 7.0, 0.5),
        (Rolloff::Inverse, 2.0, 16.0, 8.0, 0.25),
        // Beyond max_distance the inverse curve keeps its level there.
        (Rolloff::Inverse, 2.0, 16.0, 64.0, 0.125),
        // Halfway is -30 dB.
        (Rolloff::Natural, 1.0, 101.0, 51.0, 0.031_623),
        (Rolloff::Natural, 1.0, 101.0, 101.0, 0.0),
        (Rolloff::Linear, 0.0, 10.0, 2.5, 0.75),
    ] {
        let problems = a.apply(&[
            listener(),
            spatial(e, Rolloff::Off, min, max, 0.0),
            start(e, "tone", true, at(d)),
        ]);
        assert!(problems.is_empty(), "{problems:?}");
        let flat = level(&mut a);
        assert!(flat > 0.1, "{rolloff:?}: {flat}");
        a.apply(&[
            AudioCmd::StopSource { entity: e },
            spatial(e, rolloff, min, max, 0.0),
            start(e, "tone", true, at(d)),
        ]);
        let ratio = level(&mut a) / flat;
        assert!(
            (ratio - gain).abs() < 0.01,
            "{rolloff:?} {min}-{max} m at {d} m: {ratio}, expected {gain}"
        );
        a.apply(&[AudioCmd::StopSource { entity: e }]);
    }

    // Without settings a sound falls as before they existed: natural from 1 m
    // to 100 m. A stop forgets the entity's settings.
    a.apply(&[start(e, "tone", true, at(1.0))]);
    let near = level(&mut a);
    a.apply(&[AudioCmd::SetSourcePosition {
        entity: e,
        position: Vec3::new(0.0, 0.0, -50.5),
    }]);
    let ratio = level(&mut a) / near;
    assert!((ratio - 0.031_623).abs() < 0.002, "{ratio}");
}

#[test]
fn spatial_settings_change_a_playing_sound() {
    let mut a = audio();
    let e = EntityId(2);
    a.apply(&[
        listener(),
        spatial(e, Rolloff::Off, 1.0, 100.0, 0.0),
        start(e, "tone", true, Some(Vec3::new(0.0, 0.0, -50.0))),
    ]);
    let flat = level(&mut a);
    a.apply(&[spatial(e, Rolloff::Linear, 0.0, 100.0, 0.0)]);
    let half = level(&mut a);
    assert!((half / flat - 0.5).abs() < 0.01, "{}", half / flat);

    // Settings the mixer refuses leave the sound as it was.
    for bad in [
        spatial(e, Rolloff::Natural, -1.0, 10.0, 0.0),
        spatial(e, Rolloff::Natural, 10.0, 10.0, 0.0),
        spatial(e, Rolloff::Natural, 1.0, f32::INFINITY, 0.0),
        spatial(e, Rolloff::Inverse, 0.0, 10.0, 0.0),
        spatial(e, Rolloff::Natural, 1.0, 10.0, -1.0),
        spatial(e, Rolloff::Natural, 1.0, 10.0, f32::NAN),
    ] {
        assert_eq!(codes(&a.apply(&[bad])), ["RE7412"]);
    }
    let still = level(&mut a);
    assert!((still / flat - 0.5).abs() < 0.01, "{}", still / flat);

    // Turning attenuation off still wins: the sound only pans.
    a.apply(&[
        AudioCmd::StopSource { entity: e },
        AudioCmd::SetSourceAttenuation {
            entity: e,
            enabled: false,
        },
        spatial(e, Rolloff::Linear, 0.0, 100.0, 0.0),
        start(e, "tone", true, Some(Vec3::new(0.0, 0.0, -50.0))),
    ]);
    let ratio = level(&mut a) / flat;
    assert!((ratio - 1.0).abs() < 0.01, "{ratio}");
}

#[test]
fn sources_pause_resume_fade_and_stop() {
    let mut a = audio();
    let e = EntityId(3);
    a.apply(&[start(e, "tone", true, None)]);
    let full = level(&mut a);

    // A pause fades out and holds; a resume fades back in.
    a.apply(&[control(e, SoundOp::Pause { fade: 0.1 })]);
    a.render_headless(RATE as usize / 5);
    assert_eq!(rms(&a.render_headless(4800).unwrap(), 0), 0.0, "paused");
    assert!(a.is_source_playing(e), "a paused sound is kept");
    a.apply(&[control(e, SoundOp::Resume { fade: 0.1 })]);
    a.render_headless(RATE as usize / 10);
    let back = level(&mut a);
    assert!((back / full - 1.0).abs() < 0.01, "{}", back / full);

    // A volume fade takes its time: kira fades in decibels, so halfway from
    // 0 dB to -6 dB is -3 dB.
    a.apply(&[control(
        e,
        SoundOp::Volume {
            volume: 0.5,
            fade: 0.5,
        },
    )]);
    a.render_headless(RATE as usize / 4 - 240);
    let mid = rms(&a.render_headless(480).unwrap(), 0) / full;
    assert!((mid - 0.708).abs() < 0.03, "{mid}");
    a.render_headless(RATE as usize / 2);
    let low = level(&mut a) / full;
    assert!((low - 0.5).abs() < 0.01, "{low}");

    // A stop fades out too, then frees the voice.
    a.apply(&[control(e, SoundOp::Stop { fade: 0.2 })]);
    a.render_headless(RATE as usize / 10);
    assert!(a.is_source_playing(e), "still fading at 0.1 s");
    a.render_headless(RATE as usize / 5);
    assert!(!a.is_source_playing(e), "stopped by 0.3 s");
    assert_eq!(a.playing(), 0);

    // `start` is the audio sync's to resolve; in the mixer it does nothing,
    // like any control of a sound that is not playing.
    assert!(a.apply(&[control(e, SoundOp::Start)]).is_empty());
    assert_eq!(a.playing(), 0);
}

#[test]
fn seek_and_rate_fades_move_through_the_clip() {
    let mut a = audio();
    a.insert_clip("second", sine(440.0, 1.0, RATE, 0.5));
    let e = EntityId(4);
    // From rate 1 to 2 over 0.5 s covers 0.75 s of the clip; the last 0.25 s
    // at rate 2 takes 0.125 s: the clip ends at 0.625 s.
    a.apply(&[
        start(e, "second", false, None),
        control(
            e,
            SoundOp::Rate {
                rate: 2.0,
                fade: 0.5,
            },
        ),
    ]);
    a.render_headless(RATE as usize * 57 / 100);
    assert!(a.is_source_playing(e), "playing at 0.57 s");
    a.render_headless(RATE as usize * 11 / 100);
    assert!(!a.is_source_playing(e), "over by 0.68 s");

    // A seek to 0.95 s leaves 0.05 s.
    a.apply(&[
        start(e, "second", false, None),
        control(e, SoundOp::Seek { seconds: 0.95 }),
    ]);
    a.render_headless(RATE as usize / 10);
    assert!(!a.is_source_playing(e), "sought near the end");
}

#[test]
fn bad_controls_are_refused() {
    let mut a = audio();
    let e = EntityId(5);
    a.apply(&[start(e, "tone", true, None)]);
    for (op, code) in [
        (SoundOp::Stop { fade: -1.0 }, "RE7411"),
        (SoundOp::Pause { fade: f32::NAN }, "RE7411"),
        (
            SoundOp::Resume {
                fade: f32::INFINITY,
            },
            "RE7411",
        ),
        (SoundOp::Seek { seconds: -0.5 }, "RE7411"),
        (SoundOp::Seek { seconds: f64::NAN }, "RE7411"),
        (
            SoundOp::Volume {
                volume: -1.0,
                fade: 0.0,
            },
            "RE7404",
        ),
        (
            SoundOp::Volume {
                volume: 1.0,
                fade: -1.0,
            },
            "RE7411",
        ),
        (
            SoundOp::Rate {
                rate: 0.0,
                fade: 0.0,
            },
            "RE7405",
        ),
        (
            SoundOp::Rate {
                rate: 1.0,
                fade: f32::NAN,
            },
            "RE7411",
        ),
    ] {
        assert_eq!(codes(&a.apply(&[control(e, op)])), [code], "{op:?}");
        assert_eq!(
            codes(&a.apply(&[AudioCmd::SoundControl { id: 1, op }])),
            [code],
            "{op:?}"
        );
    }
    assert!(a.is_source_playing(e));
    // A fade longer than any clock still plays.
    assert!(
        a.apply(&[control(e, SoundOp::Stop { fade: 1e30 })])
            .is_empty()
    );
    a.render_headless(4800);
    assert!(a.is_source_playing(e));
}

#[test]
fn one_shots_are_controlled_by_id() {
    let mut a = audio();
    let shot = |id: Option<u64>| AudioCmd::PlayOneShot {
        clip: "tone".into(),
        position: None,
        volume: 1.0,
        id,
    };
    a.apply(&[shot(Some(7)), shot(Some(8)), shot(None)]);
    assert_eq!(a.playing(), 3);
    // An unknown id does nothing, quietly.
    let stop = SoundOp::Stop { fade: 0.0 };
    assert!(
        a.apply(&[AudioCmd::SoundControl { id: 9, op: stop }])
            .is_empty()
    );
    assert!(
        a.apply(&[AudioCmd::SoundControl { id: 7, op: stop }])
            .is_empty()
    );
    a.render_headless(480);
    assert_eq!(a.playing(), 2);
    a.apply(&[AudioCmd::SoundControl {
        id: 8,
        op: SoundOp::Pause { fade: 0.0 },
    }]);
    a.render_headless(4800);
    // Only the unnamed one-shot is heard; the paused one waits.
    let alone = rms(&a.render_headless(4800).unwrap(), 0);
    assert!(
        (alone - 0.5 / std::f32::consts::SQRT_2).abs() < 0.01,
        "{alone}"
    );
    assert_eq!(a.playing(), 2);
    // StopAll ends every handle.
    a.apply(&[AudioCmd::StopAll]);
    a.render_headless(480);
    assert_eq!(a.playing(), 0);
    assert!(
        a.apply(&[AudioCmd::SoundControl { id: 8, op: stop }])
            .is_empty()
    );
}

#[test]
fn a_fade_out_finishes_when_its_source_restarts_or_goes() {
    let mut a = audio();
    let e = EntityId(6);
    a.apply(&[start(e, "tone", true, None)]);
    a.render_headless(480);
    // Started again during its fade-out: both play until the fade ends.
    a.apply(&[
        control(e, SoundOp::Stop { fade: 0.5 }),
        start(e, "tone", true, None),
    ]);
    a.render_headless(4800);
    assert_eq!(a.playing(), 2);
    a.render_headless(RATE as usize / 2);
    assert_eq!(a.playing(), 1);
    // Stopped by the sync (the entity went) during a fade-out: it finishes.
    a.apply(&[control(e, SoundOp::Stop { fade: 0.5 })]);
    a.render_headless(480);
    a.apply(&[AudioCmd::StopSource { entity: e }]);
    a.render_headless(4800);
    assert_eq!(a.playing(), 1);
    assert!(!a.is_source_playing(e));
    a.render_headless(RATE as usize / 2);
    assert_eq!(a.playing(), 0);
    // Without a fade under way, a stop is at once.
    a.apply(&[start(e, "tone", true, None)]);
    a.apply(&[AudioCmd::StopSource { entity: e }]);
    a.render_headless(480);
    assert_eq!(a.playing(), 0);
}

/// Plays a 2 s clip at 150 m ahead with `doppler`, moving the source at
/// `source` m/s and the listener at `ear` m/s along Z, a frame at a time;
/// returns whether it still plays at `seconds`.
fn moving(doppler: f32, source: f32, ear: f32, seconds: f64) -> bool {
    let mut a = audio();
    let e = EntityId(7);
    a.apply(&[
        listener(),
        spatial(e, Rolloff::Off, 1.0, 100.0, doppler),
        start(e, "tone", false, Some(Vec3::new(0.0, 0.0, -150.0))),
    ]);
    let frames = (seconds * 60.0).round() as u32;
    for frame in 1..=frames {
        a.advance(FRAME);
        let t = f64::from(frame) / 60.0;
        a.apply(&[
            AudioCmd::SetSourcePosition {
                entity: e,
                position: Vec3::new(0.0, 0.0, -150.0 + source * t as f32),
            },
            AudioCmd::SetListener {
                position: Vec3::new(0.0, 0.0, ear * t as f32),
                rotation: Quat::IDENTITY,
            },
        ]);
    }
    a.is_source_playing(e)
}

#[test]
fn doppler_follows_the_source_and_the_listener() {
    // Without doppler a 2 s clip ends at 2 s, however things move.
    assert!(moving(0.0, 34.3, 0.0, 1.9));
    assert!(!moving(0.0, 34.3, 0.0, 2.1));
    // Closing at a tenth of the speed of sound plays it 1/0.9 times as fast:
    // it ends at about 1.8 s.
    assert!(!moving(1.0, 34.3, 0.0, 1.9));
    // Moving apart at a tenth, 1/1.1 times as fast: about 2.2 s.
    assert!(moving(1.0, -34.3, 0.0, 2.1));
    // The listener moving away: 0.9 times as fast, about 2.22 s.
    assert!(moving(1.0, 0.0, 34.3, 2.1));
    // Doppler 2 doubles the speeds: 1/0.8, about 1.6 s.
    assert!(!moving(2.0, 34.3, 0.0, 1.7));
}

#[test]
fn turning_doppler_off_returns_to_the_rate() {
    let mut a = audio();
    a.insert_clip("second", sine(440.0, 1.0, RATE, 0.5));
    let e = EntityId(8);
    a.apply(&[
        listener(),
        spatial(e, Rolloff::Off, 1.0, 100.0, 1.0),
        start(e, "second", false, Some(Vec3::new(0.0, 0.0, -150.0))),
    ]);
    // Approaching for 0.2 s, then still with doppler off: the clip ends at
    // about 1.02 s, not 0.98 s (all doppler) nor 1.0 s (none).
    for frame in 1..=66 {
        a.advance(FRAME);
        let z = -150.0 + 34.3 * frame.min(12) as f32 / 60.0;
        let mut cmds = vec![AudioCmd::SetSourcePosition {
            entity: e,
            position: Vec3::new(0.0, 0.0, z),
        }];
        if frame == 12 {
            cmds.push(spatial(e, Rolloff::Off, 1.0, 100.0, 0.0));
        }
        a.apply(&cmds);
        if frame == 58 {
            assert!(a.is_source_playing(e), "playing at 0.967 s");
        }
    }
    assert!(!a.is_source_playing(e), "over by 1.1 s");
}

#[test]
fn user_volumes_scale_after_the_meter_and_keep_through_restarts() {
    let mut a = audio();
    let play = AudioCmd::PlayOneShotOnBus {
        clip: "tone".into(),
        position: None,
        volume: 1.0,
        bus: "sfx".into(),
        id: None,
    };
    a.apply(std::slice::from_ref(&play));
    let full = level(&mut a);
    let metered = |a: &mut Audio| {
        a.bus_levels();
        a.render_headless(4800);
        a.bus_levels()
            .into_iter()
            .find(|l| l.bus == "sfx")
            .unwrap()
            .rms_db
    };
    let sfx_db = metered(&mut a);

    assert!(a.set_user_volume(Some("sfx"), 0.5).is_ok());
    assert!((level(&mut a) / full - 0.5).abs() < 0.01);
    assert!(
        (metered(&mut a) - sfx_db).abs() < 0.01,
        "the meter reads before it"
    );
    assert!(a.set_user_volume(None, 0.5).is_ok());
    assert!((level(&mut a) / full - 0.25).abs() < 0.01);
    // SetMasterVolume and the player's master volume add in decibels.
    a.apply(&[AudioCmd::SetMasterVolume { volume: 0.5 }]);
    assert!((level(&mut a) / full - 0.125).abs() < 0.005);

    // A restart and a rebuilt graph keep them.
    a.apply(&[AudioCmd::StopAll]);
    assert!(a.set_graph(&re_audio::AudioGraphDesc::default()).is_empty());
    a.apply(&[play]);
    assert!((level(&mut a) / full - 0.125).abs() < 0.005);

    // Refused values and names change nothing.
    for (bus, volume, code) in [
        (Some("sfx"), 1.5, "RE7404"),
        (None, -0.5, "RE7404"),
        (None, f32::NAN, "RE7404"),
        (Some("sfz"), 0.5, "RE7408"),
    ] {
        let d = a.set_user_volume(bus, volume).unwrap_err();
        assert_eq!(d.code.to_string(), code, "{bus:?} {volume}");
    }
    assert!((level(&mut a) / full - 0.125).abs() < 0.005);
}
