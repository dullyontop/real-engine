//! Step-time benchmark: active ragdolls (ADR-0040) tumbling onto a floor and onto each
//! other, half of them limp and half powered. The M3 budget is 32 active ragdolls
//! within 2 ms mean step.
//!
//! ```sh
//! cargo run -p re_physics --release --example ragdoll_bench -- --ragdolls 32 --steps 600 [--threads N]
//! ```
//!
//! Prints `ragdoll_bench mean_ms=<x> p95_ms=<y>` plus a detail line. `--threads 0` (the
//! default) uses the engine's default worker count.

#[path = "../tests/common/humanoid.rs"]
mod humanoid;

use re_core::TaskPool;
use re_core::time::Instant;
use re_physics::{Physics, PhysicsConfig};
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::{BodyDesc, PhysicsCmd, RagdollMode, ShapeDesc};

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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let ragdolls = arg(&args, "--ragdolls", 32);
    let steps = arg(&args, "--steps", 600).max(1);
    let threads = arg(&args, "--threads", 0);

    let pool = TaskPool::init_global(threads).unwrap_or_else(|d| panic!("{}", d.render_human()));
    let mut physics = Physics::new(PhysicsConfig::default(), pool)
        .unwrap_or_else(|d| panic!("{}", d.render_human()));

    let mut cmds = vec![PhysicsCmd::AddBody {
        id: EntityId(0),
        desc: BodyDesc {
            shape: ShapeDesc::Box {
                half_extents: Vec3::new(60.0, 0.5, 60.0),
            },
            pos: Vec3::new(0.0, -0.5, 0.0),
            ..BodyDesc::default()
        },
    }];
    let desc = humanoid::ragdoll();
    let side = (ragdolls as f64).sqrt().ceil() as usize;
    for i in 0..ragdolls {
        let (x, z) = (i % side, i / side);
        cmds.push(PhysicsCmd::AddRagdoll {
            id: EntityId(1 + i as u64),
            desc: desc.clone(),
            pos: Vec3::new(x as f32 * 0.7, 0.5 + (i % 3) as f32 * 0.8, z as f32 * 0.7),
            rot: Quat::from_rotation_y(i as f32 * 0.9),
            pose: None,
        });
    }
    physics.apply(&mut cmds);
    for i in 0..ragdolls {
        let id = EntityId(1 + i as u64);
        let mode = if i % 2 == 0 {
            RagdollMode::Simulated
        } else {
            RagdollMode::Powered { strength: 0.6 }
        };
        cmds.push(PhysicsCmd::SetRagdollMode { id, mode });
        cmds.push(PhysicsCmd::ApplyRagdollImpulse {
            id,
            joint: "head".into(),
            impulse: Vec3::new(6.0, 0.0, (i % 5) as f32),
        });
    }
    physics.apply(&mut cmds);
    for d in physics.take_diagnostics() {
        eprintln!("{}", d.render_human());
    }

    let mut times = Vec::with_capacity(steps);
    let mut poses = Vec::new();
    for _ in 0..steps {
        let t = Instant::now();
        physics
            .step(1.0 / 60.0)
            .unwrap_or_else(|d| panic!("{}", d.render_human()));
        physics.poses_into(&mut poses);
        for i in 0..ragdolls {
            let _ = physics.ragdoll(EntityId(1 + i as u64));
        }
        times.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let calm = (0..ragdolls)
        .filter(|&i| {
            physics
                .ragdoll(EntityId(1 + i as u64))
                .is_some_and(|s| s.calm)
        })
        .count();
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    times.sort_by(f64::total_cmp);
    let pct = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    println!("ragdoll_bench mean_ms={mean:.3} p95_ms={:.3}", pct(0.95));
    println!(
        "  {ragdolls} ragdolls of {} parts, {steps} steps at 60 Hz, {} workers + caller: p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms, calm at end {calm}",
        humanoid::PARTS.len(),
        pool.threads(),
        pct(0.5),
        pct(0.99),
        times[times.len() - 1],
    );
    for d in physics.take_diagnostics() {
        eprintln!("{}", d.render_human());
    }
}
