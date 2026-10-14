//! The physics spike's 12 scenarios, ported to `re_physics`'s public API
//! (research/spikes/physics/code/oxijolt-spike/src/scenarios.rs).

mod common;

use common::*;
use re_physics::{LayerTable, PhysicsConfig, QueryFilter};
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::{
    BodyKind, ConstraintDesc, ConstraintKind, ContactEvent, GroupMask, Layer, PhysicsCmd, ShapeDesc,
};

/// A flat procedural grid mesh `cells` × `cells` of `size` metres at height 0, centred on
/// the origin, wound so the front faces point up.
fn grid_mesh(cells: u32, size: f32) -> ShapeDesc {
    let n = cells + 1;
    let step = size / cells as f32;
    let half = size * 0.5;
    let mut vertices = Vec::new();
    for j in 0..n {
        for i in 0..n {
            vertices.push(Vec3::new(
                i as f32 * step - half,
                0.0,
                j as f32 * step - half,
            ));
        }
    }
    let mut triangles = Vec::new();
    for j in 0..cells {
        for i in 0..cells {
            let a = j * n + i;
            let (b, c) = (a + 1, a + n);
            let d = c + 1;
            triangles.push([a, c, b]);
            triangles.push([b, c, d]);
        }
    }
    ShapeDesc::TriangleMesh {
        vertices,
        triangles,
    }
}

const HF_N: u32 = 33;
const HF_SPACING: f32 = 0.5;
const PLATEAU: f32 = 2.0;

/// Procedural terrain: a plateau at height 2 within 3 m of the centre, sine hills outside.
fn terrain_height(x: f32, z: f32) -> f32 {
    let c = (HF_N - 1) as f32 * HF_SPACING * 0.5;
    let r = ((x - c).powi(2) + (z - c).powi(2)).sqrt();
    if r <= 3.0 {
        PLATEAU
    } else {
        PLATEAU + 0.8 * (x * 0.9).sin() * (z * 0.7).cos()
    }
}

fn terrain() -> (Vec<f32>, ShapeDesc) {
    let mut samples = Vec::new();
    for zi in 0..HF_N {
        for xi in 0..HF_N {
            samples.push(terrain_height(
                xi as f32 * HF_SPACING,
                zi as f32 * HF_SPACING,
            ));
        }
    }
    let shape = ShapeDesc::Heightfield {
        n: HF_N,
        samples: samples.clone(),
        spacing: HF_SPACING,
        offset: Vec3::ZERO,
    };
    (samples, shape)
}

/// A hexagonal prism (radius 0.5, 0.5 high) centred on the origin.
fn hex_prism() -> ShapeDesc {
    let mut points = Vec::new();
    for k in 0..6 {
        let a = k as f32 * std::f32::consts::TAU / 6.0;
        for y in [-0.25f32, 0.25] {
            points.push(Vec3::new(0.5 * a.cos(), y, 0.5 * a.sin()));
        }
    }
    ShapeDesc::ConvexHull { points }
}

