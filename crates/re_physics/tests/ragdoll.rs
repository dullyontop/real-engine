//! Ragdolls (ADR-0040): falling and coming to rest, following an animation, powered
//! recovery, the mapped pose, strength changes, queries, errors, determinism, and
//! the floating origin (ADR-0042).

mod common;

use common::humanoid::{self, joint, posed};
use common::*;
use re_core::Severity;
use re_physics::{Physics, PhysicsConfig, QueryFilter};
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::{JointPose, PhysicsCmd, RagdollMode, RagdollState};

const MAN: EntityId = EntityId(5);

fn add_man(id: EntityId, pos: Vec3, rot: Quat) -> PhysicsCmd {
    PhysicsCmd::AddRagdoll {
        id,
        desc: humanoid::ragdoll(),
        pos,
        rot,
        pose: None,
    }
}

fn mode(id: EntityId, mode: RagdollMode) -> PhysicsCmd {
    PhysicsCmd::SetRagdollMode { id, mode }
}

fn target(id: EntityId, pos: Vec3, pose: Vec<JointPose>) -> PhysicsCmd {
    PhysicsCmd::SetRagdollTarget {
        id,
        pos,
        rot: Quat::IDENTITY,
        pose,
    }
}

fn state(p: &Physics, id: EntityId) -> RagdollState {
    p.ragdoll(id).expect("the entity has a ragdoll")
}

/// The rotation of each part relative to its parent part's, from a model-space pose.
fn relative(pose: &[JointPose]) -> Vec<Quat> {
    let s = humanoid::skeleton();
    humanoid::PARTS[1..]
        .iter()
        .map(|name| {
            let j = joint(name);
            let mut parent = s.joints[j].parent.map(usize::from);
            while let Some(p) = parent {
                if humanoid::PARTS.contains(&s.joints[p].name.as_str()) {
                    break;
                }
                parent = s.joints[p].parent.map(usize::from);
            }
            let p = parent.expect("a part above");
            (pose[p].rot.conjugate() * pose[j].rot).normalize()
        })
        .collect()
}

fn worst_angle(a: &[Quat], b: &[Quat]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.angle_between(*y))
        .fold(0.0, f32::max)
}

/// A pose with bent elbows and knees and raised arms.
fn bent() -> Vec<JointPose> {
    posed(&[
        ("upper_arm_l", Quat::from_rotation_z(0.5)),
        ("forearm_l", Quat::from_rotation_y(-0.8)),
        ("upper_arm_r", Quat::from_rotation_z(-0.5)),
        ("forearm_r", Quat::from_rotation_y(0.8)),
        ("thigh_l", Quat::from_rotation_x(-0.4)),
        ("shin_l", Quat::from_rotation_x(0.6)),
        ("spine", Quat::from_rotation_x(0.15)),
        ("head", Quat::from_rotation_y(0.4)),
    ])
}

#[test]
fn a_generated_ragdoll_falls_and_becomes_calm_on_flat_ground() {
    let mut p = world(2);
    apply(
        &mut p,
        vec![
            floor(0, 0.0, 0.0, 0.0, 20.0),
            add_man(MAN, Vec3::new(0.0, 0.3, 0.0), Quat::IDENTITY),
        ],
    );
    steps(&mut p, 2);
    let standing = state(&p, MAN);
    assert_eq!(standing.mode, RagdollMode::Animated);
    assert!(
        !standing.calm,
        "a fresh ragdoll has not counted calm steps yet"
    );
    // An animated ragdoll holds its pose in the air.
    assert_near("hips height", standing.root_pos.y, 1.3, 1e-3);
    apply(
        &mut p,
        vec![
            mode(MAN, RagdollMode::Simulated),
            PhysicsCmd::ApplyRagdollImpulse {
                id: MAN,
                joint: "neck".into(),
                impulse: Vec3::new(0.0, 0.0, 30.0),
            },
        ],
    );
    let mut calm_at = None;
    for tick in 0..900 {
        p.step(DT).unwrap();
        if state(&p, MAN).calm {
            calm_at = Some(tick);
            break;
        }
    }
    assert!(p.take_diagnostics().is_empty());
    let tick = calm_at.expect("the ragdoll comes to rest within 15 s");
    let lying = state(&p, MAN);
    assert!(tick > 30, "it fell for a while first ({tick})");
    assert!(
        lying.root_pos.y < 0.4,
        "it lies on the ground: {}",
        lying.root_pos
    );
    for (name, part) in p.ragdoll_parts(MAN).unwrap() {
        assert!(
            part.pos.y > -0.05,
            "{name} sank into the floor: {}",
            part.pos
        );
    }
}

