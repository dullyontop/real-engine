//! Queries that start inside bodies they leave out, bodies that keep their motion when
//! replaced, kinematic meshes, and sensors with bodies that fall asleep inside them.

mod common;

use common::*;
use re_physics::{Physics, QueryFilter};
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::{
    BodyDesc, BodyKind, CharacterDesc, ContactEvent, GroupMask, PhysicsCmd, RayHit, ShapeDesc,
};

/// A square of two triangles, `half` metres from its centre to each side, at height 0,
/// wound so the front faces point up.
fn deck(half: f32) -> ShapeDesc {
    ShapeDesc::TriangleMesh {
        vertices: vec![
            Vec3::new(-half, 0.0, -half),
            Vec3::new(half, 0.0, -half),
            Vec3::new(-half, 0.0, half),
            Vec3::new(half, 0.0, half),
        ],
        triangles: vec![[0, 2, 1], [1, 2, 3]],
    }
}

fn sensor(mut desc: BodyDesc) -> BodyDesc {
    desc.sensor = true;
    desc
}

/// In group 1, which [`visible`] leaves out.
fn hidden(mut desc: BodyDesc) -> BodyDesc {
    desc.groups = GroupMask::group(1).unwrap();
    desc
}

/// Sees group 0 (the default group) only.
fn visible() -> QueryFilter {
    QueryFilter {
        groups: GroupMask::group(0).unwrap(),
        ..QueryFilter::default()
    }
}

#[track_caller]
fn assert_same_hit(what: &str, got: Option<RayHit>, want: Option<RayHit>) {
    let (Some(got), Some(want)) = (got, want) else {
        assert_eq!(got, want, "{what}");
        return;
    };
    assert_eq!(got.entity, want.entity, "{what}: {got:?} against {want:?}");
    assert_near(
        &format!("{what}: distance"),
        got.distance,
        want.distance,
        1e-4,
    );
    assert!(
        got.point.abs_diff_eq(want.point, 1e-4) && got.normal.abs_diff_eq(want.normal, 1e-4),
        "{what}: {got:?} against {want:?}"
    );
}

/// A ray or shape cast that starts inside two overlapping sensors, one static and one
/// moving, hits the body behind them; with sensors seen, it hits one of them where
/// it starts, and overlaps see them only then.
#[test]
fn casts_from_inside_overlapping_sensors_hit_the_body_behind() {
    let mut p = world(1);
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 10.0),
            add(
                2,
                body(BodyKind::Kinematic, cube(0.5), Vec3::new(0.0, 0.5, 0.0)),
            ),
            add(
                3,
                sensor(body(BodyKind::Static, cube(2.0), Vec3::new(0.0, 4.0, 0.0))),
            ),
            add(
                4,
                sensor(body(
                    BodyKind::Kinematic,
                    sphere(1.5),
                    Vec3::new(0.5, 4.5, 0.0),
                )),
            ),
        ],
    );
    let inside = Vec3::new(0.2, 4.2, 0.0);
    let all = QueryFilter::default();
    let hit = p
        .raycast(inside, Vec3::NEG_Y, 20.0, &all)
        .unwrap()
        .expect("the ray misses the crate behind the sensors");
    assert_eq!(hit.entity, EntityId(2));
    assert_near("ray distance", hit.distance, 3.2, 1e-4);
    assert_near("ray hit y", hit.point.y, 1.0, 1e-4);
    let cast = p
        .shape_cast(
            &sphere(0.25),
            inside,
            Quat::IDENTITY,
            Vec3::new(0.0, -10.0, 0.0),
            &all,
        )
        .unwrap()
        .expect("the shape cast misses the crate behind the sensors");
    assert_eq!(cast.entity, EntityId(2));
    assert_near("shape cast travel", cast.distance, 2.95, 1e-3);

    let with_sensors = QueryFilter {
        sensors: true,
        ..all
    };
    let hit = p
        .raycast(inside, Vec3::NEG_Y, 20.0, &with_sensors)
        .unwrap()
        .unwrap();
    assert!(
        hit.entity == EntityId(3) || hit.entity == EntityId(4),
        "{hit:?}"
    );
    assert_near("distance inside a sensor", hit.distance, 0.0, 1e-6);
    let mut found = Vec::new();
    p.overlap_sphere(inside, 0.1, &all, &mut found).unwrap();
    assert!(found.is_empty(), "{found:?}");
    p.overlap_sphere(inside, 0.1, &with_sensors, &mut found)
        .unwrap();
    assert_eq!(found, [EntityId(3), EntityId(4)]);
}

