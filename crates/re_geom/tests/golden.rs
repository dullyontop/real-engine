//! Golden hashes: the exact bytes of each primitive and of a heightfield mesh.
//! They change only when the geometry code changes on purpose; update them
//! then, and say why in the commit.

use glam::{Quat, Vec3};
use re_geom::{Heightfield, capsule, cuboid, cylinder, merge, plane, transform, uv_sphere};
use re_types::mesh::MeshData;

fn hash(mesh: &MeshData) -> String {
    let mut h = blake3::Hasher::new();
    for v in &mesh.vertices {
        for f in v.pos.iter().chain(&v.normal).chain(&v.uv).chain(&v.color) {
            h.update(&f.to_bits().to_le_bytes());
        }
    }
    for i in &mesh.indices {
        h.update(&i.to_le_bytes());
    }
    h.finalize().to_hex()[..16].to_string()
}

fn heightfield() -> Heightfield {
    let n = 17u32;
    let samples = (0..n * n)
        .map(|i| {
            let (x, z) = ((i % n) as f32, (i / n) as f32);
            (x * 0.37 - z * 0.21) * 0.5 + ((i * 7919) % 13) as f32 * 0.05
        })
        .collect();
    Heightfield::centered(n, 0.75, samples).unwrap()
}

#[test]
fn golden_hashes() {
    let mut moved = cuboid(Vec3::new(0.5, 1.0, 0.25)).unwrap();
    transform(
        &mut moved,
        Vec3::new(1.0, 2.0, 3.0),
        re_geom::math::quat_from_euler_deg(Vec3::new(10.0, 20.0, 30.0)),
        Vec3::new(1.0, 2.0, -1.0),
    );
    let merged = merge([&plane(2.0, 2.0, 2).unwrap(), &moved]).unwrap();
    let got = [
        ("box", hash(&cuboid(Vec3::new(0.5, 1.0, 0.25)).unwrap())),
        ("sphere", hash(&uv_sphere(1.0, 16, 8).unwrap())),
        ("cylinder", hash(&cylinder(0.5, 1.0, 12).unwrap())),
        ("capsule", hash(&capsule(0.4, 0.5, 12, 4).unwrap())),
        ("plane", hash(&plane(4.0, 3.0, 4).unwrap())),
        ("heightfield", hash(&heightfield().to_mesh())),
        ("transformed", hash(&moved)),
        ("merged", hash(&merged)),
    ];
    let want = [
        ("box", "edd9220a0cf2a613"),
        ("sphere", "5973811e6b8e8a59"),
        ("cylinder", "9a10d485fe33a5fd"),
        ("capsule", "277aeec5f3af8b47"),
        ("plane", "5cb90db20f67dbd8"),
        ("heightfield", "41df1eadd42de328"),
        ("transformed", "aa6e682c456063bd"),
        ("merged", "394662d9ed2d8db3"),
    ];
    let got: Vec<(&str, &str)> = got.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(got, want, "geometry bytes changed");
}

#[test]
fn identity_transform_keeps_positions() {
    let a = uv_sphere(1.0, 8, 6).unwrap();
    let mut b = a.clone();
    transform(&mut b, Vec3::ZERO, Quat::IDENTITY, Vec3::ONE);
    assert_eq!(a.indices, b.indices);
    for (va, vb) in a.vertices.iter().zip(&b.vertices) {
        assert_eq!(va.pos, vb.pos);
        // Normals are renormalised, which may move the last bit.
        assert!((Vec3::from_array(va.normal) - Vec3::from_array(vb.normal)).length() < 1e-6);
    }
}

fn shapes_hash(shapes: &re_geom::Shapes) -> String {
    let mut h = blake3::Hasher::new();
    for s in &shapes.shapes {
        for c in std::iter::once(&s.outer).chain(&s.holes) {
            for p in c {
                h.update(&p[0].to_bits().to_le_bytes());
                h.update(&p[1].to_bits().to_le_bytes());
            }
            h.update(b"|");
        }
        h.update(b"/");
    }
    h.finalize().to_hex()[..16].to_string()
}

