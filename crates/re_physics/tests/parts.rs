//! Parts and assemblies: mass, collision groups, touches, welds, joints and
//! queries.

mod common;

use re_physics::parts::geom::PartShape;
use re_physics::{
    AssemblyDesc, GroupTable, HingeDrive, JointDesc, JointKind, PartDesc, PartEvent, PartGeom,
    PartView, Physics, Pose,
};
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::BodyKind;

/// Studs per second squared.
const GRAVITY: f32 = 196.2;
/// Four sub-steps per 60 Hz frame.
const SUB: f32 = 1.0 / 240.0;

fn world() -> Physics {
    let mut p = common::world(2);
    p.set_gravity(Vec3::new(0.0, -GRAVITY, 0.0)).unwrap();
    p
}

fn frame(p: &mut Physics) {
    for _ in 0..4 {
        p.step(SUB).unwrap();
    }
}

fn part(id: u64, shape: PartShape, size: Vec3, at: Vec3) -> PartDesc {
    PartDesc {
        geom: PartGeom::new(shape, size),
        local: Pose::new(at, Quat::IDENTITY),
        ..PartDesc::block(EntityId(id), size)
    }
}

fn single(kind: BodyKind, p: PartDesc, pos: Vec3) -> AssemblyDesc {
    AssemblyDesc {
        kind,
        pose: Pose::new(pos, Quat::IDENTITY),
        linear: Vec3::ZERO,
        angular: Vec3::ZERO,
        parts: vec![p],
    }
}

fn add_block(p: &mut Physics, id: u64, kind: BodyKind, size: Vec3, pos: Vec3) {
    let desc = single(kind, PartDesc::block(EntityId(id), size), pos);
    p.add_assembly(EntityId(id), &desc).unwrap();
}

fn baseplate(p: &mut Physics) {
    add_block(
        p,
        1,
        BodyKind::Static,
        Vec3::new(512.0, 20.0, 512.0),
        Vec3::new(0.0, -10.0, 0.0),
    );
}

#[test]
fn a_part_falls_98_studs_in_a_second() {
    let mut p = world();
    add_block(
        &mut p,
        10,
        BodyKind::Dynamic,
        Vec3::new(4.0, 1.0, 2.0),
        Vec3::new(0.0, 500.0, 0.0),
    );
    for _ in 0..60 {
        frame(&mut p);
    }
    let fell = 500.0 - p.assembly(EntityId(10)).unwrap().pose.pos.y;
    assert!((fell - 98.1).abs() <= 0.5, "fell {fell} studs");
}

#[test]
fn mass_and_center_of_mass_come_from_the_parts() {
    let mut p = world();
    let mut a = part(10, PartShape::Block, Vec3::new(2.0, 2.0, 2.0), Vec3::ZERO);
    a.material.density = 1.0;
    let mut b = part(
        11,
        PartShape::Block,
        Vec3::new(2.0, 2.0, 2.0),
        Vec3::new(4.0, 0.0, 0.0),
    );
    b.material.density = 3.0;
    let mut c = part(
        12,
        PartShape::Ball,
        Vec3::splat(2.0),
        Vec3::new(-4.0, 0.0, 0.0),
    );
    c.massless = true;
    let desc = AssemblyDesc {
        kind: BodyKind::Dynamic,
        pose: Pose::new(Vec3::new(0.0, 50.0, 0.0), Quat::IDENTITY),
        linear: Vec3::ZERO,
        angular: Vec3::ZERO,
        parts: vec![a, b, c],
    };
    p.add_assembly(EntityId(10), &desc).unwrap();
    let info = p.assembly(EntityId(10)).unwrap();
    assert!((info.mass - 32.0).abs() < 1e-3, "mass {}", info.mass);
    assert!((info.center_of_mass - Vec3::new(3.0, 50.0, 0.0)).length() < 1e-3);
    // Pushing at the centre of mass does not turn it.
    p.apply_assembly_impulse(EntityId(10), Vec3::new(0.0, 0.0, 32.0), None)
        .unwrap();
    frame(&mut p);
    let info = p.assembly(EntityId(10)).unwrap();
    assert!((info.linear.z - 1.0).abs() < 1e-3);
    assert!(info.angular.length() < 1e-3, "angular {}", info.angular);
}

