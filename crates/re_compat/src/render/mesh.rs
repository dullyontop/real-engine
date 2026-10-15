//! Part meshes: blocks, balls, cylinders, wedges and corner wedges built at
//! their size, so a material's pattern keeps its scale in studs.
//!
//! Texture coordinates run one unit per [`UV_STUDS`] studs over each face, in
//! the part's own frame. A part drawn alone has its own mesh (white vertices;
//! its colour is in the material); the parts of a static cell are merged in
//! world space with their colour in the vertices.

use glam::{Affine3A, Vec2, Vec3};
use re_physics::{PartGeom, PartShape};
use re_types::mesh::{MeshData, Vertex};

/// Studs covered by one unit of texture coordinate.
pub const UV_STUDS: f32 = 8.0;

const SEGMENTS: u32 = 24;
const RINGS: u32 = 12;

/// Appends the triangles of `geom` placed by `world` with vertex colour
/// `color` (linear RGBA).
pub(crate) fn append(out: &mut MeshData, geom: &PartGeom, world: &Affine3A, color: [f32; 4]) {
    let start = out.vertices.len();
    match geom.shape {
        PartShape::Ball => ball(out, geom.size, color),
        PartShape::Cylinder => cylinder(out, geom.size, color),
        _ => polyhedron(out, geom, color),
    }
    if *world != Affine3A::IDENTITY {
        let normal = world.matrix3;
        for v in &mut out.vertices[start..] {
            v.pos = world.transform_point3(Vec3::from_array(v.pos)).to_array();
            v.normal = (normal * glam::Vec3A::from_array(v.normal))
                .normalize_or_zero()
                .to_array();
        }
    }
}

/// The mesh of one part at the origin, in white.
pub(crate) fn part(geom: &PartGeom) -> MeshData {
    let mut m = MeshData::default();
    append(&mut m, geom, &Affine3A::IDENTITY, [1.0; 4]);
    m
}

fn vertex(pos: Vec3, normal: Vec3, uv: Vec2, color: [f32; 4]) -> Vertex {
    Vertex {
        pos: pos.to_array(),
        normal: normal.to_array(),
        uv: (uv / UV_STUDS).to_array(),
        color,
    }
}

/// Two axes in the plane of a face with normal `n`, for its texture
/// coordinates: the dominant axis of `n` picks a pair of part axes.
fn face_axes(n: Vec3) -> (Vec3, Vec3) {
    let a = n.abs();
    if a.y >= a.x && a.y >= a.z {
        (Vec3::X, Vec3::Z)
    } else if a.x >= a.z {
        (Vec3::Z, -Vec3::Y)
    } else {
        (Vec3::X, -Vec3::Y)
    }
}

/// A convex polyhedron from its corners and bounding planes: each plane's
/// corners, in order around its normal, fanned into triangles.
fn polyhedron(out: &mut MeshData, geom: &PartGeom, color: [f32; 4]) {
    let corners = geom.vertices();
    let eps = 1e-4 * geom.size.max_element().max(1.0);
    for (n, d) in geom.planes() {
        let mut on: Vec<Vec3> = corners
            .iter()
            .copied()
            .filter(|c| (n.dot(*c) - d).abs() <= eps)
            .collect();
        if on.len() < 3 {
            continue;
        }
        let centre = on.iter().copied().sum::<Vec3>() / on.len() as f32;
        let (u, _) = n.any_orthonormal_pair();
        let v = n.cross(u);
        on.sort_by(|a, b| {
            let pa = *a - centre;
            let pb = *b - centre;
            pa.dot(v)
                .atan2(pa.dot(u))
                .total_cmp(&pb.dot(v).atan2(pb.dot(u)))
        });
        let (ta, tb) = face_axes(n);
        let base = out.vertices.len() as u32;
        for c in &on {
            out.vertices
                .push(vertex(*c, n, Vec2::new(c.dot(ta), c.dot(tb)), color));
        }
        for i in 1..on.len() as u32 - 1 {
            out.indices
                .extend_from_slice(&[base, base + i, base + i + 1]);
        }
    }
}

/// A sphere whose diameter is the smallest side of `size`.
fn ball(out: &mut MeshData, size: Vec3, color: [f32; 4]) {
    let r = size.min_element() * 0.5;
    let base = out.vertices.len() as u32;
    let circumference = std::f32::consts::TAU * r;
    for ring in 0..=RINGS {
        let v = ring as f32 / RINGS as f32;
        let theta = v * std::f32::consts::PI;
        for seg in 0..=SEGMENTS {
            let u = seg as f32 / SEGMENTS as f32;
            let phi = u * std::f32::consts::TAU;
            let n = Vec3::new(
                theta.sin() * phi.cos(),
                theta.cos(),
                -theta.sin() * phi.sin(),
            );
            out.vertices.push(vertex(
                n * r,
                n,
                Vec2::new(u * circumference, v * circumference * 0.5),
                color,
            ));
        }
    }
    let row = SEGMENTS + 1;
    for ring in 0..RINGS {
        for seg in 0..SEGMENTS {
            let a = base + ring * row + seg;
            let b = a + row;
            out.indices
                .extend_from_slice(&[a, b, a + 1, a + 1, b, b + 1]);
        }
    }
}