/// The bodies of [`casts_from_inside_two_rejected_bodies_see_what_lies_beyond`] that
/// its queries see: a floor, a small box at (0.1, 4.6, 0) and the querying actor.
fn seen_scene() -> Vec<PhysicsCmd> {
    vec![
        floor(1, 0.0, 0.0, 0.0, 10.0),
        add(
            4,
            body(BodyKind::Static, cube(0.1), Vec3::new(0.1, 4.6, 0.0)),
        ),
        add(
            5,
            body(
                BodyKind::Kinematic,
                ShapeDesc::Capsule {
                    half_height: 0.3,
                    radius: 0.2,
                },
                Vec3::new(0.1, 5.1, 0.0),
            ),
        ),
    ]
}

/// A ray or shape cast that starts inside two overlapping bodies it rejects (or inside
/// one and the excluded actor's own body), or that passes into both, sees what lies
/// between and behind them: the answers of a world without the rejected bodies.
#[test]
fn casts_from_inside_two_rejected_bodies_see_what_lies_beyond() {
    let (mut p, mut reference) = (world(1), world(1));
    let mut cmds = seen_scene();
    cmds.extend([
        add(
            2,
            hidden(body(BodyKind::Static, cube(1.0), Vec3::new(0.0, 5.0, 0.0))),
        ),
        add(
            3,
            hidden(body(
                BodyKind::Kinematic,
                sphere(1.0),
                Vec3::new(0.3, 5.3, 0.0),
            )),
        ),
        // A sensor around the start too, which the queries leave out.
        add(
            6,
            sensor(body(
                BodyKind::Static,
                sphere(0.5),
                Vec3::new(0.1, 5.0, 0.0),
            )),
        ),
    ]);
    apply(&mut p, cmds);
    apply(&mut reference, seen_scene());

    let inside = Vec3::new(0.1, 5.1, 0.0);
    let above = Vec3::new(0.1, 10.0, 0.0);
    let actor = QueryFilter {
        exclude: Some(EntityId(5)),
        ..visible()
    };
    let ray = |w: &Physics, from: Vec3, filter: &QueryFilter| {
        w.raycast(from, Vec3::NEG_Y, 20.0, filter).unwrap()
    };
    let cast = |w: &Physics, from: Vec3, filter: &QueryFilter| {
        w.shape_cast(
            &sphere(0.05),
            from,
            Quat::IDENTITY,
            Vec3::new(0.0, -20.0, 0.0),
            filter,
        )
        .unwrap()
    };
    let check = |p: &Physics, reference: &Physics, want: EntityId| {
        for (name, from) in [("inside", inside), ("above", above)] {
            let hit = ray(p, from, &actor);
            assert_eq!(hit.map(|h| h.entity), Some(want), "ray from {name}");
            assert_same_hit(name, hit, ray(reference, from, &actor));
            let hit = cast(p, from, &actor);
            assert_eq!(hit.map(|h| h.entity), Some(want), "cast from {name}");
            assert_same_hit(name, hit, cast(reference, from, &actor));
        }
        // Without the exclusion the ray starts inside the actor, which it hits.
        let own = ray(p, inside, &visible()).expect("the ray misses the actor");
        assert_eq!(own.entity, EntityId(5));
        assert_near("distance inside the actor", own.distance, 0.0, 1e-6);
    };
    check(&p, &reference, EntityId(4));

    // Without the box the queries go on to the floor.
    for w in [&mut p, &mut reference] {
        apply(w, vec![PhysicsCmd::RemoveBody { id: EntityId(4) }]);
    }
    check(&p, &reference, EntityId(1));

    // A character's inner body is seen as the character.
    for w in [&mut p, &mut reference] {
        apply(
            w,
            vec![PhysicsCmd::AddCharacter {
                id: EntityId(7),
                desc: CharacterDesc::default(),
                pos: Vec3::new(0.1, 2.0, 0.0),
                rot: Quat::IDENTITY,
            }],
        );
    }
    check(&p, &reference, EntityId(7));
}

