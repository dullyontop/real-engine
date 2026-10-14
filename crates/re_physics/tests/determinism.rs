//! Command-order independence and run-to-run determinism of `BodyPose`s.

mod common;

use common::*;
use re_types::EntityId;
use re_types::glam::Vec3;
use re_types::physics::PhysicsCmd;

/// A pile plus one impulse per tenth body: a command stream with every kind of
/// creation and per-body command, each key unique.
fn stream() -> Vec<PhysicsCmd> {
    let mut cmds = pile(400);
    for i in (1..=400u64).step_by(10) {
        cmds.push(PhysicsCmd::ApplyImpulse {
            id: EntityId(i),
            impulse: Vec3::new(0.3, 0.0, -0.2),
        });
        cmds.push(PhysicsCmd::SetVelocity {
            id: EntityId(i + 1),
            linear: Vec3::new(0.0, 1.0, 0.0),
            angular: Vec3::new(0.0, 0.0, 0.5),
        });
    }
    cmds
}

/// A deterministic shuffle (xorshift), so the unsorted order is fixed but scrambled.
fn shuffled(mut cmds: Vec<PhysicsCmd>) -> Vec<PhysicsCmd> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    for i in (1..cmds.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        cmds.swap(i, (state % (i as u64 + 1)) as usize);
    }
    cmds
}

fn run(cmds: Vec<PhysicsCmd>, threads: usize) -> Vec<(u64, [u32; 7])> {
    let mut p = world(threads);
    apply(&mut p, cmds);
    let mut out = Vec::new();
    for _ in 0..90 {
        p.step(DT).unwrap();
        out.extend(pose_bits(&mut p));
    }
    out
}

#[test]
fn unsorted_and_sorted_streams_give_identical_poses() {
    let mut sorted = stream();
    PhysicsCmd::sort(&mut sorted);
    let unsorted = shuffled(stream());
    assert_ne!(unsorted, sorted, "the shuffle must change the order");
    assert!(
        run(unsorted, 2) == run(sorted, 2),
        "poses depend on command order"
    );
}

#[test]
fn the_same_stream_twice_gives_identical_poses() {
    let first = run(shuffled(stream()), 3);
    let second = run(shuffled(stream()), 3);
    assert!(!first.is_empty());
    assert!(first == second, "two runs of the same stream differ");
}