/// A cylinder along X, as long as `size.x`, whose diameter is the smaller
/// of `size.y` and `size.z`.
fn cylinder(out: &mut MeshData, size: Vec3, color: [f32; 4]) {
    let r = size.y.min(size.z) * 0.5;
    let hx = size.x * 0.5;
    let circumference = std::f32::consts::TAU * r;
    let base = out.vertices.len() as u32;
    for seg in 0..=SEGMENTS {
        let u = seg as f32 / SEGMENTS as f32;
        let a = u * std::f32::consts::TAU;
        let n = Vec3::new(0.0, a.cos(), a.sin());
        for x in [-hx, hx] {
            out.vertices.push(vertex(
                Vec3::new(x, n.y * r, n.z * r),
                n,
                Vec2::new(u * circumference, x),
                color,
            ));
        }
    }
    for seg in 0..SEGMENTS {
        let a = base + seg * 2;
        out.indices
            .extend_from_slice(&[a, a + 2, a + 1, a + 1, a + 2, a + 3]);
    }
    for (x, n) in [(hx, Vec3::X), (-hx, -Vec3::X)] {
        let centre = out.vertices.len() as u32;
        out.vertices
            .push(vertex(Vec3::new(x, 0.0, 0.0), n, Vec2::ZERO, color));
        for seg in 0..=SEGMENTS {
            let a = seg as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let (y, z) = (a.cos() * r, a.sin() * r);
            out.vertices
                .push(vertex(Vec3::new(x, y, z), n, Vec2::new(z, -y), color));
        }
        for seg in 0..SEGMENTS {
            let (a, b) = (centre + 1 + seg, centre + 2 + seg);
            if n.x > 0.0 {
                out.indices.extend_from_slice(&[centre, a, b]);
            } else {
                out.indices.extend_from_slice(&[centre, b, a]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geom(shape: PartShape, size: Vec3) -> PartGeom {
        PartGeom::new(shape, size)
    }

    /// Every triangle faces away from the part's centre.
    fn outward(m: &MeshData, centre: Vec3) {
        assert!(m.is_well_formed());
        for t in m.indices.chunks(3) {
            let [a, b, c] =
                [t[0], t[1], t[2]].map(|i| Vec3::from_array(m.vertices[i as usize].pos));
            let n = (b - a).cross(c - a);
            if n.length_squared() < 1e-12 {
                continue;
            }
            assert!(
                n.dot((a + b + c) / 3.0 - centre) > 0.0,
                "inward triangle {t:?}"
            );
        }
    }

    #[test]
    fn polyhedra_have_their_faces_and_face_outward() {
        let s = Vec3::new(4.0, 1.0, 2.0);
        let block = part(&geom(PartShape::Block, s));
        assert_eq!((block.vertices.len(), block.indices.len()), (24, 36));
        outward(&block, Vec3::ZERO);
        let wedge = part(&geom(PartShape::Wedge, s));
        // Two rectangles, a sloped rectangle and two triangles.
        assert_eq!(wedge.vertices.len(), 4 * 3 + 3 * 2);
        outward(&wedge, geom(PartShape::Wedge, s).centroid());
        let corner = part(&geom(PartShape::CornerWedge, s));
        outward(&corner, geom(PartShape::CornerWedge, s).centroid());
        let aabb = block.aabb().unwrap();
        assert_eq!(aabb.max, Vec3::new(2.0, 0.5, 1.0));
    }

    #[test]
    fn round_parts_fit_their_size_and_face_outward() {
        let ball = part(&geom(PartShape::Ball, Vec3::new(3.0, 2.0, 5.0)));
        let b = ball.aabb().unwrap();
        assert!((b.max.y - 1.0).abs() < 1e-5 && (b.max.x - 1.0).abs() < 1e-5);
        outward(&ball, Vec3::ZERO);
        let cyl = part(&geom(PartShape::Cylinder, Vec3::new(6.0, 2.0, 2.0)));
        let c = cyl.aabb().unwrap();
        assert!((c.max.x - 3.0).abs() < 1e-5 && (c.max.y - 1.0).abs() < 1e-5);
        outward(&cyl, Vec3::ZERO);
    }

    #[test]
    fn placed_parts_move_and_keep_unit_normals() {
        let mut m = MeshData::default();
        let world = Affine3A::from_rotation_translation(
            glam::Quat::from_rotation_y(0.7),
            Vec3::new(10.0, 0.0, 0.0),
        );
        append(&mut m, &geom(PartShape::Block, Vec3::ONE), &world, [0.5; 4]);
        let b = m.aabb().unwrap();
        assert!((b.center() - Vec3::new(10.0, 0.0, 0.0)).length() < 1e-5);
        for v in &m.vertices {
            assert!((Vec3::from_array(v.normal).length() - 1.0).abs() < 1e-5);
            assert_eq!(v.color, [0.5; 4]);
        }
    }

    #[test]
    fn texture_coordinates_are_in_studs() {
        let m = part(&geom(PartShape::Block, Vec3::new(16.0, 1.0, 8.0)));
        let us: Vec<f32> = m.vertices.iter().map(|v| v.uv[0]).collect();
        let span = us.iter().copied().fold(f32::MIN, f32::max)
            - us.iter().copied().fold(f32::MAX, f32::min);
        assert!((span - 16.0 / UV_STUDS).abs() < 1e-5);
    }
}