#[test]
fn an_animated_ragdoll_follows_its_target_within_a_centimetre() {
    let mut p = world(2);
    let frame = Vec3::new(3.0, 0.0, -2.0);
    apply(
        &mut p,
        vec![
            floor(0, 0.0, 0.0, 0.0, 20.0),
            add_man(MAN, frame, Quat::IDENTITY),
        ],
    );
    let want = bent();
    // A turned frame: the whole pose travels with it.
    let turn = Quat::from_rotation_y(0.7);
    for _ in 0..5 {
        apply(
            &mut p,
            vec![PhysicsCmd::SetRagdollTarget {
                id: MAN,
                pos: frame,
                rot: turn,
                pose: want.clone(),
            }],
        );
        p.step(DT).unwrap();
    }
    let got = state(&p, MAN);
    assert_eq!((got.pos, got.rot), (frame, turn));
    assert_eq!(got.pose.len(), want.len());
    for (j, (g, w)) in got.pose.iter().zip(&want).enumerate() {
        let error = g.pos.distance(w.pos);
        assert!(
            error < 0.01,
            "joint {} is {error} m off",
            humanoid::JOINTS[j].0
        );
        assert!(
            g.rot.angle_between(w.rot) < 0.02,
            "joint {}",
            humanoid::JOINTS[j].0
        );
    }
    // The parts are where the target puts them in the world.
    for (name, part) in p.ragdoll_parts(MAN).unwrap() {
        let w = JointPose::new(frame, turn).mul(&want[joint(&name)]);
        assert!(part.pos.distance(w.pos) < 0.01, "{name}");
    }
}

#[test]
fn a_powered_ragdoll_recovers_its_pose_after_an_impulse() {
    // Without gravity, and damped so the spin the impulse gives dies out (a spinning
    // body bends at its joints however strong the motors).
    let damped = || re_types::physics::RagdollDesc {
        linear_damping: 2.0,
        angular_damping: 2.0,
        ..humanoid::ragdoll()
    };
    let mut p = world_with(
        PhysicsConfig {
            gravity: Vec3::ZERO,
            ..PhysicsConfig::default()
        },
        2,
    );
    let want = bent();
    apply(
        &mut p,
        vec![PhysicsCmd::AddRagdoll {
            id: MAN,
            desc: damped(),
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            pose: Some(want.clone()),
        }],
    );
    apply(
        &mut p,
        vec![
            target(MAN, Vec3::ZERO, want.clone()),
            mode(MAN, RagdollMode::Powered { strength: 1.0 }),
        ],
    );
    steps(&mut p, 10);
    let goal = relative(&want);
    assert!(worst_angle(&relative(&state(&p, MAN).pose), &goal) < 0.05);
    apply(
        &mut p,
        vec![PhysicsCmd::ApplyRagdollImpulse {
            id: MAN,
            joint: "hand_l".into(),
            impulse: Vec3::new(0.0, 0.0, 8.0),
        }],
    );
    steps(&mut p, 12);
    let hit = worst_angle(&relative(&state(&p, MAN).pose), &goal);
    assert!(hit > 0.1, "the impulse bends the arm ({hit} rad)");
    steps(&mut p, 180);
    let back = worst_angle(&relative(&state(&p, MAN).pose), &goal);
    assert!(back < 0.05, "the motors bring the pose back ({back} rad)");

    // A limp ragdoll hit the same way stays bent.
    let mut limp = world_with(
        PhysicsConfig {
            gravity: Vec3::ZERO,
            ..PhysicsConfig::default()
        },
        2,
    );
    apply(
        &mut limp,
        vec![PhysicsCmd::AddRagdoll {
            id: MAN,
            desc: damped(),
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            pose: Some(want.clone()),
        }],
    );
    apply(&mut limp, vec![mode(MAN, RagdollMode::Simulated)]);
    apply(
        &mut limp,
        vec![PhysicsCmd::ApplyRagdollImpulse {
            id: MAN,
            joint: "hand_l".into(),
            impulse: Vec3::new(0.0, 0.0, 8.0),
        }],
    );
    steps(&mut limp, 192);
    assert!(worst_angle(&relative(&state(&limp, MAN).pose), &goal) > 0.1);
}