/// A ray through more rejected bodies than it steps past one at a time still hits
/// what lies behind them.
#[test]
fn a_ray_through_many_rejected_bodies_hits_what_lies_behind() {
    let mut p = world(1);
    let mut cmds = vec![floor(1, 0.0, 0.0, 0.0, 10.0)];
    for i in 0..100u64 {
        let plate = ShapeDesc::Box {
            half_extents: Vec3::new(1.0, 0.1, 1.0),
        };
        cmds.push(add(
            10 + i,
            hidden(body(
                BodyKind::Static,
                plate,
                Vec3::new(0.0, 5.0 + i as f32 * 0.4, 0.0),
            )),
        ));
    }
    apply(&mut p, cmds);
    let hit = p
        .raycast(Vec3::new(0.0, 60.0, 0.0), Vec3::NEG_Y, 100.0, &visible())
        .unwrap()
        .expect("the ray misses the floor below the plates");
    assert_eq!(hit.entity, EntityId(1));
    assert_near("ray distance", hit.distance, 60.0, 1e-3);
}

/// A dynamic body replaced in one `apply` (as the sync replaces a body whose
/// description changed) moves on as it did, and so does one whose shape changes; a
/// body removed in one `apply` and added in a later one starts at rest.
#[test]
fn a_replaced_body_keeps_its_motion() {
    let mut p = world(1);
    let id = EntityId(2);
    let mut desc = body(BodyKind::Dynamic, cube(0.3), Vec3::new(0.0, 20.0, 0.0));
    apply(
        &mut p,
        vec![
            add(2, desc.clone()),
            PhysicsCmd::SetVelocity {
                id,
                linear: Vec3::new(3.0, 1.0, -2.0),
                angular: Vec3::new(0.5, 2.0, 0.0),
            },
        ],
    );
    steps(&mut p, 5);
    let moving = p.velocity(id).unwrap();
    assert!(
        moving.0.length() > 1.0 && moving.1.length() > 1.0,
        "{moving:?}"
    );
    let replace = |p: &mut Physics, desc: &mut BodyDesc| {
        (desc.pos, desc.rot) = p.pose(id).unwrap();
        apply(p, vec![PhysicsCmd::RemoveBody { id }, add(2, desc.clone())]);
    };
    desc.friction = 0.9;
    replace(&mut p, &mut desc);
    assert_eq!(
        p.velocity(id),
        Some(moving),
        "a new friction stops the body"
    );

    // A new shape keeps the body, and so its motion.
    steps(&mut p, 5);
    let moving = p.velocity(id).unwrap();
    apply(
        &mut p,
        vec![PhysicsCmd::SetShape {
            id,
            shape: sphere(0.4),
        }],
    );
    assert_eq!(p.velocity(id), Some(moving), "a new shape stops the body");

    // Added again in a later `apply`, it is a new body, at rest.
    apply(&mut p, vec![PhysicsCmd::RemoveBody { id }]);
    apply(&mut p, vec![add(2, desc.clone())]);
    assert_eq!(p.velocity(id), Some((Vec3::ZERO, Vec3::ZERO)));
}

