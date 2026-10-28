//! Ogg Vorbis clips (ADR-0066) on the headless backend: long clips stream from
//! their files, short ones are decoded whole after their first play, and
//! seeks, loops, controls and failures. No test opens an audio device.
//!
//! The files in `tests/data/` were made with Python 3 and oggenc 1.4.2
//! (libvorbis 1.3.7), from the WAV files the script below writes:
//!
//! ```text
//! python3 gen.py
//! oggenc -Q -q -1 --serial 1 -o sparse180.ogg sparse180.wav  # 3 min, mono, 22.05 kHz: 50 KB
//! oggenc -Q -q 0 --serial 2 -o stereo1.ogg stereo1.wav       # 1 s, stereo, 44.1 kHz: 7 KB
//! oggenc -Q -q -1 --serial 3 -o three.ogg three.wav          # 0.2 s, three channels: 4 KB
//! ```
//!
//! `gen.py`:
//!
//! ```text
//! import array, math, wave
//!
//! def write(path, rate, channels):
//!     with wave.open(path, 'wb') as w:
//!         w.setnchannels(len(channels)); w.setsampwidth(2); w.setframerate(rate)
//!         out = array.array('h')
//!         for frame in zip(*channels):
//!             out.extend(int(v * 32767) for v in frame)
//!         w.writeframes(out.tobytes())
//!
//! def sparse(seconds, rate):
//!     # A decaying quarter-second tone every 2 s, at one of four pitches.
//!     out = []
//!     for i in range(seconds * rate):
//!         t = i / rate
//!         beat = t % 2.0
//!         v = 0.0
//!         if beat < 0.25:
//!             f = 220.0 * (1 + int(t / 2.0) % 4)
//!             v = 0.3 * math.sin(2 * math.pi * f * t) * (1 - beat / 0.25)
//!         out.append(v)
//!     return out
//!
//! def sine(seconds, rate, f, a):
//!     return [a * math.sin(2 * math.pi * f * i / rate) for i in range(int(seconds * rate))]
//!
//! write('sparse180.wav', 22050, [sparse(180, 22050)])
//! write('stereo1.wav', 44100, [sine(1, 44100, 440, 0.5), sine(1, 44100, 660, 0.25)])
//! write('three.wav', 22050, [sine(0.2, 22050, f, 0.3) for f in (220, 330, 440)])
//! ```

use std::time::{Duration, Instant};

use glam::Vec3;
use re_audio::{Audio, AudioCmd, CaptureSettings, SoundOp, rms};
use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, ProjectPath};
use re_types::EntityId;

const SPARSE: &[u8] = include_bytes!("data/sparse180.ogg");
const STEREO: &[u8] = include_bytes!("data/stereo1.ogg");
const THREE: &[u8] = include_bytes!("data/three.ogg");

/// Three minutes: it streams.
const LONG: &str = "music/sparse.ogg";
/// One second: decoded whole once it has played.
const SHORT: &str = "sfx/stereo.ogg";

fn files(extra: &[(&str, Vec<u8>)]) -> ProjectFiles {
    let mut mem = MemLayer::new(LayerKind::Memory);
    let fixed = [(LONG, SPARSE.to_vec()), (SHORT, STEREO.to_vec())];
    for (path, bytes) in fixed.into_iter().chain(extra.iter().cloned()) {
        mem.insert(ProjectPath::new(path).unwrap(), bytes, Form::Source);
    }
    ProjectFiles::for_tests(mem)
}

fn audio_with(rate: u32, extra: &[(&str, Vec<u8>)]) -> Audio {
    let mut a = Audio::headless_with(CaptureSettings {
        sample_rate: rate,
        ..Default::default()
    });
    a.set_files(files(extra));
    a
}

fn audio(rate: u32) -> Audio {
    audio_with(rate, &[])
}

fn start(e: u64, clip: &str, looping: bool) -> AudioCmd {
    AudioCmd::StartSource {
        entity: EntityId(e),
        clip: clip.into(),
        volume: 1.0,
        looping,
        spatial: false,
        position: Vec3::ZERO,
    }
}

fn control(e: u64, op: SoundOp) -> AudioCmd {
    AudioCmd::SourceControl {
        entity: EntityId(e),
        op,
    }
}

fn playing(a: &mut Audio, e: u64) -> bool {
    a.is_source_playing(EntityId(e))
}

fn codes(problems: &[re_core::Diagnostic]) -> Vec<String> {
    problems.iter().map(|d| d.code.to_string()).collect()
}

