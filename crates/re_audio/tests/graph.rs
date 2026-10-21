//! The mixer graph (ADR-0041) on the headless backend: buses, sends, effects,
//! snapshots and meters. No test opens an audio device.
//!
//! Each effect case renders 0.2 s of a fixed stimulus and compares it with a
//! reference buffer under `tests/reference/` (raw little-endian `f32`,
//! interleaved stereo at 48 kHz). The comparison allows 1e-4 per sample, which
//! absorbs last-bit differences in the platform maths library on Windows and
//! macOS; a second render on the same machine must match bit for bit.
//! `RE_UPDATE_AUDIO_REFERENCE=1` rewrites the references, and a reviewer checks
//! the rewrite.

use std::path::PathBuf;
use std::sync::Arc;

use kira::Frame;
use re_audio::{Audio, AudioCmd, AudioGraphDesc, CaptureSettings, StaticSoundData, rms, sine};
use re_core::Diagnostic;
use re_types::EntityId;
use re_types::audio::EffectParam;

const RATE: u32 = 48_000;
/// Frames per render call: what a 100 Hz device callback asks for.
const BATCH: usize = 480;
/// 0.2 s.
const FRAMES: usize = 9_600;
const TOLERANCE: f32 = 1e-4;

fn audio() -> Audio {
    Audio::headless_with(CaptureSettings { sample_rate: RATE })
}

fn graph(json: &str) -> AudioGraphDesc {
    serde_json::from_str(json).expect("a valid graph description")
}

fn codes(problems: &[Diagnostic]) -> Vec<String> {
    problems.iter().map(|d| d.code.to_string()).collect()
}

/// A 440 Hz sine at amplitude 0.25 plus a full-scale click at 50 ms, 0.1 s long.
fn stimulus() -> StaticSoundData {
    let mut data = sine(440.0, 0.1, RATE, 0.25);
    let mut frames: Vec<Frame> = data.frames.to_vec();
    frames[RATE as usize / 20] = Frame::from_mono(0.9);
    data.frames = Arc::from(frames);
    data
}

fn play(bus: &str) -> AudioCmd {
    AudioCmd::PlayOneShotOnBus {
        clip: "stimulus".into(),
        position: None,
        volume: 1.0,
        bus: bus.into(),
    }
}

fn render(a: &mut Audio, frames: usize) -> Vec<f32> {
    render_in(a, frames, BATCH)
}

fn render_in(a: &mut Audio, frames: usize, batch: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(frames * 2);
    let mut left = frames;
    while left > 0 {
        let n = left.min(batch);
        out.extend(a.render_headless(n).unwrap());
        left -= n;
    }
    out
}

fn db(ratio: f32) -> f32 {
    re_audio::linear_to_decibels(ratio).0
}

/// Renders `case` from a fresh mixer: the graph, then the stimulus on `bus`,
/// then `cmds` before the first batch.
fn render_case(desc: &AudioGraphDesc, bus: &str, cmds: &[AudioCmd]) -> Vec<f32> {
    let mut a = audio();
    let problems = a.set_graph(desc);
    assert!(problems.is_empty(), "{problems:?}");
    a.insert_clip("stimulus", stimulus());
    let mut all = vec![play(bus)];
    all.extend_from_slice(cmds);
    let problems = a.apply(&all);
    assert!(problems.is_empty(), "{problems:?}");
    render(&mut a, FRAMES)
}

fn reference_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/reference/{name}.f32"))
}