#[test]
fn the_mapped_pose_round_trips_to_the_parts() {
    let mut p = world(2);
    let frame = Vec3::new(-1.0, 0.2, 4.0);
    let turn = Quat::from_rotation_y(-1.2);
    apply(
        &mut p,
        vec![floor(0, 0.0, 0.0, 0.0, 20.0), add_man(MAN, frame, turn)],
    );
    apply(
        &mut p,
        vec![
            PhysicsCmd::SetRagdollTarget {
                id: MAN,
                pos: frame,
                rot: turn,
                pose: bent(),
            },
            mode(MAN, RagdollMode::Simulated),
        ],
    );
    steps(&mut p, 90);
    let s = state(&p, MAN);
    let frame = JointPose::new(s.pos, s.rot);
    let parts = p.ragdoll_parts(MAN).unwrap();
    assert_eq!(parts.len(), humanoid::PARTS.len());
    // Mapping the animation pose back to the ragdoll (map_reverse) gives the parts: the
    // rotations exactly, the positions up to the joints' stretch under load, which the
    // translation locks hide in the animation pose.
    for (name, part) in &parts {
        let back = frame.mul(&s.pose[joint(name)]);
        assert!(
            back.pos.distance(part.pos) < 0.02,
            "{name}: {} vs {}",
            back.pos,
            part.pos
        );
        assert!(back.rot.angle_between(part.rot) < 2e-3, "{name}");
    }
    let root = frame.mul(&s.pose[joint("hips")]);
    assert!(
        root.pos.distance(parts[0].1.pos) < 1e-4,
        "the root part is not locked"
    );
    assert_eq!((s.root_pos, s.root_rot), (parts[0].1.pos, parts[0].1.rot));
    // Joints below the root part keep their bone lengths (translations are locked);
    // `root`, above the root part, keeps the target's.
    let s0 = humanoid::skeleton();
    for (j, jt) in s0.joints.iter().enumerate().skip(2) {
        let parent = usize::from(jt.parent.unwrap());
        let want = jt.neutral.pos.distance(s0.joints[parent].neutral.pos);
        let got = s.pose[j].pos.distance(s.pose[parent].pos);
        assert!((got - want).abs() < 0.02, "{}: {got} vs {want}", jt.name);
    }
}

#[test]
fn a_new_strength_rebuilds_the_ragdoll_in_place() {
    let mut p = world(2);
    apply(
        &mut p,
        vec![
            floor(0, 0.0, 0.0, 0.0, 20.0),
            add_man(MAN, Vec3::new(0.0, 0.5, 0.0), Quat::IDENTITY),
        ],
    );
    apply(
        &mut p,
        vec![mode(MAN, RagdollMode::Powered { strength: 1.0 })],
    );
    steps(&mut p, 20);
    let before = p.ragdoll_parts(MAN).unwrap();
    let speed = p.body_count();
    apply(
        &mut p,
        vec![mode(MAN, RagdollMode::Powered { strength: 0.25 })],
    );
    for ((name, a), (_, b)) in p.ragdoll_parts(MAN).unwrap().iter().zip(&before) {
        assert!(
            a.pos.abs_diff_eq(b.pos, 1e-6) && a.rot.abs_diff_eq(b.rot, 1e-6),
            "{name}"
        );
    }
    assert_eq!(p.body_count(), speed, "the old parts are gone");
    assert_eq!(state(&p, MAN).mode, RagdollMode::Powered { strength: 0.25 });
    steps(&mut p, 20);
    // Strength 0 turns the motors off without a rebuild.
    apply(
        &mut p,
        vec![mode(MAN, RagdollMode::Powered { strength: 0.0 })],
    );
    steps(&mut p, 5);
    assert!(p.take_diagnostics().is_empty());
}

#[test]
fn parts_answer_queries_as_the_ragdoll_and_report_no_poses_or_contacts() {
    let mut p = world(2);
    apply(
        &mut p,
        vec![
            floor(0, 0.0, 0.0, 0.0, 20.0),
            add_man(MAN, Vec3::new(0.0, 0.5, 0.0), Quat::IDENTITY),
        ],
    );
    apply(&mut p, vec![mode(MAN, RagdollMode::Simulated)]);
    let mut contacts = 0;
    for _ in 0..60 {
        p.step(DT).unwrap();
        contacts += p
            .events()
            .iter()
            .filter(|e| matches!(e, re_types::physics::ContactEvent::Begin { a, b, .. } if *a == MAN || *b == MAN))
            .count();
    }
    assert_eq!(contacts, 0);
    let mut poses = Vec::new();
    p.poses_into(&mut poses);
    assert!(poses.iter().all(|q| q.id != MAN), "{poses:?}");
    let hips = p.ragdoll_parts(MAN).unwrap()[0].1.pos;
    let hit = p
        .raycast(
            hips + Vec3::Y * 3.0,
            -Vec3::Y,
            10.0,
            &QueryFilter::default(),
        )
        .unwrap()
        .expect("the ray hits the ragdoll");
    assert_eq!(hit.entity, MAN);
    let excluded = QueryFilter {
        groups: re_types::physics::GroupMask::NONE,
        ..QueryFilter::default()
    };
    assert!(
        p.raycast(hips + Vec3::Y * 3.0, -Vec3::Y, 10.0, &excluded)
            .unwrap()
            .is_none()
    );
    apply(&mut p, vec![PhysicsCmd::RemoveRagdoll { id: MAN }]);
    assert!(p.ragdoll(MAN).is_none());
    assert_eq!(p.ragdoll_count(), 0);
    steps(&mut p, 2);
    assert!(
        p.raycast(
            hips + Vec3::Y * 3.0,
            -Vec3::Y,
            10.0,
            &QueryFilter::default()
        )
        .unwrap()
        .is_some_and(|h| h.entity == EntityId(0))
    );
}