/// A kinematic triangle mesh moves to its targets, carries a box that rests on it and
/// is seen by rays; it takes its mass from the description.
#[test]
fn a_kinematic_mesh_moves_and_carries() {
    let mut p = world(1);
    let mut rider = body(BodyKind::Dynamic, cube(0.3), Vec3::new(0.0, 1.31, 0.0));
    rider.friction = 1.0;
    let mut platform = body(BodyKind::Kinematic, deck(1.5), Vec3::new(0.0, 1.0, 0.0));
    platform.mass = 50.0;
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 30.0),
            add(2, platform),
            add(3, rider),
        ],
    );
    for s in 1..=180usize {
        if s <= 120 {
            let x = 2.0 * (s as f32 / 120.0);
            apply(
                &mut p,
                vec![PhysicsCmd::SetKinematicTarget {
                    id: EntityId(2),
                    pos: Vec3::new(x, 1.0, 0.0),
                    rot: Quat::IDENTITY,
                }],
            );
        }
        p.step(DT).unwrap();
    }
    assert_near("platform at its last target", pos(&p, 2).x, 2.0, 1e-3);
    assert_near("rider carried by the platform", pos(&p, 3).x, 2.0, 0.05);
    assert_rests("rider on the platform", pos(&p, 3).y, 1.3);
    let hit = p
        .raycast(
            Vec3::new(1.0, 5.0, 1.0),
            Vec3::NEG_Y,
            10.0,
            &QueryFilter::default(),
        )
        .unwrap()
        .expect("the ray misses the platform");
    assert_eq!(hit.entity, EntityId(2));
    assert_near("ray distance", hit.distance, 4.0, 1e-4);
}

/// A static sensor ends its touch when the body resting inside it falls asleep, and
/// begins it again when the body wakes (`Physics::events`): Jolt does not pair a
/// static sensor with a sleeping body.
#[test]
fn a_static_sensor_ends_its_touch_while_the_body_inside_sleeps() {
    let mut p = world(1);
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 10.0),
            add(
                2,
                sensor(body(BodyKind::Static, cube(1.0), Vec3::new(0.0, 1.0, 0.0))),
            ),
            add(
                3,
                body(BodyKind::Dynamic, cube(0.25), Vec3::new(0.0, 0.5, 0.0)),
            ),
        ],
    );
    // (begins, ends) of the sensor's touch with the body in the last step.
    let touches = |p: &Physics| {
        let mut seen = (0, 0);
        for e in p.events() {
            match *e {
                ContactEvent::Begin { a, b, sensor, .. }
                    if (a, b) == (EntityId(2), EntityId(3)) =>
                {
                    assert!(sensor);
                    seen.0 += 1;
                }
                ContactEvent::End { a, b, sensor } if (a, b) == (EntityId(2), EntityId(3)) => {
                    assert!(sensor);
                    seen.1 += 1;
                }
                _ => {}
            }
        }
        seen
    };
    let (mut began, mut ended) = (None, None);
    for s in 1..=600usize {
        p.step(DT).unwrap();
        let (begins, ends) = touches(&p);
        if begins > 0 {
            began.get_or_insert(s);
        }
        if ends > 0 {
            ended = Some(s);
            break;
        }
    }
    let began = began.expect("the sensor never saw the body");
    let ended = ended.expect("the touch did not end when the body fell asleep");
    assert!(ended > began);
    assert_eq!(p.is_awake(EntityId(3)), Some(false));
    assert_rests("the body still inside the sensor", pos(&p, 3).y, 0.25);

    // While it sleeps nothing more is reported; waking it begins the touch again.
    for _ in 0..30 {
        p.step(DT).unwrap();
        assert_eq!(touches(&p), (0, 0));
    }
    apply(
        &mut p,
        vec![PhysicsCmd::ApplyImpulse {
            id: EntityId(3),
            impulse: Vec3::new(0.0, 0.5, 0.0),
        }],
    );
    let woke = (1..=5).find(|_| {
        p.step(DT).unwrap();
        touches(&p).0 > 0
    });
    assert!(
        woke.is_some(),
        "the touch did not begin again when the body woke"
    );
}