/// Sphere, box, lying capsule and convex hull dropped on a box floor, a triangle mesh and
/// a heightfield come to rest at their geometric heights (within the slop) and sleep.
#[test]
fn drop_and_rest() {
    let mut p = world(3);
    let (_, hf) = terrain();
    let hf_origin = Vec3::new(60.0, 0.0, -8.0);
    let centre = (HF_N - 1) as f32 * HF_SPACING * 0.5;
    let (hx, hz) = (hf_origin.x + centre, hf_origin.z + centre);
    let mut cmds = vec![
        floor(1, 0.0, 0.0, 0.0, 10.0),
        add(
            2,
            body(
                BodyKind::Static,
                grid_mesh(16, 12.0),
                Vec3::new(30.0, 1.0, 0.0),
            ),
        ),
        add(3, body(BodyKind::Static, hf, hf_origin)),
    ];
    let capsule = ShapeDesc::Capsule {
        half_height: 0.5,
        radius: 0.3,
    };
    let lying = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
    // (name, shape, rotation, x, z, ground top, rest height above the ground)
    let drops = [
        (
            "floor/sphere",
            sphere(0.5),
            Quat::IDENTITY,
            -6.0,
            0.0,
            0.0,
            0.5,
        ),
        ("floor/box", cube(0.5), Quat::IDENTITY, -3.0, 0.0, 0.0, 0.5),
        ("floor/capsule", capsule.clone(), lying, 0.0, 0.0, 0.0, 0.3),
        (
            "floor/hull",
            hex_prism(),
            Quat::IDENTITY,
            3.0,
            0.0,
            0.0,
            0.25,
        ),
        (
            "mesh/sphere",
            sphere(0.5),
            Quat::IDENTITY,
            27.0,
            0.0,
            1.0,
            0.5,
        ),
        ("mesh/box", cube(0.5), Quat::IDENTITY, 30.0, 0.0, 1.0, 0.5),
        ("mesh/capsule", capsule.clone(), lying, 33.0, 0.0, 1.0, 0.3),
        (
            "mesh/hull",
            hex_prism(),
            Quat::IDENTITY,
            30.0,
            3.0,
            1.0,
            0.25,
        ),
        (
            "heightfield/box",
            cube(0.5),
            Quat::IDENTITY,
            hx,
            hz,
            PLATEAU,
            0.5,
        ),
        (
            "heightfield/hull",
            hex_prism(),
            Quat::IDENTITY,
            hx + 1.5,
            hz,
            PLATEAU,
            0.25,
        ),
        (
            "heightfield/capsule",
            capsule,
            lying,
            hx - 1.5,
            hz,
            PLATEAU,
            0.3,
        ),
    ];
    for (i, (_, shape, rot, x, z, top, _)) in drops.iter().enumerate() {
        let mut desc = body(
            BodyKind::Dynamic,
            shape.clone(),
            Vec3::new(*x, top + 3.0, *z),
        );
        desc.rot = *rot;
        cmds.push(add(100 + i as u64, desc));
    }
    apply(&mut p, cmds);
    steps(&mut p, 360);
    for (i, (name, _, _, _, _, top, rest)) in drops.iter().enumerate() {
        let id = 100 + i as u64;
        assert_rests(name, pos(&p, id).y, top + rest);
        assert_eq!(
            p.is_awake(EntityId(id)),
            Some(false),
            "{name} is still awake"
        );
    }
}

/// Rays straight down onto heightfield samples land on the sample heights within the
/// spike's tolerance of 0.02 m (Jolt quantises heights to 8 bits per sample).
#[test]
fn heightfield_matches_samples() {
    let mut p = world(1);
    let (samples, hf) = terrain();
    let origin = Vec3::new(-5.0, 0.0, 7.0);
    apply(&mut p, vec![add(1, body(BodyKind::Static, hf, origin))]);
    let mut worst = 0f32;
    let mut rays = 0;
    for zi in (0..HF_N).step_by(3) {
        for xi in (0..HF_N).step_by(3) {
            let want = samples[(zi * HF_N + xi) as usize];
            let at = origin + Vec3::new(xi as f32 * HF_SPACING, 50.0, zi as f32 * HF_SPACING);
            let hit = p
                .raycast(at, Vec3::NEG_Y, 100.0, &QueryFilter::default())
                .unwrap()
                .unwrap_or_else(|| panic!("ray at {at} missed"));
            assert_eq!(hit.entity, EntityId(1));
            worst = worst.max((hit.point.y - want).abs());
            rays += 1;
        }
    }
    assert_eq!(rays, 121);
    assert!(worst < 0.02, "heightfield ray error {worst} m");
}

