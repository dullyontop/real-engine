//! Step-time benchmark: dynamic bodies (half spheres, half boxes) falling into a pile
//! on a floor, the physics spike's scene.
//!
//! ```sh
//! cargo run -p re_physics --release --example pile_bench -- --bodies 1000 --steps 600 [--threads N]
//! ```
//!
//! Prints `pile_bench mean_ms=<x> p95_ms=<y>` plus a detail line. `--threads 0` (the
//! default) uses the engine's default worker count.

use re_core::time::Instant;

use re_core::TaskPool;
use re_physics::{Physics, PhysicsConfig};
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::{BodyDesc, BodyKind, PhysicsCmd, ShapeDesc};

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
    let bodies = arg(&args, "--bodies", 1000);
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
    let side = libm::cbrt(bodies as f64).ceil() as usize;
    for i in 0..bodies {
        let (x, y, z) = (i % side, i / (side * side), (i / side) % side);
        let shape = if i % 2 == 0 {
            ShapeDesc::Sphere { radius: 0.4 }
        } else {
            ShapeDesc::Box {
                half_extents: Vec3::splat(0.4),
            }
        };
        cmds.push(PhysicsCmd::AddBody {
            id: EntityId(1 + i as u64),
            desc: BodyDesc {
                kind: BodyKind::Dynamic,
                shape,
                pos: Vec3::new(
                    x as f32 - side as f32 * 0.5,
                    1.0 + y as f32,
                    z as f32 - side as f32 * 0.5,
                ),
                rot: Quat::from_rotation_y(i as f32 * 0.37),
                ..BodyDesc::default()
            },
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
        times.push(t.elapsed().as_secs_f64() * 1e3);
    }
    let awake = (1..=bodies as u64)
        .filter(|&i| physics.is_awake(EntityId(i)) == Some(true))
        .count();
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    times.sort_by(f64::total_cmp);
    let pct = |q: f64| times[((times.len() - 1) as f64 * q).round() as usize];
    println!("pile_bench mean_ms={mean:.3} p95_ms={:.3}", pct(0.95));
    println!(
        "  {bodies} bodies, {steps} steps at 60 Hz, {} workers + caller: p50 {:.3} ms, p99 {:.3} ms, max {:.3} ms, awake at end {awake}, jobs {}",
        pool.threads(),
        pct(0.5),
        pct(0.99),
        times[times.len() - 1],
        physics.jobs_queued()
    );
    for d in physics.take_diagnostics() {
        eprintln!("{}", d.render_human());
    }
}