#[test]
fn welded_parts_move_as_one_and_split_when_the_weld_breaks() {
    let mut p = world();
    baseplate(&mut p);
    let a = part(10, PartShape::Block, Vec3::new(2.0, 2.0, 2.0), Vec3::ZERO);
    let b = part(
        11,
        PartShape::Block,
        Vec3::new(2.0, 2.0, 2.0),
        Vec3::new(0.0, 2.0, 0.0),
    );
    let desc = AssemblyDesc {
        kind: BodyKind::Dynamic,
        pose: Pose::new(Vec3::new(0.0, 20.0, 0.0), Quat::IDENTITY),
        linear: Vec3::ZERO,
        angular: Vec3::ZERO,
        parts: vec![a.clone(), b.clone()],
    };
    p.add_assembly(EntityId(10), &desc).unwrap();
    for _ in 0..30 {
        frame(&mut p);
    }
    let (pa, pb) = (p.part(EntityId(10)).unwrap(), p.part(EntityId(11)).unwrap());
    assert!((pa.world.inv_point(pb.world.pos) - Vec3::new(0.0, 2.0, 0.0)).length() < 1e-3);
    assert!(pa.world.rot.angle_between(pb.world.rot) < 1e-3);
    assert!(pa.world.pos.y < 19.0);
    // Break the weld: 11 becomes its own assembly where it is.
    p.set_assembly_parts(EntityId(10), &[a]).unwrap();
    let mut solo = b;
    solo.local = Pose::IDENTITY;
    p.add_assembly(EntityId(11), &single(BodyKind::Dynamic, solo, pb.world.pos))
        .unwrap();
    assert_eq!(p.assembly_of(EntityId(11)), Some(EntityId(11)));
    p.apply_assembly_impulse(EntityId(11), Vec3::new(400.0, 0.0, 0.0), None)
        .unwrap();
    for _ in 0..30 {
        frame(&mut p);
    }
    let (pa, pb) = (p.part(EntityId(10)).unwrap(), p.part(EntityId(11)).unwrap());
    assert!(
        (pb.world.pos.x - pa.world.pos.x).abs() > 1.0,
        "{pa:?} {pb:?}"
    );
}

#[test]
fn collision_groups_decide_every_ordered_pair() {
    const GROUPS: u8 = 6;
    let mut table = GroupTable::default();
    // A fixed, asymmetric-looking pattern (the table itself is symmetric).
    for a in 0..GROUPS {
        for b in a..GROUPS {
            table.set(a, b, (a * 7 + b * 3) % 4 != 0);
        }
    }
    for a in 0..GROUPS {
        for b in 0..GROUPS {
            let mut p = world();
            p.set_collision_groups(table.clone());
            let mut floor = PartDesc::block(EntityId(1), Vec3::new(20.0, 2.0, 20.0));
            floor.group = a;
            p.add_assembly(EntityId(1), &single(BodyKind::Static, floor, Vec3::ZERO))
                .unwrap();
            let mut cube = PartDesc::block(EntityId(2), Vec3::splat(2.0));
            cube.group = b;
            p.add_assembly(
                EntityId(2),
                &single(BodyKind::Dynamic, cube, Vec3::new(0.0, 4.0, 0.0)),
            )
            .unwrap();
            for _ in 0..60 {
                frame(&mut p);
            }
            let y = p.assembly(EntityId(2)).unwrap().pose.pos.y;
            let stood = (1.9..2.2).contains(&y);
            assert_eq!(
                stood,
                table.collides(a, b),
                "groups {a} on {b}: cube at y = {y}"
            );
        }
    }
}

