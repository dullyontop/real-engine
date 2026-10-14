//! Carrying motion into a fresh world (`play.run`'s copy) and group masks on
//! character bodies in queries.

mod common;

use common::*;
use re_physics::{Physics, QueryFilter, codes};
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::{BodyKind, CharacterDesc, GroupMask, PhysicsCmd};

const PLAYER: EntityId = EntityId(100);
const THROWN: u64 = 2;
const RESTING: u64 = 3;
const SPINNER: u64 = 4;

/// The scene as `sync::physics::push` would build it: the same bodies in the same order.
fn scene() -> Vec<PhysicsCmd> {
    vec![
        floor(1, 0.0, 0.0, 0.0, 30.0),
        add(
            THROWN,
            body(BodyKind::Dynamic, cube(0.3), Vec3::new(-5.0, 3.0, 0.0)),
        ),
        add(
            RESTING,
            body(BodyKind::Dynamic, cube(0.4), Vec3::new(5.0, 0.4, 0.0)),
        ),
        add(
            SPINNER,
            body(BodyKind::Kinematic, cube(0.5), Vec3::new(0.0, 4.0, 8.0)),
        ),
        PhysicsCmd::AddCharacter {
            id: PLAYER,
            desc: CharacterDesc::default(),
            pos: Vec3::new(0.0, 0.92, 0.0),
            rot: Quat::IDENTITY,
        },
    ]
}

fn live_world() -> Physics {
    let mut p = world(2);
    apply(&mut p, scene());
    // Let the resting crate fall asleep, then throw a box and jump.
    steps(&mut p, 200);
    assert_eq!(p.is_awake(EntityId(RESTING)), Some(false));
    apply(
        &mut p,
        vec![
            PhysicsCmd::SetVelocity {
                id: EntityId(THROWN),
                linear: Vec3::new(4.0, 6.0, 0.0),
                angular: Vec3::new(0.0, 0.0, 3.0),
            },
            PhysicsCmd::MoveCharacter {
                id: PLAYER,
                desired_velocity: Vec3::new(2.0, 0.0, 0.0),
                jump: true,
            },
        ],
    );
    steps(&mut p, 10);
    p
}

#[test]
fn restore_state_refuses_another_world() {
    let live = live_world();
    let mut fresh = world(2);
    apply(&mut fresh, scene());
    let err = fresh.restore_state(&live.save_state()).unwrap_err();
    assert_eq!(err.code, codes::RESTORE_FAILED);
}

/// A fresh world built from the same scene takes the live world's motion: poses,
/// velocities, sleep and the character's airborne state, and continues as it would.
#[test]
fn motion_state_seeds_a_fresh_world() {
    let mut live = live_world();
    let motion = live.motion_state();
    let ids: Vec<u64> = motion.bodies().iter().map(|m| m.id.0).collect();
    assert_eq!(
        ids,
        [THROWN, RESTING, SPINNER],
        "dynamic and kinematic only"
    );

    let mut fresh = world(3);
    apply(&mut fresh, scene());
    fresh.apply_motion_state(&motion);
    assert!(fresh.take_diagnostics().is_empty());

    for id in [THROWN, RESTING, SPINNER] {
        let e = EntityId(id);
        assert_eq!(fresh.pose(e), live.pose(e), "pose of {id}");
        assert_eq!(fresh.velocity(e), live.velocity(e), "velocity of {id}");
        assert_eq!(fresh.is_awake(e), live.is_awake(e), "sleep of {id}");
    }
    let (lc, fc) = (
        live.character(PLAYER).unwrap(),
        fresh.character(PLAYER).unwrap(),
    );
    assert!(
        !lc.on_ground() && lc.velocity.y > 1.0,
        "the player is mid-jump"
    );
    assert_eq!(fc, lc);

    // Both continue the same way: the thrown box flies on, the crate stays asleep,
    // and the player lands at the same spot.
    steps(&mut live, 60);
    steps(&mut fresh, 60);
    for id in [THROWN, RESTING] {
        let (a, b) = (pos(&live, id), pos(&fresh, id));
        assert!(a.distance(b) < 1e-3, "{id}: live {a} vs fresh {b}");
    }
    assert_eq!(fresh.is_awake(EntityId(RESTING)), Some(false));
    let (lc, fc) = (
        live.character(PLAYER).unwrap(),
        fresh.character(PLAYER).unwrap(),
    );
    assert!(fc.on_ground(), "the copy's player landed");
    assert!(lc.pos.distance(fc.pos) < 1e-3, "{} vs {}", lc.pos, fc.pos);
}

/// Entities the fresh world lacks are skipped with a warning; the rest still apply.
#[test]
fn motion_state_skips_missing_entities() {
    let live = live_world();
    let mut fresh = world(1);
    let mut cmds = scene();
    cmds.retain(|c| !matches!(c, PhysicsCmd::AddBody { id, .. } if id.0 == RESTING));
    apply(&mut fresh, cmds);
    fresh.apply_motion_state(&live.motion_state());
    let diags = fresh.take_diagnostics();
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].code, codes::UNKNOWN_ENTITY);
    assert!(!diags[0].is_error());
    assert_eq!(
        fresh.velocity(EntityId(THROWN)),
        live.velocity(EntityId(THROWN))
    );
}

/// Queries apply a character's groups to its inner body, as to any body.
#[test]
fn queries_filter_characters_by_group() {
    let mut p = world(1);
    let wall = 7;
    let mut wall_desc = body(BodyKind::Static, cube(0.5), Vec3::new(6.0, 1.0, 0.0));
    wall_desc.groups = GroupMask::group(0).unwrap();
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 30.0),
            add(wall, wall_desc),
            PhysicsCmd::AddCharacter {
                id: PLAYER,
                desc: CharacterDesc {
                    groups: GroupMask::group(0).unwrap(),
                    ..CharacterDesc::default()
                },
                pos: Vec3::new(3.0, 0.92, 0.0),
                rot: Quat::IDENTITY,
            },
        ],
    );
    steps(&mut p, 5);
    let ray = |p: &Physics, groups| {
        let filter = QueryFilter {
            groups,
            ..QueryFilter::default()
        };
        p.raycast(Vec3::new(0.0, 1.0, 0.0), Vec3::X, 20.0, &filter)
            .unwrap()
            .map(|h| h.entity)
    };
    assert_eq!(ray(&p, GroupMask::ALL), Some(PLAYER));
    assert_eq!(ray(&p, GroupMask::group(5).unwrap()), None);
    assert_eq!(ray(&p, GroupMask::NONE), None);
    let mut out = Vec::new();
    let only5 = QueryFilter {
        groups: GroupMask::group(5).unwrap(),
        ..QueryFilter::default()
    };
    p.overlap_sphere(Vec3::new(3.0, 1.2, 0.0), 0.8, &only5, &mut out)
        .unwrap();
    assert!(out.is_empty(), "{out:?}");
    p.overlap_sphere(
        Vec3::new(3.0, 1.2, 0.0),
        0.8,
        &QueryFilter::default(),
        &mut out,
    )
    .unwrap();
    assert_eq!(out, [PLAYER]);
    let cast = p
        .shape_cast(
            &sphere(0.2),
            Vec3::new(0.0, 1.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(20.0, 0.0, 0.0),
            &only5,
        )
        .unwrap();
    assert!(cast.is_none(), "{cast:?}");
}