fn refused(p: &mut Physics, cmd: PhysicsCmd) -> String {
    let mut cmds = vec![cmd];
    p.apply(&mut cmds);
    let d = p.take_diagnostics();
    assert_eq!(d.len(), 1, "{d:?}");
    format!("{} {}", d[0].code, d[0].message)
}

#[test]
fn malformed_ragdolls_and_commands_are_refused() {
    let mut p = world(1);
    let with = |f: &dyn Fn(&mut re_types::physics::RagdollDesc)| {
        let mut d = humanoid::ragdoll();
        f(&mut d);
        PhysicsCmd::AddRagdoll {
            id: MAN,
            desc: d,
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            pose: None,
        }
    };
    let e = refused(&mut p, with(&|d| d.parts[3].joint = "tail".into()));
    assert!(e.starts_with("RE5019") && e.contains("`tail`"), "{e}");
    let e = refused(&mut p, with(&|d| d.parts[3].joint = "hips".into()));
    assert!(e.contains("two parts"), "{e}");
    let e = refused(
        &mut p,
        with(&|d| {
            d.parts.retain(|p| p.joint != "hips");
        }),
    );
    assert!(e.contains("no part is above"), "{e}");
    let e = refused(&mut p, with(&|d| d.parts[2].mass = 0.0));
    assert!(e.contains("mass"), "{e}");
    let e = refused(&mut p, with(&|d| d.skeleton.joints[4].parent = None));
    assert!(e.contains("second root"), "{e}");
    let e = refused(
        &mut p,
        PhysicsCmd::AddRagdoll {
            id: MAN,
            desc: humanoid::ragdoll(),
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            pose: Some(vec![JointPose::IDENTITY; 3]),
        },
    );
    assert!(e.contains("3 joints"), "{e}");
    assert_eq!(p.ragdoll_count(), 0);
    assert_eq!(p.body_count(), 0, "nothing was created");

    apply(&mut p, vec![add_man(MAN, Vec3::ZERO, Quat::IDENTITY)]);
    let e = refused(&mut p, add_man(MAN, Vec3::ZERO, Quat::IDENTITY));
    assert!(e.starts_with("RE5005"), "{e}");
    let e = refused(
        &mut p,
        add(
            MAN.0,
            body(
                re_types::physics::BodyKind::Dynamic,
                sphere(1.0),
                Vec3::ZERO,
            ),
        ),
    );
    assert!(e.starts_with("RE5005"), "{e}");
    let e = refused(
        &mut p,
        PhysicsCmd::ApplyRagdollImpulse {
            id: MAN,
            joint: "head".into(),
            impulse: Vec3::X,
        },
    );
    assert!(e.contains("animated ragdoll takes no impulses"), "{e}");
    let e = refused(&mut p, mode(MAN, RagdollMode::Powered { strength: 2.0 }));
    assert!(e.starts_with("RE5019"), "{e}");
    let e = refused(&mut p, target(MAN, Vec3::ZERO, vec![JointPose::IDENTITY]));
    assert!(e.contains("1 joints"), "{e}");
    apply(&mut p, vec![mode(MAN, RagdollMode::Simulated)]);
    let e = refused(
        &mut p,
        PhysicsCmd::ApplyRagdollImpulse {
            id: MAN,
            joint: "wing".into(),
            impulse: Vec3::X,
        },
    );
    assert!(e.contains("`wing`"), "{e}");
    // The root joint has no part on it or above it.
    let e = refused(
        &mut p,
        PhysicsCmd::ApplyRagdollImpulse {
            id: MAN,
            joint: "root".into(),
            impulse: Vec3::X,
        },
    );
    assert!(e.contains("`root`"), "{e}");
    let e = refused(&mut p, PhysicsCmd::RemoveRagdoll { id: EntityId(99) });
    assert!(e.starts_with("RE5004"), "{e}");
}

