//! Walks a 16 km × 16 km generated world from corner to corner at 30 m/s
//! (ADR-0042's acceptance walk) and checks the streaming budgets: no tick
//! waiting on generation after the initial load, tick time, accounted memory,
//! peak RSS, and the same `state_hash` from a cold and from a warm cache.
//!
//! ```sh
//! cargo run --release --locked -p re_proc --example stream_walk -- --json > out/stream.json
//! ```
//!
//! Options: `--json` (the report as JSON on stdout), `--ticks N` (stop after
//! N ticks of the walk; default: the whole diagonal, about 12.5 minutes per
//! run), `--threads N` (worker threads; default: cores minus one) and
//! `--unpaced` (run ticks back to back instead of at 60 Hz; generation then
//! falls behind and waits are expected). Headless, no rendering, no physics:
//! a tick is the streaming step plus the scene update, run at the fixed
//! step's real-time pace, as a game would. Prefab paths are relative to this
//! `examples/` directory, which stands in for a project. The exit code is 1
//! if a budget is missed.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use glam::{Quat, Vec3};
use re_command::EditState;
use re_core::TaskPool;
use re_core::time::Instant;
use re_proc::stream::{Lateness, StreamConfig, Streamer, step};
use re_proc::{Evaluator, GraphStore, NodeRegistry, register_generator};
use re_reflect::Value;
use re_types::graph::GraphDoc;
use serde_json::{Value as Json, json};

/// Metres per second along the diagonal.
const SPEED: f32 = 30.0;
/// Simulation steps per second.
const HZ: f32 = 60.0;
/// Half the world's side.
const HALF: f32 = 8192.0;
/// Where the walk starts and ends, inside the corners.
const END: f32 = 7900.0;

/// The ADR-0042 budgets.
const P95_MS: f64 = 8.0;
const MAX_MS: f64 = 16.7;
const RSS_MB: f64 = 1536.0;

struct Args {
    json: bool,
    ticks: Option<u64>,
    threads: usize,
    unpaced: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        json: false,
        ticks: None,
        threads: 0,
        unpaced: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut number = |name: &str| -> Result<u64, String> {
            let v = it.next().ok_or_else(|| format!("{name} needs a value"))?;
            v.parse()
                .map_err(|_| format!("{name}: `{v}` is not a number"))
        };
        match a.as_str() {
            "--json" => args.json = true,
            "--ticks" => args.ticks = Some(number("--ticks")?),
            "--threads" => args.threads = number("--threads")? as usize,
            "--unpaced" => args.unpaced = true,
            "-h" | "--help" => {
                return Err(
                    "usage: stream_walk [--json] [--ticks N] [--threads N] [--unpaced]".into(),
                );
            }
            other => return Err(format!("unknown option `{other}`")),
        }
    }
    Ok(args)
}

/// The world: eroded-looking hills at 4 m, water, palms on a jittered grid
/// and rocks on gentle slopes.
fn world_graph() -> GraphDoc {
    let nodes = [
        (
            "h",
            "terrain.heightfield",
            json!({ "cell": 4.0, "height": 400.0, "scale": 0.002, "octaves": 6, "noise": "warped", "warp": 0.6 }),
        ),
        (
            "ground",
            "terrain.mesh",
            json!({ "collider": true, "material": "materials/grass.material.json" }),
        ),
        ("water", "terrain.water_plane", json!({ "level": 60.0 })),
        (
            "grid",
            "scatter.grid_jitter",
            json!({ "spacing": 32.0, "jitter": 0.9, "height_min": 64.0, "slope_max": 0.6 }),
        ),
        (
            "palms",
            "points.instance",
            json!({ "prefab": "prefabs/palm.prefab.json", "random_yaw": true }),
        ),
        (
            "scatter",
            "scatter.poisson",
            json!({ "radius": 24.0, "height_min": 62.0, "slope_max": 0.4 }),
        ),
        (
            "rocks",
            "points.instance",
            json!({ "prefab": "prefabs/rock.prefab.json", "random_yaw": true, "scale_min": 0.5, "scale_max": 1.5 }),
        ),
    ];
    let edges = [
        ("h", "field", "ground", "field"),
        ("h", "field", "grid", "field"),
        ("grid", "points", "palms", "points"),
        ("h", "field", "scatter", "field"),
        ("scatter", "points", "rocks", "points"),
    ];
    let doc = json!({
        "format": "real-engine/graph",
        "version": 1,
        "name": "walk",
        "seed": 23,
        "nodes": nodes.iter().map(|(id, kind, params)| json!({
            "id": id, "kind": kind, "version": 1, "params": params,
        })).collect::<Vec<_>>(),
        "edges": edges.iter().map(|(f, fp, t, tp)| json!({
            "from": [f, fp], "to": [t, tp],
        })).collect::<Vec<_>>(),
    });
    GraphDoc::from_json(&doc.to_string()).expect("the walk graph parses")
}