fn check_reference(name: &str, desc: &AudioGraphDesc, bus: &str, cmds: &[AudioCmd]) {
    let out = render_case(desc, bus, cmds);
    let again = render_case(desc, bus, cmds);
    assert!(
        out.iter()
            .zip(&again)
            .all(|(a, b)| a.to_bits() == b.to_bits()),
        "{name}: two renders on this machine differ"
    );
    assert!(rms(&out, 0) > 1e-3, "{name}: the output is silent");
    let dry = render_case(&AudioGraphDesc::default(), "sfx", &[]);
    assert!(
        out.iter().zip(&dry).any(|(a, b)| (a - b).abs() > 1e-2),
        "{name}: the output is the dry stimulus"
    );
    let path = reference_path(name);
    if std::env::var_os("RE_UPDATE_AUDIO_REFERENCE").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bytes: Vec<u8> = out.iter().flat_map(|s| s.to_le_bytes()).collect();
        std::fs::write(&path, bytes).unwrap();
        return;
    }
    let bytes = std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "{name}: no reference at {} ({e}); run with RE_UPDATE_AUDIO_REFERENCE=1",
            path.display()
        )
    });
    let reference: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    assert_eq!(reference.len(), out.len(), "{name}: length");
    let (worst, at) = out
        .iter()
        .zip(&reference)
        .map(|(a, b)| (a - b).abs())
        .enumerate()
        .fold(
            (0.0f32, 0),
            |(w, at), (i, d)| if d > w { (d, i) } else { (w, at) },
        );
    assert!(
        worst <= TOLERANCE,
        "{name}: sample {at} differs from the reference by {worst} (tolerance {TOLERANCE})"
    );
}

/// One bus `sfx` with these effects (JSON), as the default bus.
fn one_bus(effects: &str) -> AudioGraphDesc {
    graph(&format!(
        r#"{{ "default_bus": "sfx", "buses": {{ "sfx": {{ "effects": [{effects}] }} }} }}"#
    ))
}