/// Renders `seconds`, interleaved.
fn render(a: &mut Audio, seconds: f64) -> Vec<f32> {
    let rate = a.headless_sample_rate().unwrap();
    a.render_headless((seconds * f64::from(rate)).round() as usize)
        .unwrap()
}

/// The largest sample of the left channel from `from` to `to` seconds.
fn peak(out: &[f32], rate: u32, from: f64, to: f64) -> f32 {
    let at = |s: f64| ((s * f64::from(rate)) as usize * 2).min(out.len());
    out[at(from)..at(to)]
        .iter()
        .step_by(2)
        .fold(0.0f32, |m, s| m.max(s.abs()))
}

/// The largest difference between two renders of the same length.
fn difference(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .fold(0.0f32, |m, (x, y)| m.max((x - y).abs()))
}

#[test]
fn a_long_clip_streams_and_plays_to_its_end() {
    let mut a = audio(22_050);
    assert_eq!(a.clip_seconds(LONG).unwrap(), 180.0);
    assert!(!a.has_clip(LONG), "a long clip is not decoded whole");
    let problems = a.apply(&[start(1, LONG, false)]);
    assert!(problems.is_empty(), "{problems:?}");

    // A tone at 0 s and at 2 s, silence between, on both sides.
    let out = render(&mut a, 2.5);
    assert!(peak(&out, 22_050, 0.0, 0.25) > 0.2);
    assert!(peak(&out, 22_050, 0.5, 1.9) < 0.02);
    assert!(peak(&out, 22_050, 2.0, 2.25) > 0.2);
    assert!(out.chunks(2).all(|f| f[0] == f[1]));

    // Near the end: the tone at 178 s, then the clip ends.
    a.apply(&[control(1, SoundOp::Seek { seconds: 178.0 })]);
    let out = render(&mut a, 1.0);
    assert!(peak(&out, 22_050, 0.0, 1.0) > 0.2);
    assert!(playing(&mut a, 1));
    render(&mut a, 1.5);
    assert!(!playing(&mut a, 1), "the clip ended");
    assert_eq!(a.stream_underruns(), 0);
    assert!(!a.has_clip(LONG));
}

#[test]
fn a_short_clip_streams_once_then_plays_from_memory() {
    let mut a = audio(44_100);
    assert!(!a.has_clip(SHORT));
    let problems = a.apply(&[start(1, SHORT, false)]);
    assert!(problems.is_empty(), "{problems:?}");
    // The first play streams; the frames it decoded are kept for the next.
    assert!(a.has_clip(SHORT));
    assert_eq!(a.clip_seconds(SHORT).unwrap(), 1.0);
    let streamed = render(&mut a, 1.2);
    assert!(!playing(&mut a, 1));

    a.apply(&[start(1, SHORT, false)]);
    let decoded = render(&mut a, 1.2);
    assert!(!playing(&mut a, 1));
    assert_eq!(difference(&streamed, &decoded), 0.0);

    // Left and right stay apart: 0.5 and 0.25 sines.
    let second = &decoded[..2 * 44_100];
    assert!((rms(second, 0) - 0.354).abs() < 0.02, "{}", rms(second, 0));
    assert!((rms(second, 1) - 0.177).abs() < 0.02, "{}", rms(second, 1));

    // A changed file is read again.
    a.remove_clip(SHORT);
    assert!(!a.has_clip(SHORT));
    a.apply(&[start(1, SHORT, false)]);
    assert!(a.has_clip(SHORT));
}

#[test]
fn loops_are_gapless() {
    // A short clip, streamed from the frames it decoded, then decoded.
    let mut a = audio(44_100);
    let pass = 2 * 44_100;
    let mut first = None;
    for play in ["streamed", "decoded"] {
        a.apply(&[start(1, SHORT, true)]);
        let out = render(&mut a, 2.5);
        assert_eq!(
            difference(&out[..pass], &out[pass..2 * pass]),
            0.0,
            "{play}"
        );
        let first = first.get_or_insert_with(|| out.clone());
        assert_eq!(difference(first, &out), 0.0, "{play}");
    }

    // A long clip: its second pass reads the file again from the start.
    let mut a = audio(22_050);
    a.apply(&[start(1, LONG, true)]);
    let first = render(&mut a, 1.0);
    for _ in 0..179 {
        render(&mut a, 1.0);
    }
    let second = render(&mut a, 1.0);
    assert_eq!(difference(&first, &second), 0.0);
    assert!(peak(&second, 22_050, 0.0, 0.25) > 0.2);
    assert_eq!(a.stream_underruns(), 0);
}