/// Ray casts and shape casts hit the right entity at the right place; group masks,
/// exclusion and sensors are filtered; a miss is `None`.
#[test]
fn queries() {
    let mut p = world(1);
    let mut crate_ = body(BodyKind::Kinematic, cube(0.5), Vec3::new(2.0, 0.5, 0.0));
    crate_.groups = GroupMask::group(1).unwrap();
    let mut trigger = body(BodyKind::Static, cube(1.0), Vec3::new(2.0, 4.0, 0.0));
    trigger.sensor = true;
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 10.0),
            add(2, crate_),
            add(3, trigger),
        ],
    );
    let all = QueryFilter::default();
    let origin = Vec3::new(2.0, 10.0, 0.0);
    let hit = p.raycast(origin, Vec3::NEG_Y, 20.0, &all).unwrap().unwrap();
    assert_eq!(
        hit.entity,
        EntityId(2),
        "the ray passes the sensor and hits the crate"
    );
    assert_near("ray hit y", hit.point.y, 1.0, 1e-4);
    assert_near("ray normal y", hit.normal.y, 1.0, 1e-4);
    assert_near("ray distance", hit.distance, 9.0, 1e-4);

    let with_sensors = QueryFilter {
        sensors: true,
        ..all
    };
    let hit = p
        .raycast(origin, Vec3::NEG_Y, 20.0, &with_sensors)
        .unwrap()
        .unwrap();
    assert_eq!(hit.entity, EntityId(3));

    let group0 = QueryFilter {
        groups: GroupMask::group(0).unwrap(),
        ..all
    };
    let hit = p
        .raycast(origin, Vec3::NEG_Y, 20.0, &group0)
        .unwrap()
        .unwrap();
    assert_eq!(hit.entity, EntityId(1), "group filter skips the crate");
    assert_near("filtered ray hit y", hit.point.y, 0.0, 1e-4);

    let hit = p
        .raycast(
            origin,
            Vec3::NEG_Y,
            20.0,
            &QueryFilter::excluding(EntityId(2)),
        )
        .unwrap()
        .unwrap();
    assert_eq!(hit.entity, EntityId(1), "exclusion skips the crate");

    let sc = p
        .shape_cast(
            &sphere(0.25),
            Vec3::new(2.0, 2.5, 0.0),
            Quat::IDENTITY,
            Vec3::new(0.0, -2.0, 0.0),
            &all,
        )
        .unwrap()
        .unwrap();
    assert_eq!(sc.entity, EntityId(2));
    assert_near("shape cast travel", sc.distance, 1.25, 1e-3);
    assert_near("shape cast contact y", sc.point.y, 1.0, 1e-3);
    assert!(sc.normal.y > 0.99, "normal {}", sc.normal);

    let mut found = Vec::new();
    p.overlap_sphere(Vec3::new(2.0, 1.0, 0.0), 0.3, &all, &mut found)
        .unwrap();
    assert_eq!(found, vec![EntityId(2)]);
    p.overlap_sphere(Vec3::new(2.0, 1.0, 0.0), 0.3, &group0, &mut found)
        .unwrap();
    assert!(found.is_empty());

    let miss = p
        .raycast(Vec3::new(50.0, 10.0, 0.0), Vec3::NEG_Y, 20.0, &all)
        .unwrap();
    assert!(miss.is_none());
    assert!(p.raycast(origin, Vec3::ZERO, 1.0, &all).is_err());
}