#[test]
fn touches_begin_and_end_per_part_pair() {
    let mut p = world();
    let mut floor = PartDesc::block(EntityId(1), Vec3::new(512.0, 20.0, 512.0));
    floor.material.elasticity = 0.0;
    p.add_assembly(
        EntityId(1),
        &single(BodyKind::Static, floor, Vec3::new(0.0, -10.0, 0.0)),
    )
    .unwrap();
    let mut cube = PartDesc::block(EntityId(10), Vec3::splat(2.0));
    cube.material.elasticity = 0.0;
    p.add_assembly(
        EntityId(10),
        &single(BodyKind::Dynamic, cube, Vec3::new(0.0, 1.5, 0.0)),
    )
    .unwrap();
    let mut begun = Vec::new();
    for _ in 0..120 {
        frame(&mut p);
        begun.extend(p.take_part_events());
    }
    let begins: Vec<_> = begun
        .iter()
        .filter(|e| matches!(e, PartEvent::Begin { .. }))
        .collect();
    assert_eq!(begins.len(), 1, "{begun:?}");
    assert!(
        matches!(begins[0], PartEvent::Begin { a, b, .. } if *a == EntityId(1) && *b == EntityId(10))
    );
    // It rests asleep without ending the touch.
    assert!(
        !begun.iter().any(|e| matches!(e, PartEvent::End { .. })),
        "{begun:?}"
    );
    assert!(!p.assembly(EntityId(10)).unwrap().awake);
    p.remove_assembly(EntityId(10)).unwrap();
    frame(&mut p);
    assert_eq!(
        p.take_part_events(),
        vec![PartEvent::End {
            a: EntityId(1),
            b: EntityId(10)
        }]
    );
}

#[test]
fn a_part_that_does_not_collide_still_touches() {
    let mut p = world();
    baseplate(&mut p);
    let mut ghost = PartDesc::block(EntityId(10), Vec3::splat(2.0));
    ghost.can_collide = false;
    p.add_assembly(
        EntityId(10),
        &single(BodyKind::Dynamic, ghost, Vec3::new(0.0, 3.0, 0.0)),
    )
    .unwrap();
    let mut events = Vec::new();
    for _ in 0..30 {
        frame(&mut p);
        events.extend(p.take_part_events());
    }
    assert!(
        p.assembly(EntityId(10)).unwrap().pose.pos.y < -5.0,
        "it falls through"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PartEvent::Begin { b, .. } if *b == EntityId(10))),
        "{events:?}"
    );
}

#[test]
fn can_touch_off_reports_nothing() {
    let mut p = world();
    baseplate(&mut p);
    let mut quiet = PartDesc::block(EntityId(10), Vec3::splat(2.0));
    quiet.can_touch = false;
    p.add_assembly(
        EntityId(10),
        &single(BodyKind::Dynamic, quiet, Vec3::new(0.0, 3.0, 0.0)),
    )
    .unwrap();
    for _ in 0..30 {
        frame(&mut p);
        assert!(p.take_part_events().is_empty());
    }
    let y = p.assembly(EntityId(10)).unwrap().pose.pos.y;
    assert!((0.9..1.2).contains(&y), "it still collides: {y}");
}

#[test]
fn friction_combines_per_part() {
    // A block sliding on ice stops later than on high friction.
    let slide = |friction: f32| {
        let mut p = world();
        let mut floor = PartDesc::block(EntityId(1), Vec3::new(1000.0, 2.0, 40.0));
        floor.material.friction = friction;
        p.add_assembly(EntityId(1), &single(BodyKind::Static, floor, Vec3::ZERO))
            .unwrap();
        let mut cube = PartDesc::block(EntityId(2), Vec3::splat(2.0));
        cube.material.friction = friction;
        let mut d = single(BodyKind::Dynamic, cube, Vec3::new(-400.0, 2.0, 0.0));
        d.linear = Vec3::new(50.0, 0.0, 0.0);
        p.add_assembly(EntityId(2), &d).unwrap();
        for _ in 0..120 {
            frame(&mut p);
        }
        p.assembly(EntityId(2)).unwrap().pose.pos.x + 400.0
    };
    let (ice, rough) = (slide(0.02), slide(1.0));
    assert!(ice > rough * 2.0, "ice {ice} rough {rough}");
}