/// Eight ragdolls tumbling onto each other and the floor.
fn tumble(threads: usize, ticks: usize) -> Vec<u32> {
    let mut p = world(threads);
    let mut cmds = vec![floor(0, 0.0, 0.0, 0.0, 20.0)];
    for i in 0..8u64 {
        let id = EntityId(10 + i);
        cmds.push(add_man(
            id,
            Vec3::new(
                (i % 4) as f32 * 0.6,
                0.5 + (i / 4) as f32 * 1.2,
                (i / 4) as f32 * 0.3,
            ),
            Quat::from_rotation_y(i as f32 * 0.9),
        ));
    }
    apply(&mut p, cmds);
    let mut cmds = Vec::new();
    for i in 0..8u64 {
        let id = EntityId(10 + i);
        cmds.push(mode(
            id,
            if i % 2 == 0 {
                RagdollMode::Simulated
            } else {
                RagdollMode::Powered { strength: 0.6 }
            },
        ));
        cmds.push(PhysicsCmd::ApplyRagdollImpulse {
            id,
            joint: "head".into(),
            impulse: Vec3::new(5.0, 0.0, i as f32),
        });
    }
    apply(&mut p, cmds);
    steps(&mut p, ticks);
    let mut bits = Vec::new();
    for i in 0..8u64 {
        for (_, part) in p.ragdoll_parts(EntityId(10 + i)).unwrap() {
            bits.extend(part.pos.to_array().map(f32::to_bits));
            bits.extend(part.rot.to_array().map(f32::to_bits));
        }
    }
    bits
}

#[test]
fn ragdolls_are_deterministic_across_worker_counts() {
    let one = tumble(1, 300);
    assert_eq!(one, tumble(1, 300), "same run twice");
    assert_eq!(one, tumble(4, 300), "1 vs 4 threads");
}

#[test]
fn saved_state_replays_a_ragdoll_bit_for_bit() {
    let mut p = world(2);
    apply(
        &mut p,
        vec![
            floor(0, 0.0, 0.0, 0.0, 20.0),
            add_man(MAN, Vec3::new(0.0, 0.5, 0.0), Quat::IDENTITY),
        ],
    );
    apply(&mut p, vec![mode(MAN, RagdollMode::Simulated)]);
    steps(&mut p, 10);
    let saved = p.save_state();
    steps(&mut p, 40);
    let first = (p.ragdoll_parts(MAN).unwrap(), state(&p, MAN).calm);
    p.restore_state(&saved).unwrap();
    steps(&mut p, 40);
    assert_eq!((p.ragdoll_parts(MAN).unwrap(), state(&p, MAN).calm), first);

    // Motion carries the parts into a fresh copy of the scene.
    let motion = p.motion_state();
    let mut q = world(2);
    apply(
        &mut q,
        vec![
            floor(0, 0.0, 0.0, 0.0, 20.0),
            add_man(MAN, Vec3::new(0.0, 0.5, 0.0), Quat::IDENTITY),
        ],
    );
    apply(&mut q, vec![mode(MAN, RagdollMode::Simulated)]);
    q.apply_motion_state(&motion);
    assert!(q.take_diagnostics().is_empty());
    assert_eq!(q.ragdoll_parts(MAN), p.ragdoll_parts(MAN));

    // A copy that adds the ragdoll after the motion gets it then, with its mode.
    let mut r = world(2);
    apply(&mut r, vec![floor(0, 0.0, 0.0, 0.0, 20.0)]);
    r.apply_motion_state(&motion);
    assert!(r.take_diagnostics().is_empty());
    assert_eq!(r.pending_ragdoll_mode(MAN), Some(RagdollMode::Simulated));
    apply(
        &mut r,
        vec![add_man(MAN, Vec3::new(0.0, 0.5, 0.0), Quat::IDENTITY)],
    );
    assert_eq!(r.pending_ragdoll_mode(MAN), None);
    assert_eq!(state(&r, MAN).mode, RagdollMode::Simulated);
    assert_eq!(r.ragdoll_parts(MAN), p.ragdoll_parts(MAN));
}