fn world(root: &Path, cache: &Path, pool: Arc<TaskPool>) -> EditState {
    let mut st = EditState::new();
    register_generator(&mut st.world.generators);
    let evaluator = Evaluator::new(Arc::new(NodeRegistry::m1()))
        .with_pool(pool)
        .with_disk_cache(cache);
    let mut store = GraphStore::with_evaluator(Arc::new(evaluator), Some(root.to_path_buf()));
    store.insert("world", world_graph());
    st.world.ecs.insert_resource(store);
    // Facing along +X+Z: -Z turned by -135° about Y.
    let facing = Quat::from_xyzw(0.0, -0.923_879_5, 0.0, 0.382_683_43);
    let scene = json!({
        "format": "real-engine/scene",
        "version": 1,
        "entities": [
            {
                "id": "world",
                "name": "World",
                "components": {
                    "GraphSource": { "builder": "scripts/tools/world.luau" },
                    "Procedural": { "generator": "graph", "seed": 23 },
                    "Streamed": { "bounds": HALF },
                    "Transform": {}
                }
            },
            {
                "id": "walker",
                "name": "Walker",
                "components": {
                    "StreamFocus": {},
                    "Transform": {
                        "translation": [-END, 0.0, -END],
                        "rotation": [facing.x, facing.y, facing.z, facing.w]
                    }
                }
            }
        ]
    });
    let doc = re_scene::scene::from_json(&scene.to_string()).expect("the walk scene parses");
    st.world.load(&doc).expect("the walk scene loads");
    st.world.update();
    st
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
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

/// One walk; returns its report.
fn walk(root: &Path, cache: &Path, pool: &Arc<TaskPool>, args: &Args) -> Json {
    let ticks = args.ticks;
    let config = StreamConfig::default();
    let mut st = world(root, cache, pool.clone());
    let w = &mut st.world;
    let mut streamer = Streamer::new(config.clone())
        .expect("the default settings are valid")
        .with_pool(pool.clone());
    streamer.set_lateness(Lateness::Wait);
    w.ecs.insert_resource(streamer);

    // The initial load, behind a load screen: until nothing is pending.
    let start = Instant::now();
    let mut tick = 0u64;
    let mut load_waits = 0usize;
    loop {
        let r = step(w, tick, &config);
        assert!(r.diagnostics.is_empty(), "{:#?}", r.diagnostics);
        load_waits += r.waits;
        w.update();
        tick += 1;
        if r.pending == 0 && r.in_flight == 0 {
            break;
        }
    }
    let load_ms = start.elapsed().as_secs_f64() * 1000.0;
    let load_ticks = tick;
    let load_cells = w.ecs.resource::<Streamer>().cells().count();

    // The walk.
    let diagonal = 2.0 * END;
    let per_tick = SPEED / HZ / std::f32::consts::SQRT_2;
    let total = ticks.unwrap_or((diagonal / per_tick) as u64);
    let mut times = Vec::with_capacity(total as usize);
    // The slowest ticks and what they did, to tell work from stalls.
    let mut slowest: Vec<Json> = Vec::new();
    let (mut waits, mut late, mut loaded, mut unloaded, mut deferred) = (0, 0, 0, 0, 0);
    let mut peak_bytes = 0u64;
    let mut peak_entities = 0usize;
    let mut peak_cells = 0usize;
    let mut position = -END;
    let step_time = std::time::Duration::from_secs_f64(1.0 / f64::from(HZ));
    let mut next = Instant::now();
    for _ in 0..total {
        if !args.unpaced {
            // The fixed step at real time: generation runs between ticks.
            next += step_time;
            let now = Instant::now();
            if next > now {
                std::thread::sleep(next - now);
            } else {
                next = now;
            }
        }
        position = (position + per_tick).min(END);
        let before = os_ticks();
        let sched0 = schedstat();
        let t0 = Instant::now();
        w.set(
            "walker",
            "Transform",
            "translation",
            Value::Vec3(Vec3::new(position, 0.0, position)),
        )
        .expect("the walker moves");
        let r = step(w, tick, &config);
        let step_ms = t0.elapsed().as_secs_f64() * 1000.0;
        let sched1 = schedstat();
        w.update();
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let sched2 = schedstat();
        times.push(ms);
        if slowest.len() < 5 || ms > slowest[4]["ms"].as_f64().unwrap_or(0.0) {
            let after = os_ticks();
            slowest.push(json!({
                // In 10 ms clock ticks (Linux): this thread's CPU time, and
                // the time the host took from the VM's CPUs (steal). A tick
                // far longer than its CPU time was descheduled, not working.
                "thread_cpu_ticks": after.0.saturating_sub(before.0),
                "steal_ticks": after.1.saturating_sub(before.1),
                // In ms (Linux schedstat): this thread's CPU time in the
                // step and in the world update, and its time waiting for a
                // CPU in the run queue.
                "step_cpu_ms": (sched1.0 - sched0.0) as f64 / 1.0e6,
                "update_cpu_ms": (sched2.0 - sched1.0) as f64 / 1.0e6,
                "runqueue_ms": (sched2.1 - sched0.1) as f64 / 1.0e6,
                "tick": tick,
                "ms": ms,
                "step_ms": step_ms,
                "loaded": r.loaded.len(),
                "unloaded": r.unloaded.len(),
                "entities": r.entities,
            }));
            slowest.sort_by(|a, b| {
                b["ms"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&a["ms"].as_f64().unwrap_or(0.0))
            });
            slowest.truncate(5);
        }
        assert!(
            r.diagnostics.is_empty(),
            "tick {tick}: {:#?}",
            r.diagnostics
        );
        waits += r.waits;
        late += r.late;
        loaded += r.loaded.len();
        unloaded += r.unloaded.len();
        deferred += r.deferred;
        peak_bytes = peak_bytes.max(r.memory_bytes);
        peak_cells = peak_cells.max(r.resident);
        peak_entities = peak_entities.max(w.ids().len());
        tick += 1;
    }
    let mut sorted = times.clone();
    sorted.sort_by(f64::total_cmp);
    let hash = w.state_hash().to_hex().to_string();
    json!({
        "load": { "ticks": load_ticks, "ms": load_ms, "cells": load_cells, "waits": load_waits },
        "walk": {
            "ticks": total,
            "metres": (position + END) * std::f32::consts::SQRT_2,
            "waits": waits,
            "late": late,
            "cells_loaded": loaded,
            "cells_unloaded": unloaded,
            "deferred": deferred,
            "tick_ms": {
                "p50": percentile(&sorted, 0.5),
                "p95": percentile(&sorted, 0.95),
                "p99": percentile(&sorted, 0.99),
                "max": sorted.last().copied().unwrap_or(0.0),
            },
            "slowest": slowest,
            "peak_cells": peak_cells,
            "peak_entities": peak_entities,
            "peak_memory_mib": peak_bytes as f64 / (1 << 20) as f64,
        },
        "state_hash": hash,
    })
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::from(2);
        }
    };
    let pool = match TaskPool::new(args.threads) {
        Ok(p) => Arc::new(p),
        Err(d) => {
            eprintln!("{d}");
            return ExitCode::from(2);
        }
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples");
    let cache = match tempfile::tempdir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("no temporary directory: {e}");
            return ExitCode::from(2);
        }
    };
    let cold = walk(&root, cache.path(), &pool, &args);
    let warm = walk(&root, cache.path(), &pool, &args);
    let rss = peak_rss_mb();
    let config = StreamConfig::default();
    let budgets = {
        let mut out = Vec::new();
        for (name, run) in [("cold", &cold), ("warm", &warm)] {
            let w = &run["walk"];
            out.push(json!({ "run": name, "check": "zero waits after the initial load", "value": w["waits"], "ok": w["waits"] == 0 }));
            let p95 = w["tick_ms"]["p95"].as_f64().unwrap_or(f64::MAX);
            let max = w["tick_ms"]["max"].as_f64().unwrap_or(f64::MAX);
            out.push(json!({ "run": name, "check": format!("tick p95 <= {P95_MS} ms"), "value": p95, "ok": p95 <= P95_MS }));
            out.push(json!({ "run": name, "check": format!("tick max <= {MAX_MS} ms"), "value": max, "ok": max <= MAX_MS }));
            let mem = w["peak_memory_mib"].as_f64().unwrap_or(f64::MAX);
            out.push(json!({ "run": name, "check": format!("accounted memory <= {} MiB", config.memory_mb), "value": mem, "ok": mem <= f64::from(config.memory_mb) }));
        }
        out.push(json!({ "check": format!("peak RSS <= {RSS_MB} MB"), "value": rss, "ok": rss <= RSS_MB }));
        out.push(json!({ "check": "same state_hash cold and warm", "value": [cold["state_hash"], warm["state_hash"]], "ok": cold["state_hash"] == warm["state_hash"] }));
        out
    };
    let ok = budgets.iter().all(|b| b["ok"] == true);
    let report = json!({
        "world_km": 2.0 * HALF / 1000.0,
        "speed_m_s": SPEED,
        "hz": HZ,
        "threads": pool.threads(),
        "paced": !args.unpaced,
        "jobs": config.jobs(pool.threads()),
        "stream": config,
        "cold": cold,
        "warm": warm,
        "peak_rss_mb": rss,
        "budgets": budgets,
        "ok": ok,
    });
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
    } else {
        for b in report["budgets"].as_array().into_iter().flatten() {
            println!(
                "{} {} {}: {}",
                if b["ok"] == true { "ok  " } else { "MISS" },
                b["run"].as_str().unwrap_or("both"),
                b["check"].as_str().unwrap_or(""),
                b["value"]
            );
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// This thread's CPU time and run-queue wait in nanoseconds, on Linux;
/// zeros elsewhere.
fn schedstat() -> (u64, u64) {
    let text = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap_or_default();
    let mut f = text.split_whitespace().map(|v| v.parse().unwrap_or(0u64));
    (f.next().unwrap_or(0), f.next().unwrap_or(0))
}

/// This thread's CPU time and the system's steal time, in clock ticks, on
/// Linux; zeros elsewhere.
fn os_ticks() -> (u64, u64) {
    let field = |text: &str, i: usize| -> u64 {
        text.split_whitespace()
            .nth(i)
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let cpu = std::fs::read_to_string("/proc/thread-self/stat")
        .map(|t| {
            // After the command name's closing parenthesis: utime and stime
            // are fields 14 and 15 of the whole line.
            let rest = t.rsplit_once(')').map_or("", |(_, r)| r);
            field(rest, 11) + field(rest, 12)
        })
        .unwrap_or(0);
    let steal = std::fs::read_to_string("/proc/stat")
        .map(|t| t.lines().next().map_or(0, |l| field(l, 8)))
        .unwrap_or(0);
    (cpu, steal)
}