/// Contact events name our entities: Begin on landing (normal from the smaller entity),
/// End when the pair falls asleep, Begin again on waking, and End naming a removed body.
#[test]
fn contact_events() {
    let mut p = world(2);
    let (ground, ball) = (EntityId(10), EntityId(11));
    apply(
        &mut p,
        vec![
            floor(10, 0.0, 0.0, 0.0, 10.0),
            add(
                11,
                body(BodyKind::Dynamic, sphere(0.5), Vec3::new(0.0, 2.0, 0.0)),
            ),
        ],
    );
    let mut begin = None;
    let mut slept_end = None;
    for s in 1..=120 {
        p.step(DT).unwrap();
        for e in p.events() {
            match *e {
                ContactEvent::Begin {
                    a,
                    b,
                    normal,
                    sensor,
                    ..
                } if begin.is_none() => {
                    assert_eq!((a, b), (ground, ball));
                    assert!(!sensor);
                    assert!(
                        normal.y > 0.99,
                        "normal points from the floor to the ball: {normal}"
                    );
                    begin = Some(s);
                }
                ContactEvent::End { a, b, .. } if slept_end.is_none() => {
                    assert_eq!((a, b), (ground, ball));
                    slept_end = Some(s);
                }
                _ => {}
            }
        }
    }
    let begin = begin.expect("no Begin when the ball landed");
    let slept_end = slept_end.expect("no End when the pair fell asleep");
    assert!(slept_end > begin);
    assert_eq!(p.is_awake(ball), Some(false));

    // Waking the ball brings the contact back.
    apply(
        &mut p,
        vec![PhysicsCmd::ApplyImpulse {
            id: ball,
            impulse: Vec3::new(0.0, 0.01, 0.0),
        }],
    );
    let mut rebegun = false;
    for _ in 0..3 {
        p.step(DT).unwrap();
        rebegun |= p
            .events()
            .iter()
            .any(|e| matches!(e, ContactEvent::Begin { .. }));
    }
    assert!(rebegun, "waking the ball did not begin its contact again");

    // Removing the ball ends its contact in the next step's events, and nothing more
    // is reported about it afterwards.
    apply(&mut p, vec![PhysicsCmd::RemoveBody { id: ball }]);
    p.step(DT).unwrap();
    assert_eq!(
        p.events(),
        &[ContactEvent::End {
            a: ground,
            b: ball,
            sensor: false
        }]
    );
    steps(&mut p, 5);
    assert!(p.events().is_empty());
    assert!(p.pose(ball).is_none());
}

/// Velocities, teleports and removal.
#[test]
fn transforms_and_removal() {
    let config = PhysicsConfig {
        gravity: Vec3::ZERO,
        ..PhysicsConfig::default()
    };
    let mut p = world_with(config, 1);
    let mut desc = body(BodyKind::Dynamic, cube(0.5), Vec3::ZERO);
    desc.linear_damping = 0.0;
    desc.angular_damping = 0.0;
    apply(
        &mut p,
        vec![
            add(1, desc),
            PhysicsCmd::SetVelocity {
                id: EntityId(1),
                linear: Vec3::new(3.0, 0.0, 0.0),
                angular: Vec3::new(0.0, std::f32::consts::PI, 0.0),
            },
        ],
    );
    steps(&mut p, 60);
    assert_near("x after 1 s at 3 m/s", pos(&p, 1).x, 3.0, 1e-3);
    let (v, _) = p.velocity(EntityId(1)).unwrap();
    assert_near("vx kept", v.x, 3.0, 1e-4);
    assert_near("half a turn about y", rot(&p, 1).y.abs(), 1.0, 1e-3);

    let target = Quat::from_rotation_x(0.5);
    apply(
        &mut p,
        vec![PhysicsCmd::SetTransform {
            id: EntityId(1),
            pos: Vec3::new(-4.0, 2.0, 1.0),
            rot: target,
        }],
    );
    assert_near("teleported x", pos(&p, 1).x, -4.0, 1e-6);
    assert_near("teleported rotation", rot(&p, 1).x, target.x, 1e-6);

    let before = p.body_count();
    apply(&mut p, vec![PhysicsCmd::RemoveBody { id: EntityId(1) }]);
    assert_eq!(p.body_count(), before - 1);
    assert!(p.pose(EntityId(1)).is_none());
    assert!(p.velocity(EntityId(1)).is_none());
    let mut again = vec![PhysicsCmd::RemoveBody { id: EntityId(1) }];
    p.apply(&mut again);
    assert!(again.is_empty(), "apply leaves the buffer empty");
    let diags = p.take_diagnostics();
    assert_eq!(diags.len(), 1);
    assert_eq!(diags[0].code, re_physics::codes::UNKNOWN_ENTITY);
}

