//! The character controller: slopes, walls, sensors and pushing bodies.

mod common;

use common::*;
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::{BodyKind, CharacterDesc, ContactEvent, PhysicsCmd, ShapeDesc};

const PLAYER: EntityId = EntityId(100);

fn spawn(at: Vec3) -> PhysicsCmd {
    PhysicsCmd::AddCharacter {
        id: PLAYER,
        desc: CharacterDesc::default(),
        pos: at,
        rot: Quat::IDENTITY,
    }
}

fn walk(p: &mut re_physics::Physics, velocity: Vec3) {
    apply(
        p,
        vec![PhysicsCmd::MoveCharacter {
            id: PLAYER,
            desired_velocity: velocity,
            jump: false,
        }],
    );
    p.step(DT).unwrap();
}

fn slab(half: Vec3, centre: Vec3, rot: Quat) -> re_types::physics::BodyDesc {
    let mut desc = body(
        BodyKind::Static,
        ShapeDesc::Box { half_extents: half },
        centre,
    );
    desc.rot = rot;
    desc
}

/// A 30° ramp rising along +X from y = 0 to y = 3 onto a platform, with a wall at
/// x = 8 on the platform. The character walks up the ramp and stops at the wall.
#[test]
fn walks_up_a_30_degree_slope_and_stops_at_a_wall() {
    let mut p = world(2);
    let angle = 30f32.to_radians();
    let rot = Quat::from_rotation_z(angle);
    let length = 6.0;
    let top_mid = Vec3::new(length * 0.5 * angle.cos(), length * 0.5 * angle.sin(), 0.0);
    let ramp_centre = top_mid - rot * Vec3::new(0.0, 0.25, 0.0);
    let top_x = length * angle.cos();
    apply(
        &mut p,
        vec![
            floor(1, -10.0, 0.0, 0.0, 10.0),
            add(
                2,
                slab(Vec3::new(length * 0.5, 0.25, 3.0), ramp_centre, rot),
            ),
            add(
                3,
                slab(
                    Vec3::new(2.0, 0.5, 3.0),
                    Vec3::new(top_x + 1.95, 2.5, 0.0),
                    Quat::IDENTITY,
                ),
            ),
            add(
                4,
                slab(
                    Vec3::new(0.5, 1.5, 3.0),
                    Vec3::new(8.5, 4.5, 0.0),
                    Quat::IDENTITY,
                ),
            ),
            spawn(Vec3::new(-2.0, 0.92, 0.0)),
        ],
    );
    let mut on_ramp = false;
    for _ in 0..420 {
        walk(&mut p, Vec3::new(3.0, 0.0, 0.0));
        let c = p.character(PLAYER).unwrap();
        if c.pos.x > 1.0 && c.pos.x < top_x - 1.0 {
            assert!(
                c.on_ground(),
                "slid or fell on the ramp: {:?} at {}",
                c.ground,
                c.pos
            );
            on_ramp = true;
        }
    }
    assert!(on_ramp, "never reached the ramp");
    let c = p.character(PLAYER).unwrap();
    assert_near("standing on the platform", c.pos.y, 3.0 + 0.9, 0.06);
    assert!(
        c.pos.x <= 8.0 - 0.4 + 0.03 && c.pos.x > 7.3,
        "stopped at the wall face x = 8: centre x {}",
        c.pos.x
    );
    let (v, _) = p.velocity(PLAYER).unwrap();
    assert!(v.x.abs() < 0.1, "still moving into the wall at {} m/s", v.x);
    assert_eq!(c.ground_entity, Some(EntityId(3)));
}

