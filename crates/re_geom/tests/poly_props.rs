//! Property tests of the modelling kernel (ADR-0062 §12): random primitives,
//! transforms and edits never panic, and every output is valid, finite,
//! closed and outward where its input was, and keeps its input's columns.
//!
//! Each test has a fixed seed and writes no failure files, so a run is the
//! same everywhere; set `PROPTEST_CASES` for more cases than the 256 default.

use glam::{Quat, Vec3};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};
use re_geom::GeomError;
use re_geom::math::quat_from_euler_deg;
use re_geom::poly::{self, AttrDomain, AttrValue, Axis, ColumnType, PolyMesh};

fn config(seed: u64) -> Config {
    Config {
        failure_persistence: None,
        rng_seed: RngSeed::Fixed(seed),
        ..Config::default()
    }
}

/// A primitive and its parameters, all within range.
#[derive(Clone, Debug)]
enum Prim {
    Box(Vec3, f32, u32),
    Icosphere(f32, u32),
    Cone(f32, f32, f32, u32, bool),
    Torus(f32, f32, u32, u32),
    Stairs(u32, f32, f32, f32),
    Arch(f32, f32, f32, f32, u32),
}

impl Prim {
    fn build(&self) -> Result<PolyMesh, GeomError> {
        match *self {
            Prim::Box(size, r, segments) => poly::rounded_box(size, r, segments),
            Prim::Icosphere(r, n) => poly::icosphere(r, n),
            Prim::Cone(r, top, h, segments, caps) => poly::cone(r, top, h, segments, caps),
            Prim::Torus(r, tube, segments, sides) => poly::torus(r, tube, segments, sides),
            Prim::Stairs(steps, w, rise, run) => poly::stairs(steps, w, rise, run),
            Prim::Arch(span, rise, t, d, segments) => poly::arch(span, rise, t, d, segments),
        }
    }

    /// False only for a cone without caps.
    fn closed(&self) -> bool {
        !matches!(self, Prim::Cone(.., false))
    }
}

fn size() -> impl Strategy<Value = f32> {
    0.05f32..20.0
}

fn prim() -> impl Strategy<Value = Prim> {
    prop_oneof![
        (size(), size(), size(), 0.0f32..=1.0, 1u32..=8).prop_map(|(x, y, z, f, s)| {
            let half = x.min(y).min(z) * 0.5;
            // Every fifth box sharp; the rest rounded up to the whole half side.
            let r = if f < 0.2 { 0.0 } else { half * f };
            Prim::Box(Vec3::new(x, y, z), r, s * 2)
        }),
        (size(), 0u32..=3).prop_map(|(r, n)| Prim::Icosphere(r, n)),
        (0.0f32..10.0, 0.0f32..10.0, size(), 3u32..=24, any::<bool>()).prop_map(
            |(r, top, h, s, caps)| {
                // Not both radii 0.
                let r = if r == 0.0 && top == 0.0 { 1.0 } else { r };
                Prim::Cone(r, top, h, s, caps)
            }
        ),
        (size(), 0.01f32..0.99, 3u32..=24, 3u32..=16).prop_map(|(r, f, s, sides)| Prim::Torus(
            r,
            r * f,
            s,
            sides
        )),
        (1u32..=12, size(), 0.01f32..2.0, 0.01f32..2.0)
            .prop_map(|(n, w, rise, run)| Prim::Stairs(n, w, rise, run)),
        (size(), 0.01f32..=0.5, size(), size(), 1u32..=24)
            .prop_map(|(span, f, t, d, s)| Prim::Arch(span, span * f, t, d, s)),
    ]
}

/// Translation, Euler angles in degrees, and a scale whose axes are each
/// 0.1 to 10 in size, of either sign.
fn placement() -> impl Strategy<Value = (Vec3, Vec3, Vec3)> {
    let v = |r: f32| (-r..r, -r..r, -r..r).prop_map(|(x, y, z)| Vec3::new(x, y, z));
    let s = || (0.1f32..10.0, any::<bool>()).prop_map(|(s, neg)| if neg { -s } else { s });
    (
        v(100.0),
        v(360.0),
        (s(), s(), s()).prop_map(|(x, y, z)| Vec3::new(x, y, z)),
    )
}

fn axis() -> impl Strategy<Value = Axis> {
    prop_oneof![Just(Axis::X), Just(Axis::Y), Just(Axis::Z)]
}

/// Valid, with every face triangulated into a well-formed mesh per part.
fn check(m: &PolyMesh) {
    m.validate().unwrap();
    let parts = m.to_meshes().unwrap();
    assert_eq!(parts.len(), m.part_count());
    for p in &parts {
        assert!(p.is_well_formed());
        assert!(p.vertices.iter().all(|v| {
            v.pos
                .iter()
                .chain(&v.normal)
                .chain(&v.uv)
                .chain(&v.color)
                .all(|x| x.is_finite())
        }));
    }
}