#[test]
fn hinge_motor_turns_and_servo_reaches_its_angle() {
    let mut p = world();
    p.set_gravity(Vec3::ZERO).unwrap();
    add_block(&mut p, 1, BodyKind::Static, Vec3::splat(2.0), Vec3::ZERO);
    add_block(
        &mut p,
        2,
        BodyKind::Dynamic,
        Vec3::new(1.0, 4.0, 1.0),
        Vec3::new(2.0, 0.0, 0.0),
    );
    let frame_a = Pose::new(Vec3::new(1.0, 0.0, 0.0), Quat::IDENTITY);
    let frame_b = Pose::new(Vec3::new(-1.0, 0.0, 0.0), Quat::IDENTITY);
    let mut desc = JointDesc {
        a: EntityId(1),
        b: EntityId(2),
        frame_a,
        frame_b,
        kind: JointKind::Hinge {
            limits: None,
            drive: HingeDrive::Motor {
                velocity: 3.0,
                max_torque: 1e9,
            },
        },
    };
    p.set_joint(EntityId(100), &desc).unwrap();
    for _ in 0..30 {
        frame(&mut p);
    }
    let w = p.assembly(EntityId(2)).unwrap().angular;
    assert!((w.x.abs() - 3.0).abs() < 0.1, "angular {w}");
    desc.kind = JointKind::Hinge {
        limits: None,
        drive: HingeDrive::Servo {
            angle: 1.0,
            speed: 2.0,
            max_torque: 1e9,
        },
    };
    p.set_joint(EntityId(100), &desc).unwrap();
    for _ in 0..120 {
        frame(&mut p);
    }
    let angle = p.hinge_angle(EntityId(100)).unwrap();
    assert!((angle - 1.0).abs() < 0.05, "angle {angle}");
    // The joint stays through a reshape and goes with its assembly.
    p.set_assembly_parts(
        EntityId(2),
        &[PartDesc::block(EntityId(2), Vec3::new(1.0, 6.0, 1.0))],
    )
    .unwrap();
    assert!(p.has_joint(EntityId(100)));
    p.remove_assembly(EntityId(2)).unwrap();
    assert_eq!(p.take_dropped_joints(), vec![EntityId(100)]);
}

#[test]
fn a_rope_holds_a_hanging_part() {
    let mut p = world();
    add_block(
        &mut p,
        1,
        BodyKind::Static,
        Vec3::splat(2.0),
        Vec3::new(0.0, 50.0, 0.0),
    );
    add_block(
        &mut p,
        2,
        BodyKind::Dynamic,
        Vec3::splat(1.0),
        Vec3::new(0.0, 45.0, 0.0),
    );
    p.set_joint(
        EntityId(100),
        &JointDesc {
            a: EntityId(1),
            b: EntityId(2),
            frame_a: Pose::IDENTITY,
            frame_b: Pose::IDENTITY,
            kind: JointKind::Rope { length: 10.0 },
        },
    )
    .unwrap();
    for _ in 0..120 {
        frame(&mut p);
    }
    let y = p.assembly(EntityId(2)).unwrap().pose.pos.y;
    assert!((y - 40.0).abs() < 0.2, "hangs at {y}");
}

// ---- Queries against brute force ----

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }
    fn vec(&mut self, lo: f32, hi: f32) -> Vec3 {
        Vec3::new(self.range(lo, hi), self.range(lo, hi), self.range(lo, hi))
    }
    fn rot(&mut self) -> Quat {
        Quat::from_euler(
            re_types::glam::EulerRot::XYZ,
            self.range(-3.0, 3.0),
            self.range(-3.0, 3.0),
            self.range(-3.0, 3.0),
        )
    }
}

const SHAPES: [PartShape; 5] = [
    PartShape::Block,
    PartShape::Ball,
    PartShape::Cylinder,
    PartShape::Wedge,
    PartShape::CornerWedge,
];

