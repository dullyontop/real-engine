//! Measures the large static map (ADR-0031): builds the synthetic map
//! (`tests/fixtures/physics/bigmap.rs`), writes it as a scene file, then
//! loads it into a fresh world, steps the simulation and reports the load
//! time, the fixed-step times and the peak memory against the budgets.
//!
//! ```sh
//! cargo run --release --locked -p re_compat --example bigmap -- --parts 250000 --frames 600 --json
//! ```
//!
//! Options: `--parts N` (250000), `--frames N` (600), `--seed N` (1),
//! `--touch-every N` (0: every N frames a "script" moves one anchored part,
//! which takes it out of its cell), `--json` (one JSON object on stdout
//! instead of text).
//!
//! **Load** is reading the scene file, spawning it, and the first simulation
//! sync (which builds the static cells). **Step** is one 60 Hz frame of
//! `re_compat::physics::step`: the sync with the world, the substeps and the
//! write-back. **Peak RSS** is the process's high-water mark.

#[path = "../tests/fixtures/physics/bigmap.rs"]
mod bigmap;

use std::time::Duration;

use re_compat::{CompatSetup, physics};
use re_core::time::Instant;
use re_scene::World;

const LOAD_BUDGET_S: f64 = 30.0;
const STEP_BUDGET_MS: f64 = 8.0;
const RSS_BUDGET_MB: f64 = 4096.0;

struct Args {
    parts: usize,
    frames: usize,
    seed: u64,
    touch_every: usize,
    json: bool,
}

fn args() -> Args {
    let mut a = Args {
        parts: 250_000,
        frames: 600,
        seed: 1,
        touch_every: 0,
        json: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(k) = it.next() {
        let mut num = |name: &str| -> usize {
            it.next()
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| panic!("{name} takes a number"))
        };
        match k.as_str() {
            "--parts" => a.parts = num("--parts"),
            "--frames" => a.frames = num("--frames"),
            "--seed" => a.seed = num("--seed") as u64,
            "--touch-every" => a.touch_every = num("--touch-every"),
            "--json" => a.json = true,
            other => panic!("unknown option {other}"),
        }
    }
    a
}

/// The process's peak resident set, in MB (Linux; 0 elsewhere).
fn peak_rss_mb() -> f64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|kb| kb.parse::<f64>().ok())
        })
        .map_or(0.0, |kb| kb / 1024.0)
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn main() {
    let a = args();
    let setup = CompatSetup::standard().expect("the built-in classes load");
    let classes = &setup.classes;

    // The scene file, as a place would ship it.
    let map = bigmap::generate(a.parts, a.seed);
    let text = {
        let doc = bigmap::scene(&map, classes).expect("the map is a valid scene");
        re_scene::scene::to_json(&doc)
    };
    let dir = std::env::temp_dir().join(format!("re-bigmap-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory");
    let path = dir.join("bigmap.scene.json");
    std::fs::write(&path, &text).expect("the scene file is written");
    let file_mb = text.len() as f64 / 1e6;
    drop(text);
    drop(map);

    // Load: read, spawn, first sync.
    let t = Instant::now();
    let text = std::fs::read_to_string(&path).expect("the scene file reads back");
    let doc = re_scene::scene::from_json(&text).expect("the scene file parses");
    drop(text);
    let mut world = World::new();
    classes
        .register_components(&mut world)
        .expect("the class components register");
    world.load(&doc).expect("the scene loads");
    drop(doc);
    physics::prepare_world(&mut world, classes).expect("the workspace is prepared");
    let first = physics::step(&mut world, 0.0);
    let load = t.elapsed();
    assert!(first.is_empty(), "{first:?}");
    let _ = std::fs::remove_dir_all(&dir);

    let cells = world.ecs.resource::<physics::StaticCells>().cells().len();
    let (bodies, simulated) =
        physics::with_physics(&mut world, |p| (p.body_count(), p.part_count())).unwrap_or((0, 0));

    // The fixed steps.
    let movable: Vec<re_scene::Entity> = if a.touch_every > 0 {
        let mut q = world
            .ecs
            .query::<(re_scene::Entity, &re_scene::Transform)>();
        q.iter(&world.ecs).map(|(e, _)| e).take(10_000).collect()
    } else {
        Vec::new()
    };
    let mut times = Vec::with_capacity(a.frames);
    let mut moved = 0usize;
    for f in 0..a.frames {
        if a.touch_every > 0 && f % a.touch_every == a.touch_every - 1 && !movable.is_empty() {
            let e = movable[(f * 7919) % movable.len()];
            if let Some(p) = physics::world_pose(&world, e) {
                let to = re_physics::Pose::new(p.pos + glam::Vec3::new(0.0, 0.5, 0.0), p.rot);
                if setup
                    .classes
                    .set_property(&mut world, e, "CFrame", physics::cframe_value(&to))
                    .is_ok()
                {
                    moved += 1;
                }
            }
        }
        let t = Instant::now();
        let d = physics::step(&mut world, 1.0 / 60.0);
        times.push(t.elapsed());
        assert!(d.is_empty(), "frame {f}: {d:?}");
    }
    let n = times.len().max(1) as f64;
    let mean = times.iter().map(|d| ms(*d)).sum::<f64>() / n;
    let mut sorted: Vec<f64> = times.iter().map(|d| ms(*d)).collect();
    sorted.sort_by(f64::total_cmp);
    let pct = |p: f64| {
        sorted
            .get(((sorted.len() as f64 - 1.0) * p) as usize)
            .copied()
            .unwrap_or(0.0)
    };
    let rss = peak_rss_mb();
    let load_s = load.as_secs_f64();
    let pass = load_s <= LOAD_BUDGET_S && mean <= STEP_BUDGET_MS && rss <= RSS_BUDGET_MB;

    if a.json {
        println!(
            "{{\"parts\":{},\"frames\":{},\"seed\":{},\"touch_every\":{},\"parts_moved\":{moved},\
             \"scene_file_mb\":{file_mb:.1},\"cells\":{cells},\"bodies\":{bodies},\"simulated_parts\":{simulated},\
             \"load_s\":{load_s:.3},\"step_mean_ms\":{mean:.3},\"step_p50_ms\":{:.3},\"step_p95_ms\":{:.3},\"step_max_ms\":{:.3},\
             \"peak_rss_mb\":{rss:.0},\
             \"budgets\":{{\"load_s\":{LOAD_BUDGET_S},\"step_mean_ms\":{STEP_BUDGET_MS},\"peak_rss_mb\":{RSS_BUDGET_MB}}},\
             \"pass\":{pass}}}",
            a.parts,
            a.frames,
            a.seed,
            a.touch_every,
            pct(0.5),
            pct(0.95),
            pct(1.0),
        );
    } else {
        println!(
            "parts {} in {cells} static cells ({bodies} bodies), scene file {file_mb:.1} MB",
            a.parts
        );
        println!("load {load_s:.2} s (budget {LOAD_BUDGET_S} s)");
        println!(
            "step mean {mean:.3} ms, p50 {:.3}, p95 {:.3}, max {:.3} over {} frames (budget {STEP_BUDGET_MS} ms)",
            pct(0.5),
            pct(0.95),
            pct(1.0),
            a.frames
        );
        println!("peak RSS {rss:.0} MB (budget {RSS_BUDGET_MB} MB)");
        println!("{}", if pass { "PASS" } else { "FAIL" });
    }
    if !pass {
        std::process::exit(1);
    }
}