/// Every column of `before` is in `after`, with its type.
fn keeps_columns(before: &PolyMesh, after: &PolyMesh) {
    for (b, a) in [
        (&before.points, &after.points),
        (&before.corners, &after.corners),
        (&before.faces, &after.faces),
    ] {
        for (name, col) in b.iter() {
            assert_eq!(
                a.get(name).map(|c| c.ty()),
                Some(col.ty()),
                "column `{name}`"
            );
        }
    }
}

fn close(a: f64, b: f64, rel: f64) -> bool {
    (a - b).abs() <= rel * a.abs().max(b.abs()).max(1e-9)
}

proptest! {
    #![proptest_config(config(0x4110_0001))]

    #[test]
    fn primitives_are_valid_solids(p in prim()) {
        let m = p.build().unwrap();
        check(&m);
        prop_assert_eq!(m.is_closed(), p.closed());
        prop_assert!(m.is_manifold());
        prop_assert!(m.corners.vec2s(poly::UV).is_some());
        if p.closed() {
            let v = m.volume().unwrap();
            prop_assert!(v > 0.0, "volume {}", v);
        }
    }
}

proptest! {
    #![proptest_config(config(0x4110_0002))]

    #[test]
    fn bad_arguments_are_refused_not_panicked(
        which in 0u32..6,
        a in prop_oneof![Just(f32::NAN), Just(f32::INFINITY), Just(-1.0f32), Just(0.0f32), -5.0f32..5.0],
        b in prop_oneof![Just(f32::NAN), Just(-1.0f32), Just(0.0f32), 0.0f32..5.0],
        n in prop_oneof![Just(0u32), Just(1u32), Just(3u32), Just(257u32), Just(u32::MAX), 0u32..40],
    ) {
        let r = match which {
            0 => poly::rounded_box(Vec3::new(a, 1.0, 1.0), b, n),
            1 => poly::icosphere(a, n),
            2 => poly::cone(a, b, 1.0, n, true),
            3 => poly::torus(a, b, n, n),
            4 => poly::stairs(n, a, b, 1.0),
            _ => poly::arch(a, b, 0.2, 0.3, n),
        };
        match r {
            Ok(m) => check(&m),
            Err(e) => prop_assert!(matches!(e, GeomError::Argument(_)), "{}", e),
        }
    }
}

proptest! {
    #![proptest_config(config(0x4110_0003))]

    #[test]
    fn transforms_keep_solids_closed_and_outward(p in prim(), (t, euler, s) in placement()) {
        let m = p.build().unwrap();
        let out = poly::transform(&m, t, quat_from_euler_deg(euler), s).unwrap();
        check(&out);
        keeps_columns(&m, &out);
        prop_assert_eq!(out.is_closed(), m.is_closed());
        if let (Some(v0), Some(v1)) = (m.volume(), out.volume()) {
            let want = v0 * f64::from(s.x * s.y * s.z).abs();
            prop_assert!(v1 > 0.0 && close(v1, want, 1e-3), "{} vs {}", v1, want);
        }
    }

    #[test]
    fn mirrors_keep_solids_closed_and_outward(p in prim(), a in axis(), gap in 0.0f32..10.0) {
        let m = p.build().unwrap();
        let b = m.bounds().unwrap();
        // A plane beyond the mesh, so the halves are apart: each closed.
        let offset = b.max.to_array()[a.index()] + gap + 0.01;
        for keep in [false, true] {
            let out = poly::mirror(&m, a, offset, keep).unwrap();
            check(&out);
            keeps_columns(&m, &out);
            prop_assert_eq!(out.is_closed(), m.is_closed());
            if let (Some(v0), Some(v1)) = (m.volume(), out.volume()) {
                let want = if keep { 2.0 * v0 } else { v0 };
                prop_assert!(close(v1, want, 1e-3), "{} vs {}", v1, want);
            }
        }
    }

    #[test]
    fn a_sharp_box_mirrored_on_its_face_joins_into_one(size in (size(), size(), size()), a in axis()) {
        let m = poly::rounded_box(Vec3::new(size.0, size.1, size.2), 0.0, 2).unwrap();
        let offset = m.bounds().unwrap().max.to_array()[a.index()];
        let out = poly::mirror(&m, a, offset, true).unwrap();
        check(&out);
        prop_assert!(out.is_closed() && out.is_manifold());
        prop_assert_eq!((out.point_count(), out.face_count()), (12, 10));
        prop_assert!(close(out.volume().unwrap(), 2.0 * m.volume().unwrap(), 1e-5));
    }
}