/// Polygon and road outputs (ADR-0043 §5). i_overlay and earcut are pinned
/// exactly; an upgrade that changes any of these needs version bumps for the
/// node kinds it changes.
#[test]
fn polygon_and_road_golden_hashes() {
    use re_geom::polygon::{BoolOp, Cap, Join, Stroke, stroke, triangulate};
    use re_geom::road::{ControlPoint, RoadStyle, footprint, road_mesh, spline};
    let square = |x: f64, z: f64, w: f64| vec![[x, z], [x + w, z], [x + w, z + w], [x, z + w]];
    let a = re_geom::Shapes::polygon(square(0.0, 0.0, 10.0));
    let b = re_geom::Shapes::polygon(vec![[3.0, -2.0], [14.0, 5.0], [4.0, 12.0]]);
    let diff = a.boolean(&b, BoolOp::Difference).unwrap();
    let grown = b.offset(1.5, Join::Round).unwrap();
    let line = stroke(
        &[[0.0, 0.0], [10.0, 3.0], [20.0, -2.0]],
        Stroke {
            width: 4.0,
            join: Join::Round,
            cap: Cap::Round,
        },
        false,
    )
    .unwrap();
    let cp = |x: f64, y: f64, z: f64, bank: f64| ControlPoint {
        position: [x, y, z],
        width: 6.0,
        bank,
    };
    let r1 = spline(
        &[
            cp(-40.0, 0.0, -5.0, 0.0),
            cp(0.0, 1.0, 0.0, 4.0),
            cp(40.0, 2.0, 10.0, 0.0),
        ],
        1.0,
        false,
    )
    .unwrap();
    let r2 = spline(
        &[cp(5.0, 3.0, -40.0, 0.0), cp(-5.0, 1.0, 40.0, 0.0)],
        1.0,
        false,
    )
    .unwrap();
    let style = RoadStyle {
        junction_radius: 9.0,
        snap: 3.0,
        bank_fade: 8.0,
    };
    let road = road_mesh(&[r1.clone(), r2.clone()], &style).unwrap();
    let got = [
        ("difference", shapes_hash(&diff)),
        ("offset", shapes_hash(&grown)),
        ("stroke", shapes_hash(&line)),
        ("triangulated", hash(&triangulate(&diff, 0.5).unwrap())),
        (
            "footprint",
            shapes_hash(&footprint(&[r1, r2], 1.0).unwrap()),
        ),
        ("road", hash(&road.mesh)),
    ];
    let want: [(&str, &str); 6] = [
        ("difference", "dab692af6f3a5bd8"),
        ("offset", "bf668b9bc7c95d6f"),
        ("stroke", "730567d530cd692e"),
        ("triangulated", "c8e1074f214b089b"),
        ("footprint", "d04c951dfc4b2987"),
        ("road", "5d71273a7f49fa65"),
    ];
    let got: Vec<(&str, &str)> = got.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(got, want, "polygon or road bytes changed");
}

/// Every value of a polygon mesh, in order: positions, faces, corners, each
/// domain's columns by name, the materials, the collider and visibility.
fn poly_hash(m: &re_geom::poly::PolyMesh) -> String {
    use re_geom::poly::AttrValue;
    let mut h = blake3::Hasher::new();
    let floats = |h: &mut blake3::Hasher, fs: &[f32]| {
        for f in fs {
            h.update(&f.to_bits().to_le_bytes());
        }
    };
    for p in &m.positions {
        floats(&mut h, &p.to_array());
    }
    for i in m.face_start.iter().chain(&m.corner_point) {
        h.update(&i.to_le_bytes());
    }
    for attrs in [&m.points, &m.corners, &m.faces] {
        h.update(b"|");
        for (name, col) in attrs.iter() {
            h.update(name.as_bytes());
            h.update(col.ty().name().as_bytes());
            for i in 0..col.len() {
                match col.get(i) {
                    AttrValue::Float(v) => floats(&mut h, &[v]),
                    AttrValue::Int(v) => {
                        h.update(&v.to_le_bytes());
                    }
                    AttrValue::Bool(v) => {
                        h.update(&[u8::from(v)]);
                    }
                    AttrValue::Vec2(v) => floats(&mut h, &v),
                    AttrValue::Vec3(v) => floats(&mut h, &v.to_array()),
                    AttrValue::Vec4(v) => floats(&mut h, &v),
                    AttrValue::Quat(v) => floats(&mut h, &v.to_array()),
                }
            }
        }
    }
    for path in &m.materials {
        h.update(path.as_bytes());
        h.update(b"\0");
    }
    h.update(format!("{:?}{}", m.collider, m.visible).as_bytes());
    h.finalize().to_hex()[..16].to_string()
}