/// A scattered world of anchored parts, some in two-part assemblies, with
/// mixed groups and flags.
fn scattered(rng: &mut Rng, n: u64) -> Physics {
    let mut p = world();
    let mut id = 1;
    while id <= n {
        let mut parts = Vec::new();
        let count = if rng.next() < 0.3 { 2 } else { 1 };
        for k in 0..count {
            let shape = SHAPES[(rng.next() * 5.0) as usize % 5];
            let size = rng.vec(0.5, 6.0);
            let mut d = part(id + k, shape, size, Vec3::ZERO);
            d.local = Pose::new(
                if k == 0 {
                    Vec3::ZERO
                } else {
                    rng.vec(-4.0, 4.0)
                },
                rng.rot(),
            );
            d.group = (rng.next() * 4.0) as u8 % 4;
            d.can_collide = rng.next() < 0.8;
            d.can_query = rng.next() < 0.5;
            parts.push(d);
        }
        let pos = rng.vec(-40.0, 40.0);
        p.add_assembly(
            EntityId(id),
            &AssemblyDesc {
                kind: BodyKind::Static,
                pose: Pose::new(pos, rng.rot()),
                linear: Vec3::ZERO,
                angular: Vec3::ZERO,
                parts,
            },
        )
        .unwrap();
        id += count;
    }
    p
}

fn sees(v: &PartView, excluded_group: u8) -> bool {
    v.can_query && v.group != excluded_group && !v.id.0.is_multiple_of(7)
}

#[test]
fn raycasts_match_brute_force() {
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let p = scattered(&mut rng, 1500);
    let all = p.all_parts();
    let mut hits = 0;
    for i in 0..1000 {
        let origin = rng.vec(-50.0, 50.0);
        let dir = (rng.vec(-1.0, 1.0)).normalize_or_zero() * rng.range(20.0, 200.0);
        let group = (i % 5) as u8;
        let got = p.raycast_parts(origin, dir, &mut |v: &PartView| sees(v, group));
        // Brute force: every part, in its own frame.
        let reach = dir.length();
        let unit = dir / reach;
        let mut best: Option<(f32, EntityId)> = None;
        for v in &all {
            if !sees(v, group) {
                continue;
            }
            let o = v.world.inv_point(origin);
            let d = v.world.rot.inverse() * unit;
            if let Some((t, _)) = v.geom.sanitized().ray_entry(o, d, reach)
                && best.is_none_or(|(bt, bid)| t < bt || (t == bt && v.id < bid))
            {
                best = Some((t, v.id));
            }
        }
        match (got, best) {
            (None, None) => {}
            (Some(h), Some((t, id))) => {
                hits += 1;
                assert!(
                    h.part == id || (h.distance - t).abs() < 1e-4,
                    "ray {i}: got {h:?}, brute force {id} at {t}"
                );
                assert!((h.distance - t).abs() < 1e-3);
                assert!((h.position - (origin + unit * h.distance)).length() < 1e-3);
            }
            (g, b) => panic!("ray {i}: got {g:?}, brute force {b:?}"),
        }
    }
    assert!(hits > 200, "only {hits} rays hit");
}