#[test]
fn a_stream_follows_controls_as_a_decoded_clip_does() {
    fn script(a: &mut Audio) -> Vec<f32> {
        let mut out = Vec::new();
        a.apply(&[start(1, SHORT, true)]);
        out.extend(render(a, 0.2));
        for op in [
            SoundOp::Volume {
                volume: 0.5,
                fade: 0.1,
            },
            SoundOp::Rate {
                rate: 1.5,
                fade: 0.1,
            },
            SoundOp::Pause { fade: 0.05 },
            SoundOp::Resume { fade: 0.05 },
            SoundOp::Stop { fade: 0.1 },
        ] {
            a.apply(&[control(1, op)]);
            out.extend(render(a, 0.25));
        }
        assert!(!playing(a, 1));
        out
    }
    let mut a = audio(44_100);
    let streamed = script(&mut a);
    assert!(a.has_clip(SHORT));
    let decoded = script(&mut a);
    let diff = difference(&streamed, &decoded);
    assert!(diff < 1e-4, "{diff}");
    // The pause was silent and the stop faded out: the controls did something.
    let at = |s: f64| (s * 44_100.0) as usize * 2;
    assert!(rms(&decoded[at(0.0)..at(0.2)], 0) > 0.3);
    assert!(rms(&decoded[at(0.31)..at(0.45)], 0) < 0.2);
    assert_eq!(rms(&decoded[at(0.8)..at(0.95)], 0), 0.0);
    assert_eq!(rms(&decoded[at(1.35)..], 0), 0.0);
}

#[test]
fn seeks_jump_and_one_past_the_end_ends_the_clip() {
    let mut a = audio(22_050);
    a.apply(&[start(1, LONG, false), start(2, LONG, true)]);
    render(&mut a, 0.5);
    // To the tone at 4 s, not the one at 2 s.
    a.apply(&[control(1, SoundOp::Seek { seconds: 4.0 })]);
    let out = render(&mut a, 0.75);
    assert!(peak(&out, 22_050, 0.0, 0.75) > 0.2);

    a.apply(&[
        control(1, SoundOp::Seek { seconds: 1e300 }),
        control(2, SoundOp::Seek { seconds: 1e300 }),
    ]);
    render(&mut a, 1.0);
    assert!(!playing(&mut a, 1), "past the end is the end");
    assert!(playing(&mut a, 2), "a looping clip wraps around");
    assert_eq!(a.stream_underruns(), 0);
}

#[test]
fn files_that_do_not_play_are_reported() {
    let mut middle = SPARSE.to_vec();
    // A damaged page in the middle: its checksum fails when the stream gets there.
    for b in &mut middle[25_000..25_100] {
        *b ^= 0x5a;
    }
    let mut a = audio_with(
        22_050,
        &[
            ("bad/three.ogg", THREE.to_vec()),
            ("bad/chained.ogg", [STEREO, THREE].concat()),
            ("bad/wave.ogg", b"RIFF\0\0\0\0WAVEfmt ".to_vec()),
            ("bad/cut.ogg", SPARSE[..200].to_vec()),
            ("bad/middle.ogg", middle),
        ],
    );
    for (clip, code) in [
        ("bad/missing.ogg", "RE7401"),
        ("bad/three.ogg", "RE7409"),
        ("bad/chained.ogg", "RE7409"),
        ("bad/wave.ogg", "RE7409"),
        ("bad/cut.ogg", "RE7402"),
    ] {
        let d = a.clip_seconds(clip).unwrap_err();
        assert_eq!(d.code.to_string(), code, "{clip}: {d:?}");
        assert!(d.message.contains(clip), "{d:?}");
        // Played, its stream says the same in that frame and stops.
        let problems = a.apply(&[start(1, clip, false)]);
        assert_eq!(codes(&problems), [code], "{clip}");
        assert!(!playing(&mut a, 1), "{clip}");
    }

    // A stream that fails while it plays stops alone, with where it was.
    let problems = a.apply(&[start(1, "bad/middle.ogg", false), start(2, LONG, true)]);
    assert!(problems.is_empty(), "{problems:?}");
    let mut seen = Vec::new();
    for _ in 0..200 {
        render(&mut a, 1.0);
        seen.extend(a.apply(&[]));
        if !seen.is_empty() {
            break;
        }
    }
    assert_eq!(codes(&seen), ["RE7410"]);
    assert!(
        seen[0].message.contains("bad/middle.ogg"),
        "{}",
        seen[0].message
    );
    assert!(seen[0].message.contains(" s)"), "{}", seen[0].message);
    assert!(!playing(&mut a, 1));
    assert!(playing(&mut a, 2), "the other stream plays on");
    let out = render(&mut a, 2.0);
    assert!(peak(&out, 22_050, 0.0, 2.0) > 0.2);
}

