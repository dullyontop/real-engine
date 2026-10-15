//! Micro-benchmarks of the script runtime, through its public API (advisory).
//!
//! ```sh
//! cargo run --locked -p re_script --release --example script_bench
//! ```
//!
//! Each figure is the best of 5 runs, in nanoseconds per operation. Compare with
//! the spike's numbers in `docs/research/scripting.md` §6.

use std::time::{Duration, Instant};

use re_core::Phase;
use re_scene::World;
use re_script::{ContextKind, NoServices, RuntimeConfig, ScriptRuntime, WorldHost};

struct Bench {
    rt: ScriptRuntime,
    world: World,
    services: NoServices,
}

impl Bench {
    fn new() -> Bench {
        let mut config = RuntimeConfig::new(std::env::temp_dir());
        config.cpu_budget = Duration::from_secs(60);
        Bench {
            rt: ScriptRuntime::new(config).expect("runtime"),
            world: World::new(),
            services: NoServices::default(),
        }
    }

    fn exec(&mut self, source: &str) {
        let Bench {
            rt,
            world,
            services,
        } = self;
        rt.exec(
            &mut WorldHost { world, services },
            ContextKind::Game,
            "bench",
            source,
        )
        .unwrap_or_else(|d| panic!("{d:#?}"));
    }

    fn phase(&mut self, phase: Phase, dt: f64) {
        let Bench {
            rt,
            world,
            services,
        } = self;
        let d = rt.run_phase(&mut WorldHost { world, services }, phase, dt);
        assert!(d.is_empty(), "{d:#?}");
    }

    /// Times `source` (which runs `ops` operations) as the best of 5 runs.
    fn luau(&mut self, name: &str, ops: u64, source: &str) {
        let ns = best(ops, || self.exec(source));
        row(name, ns);
    }
}

fn best(ops: u64, mut f: impl FnMut()) -> f64 {
    let mut best = Duration::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        f();
        best = best.min(t.elapsed());
    }
    best.as_nanos() as f64 / ops as f64
}

fn row(name: &str, ns: f64) {
    println!("{name:58} {ns:>9.1} ns/op");
}

fn main() {
    const N: u64 = 1_000_000;
    let mut b = Bench::new();
    b.exec("world.spawn({ id = 'bench', components = { Transform = {} } })");

    b.luau(
        "Luau calls an engine function (time.now)",
        N,
        &format!("for i = 1, {N} do time.now() end"),
    );
    b.luau(
        "e:get(\"Transform\", \"translation\") (returns vector)",
        N,
        &format!(
            "local e = world.get('bench') for i = 1, {N} do e:get('Transform', 'translation') end"
        ),
    );
    b.luau(
        "e:set(\"Transform\", \"translation\", v)",
        N,
        &format!(
            "local e, v = world.get('bench'), vector.create(1, 2, 3) for i = 1, {N} do e:set('Transform', 'translation', v) end"
        ),
    );
    b.luau(
        "native vector p = p + v * 0.5",
        N,
        &format!(
            "local p, v = vector.zero, vector.create(1, 2, 3) for i = 1, {N} do p = p + v * 0.5 end"
        ),
    );
    b.luau(
        "fib(30) per Luau call",
        2_692_537,
        "local function fib(n) if n < 2 then return n end return fib(n-1) + fib(n-2) end fib(30)",
    );
    b.luau(
        "math.sqrt loop (per-script env with safeenv)",
        N,
        &format!("local s = 0 for i = 1, {N} do s += math.sqrt(i) end"),
    );
    b.luau(
        "empty-ish loop iteration (raw interrupt)",
        N * 10,
        &format!("local s = 0 for i = 1, {} do s += i end", N * 10),
    );

    b.exec("for i = 1, 999 do world.spawn({ components = { Transform = {} } }) end");
    b.luau(
        "world.query(\"Transform\") over 1000 entities, per entity",
        100 * 1000,
        "for i = 1, 100 do world.query('Transform') end",
    );

    // Scheduler: 10k script threads each waiting every step.
    let mut s = Bench::new();
    s.exec("for i = 1, 10000 do task.spawn(function() while true do task.wait() end end) end");
    let frames = 20u64;
    row(
        "scheduler: resume + task.wait per thread (10k threads)",
        best(frames * 10_000, || {
            for _ in 0..frames {
                s.phase(Phase::PreSimulation, 1.0 / 60.0);
                s.phase(Phase::PostSimulation, 1.0 / 60.0);
            }
        }),
    );

    // A step callback: one thread per firing.
    let mut c = Bench::new();
    c.exec("time.on_step(function(dt) end)");
    row(
        "time.on_step: fire + run the callback thread",
        best(10_000, || {
            for _ in 0..10_000 {
                c.phase(Phase::PreSimulation, 1.0 / 60.0);
            }
        }),
    );

    // Compile throughput on a mid-sized generated module.
    let mut big = String::from("local M = {}\n");
    for i in 0..2000 {
        big.push_str(&format!(
            "M.f{i} = function(a, b) if a > b then return a * {i} else return b + {i} end end\n"
        ));
    }
    let ns = best(big.len() as u64, || {
        assert!(re_script::compile_check(&big, "big.luau").is_empty());
    });
    row("compile_check (per byte)", ns);
    println!("  -> {:.1} MB/s", 1e3 / ns);
}
