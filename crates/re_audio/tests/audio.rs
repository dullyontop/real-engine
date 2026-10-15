//! Audio through the headless capture backend. No test opens an audio device.

use glam::{Quat, Vec3};
use re_audio::{Audio, AudioCmd, CaptureSettings, rms, sine};
use re_types::EntityId;

const RATE: u32 = 48_000;

fn audio() -> Audio {
    let a = Audio::headless_with(CaptureSettings { sample_rate: RATE });
    assert!(a.is_headless());
    a
}

fn one_shot(clip: &str, volume: f32) -> AudioCmd {
    AudioCmd::PlayOneShot {
        clip: clip.into(),
        position: None,
        volume,
    }
}

#[test]
fn sine_440_has_the_expected_rms() {
    let mut a = audio();
    assert_eq!(
        rms(&a.render_headless(4800).unwrap(), 0),
        0.0,
        "silent before playing"
    );
    a.insert_clip("tone", sine(440.0, 0.25, RATE, 0.5));
    assert!(a.apply(&[one_shot("tone", 1.0)]).is_empty());
    let out = a.render_headless(4800).unwrap(); // 0.1 s
    let (l, r) = (rms(&out, 0), rms(&out, 1));
    // Amplitude 0.5 sine: RMS 0.5 / sqrt(2). A centred non-spatial sound is not attenuated.
    let expect = 0.5 / std::f32::consts::SQRT_2;
    assert!(
        (l - expect).abs() < 2e-3 && (r - expect).abs() < 2e-3,
        "rms L={l} R={r}, expected {expect}"
    );

    // 440 Hz over 0.1 s: 88 zero crossings.
    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    let crossings = left
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    assert!((86..=90).contains(&crossings), "{crossings} zero crossings");

    // After the 0.25 s clip ends, the one-shot is cleaned up and output is silent.
    a.render_headless(RATE as usize);
    assert_eq!(a.playing(), 0);
    assert_eq!(rms(&a.render_headless(4800).unwrap(), 0), 0.0);
}

#[test]
fn volume_is_linear() {
    let mut a = audio();
    a.insert_clip("tone", sine(440.0, 1.0, RATE, 0.5));
    a.apply(&[one_shot("tone", 0.1)]);
    let quiet = rms(&a.render_headless(4800).unwrap(), 0);
    let expect = 0.05 / std::f32::consts::SQRT_2;
    assert!(
        (quiet - expect).abs() < 1e-3,
        "rms {quiet}, expected {expect}"
    );

    a.apply(&[AudioCmd::SetMasterVolume { volume: 0.0 }]);
    a.render_headless(480);
    assert!(
        rms(&a.render_headless(4800).unwrap(), 0) < 1e-4,
        "master volume 0 is silent"
    );
}

#[test]
fn entity_source_volume_stop_and_loop() {
    let mut a = audio();
    a.insert_clip("hum", sine(220.0, 0.1, RATE, 0.5));
    let e = EntityId(7);
    let start = AudioCmd::StartSource {
        entity: e,
        clip: "hum".into(),
        volume: 1.0,
        looping: true,
        spatial: false,
        position: Vec3::ZERO,
    };
    assert!(a.apply(&[start]).is_empty());
    let loud = rms(&a.render_headless(4800).unwrap(), 0);
    // Looping: still playing well past the 0.1 s clip length.
    a.render_headless(RATE as usize / 2);
    assert!(a.is_source_playing(e));
    assert!(rms(&a.render_headless(4800).unwrap(), 0) > 0.3);

    a.apply(&[AudioCmd::SetSourceVolume {
        entity: e,
        volume: 0.5,
    }]);
    a.render_headless(480);
    let half = rms(&a.render_headless(4800).unwrap(), 0);
    assert!((half / loud - 0.5).abs() < 0.02, "ratio {}", half / loud);

    a.apply(&[AudioCmd::StopSource { entity: e }]);
    a.render_headless(480);
    assert!(!a.is_source_playing(e));
    assert_eq!(rms(&a.render_headless(4800).unwrap(), 0), 0.0);
}

#[test]
fn spatial_source_pans_toward_its_side() {
    let mut a = audio();
    a.insert_clip("tone", sine(440.0, 2.0, RATE, 0.5));
    let e = EntityId(1);
    a.apply(&[
        AudioCmd::SetListener {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
        },
        AudioCmd::StartSource {
            entity: e,
            clip: "tone".into(),
            volume: 1.0,
            looping: false,
            spatial: true,
            position: Vec3::new(5.0, 0.0, 0.0),
        },
    ]);
    a.render_headless(2400);
    let out = a.render_headless(4800).unwrap();
    let (l, r) = (rms(&out, 0), rms(&out, 1));
    assert!(
        r > l * 1.5,
        "source at +x is louder on the right: L={l} R={r}"
    );

    a.apply(&[AudioCmd::SetSourcePosition {
        entity: e,
        position: Vec3::new(-5.0, 0.0, 0.0),
    }]);
    a.render_headless(2400);
    let out = a.render_headless(4800).unwrap();
    let (l2, r2) = (rms(&out, 0), rms(&out, 1));
    assert!(l2 > r2 * 1.5, "moved to -x: L={l2} R={r2}");

    // Turning the listener around (facing +Z) swaps the sides.
    a.apply(&[AudioCmd::SetListener {
        position: Vec3::ZERO,
        rotation: Quat::from_rotation_y(std::f32::consts::PI),
    }]);
    a.render_headless(2400);
    let out = a.render_headless(4800).unwrap();
    assert!(rms(&out, 1) > rms(&out, 0) * 1.5, "listener turned around");
}