/// The modelling kernel (ADR-0062 §12): each primitive and operation on fixed
/// inputs, as the polygon mesh and as its triangulation. Any change to these
/// bytes changes the output of a `poly.*` node kind, so it needs that kind's
/// version bumped (ADR-0043 §1).
#[test]
fn poly_golden_hashes() {
    use re_geom::math::quat_from_euler_deg;
    use re_geom::poly::{self, AttrDomain, AttrValue, Axis};
    let rounded = poly::rounded_box(Vec3::new(2.0, 1.0, 1.5), 0.2, 4).unwrap();
    let stairs = poly::stairs(5, 1.2, 0.18, 0.28).unwrap();
    let ico = poly::icosphere(1.0, 2).unwrap();
    let cone = poly::cone(0.5, 0.25, 1.0, 12, true).unwrap();
    let torus = poly::torus(1.0, 0.3, 12, 8).unwrap();
    let block = poly::rounded_box(Vec3::ONE, 0.0, 2).unwrap();
    let moved = poly::transform(
        &block,
        Vec3::new(1.0005, 0.0, 0.0),
        Quat::IDENTITY,
        Vec3::ONE,
    )
    .unwrap();
    let mut painted = rounded.clone();
    let top = painted.group("top").unwrap();
    painted.set_material(&top, "materials/roof.mat").unwrap();
    painted
        .fill_attribute(
            AttrDomain::Face,
            "color",
            AttrValue::Vec3(Vec3::new(0.8, 0.3, 0.2)),
            &top,
        )
        .unwrap();
    let mut coloured_cone = cone.clone();
    let side = coloured_cone.group("side").unwrap();
    coloured_cone
        .set_material(&side, "materials/metal.mat")
        .unwrap();
    let cases = [
        (
            "box",
            poly::rounded_box(Vec3::new(1.0, 2.0, 3.0), 0.0, 2).unwrap(),
        ),
        ("rounded box", rounded.clone()),
        ("icosphere", ico.clone()),
        ("cone", cone.clone()),
        ("cylinder", poly::cone(0.5, 0.5, 2.0, 16, false).unwrap()),
        ("torus", torus.clone()),
        ("stairs", stairs.clone()),
        ("arch", poly::arch(2.0, 0.8, 0.3, 0.5, 8).unwrap()),
        (
            "transform",
            poly::transform(
                &rounded,
                Vec3::new(1.0, 2.0, 3.0),
                quat_from_euler_deg(Vec3::new(10.0, 20.0, 30.0)),
                Vec3::new(1.0, 2.0, -1.0),
            )
            .unwrap(),
        ),
        ("mirror", poly::mirror(&stairs, Axis::X, 0.6, true).unwrap()),
        (
            "weld",
            poly::weld(&poly::merge([&block, &moved]).unwrap(), 0.001).unwrap(),
        ),
        (
            "merge",
            poly::merge([&ico, &coloured_cone, &painted]).unwrap(),
        ),
        ("flip", torus.flip("").unwrap()),
        ("material", painted),
        (
            "from mesh",
            poly::from_mesh(&uv_sphere(1.0, 12, 6).unwrap()).unwrap(),
        ),
    ];
    let got: Vec<(&str, String)> = cases
        .iter()
        .map(|(name, m)| {
            m.validate().unwrap();
            (
                *name,
                format!("{} {}", poly_hash(m), hash(&m.to_mesh().unwrap())),
            )
        })
        .collect();
    let want: [(&str, &str); 15] = [
        ("box", "c93b2cf3a94edbb7 4cf9915e89caa76b"),
        ("rounded box", "6bae919239e70cab 1bf7ff9af4f6b73e"),
        ("icosphere", "17e96217908cbc0b e0d6dd46fc8d3b3c"),
        ("cone", "59e441ea33e9a563 d40edd4374a33f8e"),
        ("cylinder", "d67c565aabc7ad60 3f25263bcb9a1dee"),
        ("torus", "dac23c182de296b8 247150f7c398454e"),
        ("stairs", "765af52b137ad6ec 026bce30e153c895"),
        ("arch", "a4f50dbac285c73e d2779f87197290ff"),
        ("transform", "7acd7144fbc3c5bd 5d1103c83427dd8c"),
        ("mirror", "c51f3cd810756751 852b0b69a257137c"),
        ("weld", "fa39cff0c3cc3e44 4160741660a3bad8"),
        ("merge", "c0426bc5c3a083b3 42db748642995633"),
        ("flip", "b599441ab0e646c0 631f969c5279b91e"),
        ("material", "4474104f22288550 5e14fb086927a051"),
        ("from mesh", "4024c1b6b47cf81b 7bca241760ad1ab1"),
    ];
    let got: Vec<(&str, &str)> = got.iter().map(|(k, v)| (*k, v.as_str())).collect();
    assert_eq!(got, want, "modelling bytes changed");
}