#[test]
fn a_build_decodes_each_file_once() {
    assert_eq!(re_audio::check_ogg("a.ogg", SPARSE).unwrap(), 180.0);
    assert_eq!(re_audio::check_ogg("b.ogg", STEREO).unwrap(), 1.0);
    let mut middle = SPARSE.to_vec();
    for b in &mut middle[25_000..25_100] {
        *b ^= 0x5a;
    }
    let d = re_audio::check_ogg("c.ogg", &middle).unwrap_err();
    assert_eq!(d.code.to_string(), "RE7402", "{d:?}");
    assert!(d.message.contains("c.ogg"), "{d:?}");
    let d = re_audio::check_ogg("d.ogg", THREE).unwrap_err();
    assert_eq!(d.code.to_string(), "RE7409", "{d:?}");
}

#[test]
fn a_decoder_that_panics_stops_only_its_stream() {
    let mut a = audio(22_050);
    a.panic_streams_of(LONG);
    let problems = a.apply(&[start(1, LONG, false), start(2, SHORT, true)]);
    assert_eq!(codes(&problems), ["RE7410"]);
    assert!(problems[0].message.contains("panicked"), "{problems:?}");
    assert!(!playing(&mut a, 1));
    assert!(playing(&mut a, 2));
    let out = render(&mut a, 0.5);
    assert!(rms(&out, 0) > 0.2);
}

#[test]
fn at_most_32_streams_play_at_once() {
    let mut a = audio(22_050);
    let all: Vec<_> = (1..=32).map(|e| start(e, LONG, true)).collect();
    let problems = a.apply(&all);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(codes(&a.apply(&[start(33, LONG, true)])), ["RE7403"]);
    let one_shot = AudioCmd::PlayOneShot {
        clip: LONG.into(),
        position: None,
        volume: 1.0,
        id: None,
    };
    assert_eq!(codes(&a.apply(std::slice::from_ref(&one_shot))), ["RE7403"]);
    // A short clip known to be short plays decoded, not as a stream.
    a.load_clip(SHORT).unwrap();
    assert!(a.apply(&[start(40, SHORT, false)]).is_empty());
    // A stream that stops frees its place.
    a.apply(&[AudioCmd::StopSource {
        entity: EntityId(1),
    }]);
    assert!(a.apply(&[start(33, LONG, true)]).is_empty());
    render(&mut a, 0.5);
    assert_eq!(a.stream_underruns(), 0);
}

fn threaded(rate: u32) -> Audio {
    let mut a = Audio::headless_with(CaptureSettings {
        sample_rate: rate,
        stream_thread: true,
    });
    a.set_files(files(&[("bad/three.ogg", THREE.to_vec())]));
    a
}

/// Renders in short steps, giving the stream thread time, until `done`;
/// returns the problems reported meanwhile.
fn wait(
    a: &mut Audio,
    mut done: impl FnMut(&mut Audio, &[f32], &[re_core::Diagnostic]) -> bool,
) -> Vec<re_core::Diagnostic> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut problems = Vec::new();
    loop {
        let out = render(a, 0.05);
        problems.extend(a.apply(&[]));
        if done(a, &out, &problems) {
            return problems;
        }
        assert!(Instant::now() < deadline, "{problems:?}");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn the_stream_thread_opens_and_decodes_off_the_game_thread() {
    let mut a = threaded(22_050);
    // The thread opens the files, so the refusal comes in this frame or a
    // later one.
    let first = a.apply(&[start(1, LONG, false), start(2, "bad/three.ogg", false)]);
    let mut heard = false;
    let later = wait(&mut a, |_, out, p| {
        heard |= peak(out, 22_050, 0.0, 0.05) > 0.2;
        heard && !(first.is_empty() && p.is_empty())
    });
    assert_eq!(codes(&[first, later].concat()), ["RE7409"]);
    assert!(!playing(&mut a, 2));
    assert!(playing(&mut a, 1));
}

#[test]
fn a_clip_forgotten_while_its_stream_opens_is_not_cached() {
    let mut a = threaded(44_100);
    a.apply(&[start(1, SHORT, false)]);
    a.remove_clip(SHORT);
    // Once the clip has played out, the thread's classification has come in.
    wait(&mut a, |a, _, _| !playing(a, 1));
    a.apply(&[]);
    assert!(
        !a.has_clip(SHORT),
        "the stream opened before the clip changed"
    );
    // A play after the change caches it.
    a.apply(&[start(1, SHORT, false)]);
    wait(&mut a, |a, _, _| !playing(a, 1));
    a.apply(&[]);
    assert!(a.has_clip(SHORT));
}