#[test]
fn a_character_walks_through_its_own_animated_ragdoll_but_not_anothers() {
    let walk = |own: bool| {
        let mut p = world(2);
        let ragdoll_of = if own { MAN } else { EntityId(6) };
        apply(
            &mut p,
            vec![
                floor(0, 0.0, 0.0, 0.0, 20.0),
                PhysicsCmd::AddCharacter {
                    id: MAN,
                    desc: re_types::physics::CharacterDesc::default(),
                    pos: Vec3::new(0.0, 0.95, -1.0),
                    rot: Quat::IDENTITY,
                },
                add_man(ragdoll_of, Vec3::ZERO, Quat::IDENTITY),
            ],
        );
        for _ in 0..90 {
            apply(
                &mut p,
                vec![PhysicsCmd::MoveCharacter {
                    id: MAN,
                    desired_velocity: Vec3::new(0.0, 0.0, 1.5),
                    jump: false,
                }],
            );
            p.step(DT).unwrap();
        }
        p.character(MAN).unwrap().pos.z
    };
    assert!(walk(true) > 0.3, "the own ragdoll does not block");
    assert!(walk(false) < -0.2, "another ragdoll blocks");
}

#[test]
fn a_restore_brings_back_the_mode_frame_and_target() {
    let neutral = humanoid::skeleton().neutral_pose();
    let start = |p: &mut Physics| {
        apply(
            p,
            vec![
                floor(0, 0.0, 0.0, 0.0, 20.0),
                add_man(MAN, Vec3::new(0.0, 0.5, 0.0), Quat::IDENTITY),
            ],
        );
        apply(
            p,
            vec![target(MAN, Vec3::new(0.0, 0.5, 0.0), neutral.clone())],
        );
        steps(p, 5);
    };
    // Animated: a target sent after the save is forgotten by the restore.
    let mut a = world(2);
    start(&mut a);
    let saved = a.save_state();
    apply(
        &mut a,
        vec![target(MAN, Vec3::new(1.0, 0.5, 0.0), neutral.clone())],
    );
    steps(&mut a, 3);
    a.restore_state(&saved).unwrap();
    steps(&mut a, 10);
    let mut b = world(2);
    start(&mut b);
    steps(&mut b, 10);
    assert_eq!(a.ragdoll_parts(MAN), b.ragdoll_parts(MAN));
    assert_eq!(a.ragdoll_state_bytes(), b.ragdoll_state_bytes());

    // Simulated, then limp-powered (no rebuild): the restore makes it simulated again.
    apply(&mut a, vec![mode(MAN, RagdollMode::Simulated)]);
    steps(&mut a, 2);
    let saved = a.save_state();
    apply(
        &mut a,
        vec![mode(MAN, RagdollMode::Powered { strength: 0.0 })],
    );
    assert_eq!(state(&a, MAN).mode, RagdollMode::Powered { strength: 0.0 });
    a.restore_state(&saved).unwrap();
    assert_eq!(state(&a, MAN).mode, RagdollMode::Simulated);

    // A rebuilt ragdoll cannot go back to before its rebuild.
    apply(
        &mut a,
        vec![mode(MAN, RagdollMode::Powered { strength: 0.5 })],
    );
    assert!(a.restore_state(&saved).is_err());
}

#[test]
fn ragdoll_state_bytes_follow_the_parts() {
    let mut p = world(2);
    apply(&mut p, vec![floor(0, 0.0, 0.0, 0.0, 20.0)]);
    assert!(p.ragdoll_state_bytes().is_empty());
    apply(
        &mut p,
        vec![add_man(MAN, Vec3::new(0.0, 0.5, 0.0), Quat::IDENTITY)],
    );
    apply(&mut p, vec![mode(MAN, RagdollMode::Simulated)]);
    let before = p.ragdoll_state_bytes();
    steps(&mut p, 5);
    assert_ne!(p.ragdoll_state_bytes(), before);
}

/// Where world streaming puts the simulation origin for a player 7 km out (ADR-0042).
const FAR: Vec3 = Vec3::new(7168.0, 0.0, 7168.0);

/// A man pushed over onto a floor around `at`, with the simulation origin at
/// `origin`: his model-space pose and whether he is calm, every 30 steps for 10 s.
fn fall_at(at: Vec3, origin: Vec3) -> (Physics, Vec<(Vec<JointPose>, bool)>) {
    let mut p = world(2);
    p.set_origin(origin).unwrap();
    apply(
        &mut p,
        vec![
            floor(0, at.x, at.z, 0.0, 20.0),
            add_man(MAN, at + Vec3::new(0.0, 0.25, 0.0), Quat::IDENTITY),
        ],
    );
    apply(
        &mut p,
        vec![
            mode(MAN, RagdollMode::Simulated),
            PhysicsCmd::ApplyRagdollImpulse {
                id: MAN,
                joint: "neck".into(),
                impulse: Vec3::new(0.0, 0.0, 30.0),
            },
        ],
    );
    let mut seen = Vec::new();
    for _ in 0..20 {
        steps(&mut p, 30);
        let s = state(&p, MAN);
        seen.push((s.pose, s.calm));
    }
    assert!(p.take_diagnostics().is_empty());
    (p, seen)
}

