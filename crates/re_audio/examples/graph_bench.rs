//! Mixer-graph benchmark (ADR-0041's budget): CPU time per second of audio for
//! the default graph and for a typical project graph (4 buses with a filter, a
//! delay and a compressor, and a reverb send), with looping sounds on every bus.
//!
//! ```sh
//! cargo run -p re_audio --release --example graph_bench -- [--sounds 8] [--seconds 10]
//! ```
//!
//! Prints one `graph_bench graph=<name> ms_per_audio_s=<x>` line per graph, the
//! best of five runs. The budget is 20 ms per audio second (2% of one core).

use re_audio::{Audio, AudioCmd, AudioGraphDesc, CaptureSettings, sine};
use re_core::time::Instant;

const RATE: u32 = 48_000;
/// Frames per render call, as a device callback asks for.
const BATCH: usize = 512;

fn arg(args: &[String], name: &str, default: usize) -> usize {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(|v| {
            v.parse()
                .unwrap_or_else(|_| panic!("{name} takes a number, got {v:?}"))
        })
        .unwrap_or(default)
}

fn typical() -> AudioGraphDesc {
    serde_json::from_str(
        r#"{
            "default_bus": "sfx",
            "buses": {
                "music": { "volume_db": -6.0 },
                "sfx": {
                    "sends": { "world_reverb": -9.0 },
                    "effects": [{ "kind": "compressor", "threshold_db": -18.0, "ratio": 3.0 }]
                },
                "footsteps": {
                    "parent": "sfx",
                    "effects": [{ "kind": "filter", "mode": "high_pass", "cutoff_hz": 120.0 }]
                },
                "ui": { "effects": [{ "kind": "delay", "seconds": 0.25 }] }
            },
            "sends": {
                "world_reverb": { "effects": [{ "kind": "reverb", "feedback": 0.85, "damping": 0.3, "mix": 1.0 }] }
            }
        }"#,
    )
    .expect("the typical graph parses")
}

/// CPU milliseconds per second of audio rendered.
fn run(desc: &AudioGraphDesc, sounds: usize, seconds: usize) -> f64 {
    let mut audio = Audio::headless_with(CaptureSettings { sample_rate: RATE });
    assert!(audio.set_graph(desc).is_empty());
    audio.insert_clip("tone", sine(440.0, 1.0, RATE, 0.1));
    let buses: Vec<String> = desc.buses.keys().cloned().collect();
    let cmds: Vec<AudioCmd> = (0..sounds)
        .flat_map(|i| {
            let entity = re_types::EntityId(i as u64 + 1);
            [
                AudioCmd::SetSourceBus {
                    entity,
                    bus: buses[i % buses.len()].clone(),
                },
                AudioCmd::StartSource {
                    entity,
                    clip: "tone".into(),
                    volume: 1.0,
                    looping: true,
                    spatial: false,
                    position: re_types::glam::Vec3::ZERO,
                },
            ]
        })
        .collect();
    assert!(audio.apply(&cmds).is_empty());
    let frames = seconds * RATE as usize;
    let start = Instant::now();
    let mut done = 0;
    while done < frames {
        audio.render_headless(BATCH);
        done += BATCH;
    }
    start.elapsed().as_secs_f64() * 1000.0 / (done as f64 / f64::from(RATE))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let sounds = arg(&args, "--sounds", 8);
    let seconds = arg(&args, "--seconds", 10);
    for (name, desc) in [("default", AudioGraphDesc::default()), ("typical", typical())] {
        let best = (0..5)
            .map(|_| run(&desc, sounds, seconds))
            .fold(f64::INFINITY, f64::min);
        println!("graph_bench graph={name} sounds={sounds} ms_per_audio_s={best:.2}");
    }
}
