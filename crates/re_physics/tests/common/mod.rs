//! Helpers shared by the integration tests.

#![allow(dead_code)]

use re_core::TaskPool;
use re_physics::{Physics, PhysicsConfig};
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::{BodyDesc, BodyKind, PhysicsCmd, ShapeDesc};

pub const DT: f32 = 1.0 / 60.0;

/// Jolt's default penetration slop (0.02 m): a resting body sits between its geometric
/// height minus the slop and its geometric height. oxijolt 1.1.0 does not expose it.
pub const PENETRATION_SLOP: f32 = 0.02;

/// A worker pool with `threads` threads that lives for the rest of the process.
pub fn pool(threads: usize) -> &'static TaskPool {
    Box::leak(Box::new(TaskPool::new(threads).expect("worker pool")))
}

pub fn world(threads: usize) -> Physics {
    Physics::new(PhysicsConfig::default(), pool(threads)).expect("physics world")
}

pub fn world_with(config: PhysicsConfig, threads: usize) -> Physics {
    Physics::new(config, pool(threads)).expect("physics world")
}

/// Applies `cmds` and panics on any diagnostic.
pub fn apply(p: &mut Physics, cmds: Vec<PhysicsCmd>) {
    let mut cmds = cmds;
    p.apply(&mut cmds);
    let diags = p.take_diagnostics();
    assert!(
        diags.is_empty(),
        "unexpected diagnostics: {}",
        diags
            .iter()
            .map(|d| d.render_human())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

pub fn steps(p: &mut Physics, n: usize) {
    for _ in 0..n {
        p.step(DT).expect("step");
    }
}

pub fn add(id: u64, desc: BodyDesc) -> PhysicsCmd {
    PhysicsCmd::AddBody {
        id: EntityId(id),
        desc,
    }
}

pub fn body(kind: BodyKind, shape: ShapeDesc, pos: Vec3) -> BodyDesc {
    BodyDesc {
        kind,
        shape,
        pos,
        ..BodyDesc::default()
    }
}

pub fn cube(half: f32) -> ShapeDesc {
    ShapeDesc::Box {
        half_extents: Vec3::splat(half),
    }
}

pub fn sphere(radius: f32) -> ShapeDesc {
    ShapeDesc::Sphere { radius }
}

/// A static box floor whose top face is at `top`, centred on (x, z), `half` wide.
pub fn floor(id: u64, x: f32, z: f32, top: f32, half: f32) -> PhysicsCmd {
    add(
        id,
        body(
            BodyKind::Static,
            ShapeDesc::Box {
                half_extents: Vec3::new(half, 0.5, half),
            },
            Vec3::new(x, top - 0.5, z),
        ),
    )
}

pub fn pos(p: &Physics, id: u64) -> Vec3 {
    p.pose(EntityId(id)).expect("entity has a pose").0
}

pub fn rot(p: &Physics, id: u64) -> Quat {
    p.pose(EntityId(id)).expect("entity has a pose").1
}

/// Asserts a rest height within `[geometric - slop, geometric]` (plus 1 mm).
#[track_caller]
pub fn assert_rests(what: &str, got: f32, geometric: f32) {
    assert!(
        got >= geometric - PENETRATION_SLOP - 1e-3 && got <= geometric + 1e-3,
        "{what}: expected {:.4}..={geometric:.4}, got {got:.4}",
        geometric - PENETRATION_SLOP
    );
}

#[track_caller]
pub fn assert_near(what: &str, got: f32, want: f32, tol: f32) {
    assert!(
        (got - want).abs() <= tol,
        "{what}: expected {want:.4} ± {tol}, got {got:.4}"
    );
}

/// `count` dynamic bodies (alternating spheres and boxes) above a 120 m floor: the
/// spike's pile scene. Entity 0 is the floor; bodies are 1..=count.
pub fn pile(count: usize) -> Vec<PhysicsCmd> {
    let mut cmds = vec![floor(0, 0.0, 0.0, 0.0, 60.0)];
    let side = (count as f64).cbrt().ceil() as usize;
    for i in 0..count {
        let (x, y, z) = (i % side, i / (side * side), (i / side) % side);
        let shape = if i % 2 == 0 { sphere(0.4) } else { cube(0.4) };
        let mut desc = body(
            BodyKind::Dynamic,
            shape,
            Vec3::new(
                x as f32 - side as f32 * 0.5,
                1.0 + y as f32,
                z as f32 - side as f32 * 0.5,
            ),
        );
        desc.rot = Quat::from_rotation_y(i as f32 * 0.37);
        cmds.push(add(1 + i as u64, desc));
    }
    cmds
}

/// The bit patterns of every pose, for exact comparisons.
pub fn pose_bits(p: &mut Physics) -> Vec<(u64, [u32; 7])> {
    let mut poses = Vec::new();
    p.poses_into(&mut poses);
    poses
        .iter()
        .map(|q| {
            (
                q.id.0,
                [
                    q.pos.x.to_bits(),
                    q.pos.y.to_bits(),
                    q.pos.z.to_bits(),
                    q.rot.x.to_bits(),
                    q.rot.y.to_bits(),
                    q.rot.z.to_bits(),
                    q.rot.w.to_bits(),
                ],
            )
        })
        .collect()
}

/// Every body's position bits, awake or asleep, for `ids`.
pub fn position_bits(p: &Physics, ids: impl IntoIterator<Item = u64>) -> Vec<[u32; 3]> {
    ids.into_iter()
        .map(|id| {
            let v = pos(p, id);
            [v.x.to_bits(), v.y.to_bits(), v.z.to_bits()]
        })
        .collect()
}