proptest! {
    #![proptest_config(config(0x4110_0004))]

    #[test]
    fn welds_give_valid_meshes(p in prim(), distance in prop_oneof![Just(0.0f32), 0.0f32..2.0]) {
        let m = p.build().unwrap();
        // Welding far enough to fold a face over is refused.
        let out = match poly::weld(&m, distance) {
            Err(GeomError::Triangulation(_)) if distance > 0.0 => return Ok(()),
            r => r.unwrap(),
        };
        check(&out);
        keeps_columns(&m, &out);
        prop_assert!(out.point_count() <= m.point_count());
        prop_assert!(out.face_count() <= m.face_count());
        if distance == 0.0 {
            // Primitives have no two points in one place.
            prop_assert_eq!(out, m);
        }
    }

    #[test]
    fn merges_keep_every_part(ps in prop::collection::vec((prim(), any::<bool>()), 1..4)) {
        let mut meshes = Vec::new();
        for (i, (p, painted)) in ps.iter().enumerate() {
            let m = p.build().unwrap();
            // Apart along X, so the parts stay closed.
            let mut m = poly::transform(&m, Vec3::new(100.0 * i as f32, 0.0, 0.0), Quat::IDENTITY, Vec3::ONE).unwrap();
            if *painted {
                let all = vec![true; m.face_count()];
                m.set_material(&all, &format!("materials/m{i}.mat")).unwrap();
                m.fill_attribute(AttrDomain::Point, "heat", AttrValue::Float(i as f32), &all).unwrap();
            }
            meshes.push(m);
        }
        let out = poly::merge(&meshes).unwrap();
        check(&out);
        for m in &meshes {
            keeps_columns(m, &out);
        }
        prop_assert_eq!(out.face_count(), meshes.iter().map(PolyMesh::face_count).sum::<usize>());
        prop_assert_eq!(out.is_closed(), meshes.iter().all(PolyMesh::is_closed));
        // A mesh painted all over has a table of its one material; the
        // others add the empty path once, if any is painted at all.
        let painted = ps.iter().filter(|(_, p)| *p).count();
        let plain = ps.len() - painted;
        let want = if painted == 0 { 0 } else { painted + usize::from(plain > 0) };
        prop_assert_eq!(out.materials.len(), want);
        for (m, (_, painted)) in meshes.iter().zip(&ps) {
            prop_assert_eq!(m.materials.len(), usize::from(*painted));
        }
    }
}

proptest! {
    #![proptest_config(config(0x4110_0005))]

    #[test]
    fn flipping_turns_solids_inside_out(p in prim()) {
        let m = p.build().unwrap();
        let out = m.flip("").unwrap();
        check(&out);
        keeps_columns(&m, &out);
        prop_assert_eq!(out.is_closed(), m.is_closed());
        if let (Some(v0), Some(v1)) = (m.volume(), out.volume()) {
            prop_assert!(close(v1, -v0, 1e-9), "{} vs {}", v1, v0);
        }
        prop_assert_eq!(out.flip("").unwrap(), m);
    }

    #[test]
    fn triangle_meshes_convert_back_closed(p in prim()) {
        let m = p.build().unwrap();
        let tri = m.to_mesh().unwrap();
        let back = poly::from_mesh(&tri).unwrap();
        check(&back);
        prop_assert_eq!(back.is_closed(), m.is_closed());
        prop_assert_eq!(back.corner_count(), tri.indices.len());
        if let (Some(v0), Some(v1)) = (m.volume(), back.volume()) {
            prop_assert!(close(v1, v0, 1e-3), "{} vs {}", v1, v0);
        }
    }

    #[test]
    fn groups_take_materials_and_colours(p in prim(), name in "[a-z][a-z0-9_]{0,10}", c in (0.0f32..1.0, 0.0f32..1.0, 0.0f32..1.0)) {
        let mut m = p.build().unwrap();
        let groups: Vec<String> = m
            .faces
            .iter()
            .filter(|(_, col)| col.ty() == ColumnType::Bool)
            .map(|(n, _)| n.to_string())
            .collect();
        for (i, g) in groups.iter().enumerate() {
            let sel = m.group(g).unwrap();
            m.set_material(&sel, &format!("materials/{g}.mat")).unwrap();
            m.fill_attribute(AttrDomain::Face, poly::COLOR, AttrValue::Vec3(Vec3::new(c.0, c.1, i as f32 * 0.1)), &sel).unwrap();
        }
        let all = vec![true; m.face_count()];
        let standard = ["position", "uv", "normal", "color", "material"].contains(&name.as_str());
        let r = m.fill_attribute(AttrDomain::Corner, &name, AttrValue::Int(3), &all);
        if standard {
            prop_assert!(matches!(r, Err(GeomError::Argument(_))));
        } else {
            r.unwrap();
        }
        let r = m.set_group(&name, all.clone());
        if standard {
            prop_assert!(matches!(r, Err(GeomError::Argument(_))));
        } else {
            r.unwrap();
        }
        // The material table is only reached through `set_material`.
        let r = m.fill_attribute(AttrDomain::Face, poly::MATERIAL, AttrValue::Int(0), &all);
        prop_assert!(matches!(r, Err(GeomError::Argument(_))));
        check(&m);
        prop_assert!(m.group("no_such_group").is_err());
    }
}