#[test]
fn reference_reverb() {
    check_reference(
        "reverb",
        &one_bus(r#"{ "kind": "reverb", "feedback": 0.8, "damping": 0.3, "mix": 0.5 }"#),
        "sfx",
        &[],
    );
}

#[test]
fn reference_low_pass() {
    check_reference(
        "low_pass",
        &one_bus(r#"{ "kind": "filter", "mode": "low_pass", "cutoff_hz": 800.0 }"#),
        "sfx",
        &[],
    );
}

#[test]
fn reference_high_pass() {
    check_reference(
        "high_pass",
        &one_bus(
            r#"{ "kind": "filter", "mode": "high_pass", "cutoff_hz": 2000.0, "resonance": 0.5 }"#,
        ),
        "sfx",
        &[],
    );
}

#[test]
fn reference_delay() {
    check_reference(
        "delay",
        &one_bus(r#"{ "kind": "delay", "seconds": 0.03, "feedback_db": -6.0, "mix": 0.5 }"#),
        "sfx",
        &[],
    );
}

#[test]
fn reference_compressor() {
    check_reference(
        "compressor",
        &one_bus(
            r#"{ "kind": "compressor", "threshold_db": -18.0, "ratio": 4.0, "attack_ms": 1.0, "release_ms": 50.0, "makeup_db": 3.0 }"#,
        ),
        "sfx",
        &[],
    );
}

#[test]
fn reference_eq() {
    check_reference(
        "eq",
        &one_bus(r#"{ "kind": "eq", "shape": "bell", "hz": 440.0, "gain_db": 6.0, "q": 2.0 }"#),
        "sfx",
        &[],
    );
}

#[test]
fn reference_distortion() {
    check_reference(
        "distortion",
        &one_bus(r#"{ "kind": "distortion", "mode": "soft_clip", "drive_db": 12.0 }"#),
        "sfx",
        &[],
    );
}

#[test]
fn reference_send() {
    check_reference(
        "send",
        &graph(
            r#"{
                "default_bus": "sfx",
                "buses": { "sfx": { "sends": { "verb": -6.0 } } },
                "sends": { "verb": { "effects": [{ "kind": "reverb", "feedback": 0.85, "mix": 1.0 }] } }
            }"#,
        ),
        "sfx",
        &[],
    );
}

#[test]
fn reference_nested_buses() {
    check_reference(
        "nested",
        &graph(
            r#"{
                "default_bus": "sfx",
                "buses": {
                    "sfx": { "volume_db": -3.0, "effects": [{ "kind": "filter", "cutoff_hz": 3000.0 }] },
                    "footsteps": { "parent": "sfx", "effects": [{ "kind": "filter", "mode": "high_pass", "cutoff_hz": 300.0 }] }
                }
            }"#,
        ),
        "footsteps",
        &[],
    );
}

#[test]
fn reference_snapshot_tween() {
    check_reference(
        "snapshot",
        &graph(
            r#"{
                "default_bus": "sfx",
                "buses": { "sfx": { "effects": [{ "kind": "filter", "cutoff_hz": 4000.0 }] } },
                "snapshots": { "muffled": {
                    "buses": { "sfx": -6.0 },
                    "effects": [{ "bus": "sfx", "effect": 0, "cutoff_hz": 500.0 }]
                } }
            }"#,
        ),
        "sfx",
        &[AudioCmd::ApplySnapshot {
            name: "muffled".into(),
            seconds: 0.1,
        }],
    );
}

/// The RMS in dB of a 440 Hz sine of amplitude 0.5 on `bus` of `desc`, over
/// 0.1 s after 10 ms.
fn tone_db(desc: &AudioGraphDesc, bus: &str, freq: f32) -> f32 {
    let mut a = audio();
    assert!(a.set_graph(desc).is_empty());
    a.insert_clip("tone", sine(freq, 1.0, RATE, 0.5));
    let play = AudioCmd::PlayOneShotOnBus {
        clip: "tone".into(),
        position: None,
        volume: 1.0,
        bus: bus.into(),
    };
    assert!(a.apply(&[play]).is_empty());
    render(&mut a, BATCH);
    db(rms(&render(&mut a, 4800), 0))
}

#[test]
fn a_bus_at_minus_6_db_measures_6_db_below_the_main_track() {
    let main = tone_db(&AudioGraphDesc::default(), "sfx", 440.0);
    let quiet = tone_db(
        &graph(r#"{ "default_bus": "sfx", "buses": { "sfx": { "volume_db": -6.0 } } }"#),
        "sfx",
        440.0,
    );
    assert!(
        (quiet - main - -6.0).abs() < 0.1,
        "{quiet} dB against {main} dB"
    );
}

#[test]
fn a_low_pass_at_500_hz_cuts_a_4_khz_sine_by_18_db() {
    let open = tone_db(&AudioGraphDesc::default(), "sfx", 4000.0);
    let cut = tone_db(
        &one_bus(r#"{ "kind": "filter", "cutoff_hz": 500.0 }"#),
        "sfx",
        4000.0,
    );
    assert!(open - cut >= 18.0, "{cut} dB against {open} dB");
}

#[test]
fn a_send_adds_the_reverb_tail() {
    let tail = |send: bool| {
        let mut a = audio();
        let sends = if send {
            r#""sends": { "verb": -9.0 }"#
        } else {
            ""
        };
        let desc = graph(&format!(
            r#"{{
                "default_bus": "sfx",
                "buses": {{ "sfx": {{ {sends} }} }},
                "sends": {{ "verb": {{ "effects": [{{ "kind": "reverb", "feedback": 0.9, "mix": 1.0 }}] }} }}
            }}"#
        ));
        assert!(a.set_graph(&desc).is_empty());
        a.insert_clip("tone", sine(440.0, 0.05, RATE, 0.5));
        a.apply(&[AudioCmd::PlayOneShot {
            clip: "tone".into(),
            position: None,
            volume: 1.0,
        }]);
        // The 50 ms tone ends; 0.1 s later only a tail remains.
        render(&mut a, 7200);
        a.bus_levels();
        let out = render(&mut a, 4800);
        let verb = a
            .bus_levels()
            .into_iter()
            .find(|l| l.bus == "verb")
            .unwrap();
        (rms(&out, 0), verb.rms_db)
    };
    let (dry, dry_meter) = tail(false);
    let (wet, wet_meter) = tail(true);
    assert_eq!(dry, 0.0, "without the send nothing is left after the tone");
    assert_eq!(dry_meter, -60.0);
    assert!(wet > 1e-3, "the reverb tail is audible: rms {wet}");
    assert!(
        wet_meter > -50.0,
        "the send's meter sees the tail: {wet_meter} dB"
    );
}

#[test]
fn a_snapshot_tween_reaches_its_target_one_batch_after_its_length() {
    let mut a = audio();
    let desc = graph(
        r#"{
            "default_bus": "music",
            "buses": { "music": {} },
            "snapshots": { "quiet": { "buses": { "music": -12.0 } } }
        }"#,
    );
    assert!(a.set_graph(&desc).is_empty());
    a.insert_clip("tone", sine(440.0, 2.0, RATE, 0.5));
    a.apply(&[AudioCmd::PlayOneShot {
        clip: "tone".into(),
        position: None,
        volume: 1.0,
    }]);
    render(&mut a, BATCH);
    let before = db(rms(&render(&mut a, 4800), 0));
    a.apply(&[AudioCmd::ApplySnapshot {
        name: "quiet".into(),
        seconds: 0.1,
    }]);
    render(&mut a, 4800 + BATCH);
    let after = db(rms(&render(&mut a, 4800), 0));
    assert!(
        (after - before - -12.0).abs() < 0.1,
        "{after} dB against {before} dB"
    );

    // `default` returns to the graph's own values.
    a.apply(&[AudioCmd::ApplySnapshot {
        name: re_audio::DEFAULT_SNAPSHOT.into(),
        seconds: 0.0,
    }]);
    render(&mut a, BATCH);
    let back = db(rms(&render(&mut a, 4800), 0));
    assert!((back - before).abs() < 0.1, "{back} dB against {before} dB");

    // So does a restart (`StopAll`).
    a.apply(&[AudioCmd::ApplySnapshot {
        name: "quiet".into(),
        seconds: 0.0,
    }]);
    a.apply(&[
        AudioCmd::StopAll,
        AudioCmd::PlayOneShot {
            clip: "tone".into(),
            position: None,
            volume: 1.0,
        },
    ]);
    render(&mut a, BATCH);
    let restarted = db(rms(&render(&mut a, 4800), 0));
    assert!(
        (restarted - before).abs() < 0.1,
        "{restarted} dB against {before} dB"
    );
}

#[test]
fn bus_levels_meter_after_effects_and_volume() {
    let mut a = audio();
    let desc =
        graph(r#"{ "default_bus": "sfx", "buses": { "sfx": { "volume_db": -6.0 }, "ui": {} } }"#);
    assert!(a.set_graph(&desc).is_empty());
    a.insert_clip("tone", sine(440.0, 1.0, RATE, 0.5));
    a.apply(&[AudioCmd::PlayOneShot {
        clip: "tone".into(),
        position: None,
        volume: 1.0,
    }]);
    render(&mut a, BATCH);
    a.bus_levels();
    render(&mut a, 4800);
    let levels = a.bus_levels();
    assert_eq!(
        levels.iter().map(|l| l.bus.as_str()).collect::<Vec<_>>(),
        ["sfx", "ui"]
    );
    // Amplitude 0.5 at -6 dB: RMS 0.5 / sqrt(2) is -9.03 dB, peak -6.02 dB.
    assert!((levels[0].rms_db - (-9.03 - 6.0)).abs() < 0.1, "{levels:?}");
    assert!(
        (levels[0].peak_db - (-6.02 - 6.0)).abs() < 0.1,
        "{levels:?}"
    );
    assert_eq!(levels[1].rms_db, -60.0, "{levels:?}");
    // Nothing rendered since the last read.
    assert!(a.bus_levels().iter().all(|l| l.rms_db == -60.0));
}

#[test]
fn an_invalid_graph_falls_back_to_the_default_graph() {
    let mut a = audio();
    for (json, says) in [
        (
            r#"{ "buses": { "sfx": {}, "footsteps": { "parent": "sxf" } } }"#,
            "`sxf`",
        ),
        (
            r#"{ "buses": { "sfx": { "parent": "a" }, "a": { "parent": "sfx" } } }"#,
            "cycle",
        ),
        (
            r#"{ "buses": { "sfx": { "sends": { "verb": -3.0 } } } }"#,
            "`verb`",
        ),
        (
            r#"{ "default_bus": "music", "buses": { "sfx": {} } }"#,
            "default_bus",
        ),
        (
            r#"{ "buses": { "sfx": {} }, "sends": { "sfx": {} } }"#,
            "both",
        ),
    ] {
        let problems = a.set_graph(&graph(json));
        assert_eq!(codes(&problems), ["RE7407"], "{json}: {problems:?}");
        assert!(problems[0].message.contains(says), "{problems:?}");
        assert!(a.graph().is_default(), "{json}");
    }
    let mut many = AudioGraphDesc::default();
    for i in 0..=re_audio::MAX_SENDS {
        many.sends.insert(format!("send{i}"), Default::default());
    }
    assert_eq!(codes(&a.set_graph(&many)), ["RE7407"]);

    // Sounds still play on the fallback.
    a.insert_clip("tone", sine(440.0, 1.0, RATE, 0.5));
    a.apply(&[AudioCmd::PlayOneShot {
        clip: "tone".into(),
        position: None,
        volume: 1.0,
    }]);
    render(&mut a, BATCH);
    assert!(rms(&render(&mut a, 4800), 0) > 0.1);
}

#[test]
fn an_invalid_effect_is_dropped_and_keeps_its_index() {
    let mut a = audio();
    let desc = one_bus(
        r#"{ "kind": "filter", "cutoff_hz": 5.0 }, { "kind": "delay", "seconds": 0.0 }, { "kind": "filter", "cutoff_hz": 500.0 }"#,
    );
    let problems = a.set_graph(&desc);
    assert_eq!(codes(&problems), ["RE7406", "RE7406"], "{problems:?}");
    assert!(
        problems[0].message.contains("effect 0 (filter)"),
        "{problems:?}"
    );
    assert!(
        problems[1].message.contains("effect 1 (delay)"),
        "{problems:?}"
    );
    assert!(!a.graph().is_default(), "the rest of the graph builds");

    let set = |effect: u32, param: &str, value: EffectParam| AudioCmd::SetEffectParam {
        bus: "sfx".into(),
        effect,
        param: param.into(),
        value,
        seconds: 0.0,
    };
    assert!(
        a.apply(&[set(2, "cutoff_hz", EffectParam::Number(800.0))])
            .is_empty()
    );
    assert!(
        a.apply(&[set(2, "mode", EffectParam::Choice("high_pass".into()))])
            .is_empty()
    );
    for (cmd, says) in [
        (set(0, "cutoff_hz", EffectParam::Number(800.0)), "dropped"),
        (
            set(3, "cutoff_hz", EffectParam::Number(800.0)),
            "no effect 3",
        ),
        (
            set(2, "feedback_db", EffectParam::Number(-3.0)),
            "kind cannot change",
        ),
        (
            set(2, "cutoff_hz", EffectParam::Number(30_000.0)),
            "between",
        ),
        (
            set(2, "cutoff_hz", EffectParam::Choice("loud".into())),
            "number",
        ),
        (
            set(2, "mode", EffectParam::Choice("loud".into())),
            "low_pass",
        ),
    ] {
        let problems = a.apply(&[cmd]);
        assert_eq!(codes(&problems), ["RE7406"], "{problems:?}");
        assert!(problems[0].message.contains(says), "{says}: {problems:?}");
    }
}

#[test]
fn an_unknown_bus_warns_once_and_plays_on_the_default_bus() {
    let mut a = audio();
    a.insert_clip("tone", sine(440.0, 1.0, RATE, 0.5));
    let on = |bus: &str| AudioCmd::PlayOneShotOnBus {
        clip: "tone".into(),
        position: None,
        volume: 1.0,
        bus: bus.into(),
    };
    let first = a.apply(&[on("fottsteps")]);
    assert_eq!(codes(&first), ["RE7408"]);
    assert!(a.apply(&[on("fottsteps")]).is_empty(), "once per name");
    assert_eq!(a.playing(), 2, "both play, on the default bus");
    let quiet = AudioCmd::SetBusVolume {
        bus: "nowhere".into(),
        volume_db: -3.0,
        seconds: 0.0,
    };
    assert_eq!(codes(&a.apply(std::slice::from_ref(&quiet))), ["RE7408"]);
    assert!(a.apply(&[quiet]).is_empty());
    let snap = AudioCmd::ApplySnapshot {
        name: "underwater".into(),
        seconds: 1.0,
    };
    assert_eq!(codes(&a.apply(&[snap])), ["RE7408"]);
}

#[test]
fn an_entity_plays_on_its_bus_until_it_stops() {
    let mut a = audio();
    let desc =
        graph(r#"{ "default_bus": "sfx", "buses": { "sfx": {}, "ui": { "volume_db": -60.0 } } }"#);
    assert!(a.set_graph(&desc).is_empty());
    a.insert_clip("tone", sine(440.0, 1.0, RATE, 0.5));
    let e = EntityId(3);
    let start = AudioCmd::StartSource {
        entity: e,
        clip: "tone".into(),
        volume: 1.0,
        looping: true,
        spatial: false,
        position: glam::Vec3::ZERO,
    };
    a.apply(&[
        AudioCmd::SetSourceBus {
            entity: e,
            bus: "ui".into(),
        },
        start.clone(),
    ]);
    render(&mut a, BATCH);
    assert!(
        rms(&render(&mut a, 4800), 0) < 1e-3,
        "the ui bus is at -60 dB"
    );
    // After a stop, the entity's next sound plays on the default bus again.
    a.apply(&[AudioCmd::StopSource { entity: e }, start]);
    render(&mut a, BATCH);
    assert!(rms(&render(&mut a, 4800), 0) > 0.1);
}

#[test]
fn rebuilding_the_graph_stops_the_sounds_on_it() {
    let mut a = audio();
    a.insert_clip("tone", sine(440.0, 5.0, RATE, 0.5));
    a.apply(&[AudioCmd::PlayOneShot {
        clip: "tone".into(),
        position: Some(glam::Vec3::new(0.0, 0.0, -1.0)),
        volume: 1.0,
    }]);
    render(&mut a, 4800);
    assert_eq!(a.playing(), 1);
    assert!(a.set_graph(&one_bus(r#"{ "kind": "reverb" }"#)).is_empty());
    assert_eq!(a.playing(), 0);
    render(&mut a, BATCH);
    assert_eq!(rms(&render(&mut a, 4800), 0), 0.0);
}

#[test]
fn bad_tweens_and_volumes_are_refused() {
    let mut a = audio();
    let problems = a.apply(&[
        AudioCmd::SetBusVolume {
            bus: "sfx".into(),
            volume_db: f32::NAN,
            seconds: 0.0,
        },
        AudioCmd::SetBusVolume {
            bus: "sfx".into(),
            volume_db: -3.0,
            seconds: -1.0,
        },
        AudioCmd::ApplySnapshot {
            name: "default".into(),
            seconds: f32::INFINITY,
        },
    ]);
    assert_eq!(codes(&problems), ["RE7404", "RE7404", "RE7406"]);
}

#[test]
fn the_output_does_not_depend_on_the_render_call_size() {
    let desc = graph(
        r#"{
            "default_bus": "sfx",
            "buses": { "sfx": {
                "sends": { "verb": -6.0 },
                "effects": [{ "kind": "compressor", "threshold_db": -12.0, "ratio": 3.0 }, { "kind": "delay", "seconds": 0.02 }]
            } },
            "sends": { "verb": { "effects": [{ "kind": "reverb" }] } }
        }"#,
    );
    let run = |batch: usize| {
        let mut a = audio();
        assert!(a.set_graph(&desc).is_empty());
        a.insert_clip("stimulus", stimulus());
        a.apply(&[play("sfx")]);
        render_in(&mut a, FRAMES, batch)
    };
    let reference = run(BATCH);
    for batch in [128, 512, 800, FRAMES] {
        let out = run(batch);
        assert!(
            out.iter()
                .zip(&reference)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "render calls of {batch} frames differ from {BATCH}"
        );
    }
}