/// The layer table acts in the solver: a DEBRIS sphere falls through a moving box onto
/// the floor while a default sphere rests on it, and two layers set not to collide pass
/// through each other.
#[test]
fn layer_filters() {
    let mut layers = LayerTable::standard();
    layers.set_collides(Layer(2), Layer(3), false);
    let config = PhysicsConfig {
        layers,
        ..PhysicsConfig::default()
    };
    let mut p = world_with(config, 1);
    let mut debris = body(BodyKind::Dynamic, sphere(0.25), Vec3::new(0.0, 4.0, 0.0));
    debris.layer = LayerTable::DEBRIS;
    let mut low = body(BodyKind::Dynamic, cube(0.4), Vec3::new(5.0, 0.4, 0.0));
    low.layer = Layer(2);
    let mut high = body(BodyKind::Dynamic, cube(0.4), Vec3::new(5.0, 3.0, 0.0));
    high.layer = Layer(3);
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 10.0),
            add(
                2,
                body(BodyKind::Dynamic, cube(1.0), Vec3::new(0.0, 1.0, 0.0)),
            ),
            add(3, debris),
            add(
                4,
                body(BodyKind::Dynamic, sphere(0.25), Vec3::new(0.3, 4.0, 0.3)),
            ),
            add(5, low),
            add(6, high),
        ],
    );
    steps(&mut p, 240);
    assert_rests(
        "debris sphere on the floor, through the box",
        pos(&p, 3).y,
        0.25,
    );
    assert!(
        pos(&p, 4).y > 2.0,
        "default sphere rests on the box: {}",
        pos(&p, 4).y
    );
    assert_rests(
        "layer-3 box falls through the layer-2 box",
        pos(&p, 6).y,
        0.4,
    );

    let mut bad = body(BodyKind::Dynamic, cube(0.4), Vec3::ZERO);
    bad.layer = Layer(40);
    let mut cmds = vec![add(7, bad)];
    p.apply(&mut cmds);
    assert_eq!(
        p.take_diagnostics()[0].code,
        re_physics::codes::UNKNOWN_LAYER
    );
}

/// A sensor reports Begin and End (flagged as sensor contacts) and does not push; a
/// kinematic platform carries a box, and stops dead when its targets stop.
#[test]
fn sensors_and_kinematic() {
    let mut p = world(1);
    let mut trigger = body(BodyKind::Static, cube(1.0), Vec3::new(0.0, 3.0, 0.0));
    trigger.sensor = true;
    let platform = body(
        BodyKind::Kinematic,
        ShapeDesc::Box {
            half_extents: Vec3::new(1.5, 0.1, 1.5),
        },
        Vec3::new(10.0, 1.0, 0.0),
    );
    let mut rider = body(BodyKind::Dynamic, cube(0.3), Vec3::new(10.0, 1.45, 0.0));
    rider.friction = 1.0;
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 30.0),
            add(2, trigger),
            add(
                3,
                body(BodyKind::Dynamic, sphere(0.25), Vec3::new(0.0, 6.0, 0.0)),
            ),
            add(4, platform),
            add(5, rider),
        ],
    );
    let (mut entered, mut exited) = (None, None);
    for s in 1..=240usize {
        if s <= 120 {
            let x = 10.0 + 2.0 * (s as f32 / 120.0);
            apply(
                &mut p,
                vec![PhysicsCmd::SetKinematicTarget {
                    id: EntityId(4),
                    pos: Vec3::new(x, 1.0, 0.0),
                    rot: Quat::IDENTITY,
                }],
            );
        }
        p.step(DT).unwrap();
        for e in p.events() {
            match *e {
                ContactEvent::Begin { a, b, sensor, .. }
                    if (a, b) == (EntityId(2), EntityId(3)) =>
                {
                    assert!(sensor);
                    entered.get_or_insert(s);
                }
                ContactEvent::End { a, b, sensor } if (a, b) == (EntityId(2), EntityId(3)) => {
                    assert!(sensor);
                    exited.get_or_insert(s);
                }
                _ => {}
            }
        }
    }
    let (entered, exited) = (
        entered.expect("sensor saw no enter"),
        exited.expect("no exit"),
    );
    assert!(exited > entered);
    assert_rests("ball fell through the sensor", pos(&p, 3).y, 0.25);
    // 120 steps after its last target the platform has not drifted on.
    assert_near(
        "platform stopped at its last target",
        pos(&p, 4).x,
        12.0,
        1e-3,
    );
    assert_near("rider carried by the platform", pos(&p, 5).x, 12.0, 0.05);
}