#[test]
fn a_ragdoll_7_km_out_falls_as_one_at_the_origin_does() {
    let (near, near_seen) = fall_at(Vec3::ZERO, Vec3::ZERO);
    let (far, far_seen) = fall_at(FAR, FAR);
    // Jolt sees the same numbers relative to the origin, so the fall and the pose
    // (relative to the frame) are the same bit for bit.
    assert_eq!(far_seen, near_seen);
    assert!(far_seen.last().unwrap().1, "it comes to rest");
    let lying = state(&far, MAN);
    assert!(lying.root_pos.y < 0.4, "it lies down: {}", lying.root_pos);
    assert!((lying.root_pos - FAR).length() < 3.0, "{}", lying.root_pos);
    // World positions are the same up to their rounding 7 km out.
    let near_parts = near.ragdoll_parts(MAN).unwrap();
    let far_parts = far.ragdoll_parts(MAN).unwrap();
    for ((name, a), (_, b)) in near_parts.iter().zip(&far_parts) {
        assert!((b.pos - FAR).distance(a.pos) < 1e-3, "{name}: {}", b.pos);
        assert_eq!(b.rot, a.rot, "{name}");
    }
    assert_ne!(far.ragdoll_state_bytes(), near.ragdoll_state_bytes());
}

#[test]
fn a_far_ragdoll_replays_and_carries_into_a_copy() {
    let (mut p, _) = fall_at(FAR, FAR);
    apply(
        &mut p,
        vec![PhysicsCmd::ApplyRagdollImpulse {
            id: MAN,
            joint: "head".into(),
            impulse: Vec3::new(20.0, 10.0, 0.0),
        }],
    );
    let saved = p.save_state();
    steps(&mut p, 30);
    let first = (p.ragdoll_parts(MAN).unwrap(), p.ragdoll_state_bytes());
    p.restore_state(&saved).unwrap();
    steps(&mut p, 30);
    assert_eq!(
        (p.ragdoll_parts(MAN).unwrap(), p.ragdoll_state_bytes()),
        first
    );

    // A copy at the same origin takes the parts' world poses.
    let motion = p.motion_state();
    let mut q = world(2);
    q.set_origin(FAR).unwrap();
    apply(
        &mut q,
        vec![
            floor(0, FAR.x, FAR.z, 0.0, 20.0),
            add_man(MAN, FAR + Vec3::new(0.0, 0.25, 0.0), Quat::IDENTITY),
        ],
    );
    q.apply_motion_state(&motion);
    assert!(q.take_diagnostics().is_empty());
    assert_eq!(state(&q, MAN).mode, RagdollMode::Simulated);
    for ((name, a), (_, b)) in p
        .ragdoll_parts(MAN)
        .unwrap()
        .iter()
        .zip(&q.ragdoll_parts(MAN).unwrap())
    {
        assert!(
            a.pos.distance(b.pos) < 1e-3,
            "{name}: {} vs {}",
            a.pos,
            b.pos
        );
    }
}

#[test]
fn ragdolls_keep_falling_and_following_across_a_move_of_the_origin() {
    let mut p = world(2);
    let dancer = EntityId(6);
    let frame = Vec3::new(3.0, 0.0, -2.0);
    let turn = Quat::from_rotation_y(0.7);
    apply(
        &mut p,
        vec![
            floor(0, 0.0, 0.0, 0.0, 20.0),
            add_man(MAN, Vec3::new(0.0, 0.25, 0.0), Quat::IDENTITY),
            add_man(dancer, frame, turn),
        ],
    );
    apply(
        &mut p,
        vec![
            mode(MAN, RagdollMode::Simulated),
            PhysicsCmd::ApplyRagdollImpulse {
                id: MAN,
                joint: "neck".into(),
                impulse: Vec3::new(0.0, 0.0, 30.0),
            },
        ],
    );
    let dance = |p: &mut Physics, pose: &[JointPose], n: usize| {
        for _ in 0..n {
            apply(
                p,
                vec![PhysicsCmd::SetRagdollTarget {
                    id: dancer,
                    pos: frame,
                    rot: turn,
                    pose: pose.to_vec(),
                }],
            );
            p.step(DT).unwrap();
        }
    };
    dance(&mut p, &bent(), 20);
    // Mid-fall, the origin moves 2 km: every part keeps its world pose.
    let before = p.ragdoll_parts(MAN).unwrap();
    p.set_origin(Vec3::new(2048.0, 0.0, 0.0)).unwrap();
    for ((name, a), (_, b)) in before.iter().zip(&p.ragdoll_parts(MAN).unwrap()) {
        assert!(
            a.pos.distance(b.pos) < 1e-3,
            "{name}: {} to {}",
            a.pos,
            b.pos
        );
        assert_eq!(a.rot, b.rot, "{name}");
    }
    // The dancer follows a new target within a centimetre.
    let want = humanoid::skeleton().neutral_pose();
    dance(&mut p, &want, 5);
    for (name, part) in p.ragdoll_parts(dancer).unwrap() {
        let w = JointPose::new(frame, turn).mul(&want[joint(&name)]);
        assert!(part.pos.distance(w.pos) < 0.01, "{name}: {}", part.pos);
    }
    for (g, w) in state(&p, dancer).pose.iter().zip(&want) {
        assert!(g.pos.distance(w.pos) < 0.01);
    }
    // The fallen man comes to rest on the floor.
    let mut calm = false;
    for _ in 0..900 {
        dance(&mut p, &want, 1);
        if state(&p, MAN).calm {
            calm = true;
            break;
        }
    }
    assert!(calm, "the man comes to rest");
    assert!(p.take_diagnostics().is_empty());
    assert!(state(&p, MAN).root_pos.y < 0.4);
    for (name, part) in p.ragdoll_parts(MAN).unwrap() {
        assert!(part.pos.y > -0.05, "{name} sank: {}", part.pos);
    }
}