#[test]
fn stop_all_silences_everything() {
    let mut a = audio();
    a.insert_clip("tone", sine(440.0, 2.0, RATE, 0.5));
    a.apply(&[
        one_shot("tone", 1.0),
        AudioCmd::PlayOneShot {
            clip: "tone".into(),
            position: Some(Vec3::new(1.0, 0.0, 0.0)),
            volume: 1.0,
        },
    ]);
    assert_eq!(a.playing(), 2);
    a.apply(&[AudioCmd::StopAll]);
    a.render_headless(480);
    assert_eq!(a.playing(), 0);
    assert_eq!(rms(&a.render_headless(4800).unwrap(), 0), 0.0);
}

/// A 16-bit PCM mono WAV file.
fn wav(samples: &[i16], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[test]
fn clips_load_from_the_asset_root() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("assets/sfx")).unwrap();
    let samples: Vec<i16> = (0..RATE / 2)
        .map(|i| ((std::f32::consts::TAU * 440.0 * i as f32 / RATE as f32).sin() * 16384.0) as i16)
        .collect();
    std::fs::write(dir.path().join("assets/sfx/beep.wav"), wav(&samples, RATE)).unwrap();
    std::fs::write(dir.path().join("assets/sfx/broken.wav"), b"RIFF nonsense").unwrap();

    let mut a = audio();
    let missing = a.apply(&[one_shot("assets/sfx/beep.wav", 1.0)]);
    assert_eq!(missing[0].code.to_string(), "RE7401", "no asset root yet");

    a.set_asset_root(dir.path());
    let errs = a.apply(&[one_shot("assets/sfx/beep.wav", 1.0)]);
    assert!(errs.is_empty(), "{errs:#?}");
    assert!(a.has_clip("assets/sfx/beep.wav"));
    let level = rms(&a.render_headless(4800).unwrap(), 0);
    assert!(
        (level - 0.5 / std::f32::consts::SQRT_2).abs() < 5e-3,
        "rms {level}"
    );

    let errs = a.apply(&[
        one_shot("assets/sfx/nope.wav", 1.0),
        one_shot("assets/sfx/broken.wav", 1.0),
    ]);
    let codes: Vec<String> = errs.iter().map(|d| d.code.to_string()).collect();
    assert_eq!(codes, ["RE7401", "RE7402"]);
}

#[test]
fn invalid_volume_is_a_diagnostic() {
    let mut a = audio();
    a.insert_clip("tone", sine(440.0, 0.1, RATE, 0.5));
    let errs = a.apply(&[
        one_shot("tone", -1.0),
        AudioCmd::SetMasterVolume { volume: f32::NAN },
    ]);
    assert!(errs.iter().all(|d| d.code.to_string() == "RE7404"));
    assert_eq!(errs.len(), 2);
    assert_eq!(a.playing(), 0);
}

#[test]
fn commands_round_trip_through_json() {
    let cmd = AudioCmd::StopSource {
        entity: EntityId(7),
    };
    let text = serde_json::to_string(&cmd).unwrap();
    assert_eq!(text, r#"{"cmd":"stop_source","entity":7}"#);
}

fn start(e: EntityId, clip: &str, spatial: bool, position: Vec3) -> AudioCmd {
    AudioCmd::StartSource {
        entity: e,
        clip: clip.into(),
        volume: 1.0,
        looping: false,
        spatial,
        position,
    }
}

#[test]
fn rate_and_seek_move_through_the_clip() {
    let mut a = audio();
    // 0.5 s clip: rate 2 ends it after 0.25 s.
    a.insert_clip("tone", sine(440.0, 0.5, RATE, 0.5));
    assert_eq!(a.clip_seconds("tone").unwrap(), 0.5);
    let e = EntityId(3);
    assert!(
        a.apply(&[
            start(e, "tone", false, Vec3::ZERO),
            AudioCmd::SetSourceRate {
                entity: e,
                rate: 2.0
            },
        ])
        .is_empty()
    );
    a.render_headless(RATE as usize / 5);
    assert!(a.is_source_playing(e), "playing at 0.2 s");
    a.render_headless(RATE as usize / 10);
    assert!(!a.is_source_playing(e), "over by 0.3 s at double speed");

    // A seek to 0.45 s leaves 0.05 s to play.
    a.apply(&[
        start(e, "tone", false, Vec3::ZERO),
        AudioCmd::SeekSource {
            entity: e,
            seconds: 0.45,
        },
    ]);
    a.render_headless(RATE as usize / 10);
    assert!(!a.is_source_playing(e), "seeked near the end");

    let errs = a.apply(&[AudioCmd::SetSourceRate {
        entity: e,
        rate: 0.0,
    }]);
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].code.to_string(), "RE7405");
}

#[test]
fn a_source_without_attenuation_keeps_its_volume_far_away() {
    let mut a = audio();
    a.insert_clip("tone", sine(440.0, 2.0, RATE, 0.5));
    let far = Vec3::new(0.0, 0.0, -500.0);
    let (near_e, far_e) = (EntityId(1), EntityId(2));
    a.apply(&[
        AudioCmd::SetListener {
            position: Vec3::ZERO,
            rotation: Quat::IDENTITY,
        },
        AudioCmd::SetSourceAttenuation {
            entity: near_e,
            enabled: false,
        },
        start(near_e, "tone", true, far),
    ]);
    a.render_headless(2400);
    let flat = rms(&a.render_headless(4800).unwrap(), 0);
    a.apply(&[
        AudioCmd::StopSource { entity: near_e },
        start(far_e, "tone", true, far),
    ]);
    a.render_headless(2400);
    let attenuated = rms(&a.render_headless(4800).unwrap(), 0);
    assert!(
        flat > 0.1 && attenuated < flat * 0.5,
        "unattenuated {flat}, attenuated {attenuated}"
    );
}