/// A character walks 2 m over a floor in 1 s at 2 m/s and stands on it.
#[test]
fn character() {
    let mut p = world(1);
    let id = EntityId(2);
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 30.0),
            PhysicsCmd::AddCharacter {
                id,
                desc: Default::default(),
                pos: Vec3::new(0.0, 0.92, 0.0),
                rot: Quat::IDENTITY,
            },
        ],
    );
    for _ in 0..60 {
        apply(
            &mut p,
            vec![PhysicsCmd::MoveCharacter {
                id,
                desired_velocity: Vec3::new(2.0, 0.0, 0.0),
                jump: false,
            }],
        );
        p.step(DT).unwrap();
    }
    let c = p.character(id).unwrap();
    assert_near("character x after 1 s at 2 m/s", c.pos.x, 2.0, 0.05);
    assert!(c.on_ground(), "ground state {:?}", c.ground);
    assert_eq!(c.ground_entity, Some(EntityId(1)));
    assert_near("capsule centre above the floor", c.pos.y, 0.9, 0.05);
}

/// Constraints: a distance-constraint pendulum keeps its length, a weld holds and a
/// hinge with limits keeps its pivot.
#[test]
fn constraints() {
    let mut p = world(1);
    let anchor = body(BodyKind::Static, cube(0.1), Vec3::new(0.0, 5.0, 0.0));
    apply(
        &mut p,
        vec![
            add(1, anchor),
            add(
                2,
                body(BodyKind::Dynamic, sphere(0.2), Vec3::new(2.0, 5.0, 0.0)),
            ),
            PhysicsCmd::AddConstraint {
                id: EntityId(3),
                desc: ConstraintDesc {
                    a: EntityId(1),
                    b: EntityId(2),
                    kind: ConstraintKind::Distance {
                        point_a: Vec3::new(0.0, 5.0, 0.0),
                        point_b: Vec3::new(2.0, 5.0, 0.0),
                        min: 2.0,
                        max: 2.0,
                    },
                },
            },
            // A weld between two boxes, hanging from the anchor by a hinge.
            add(
                4,
                body(BodyKind::Static, cube(0.1), Vec3::new(10.0, 5.0, 0.0)),
            ),
            add(
                5,
                body(BodyKind::Dynamic, cube(0.2), Vec3::new(11.0, 5.0, 0.0)),
            ),
            add(
                6,
                body(BodyKind::Dynamic, cube(0.2), Vec3::new(11.5, 5.0, 0.0)),
            ),
            PhysicsCmd::AddConstraint {
                id: EntityId(7),
                desc: ConstraintDesc {
                    a: EntityId(4),
                    b: EntityId(5),
                    kind: ConstraintKind::Hinge {
                        pivot: Vec3::new(10.0, 5.0, 0.0),
                        axis: Vec3::Z,
                        limits: None,
                    },
                },
            },
            PhysicsCmd::AddConstraint {
                id: EntityId(8),
                desc: ConstraintDesc {
                    a: EntityId(5),
                    b: EntityId(6),
                    kind: ConstraintKind::Fixed,
                },
            },
        ],
    );
    let mut worst = 0f32;
    let mut lowest = f32::MAX;
    let mut weld = 0f32;
    let mut pivot = 0f32;
    for _ in 0..180 {
        p.step(DT).unwrap();
        let b = pos(&p, 2);
        worst = worst.max(((b - Vec3::new(0.0, 5.0, 0.0)).length() - 2.0).abs());
        lowest = lowest.min(b.y);
        weld = weld.max(((pos(&p, 6) - pos(&p, 5)).length() - 0.5).abs());
        pivot = pivot.max(((pos(&p, 5) - Vec3::new(10.0, 5.0, 0.0)).length() - 1.0).abs());
    }
    assert!(worst < 0.01, "pendulum length drifted by {worst} m");
    assert_near("pendulum swings to the bottom", lowest, 3.0, 0.02);
    assert!(weld < 0.01, "weld stretched by {weld} m");
    assert!(pivot < 0.01, "hinge pivot drifted by {pivot} m");

    // Removing a welded body removes the weld too, with a warning.
    let mut cmds = vec![PhysicsCmd::RemoveBody { id: EntityId(6) }];
    p.apply(&mut cmds);
    let diags = p.take_diagnostics();
    assert_eq!(diags.len(), 1);
    assert_eq!(
        diags[0].code,
        re_physics::codes::CONSTRAINT_REMOVED_WITH_BODY
    );
}

