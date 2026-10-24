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