/// With its inner body, a character walking into a sensor produces a `ContactEvent`
/// that names the sensor and the character.
#[test]
fn walking_into_a_sensor_reports_a_contact_event() {
    let mut p = world(1);
    let sensor = EntityId(5);
    let mut trigger = body(
        BodyKind::Static,
        ShapeDesc::Box {
            half_extents: Vec3::new(0.5, 1.0, 1.0),
        },
        Vec3::new(3.0, 1.0, 0.0),
    );
    trigger.sensor = true;
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 20.0),
            add(5, trigger),
            spawn(Vec3::new(0.0, 0.92, 0.0)),
        ],
    );
    let mut entered = None;
    for s in 0..120 {
        walk(&mut p, Vec3::new(2.0, 0.0, 0.0));
        for e in p.events() {
            if let ContactEvent::Begin {
                a,
                b,
                sensor: flagged,
                ..
            } = *e
                && (a, b) == (sensor, PLAYER)
            {
                assert!(flagged);
                entered.get_or_insert(s);
            }
        }
    }
    let entered = entered.expect("no ContactEvent naming the sensor and the character");
    assert!(entered > 10, "entered too early: step {entered}");
    // The sensor does not block: the character walked on through it.
    let x = p.character(PLAYER).unwrap().pos.x;
    assert!(x > 3.6, "blocked by the sensor at x = {x}");
}

/// A character walking into a 20 kg crate for 2 s pushes it at least 0.5 m.
#[test]
fn walking_into_a_crate_pushes_it() {
    let mut p = world(2);
    let mut crate_ = body(BodyKind::Dynamic, cube(0.4), Vec3::new(1.5, 0.4, 0.0));
    crate_.mass = 20.0;
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 20.0),
            add(2, crate_),
            spawn(Vec3::new(0.0, 0.92, 0.0)),
        ],
    );
    steps(&mut p, 30);
    let start = pos(&p, 2).x;
    for _ in 0..120 {
        walk(&mut p, Vec3::new(2.0, 0.0, 0.0));
    }
    let moved = pos(&p, 2).x - start;
    assert!(moved >= 0.5, "the crate moved only {moved} m");
}

/// A character jumps from the ground and lands again.
#[test]
fn jumps_and_lands() {
    let mut p = world(1);
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 20.0),
            spawn(Vec3::new(0.0, 0.92, 0.0)),
        ],
    );
    steps(&mut p, 10);
    apply(
        &mut p,
        vec![PhysicsCmd::MoveCharacter {
            id: PLAYER,
            desired_velocity: Vec3::ZERO,
            jump: true,
        }],
    );
    let mut highest = 0f32;
    for _ in 0..120 {
        p.step(DT).unwrap();
        highest = highest.max(p.character(PLAYER).unwrap().pos.y);
    }
    // v² / 2g with the default 5 m/s jump: about 1.27 m.
    assert!(highest > 0.9 + 1.0, "jumped only to {highest}");
    assert!(p.character(PLAYER).unwrap().on_ground());
}

/// `SetVelocity` on a character sets its velocity: an upward speed makes it
/// leave the ground and rise about v² / 2g, whatever the configured jump speed.
#[test]
fn set_velocity_launches_a_character() {
    let mut p = world(1);
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 20.0),
            spawn(Vec3::new(0.0, 0.92, 0.0)),
        ],
    );
    steps(&mut p, 10);
    let start = p.character(PLAYER).unwrap().pos.y;
    apply(
        &mut p,
        vec![PhysicsCmd::SetVelocity {
            id: PLAYER,
            linear: Vec3::new(0.0, 8.0, 0.0),
            angular: Vec3::ZERO,
        }],
    );
    let mut highest = start;
    for _ in 0..180 {
        walk(&mut p, Vec3::ZERO);
        highest = highest.max(p.character(PLAYER).unwrap().pos.y);
    }
    let rise = highest - start;
    let want = 8.0 * 8.0 / (2.0 * 9.81);
    assert!(
        (rise - want).abs() < want * 0.1,
        "rose {rise}, expected about {want}"
    );
    assert!(p.character(PLAYER).unwrap().on_ground());
    // A non-finite velocity is refused.
    let mut bad = vec![PhysicsCmd::SetVelocity {
        id: PLAYER,
        linear: Vec3::new(f32::NAN, 0.0, 0.0),
        angular: Vec3::ZERO,
    }];
    p.apply(&mut bad);
    assert!(
        p.take_diagnostics()
            .iter()
            .any(|d| d.message.contains("not finite")),
    );
}