/// The same 300-body pile stepped with 1 and 3 workers gives bit-identical positions,
/// and a save/restore replay is bit-identical.
#[test]
fn determinism_and_snapshots() {
    let run = |threads: usize| {
        let mut p = world(threads);
        apply(&mut p, pile(300));
        steps(&mut p, 120);
        position_bits(&p, 1..=300)
    };
    assert!(run(1) == run(3), "1-thread and 3-thread runs differ");

    let mut p = world(2);
    apply(&mut p, pile(300));
    steps(&mut p, 30);
    let snap = p.save_state();
    steps(&mut p, 60);
    let after = position_bits(&p, 1..=300);
    let events_after: Vec<ContactEvent> = p.events().to_vec();
    p.restore_state(&snap).unwrap();
    steps(&mut p, 60);
    assert!(
        after == position_bits(&p, 1..=300),
        "replay after restore differs"
    );
    assert_eq!(events_after, p.events());

    // A snapshot does not restore across a structural change.
    apply(&mut p, vec![PhysicsCmd::RemoveBody { id: EntityId(5) }]);
    let err = p.restore_state(&snap).unwrap_err();
    assert_eq!(err.code, re_physics::codes::RESTORE_FAILED);
}

/// A body 4 km out rests correctly; one beyond the world limit is refused with RE5006,
/// and meshes on moving bodies with RE5003, never with a panic.
#[test]
fn large_world() {
    let mut p = world(1);
    let far = 4000.0;
    apply(
        &mut p,
        vec![
            floor(1, far, far, 0.0, 10.0),
            add(
                2,
                body(
                    BodyKind::Dynamic,
                    sphere(0.5),
                    Vec3::new(far + 0.3, 3.0, far - 0.2),
                ),
            ),
        ],
    );
    steps(&mut p, 180);
    assert_rests("sphere 4 km out", pos(&p, 2).y, 0.5);

    let (_, hf) = terrain();
    let mut cmds = vec![
        add(
            3,
            body(
                BodyKind::Dynamic,
                sphere(0.5),
                Vec3::new(10_000.0, 0.0, 0.0),
            ),
        ),
        add(4, body(BodyKind::Dynamic, grid_mesh(2, 1.0), Vec3::ZERO)),
        add(5, body(BodyKind::Kinematic, hf, Vec3::ZERO)),
    ];
    p.apply(&mut cmds);
    let codes: Vec<_> = p.take_diagnostics().iter().map(|d| d.code).collect();
    assert_eq!(
        codes,
        [
            re_physics::codes::OUTSIDE_WORLD,
            re_physics::codes::STATIC_ONLY_SHAPE,
            re_physics::codes::STATIC_ONLY_SHAPE
        ]
    );
}