#[test]
fn a_ragdoll_left_out_of_reach_waits_for_the_origin() {
    let mut p = world(2);
    let at = Vec3::new(0.0, 0.25, 0.0);
    apply(
        &mut p,
        vec![
            floor(0, 0.0, 0.0, 0.0, 20.0),
            add_man(MAN, at, Quat::IDENTITY),
        ],
    );
    // Walking: the parts are moving when the origin leaves.
    for i in 1..=3 {
        apply(
            &mut p,
            vec![target(MAN, at + Vec3::X * (0.02 * i as f32), bent())],
        );
        p.step(DT).unwrap();
    }
    // The player went 7 km away: the man is out of reach of the new origin.
    let before = p.ragdoll_parts(MAN).unwrap();
    p.set_origin(FAR).unwrap();
    let neutral = humanoid::skeleton().neutral_pose();
    for _ in 0..30 {
        p.apply(&mut vec![target(MAN, at, neutral.clone())]);
        p.step(DT).unwrap();
    }
    let d = p.take_diagnostics();
    assert_eq!(d.len(), 1, "reported once: {d:?}");
    assert_eq!(d[0].code, re_physics::codes::OUTSIDE_WORLD);
    assert_eq!(d[0].severity, Severity::Warning);
    assert!(d[0].message.contains("once in reach"), "{}", d[0].message);
    // It stays as it was, and still reports its state.
    for ((name, a), (_, b)) in before.iter().zip(&p.ragdoll_parts(MAN).unwrap()) {
        assert!(
            a.pos.distance(b.pos) < 1e-3,
            "{name}: {} to {}",
            a.pos,
            b.pos
        );
    }
    assert!(state(&p, MAN).pose.iter().all(|j| j.pos.is_finite()));
    // A ragdoll added out of reach is refused, with a warning while the origin is
    // moved: re_app adds it again when the origin moves.
    p.apply(&mut vec![add_man(
        EntityId(6),
        Vec3::new(3.0, 0.0, 0.0),
        Quat::IDENTITY,
    )]);
    let e = p.take_diagnostics();
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(
        (e[0].code, e[0].severity),
        (re_physics::codes::OUTSIDE_WORLD, Severity::Warning)
    );
    assert!(!p.has_ragdoll(EntityId(6)));

    // Back home, it follows its latest target again.
    p.set_origin(Vec3::ZERO).unwrap();
    for _ in 0..5 {
        apply(&mut p, vec![target(MAN, at, neutral.clone())]);
        p.step(DT).unwrap();
    }
    assert!(p.take_diagnostics().is_empty());
    for (name, part) in p.ragdoll_parts(MAN).unwrap() {
        let w = JointPose::new(at, Quat::IDENTITY).mul(&neutral[joint(&name)]);
        assert!(part.pos.distance(w.pos) < 0.01, "{name}: {}", part.pos);
    }
    // At the world origin, out of reach stays an error.
    let e = refused(
        &mut p,
        add_man(EntityId(6), Vec3::new(8000.0, 0.0, 0.0), Quat::IDENTITY),
    );
    assert!(e.starts_with("RE5006"), "{e}");
    let e = refused(&mut p, target(MAN, Vec3::new(8000.0, 0.0, 0.0), neutral));
    assert!(e.starts_with("RE5006"), "{e}");
}