#[test]
fn overlaps_match_brute_force() {
    use re_physics::parts::geom::{Placed, obb_overlap, obb_sphere_overlap, overlaps};
    let mut rng = Rng(0x2545f4914f6cdd1d);
    let p = scattered(&mut rng, 400);
    let all = p.all_parts();
    let mut found = 0;
    for i in 0..1000 {
        let group = (i % 5) as u8;
        let center = rng.vec(-70.0, 70.0);
        let max = if i % 3 == 0 { 3 } else { usize::MAX };
        let mut want: Vec<EntityId> = match i % 3 {
            0 => {
                let pose = Pose::new(center, rng.rot());
                let size = rng.vec(1.0, 30.0);
                let got = p.parts_in_box(pose, size, &mut |v: &PartView| sees(v, group), max);
                let want = all
                    .iter()
                    .filter(|v| sees(v, group))
                    .filter(|v| obb_overlap(&v.world, v.geom.half(), &pose, size * 0.5))
                    .map(|v| v.id)
                    .collect::<Vec<_>>();
                assert_eq!(
                    got,
                    want.iter().copied().take(max).collect::<Vec<_>>(),
                    "box {i}"
                );
                want
            }
            1 => {
                let r = rng.range(0.5, 25.0);
                let got = p.parts_in_radius(center, r, &mut |v: &PartView| sees(v, group), max);
                let want = all
                    .iter()
                    .filter(|v| sees(v, group))
                    .filter(|v| obb_sphere_overlap(&v.world, v.geom.half(), center, r))
                    .map(|v| v.id)
                    .collect::<Vec<_>>();
                assert_eq!(got, want, "radius {i}");
                want
            }
            _ => {
                let shape = SHAPES[(rng.next() * 5.0) as usize % 5];
                let geom = PartGeom::new(shape, rng.vec(2.0, 20.0));
                let pose = Pose::new(center, rng.rot());
                let got = p.parts_in_solid(geom, pose, &mut |v: &PartView| sees(v, group), max);
                let mut query = Placed::new(geom, &pose);
                query.margin = -1e-3;
                let want = all
                    .iter()
                    .filter(|v| sees(v, group))
                    .filter(|v| overlaps(&query, &Placed::new(v.geom.sanitized(), &v.world)))
                    .map(|v| v.id)
                    .collect::<Vec<_>>();
                assert_eq!(got, want, "solid {i}");
                want
            }
        };
        want.truncate(max);
        found += want.len();
    }
    assert!(found > 300, "only {found} overlaps");
}

#[test]
fn shapecasts_match_brute_force() {
    use re_physics::parts::geom::{Placed, shape_cast};
    let mut rng = Rng(0x853c49e6748fea9b);
    let p = scattered(&mut rng, 300);
    let all = p.all_parts();
    let mut hits = 0;
    for i in 0..300 {
        let group = (i % 5) as u8;
        let shape = [PartShape::Block, PartShape::Ball][i % 2];
        let geom = PartGeom::new(shape, rng.vec(1.0, 4.0));
        let from = Pose::new(rng.vec(-60.0, 60.0), rng.rot().normalize());
        let dir = rng.vec(-1.0, 1.0).normalize_or_zero() * rng.range(10.0, 120.0);
        let got = p.shapecast_parts(geom, from, dir, &mut |v: &PartView| sees(v, group));
        let moving = Placed::new(geom, &from);
        let mut best: Option<(f64, EntityId)> = None;
        for v in &all {
            if !sees(v, group) {
                continue;
            }
            let target = Placed::new(v.geom.sanitized(), &v.world);
            if let Some((f, _, _)) = shape_cast(&moving, &target, dir.as_dvec3(), 1e-4)
                && best.is_none_or(|(bf, _)| f < bf)
            {
                best = Some((f, v.id));
            }
        }
        match (got, best) {
            (None, None) => {}
            (Some(h), Some((f, id))) => {
                hits += 1;
                let d = (f * dir.as_dvec3().length()) as f32;
                assert!(h.part == id || (h.distance - d).abs() < 1e-3, "cast {i}");
                assert!(
                    (h.distance - d).abs() < 1e-3,
                    "cast {i}: {} vs {d}",
                    h.distance
                );
            }
            (g, b) => panic!("cast {i}: got {g:?}, brute force {b:?}"),
        }
    }
    assert!(hits > 30, "only {hits} casts hit");
}

#[test]
fn queries_follow_moving_parts() {
    let mut p = world();
    add_block(
        &mut p,
        10,
        BodyKind::Dynamic,
        Vec3::splat(2.0),
        Vec3::new(0.0, 100.0, 0.0),
    );
    for _ in 0..30 {
        frame(&mut p);
    }
    let y = p.assembly(EntityId(10)).unwrap().pose.pos.y;
    let hit = p
        .raycast_parts(
            Vec3::new(0.0, 200.0, 0.0),
            Vec3::new(0.0, -300.0, 0.0),
            &mut |_: &PartView| true,
        )
        .unwrap();
    assert_eq!(hit.part, EntityId(10));
    assert!((hit.position.y - (y + 1.0)).abs() < 1e-3);
}
