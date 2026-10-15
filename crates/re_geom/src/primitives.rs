//! Mesh primitives: box, sphere, cylinder, capsule and plane.
//!
//! Every primitive is centred on the origin, with +Y up, counter-clockwise
//! front faces, unit normals, UVs and white vertex colours. Angles come from
//! [`sin_cos`](crate::math::sin_cos), so the vertex data is bit-identical on
//! every platform.

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use glam::Vec3;
use re_types::mesh::{MeshData, Vertex};

use crate::error::GeomError;
use crate::heightfield::grid_indices;
use crate::math::{cross, sin_cos};

/// The most segments or rings a primitive may have.
pub const MAX_TESSELLATION: u32 = 1024;

fn vertex(pos: Vec3, normal: Vec3, uv: [f32; 2]) -> Vertex {
    Vertex {
        pos: pos.to_array(),
        normal: normal.to_array(),
        uv,
        color: [1.0; 4],
    }
}

fn positive(shape: &'static str, what: &str, v: f32) -> Result<(), GeomError> {
    if v.is_finite() && v > 0.0 {
        Ok(())
    } else {
        Err(GeomError::Primitive {
            shape,
            message: format!("{what} must be finite and positive, got {v}"),
        })
    }
}

fn tessellation(shape: &'static str, what: &str, v: u32, min: u32) -> Result<(), GeomError> {
    if (min..=MAX_TESSELLATION).contains(&v) {
        Ok(())
    } else {
        Err(GeomError::Primitive {
            shape,
            message: format!("{what} must be {min} to {MAX_TESSELLATION}, got {v}"),
        })
    }
}

/// A box with the given half extents: 24 vertices (4 per face, so each face
/// has flat normals) and 12 triangles.
pub fn cuboid(half_extents: Vec3) -> Result<MeshData, GeomError> {
    for (axis, v) in [
        ("x", half_extents.x),
        ("y", half_extents.y),
        ("z", half_extents.z),
    ] {
        positive("box", &format!("half extent {axis}"), v)?;
    }
    // (face normal, face "up" direction); "right" is up × normal.
    let faces = [
        (Vec3::X, Vec3::Y),
        (Vec3::NEG_X, Vec3::Y),
        (Vec3::Y, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::Z),
        (Vec3::Z, Vec3::Y),
        (Vec3::NEG_Z, Vec3::Y),
    ];
    let mut mesh = MeshData::default();
    for (n, v) in faces {
        let u = cross(v, n);
        let base = mesh.vertices.len() as u32;
        for (su, sv) in [(-1.0f32, -1.0f32), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
            let p = (n + u * su + v * sv) * half_extents;
            mesh.vertices
                .push(vertex(p, n, [(su + 1.0) * 0.5, 1.0 - (sv + 1.0) * 0.5]));
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Ok(mesh)
}

/// One ring of a surface of revolution: its height, its radius and the
/// normal's vertical and radial parts.
struct Ring {
    y: f32,
    radius: f32,
    ny: f64,
    nr: f64,
    v: f32,
}

/// A surface of revolution about +Y from rings listed top to bottom, with
/// `segments + 1` vertices per ring (the seam is duplicated for UVs).
/// Triangles touching a zero-radius ring are left out where they would be
/// degenerate.
fn revolve(rings: &[Ring], segments: u32, mesh: &mut MeshData) {
    let base = mesh.vertices.len() as u32;
    let cols = segments + 1;
    for ring in rings {
        for j in 0..cols {
            let (s, c) = sin_cos(TAU * f64::from(j) / f64::from(segments));
            let p = Vec3::new(s as f32 * ring.radius, ring.y, c as f32 * ring.radius);
            let n = Vec3::new((ring.nr * s) as f32, ring.ny as f32, (ring.nr * c) as f32);
            mesh.vertices
                .push(vertex(p, n, [j as f32 / segments as f32, ring.v]));
        }
    }
    for i in 0..rings.len() as u32 - 1 {
        let top_point = rings[i as usize].radius == 0.0;
        let bottom_point = rings[i as usize + 1].radius == 0.0;
        for j in 0..segments {
            let a = base + i * cols + j;
            let b = a + cols;
            if !top_point {
                mesh.indices.extend_from_slice(&[a, b, a + 1]);
            }
            if !bottom_point {
                mesh.indices.extend_from_slice(&[a + 1, b, b + 1]);
            }
        }
    }
}

/// A UV sphere with `segments` around and `rings` from pole to pole.
pub fn uv_sphere(radius: f32, segments: u32, rings: u32) -> Result<MeshData, GeomError> {
    positive("sphere", "radius", radius)?;
    tessellation("sphere", "segments", segments, 3)?;
    tessellation("sphere", "rings", rings, 2)?;
    let list: Vec<Ring> = (0..=rings)
        .map(|i| {
            let phi = PI * f64::from(i) / f64::from(rings);
            let (s, c) = sin_cos(phi);
            // Exact zeros at the poles keep the pole vertices on the axis.
            let s = if i == 0 || i == rings { 0.0 } else { s };
            Ring {
                y: c as f32 * radius,
                radius: s as f32 * radius,
                ny: c,
                nr: s,
                v: i as f32 / rings as f32,
            }
        })
        .collect();
    let mut mesh = MeshData::default();
    revolve(&list, segments, &mut mesh);
    Ok(mesh)
}

/// A capped cylinder along Y, from `-half_height` to `+half_height`.
pub fn cylinder(radius: f32, half_height: f32, segments: u32) -> Result<MeshData, GeomError> {
    positive("cylinder", "radius", radius)?;
    positive("cylinder", "half height", half_height)?;
    tessellation("cylinder", "segments", segments, 3)?;
    let mut mesh = MeshData::default();
    let side = [
        Ring {
            y: half_height,
            radius,
            ny: 0.0,
            nr: 1.0,
            v: 0.0,
        },
        Ring {
            y: -half_height,
            radius,
            ny: 0.0,
            nr: 1.0,
            v: 1.0,
        },
    ];
    revolve(&side, segments, &mut mesh);
    for (y, n) in [(half_height, Vec3::Y), (-half_height, Vec3::NEG_Y)] {
        let centre = mesh.vertices.len() as u32;
        mesh.vertices
            .push(vertex(Vec3::new(0.0, y, 0.0), n, [0.5, 0.5]));
        for j in 0..=segments {
            let (s, c) = sin_cos(TAU * f64::from(j) / f64::from(segments));
            let (s, c) = (s as f32, c as f32);
            mesh.vertices.push(vertex(
                Vec3::new(s * radius, y, c * radius),
                n,
                [0.5 + s * 0.5, 0.5 - c * 0.5],
            ));
        }
        for j in 0..segments {
            let (a, b) = (centre + 1 + j, centre + 2 + j);
            if n.y > 0.0 {
                mesh.indices.extend_from_slice(&[centre, a, b]);
            } else {
                mesh.indices.extend_from_slice(&[centre, b, a]);
            }
        }
    }
    Ok(mesh)
}

/// A capsule along Y: a cylinder of `half_height` (which may be 0) capped by
/// two hemispheres of `radius`, with `segments` around and `rings` per
/// hemisphere. Its total height is `2 * (half_height + radius)`, as for
/// `re_types::physics::ShapeDesc::Capsule`.
pub fn capsule(
    radius: f32,
    half_height: f32,
    segments: u32,
    rings: u32,
) -> Result<MeshData, GeomError> {
    positive("capsule", "radius", radius)?;
    if !(half_height.is_finite() && half_height >= 0.0) {
        return Err(GeomError::Primitive {
            shape: "capsule",
            message: format!("half height must be finite and not negative, got {half_height}"),
        });
    }
    tessellation("capsule", "segments", segments, 3)?;
    tessellation("capsule", "rings", rings, 1)?;
    let total = 2.0 * (half_height + radius);
    let mut list = Vec::with_capacity(2 * rings as usize + 2);
    for (from, centre) in [(0.0, half_height), (FRAC_PI_2, -half_height)] {
        for i in 0..=rings {
            if from > 0.0 && i == 0 && half_height == 0.0 {
                // Without a cylinder part the two equators coincide.
                continue;
            }
            let phi = from + FRAC_PI_2 * f64::from(i) / f64::from(rings);
            let (s, c) = sin_cos(phi);
            let pole = (from == 0.0 && i == 0) || (from > 0.0 && i == rings);
            let s = if pole { 0.0 } else { s };
            let y = centre + c as f32 * radius;
            list.push(Ring {
                y,
                radius: s as f32 * radius,
                ny: c,
                nr: s,
                v: (total * 0.5 - y) / total,
            });
        }
    }
    let mut mesh = MeshData::default();
    revolve(&list, segments, &mut mesh);
    Ok(mesh)
}

/// A flat grid in the XZ plane facing +Y, `size_x` by `size_z` metres, with
/// `subdivisions` cells per side.
pub fn plane(size_x: f32, size_z: f32, subdivisions: u32) -> Result<MeshData, GeomError> {
    positive("plane", "size x", size_x)?;
    positive("plane", "size z", size_z)?;
    tessellation("plane", "subdivisions", subdivisions, 1)?;
    let n = subdivisions + 1;
    let mut vertices = Vec::with_capacity((n * n) as usize);
    for z in 0..n {
        for x in 0..n {
            let (u, v) = (
                x as f32 / subdivisions as f32,
                z as f32 / subdivisions as f32,
            );
            vertices.push(vertex(
                Vec3::new((u - 0.5) * size_x, 0.0, (v - 0.5) * size_z),
                Vec3::Y,
                [u, v],
            ));
        }
    }
    Ok(MeshData {
        vertices,
        indices: grid_indices(n, n),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::compute_normals;

    /// Every triangle faces the same way as the stored normals of its corners.
    fn outward(mesh: &MeshData) {
        assert!(mesh.is_well_formed());
        for tri in mesh.indices.as_chunks::<3>().0 {
            let p: Vec<Vec3> = tri
                .iter()
                .map(|&i| Vec3::from_array(mesh.vertices[i as usize].pos))
                .collect();
            let face = cross(p[1] - p[0], p[2] - p[0]);
            assert!(face.length() > 0.0, "degenerate triangle {tri:?}");
            for &i in tri {
                let n = Vec3::from_array(mesh.vertices[i as usize].normal);
                assert!(face.dot(n) > 0.0, "triangle {tri:?} faces inwards");
            }
        }
        for v in &mesh.vertices {
            assert!((Vec3::from_array(v.normal).length() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn every_primitive_is_closed_and_outward() {
        outward(&cuboid(Vec3::new(1.0, 2.0, 3.0)).unwrap());
        outward(&uv_sphere(1.5, 12, 8).unwrap());
        outward(&cylinder(0.5, 2.0, 10).unwrap());
        outward(&capsule(0.5, 1.0, 10, 4).unwrap());
        outward(&capsule(0.5, 0.0, 3, 1).unwrap());
        outward(&plane(4.0, 2.0, 3).unwrap());
    }

    #[test]
    fn sizes() {
        let b = cuboid(Vec3::new(1.0, 2.0, 3.0)).unwrap().aabb().unwrap();
        assert_eq!(b.max, Vec3::new(1.0, 2.0, 3.0));
        let s = uv_sphere(2.0, 16, 8).unwrap().aabb().unwrap();
        assert!((s.max.y - 2.0).abs() < 1e-6 && (s.min.y + 2.0).abs() < 1e-6);
        let c = capsule(0.5, 1.0, 8, 4).unwrap().aabb().unwrap();
        assert!((c.max.y - 1.5).abs() < 1e-6 && (c.min.y + 1.5).abs() < 1e-6);
        let p = plane(4.0, 2.0, 2).unwrap().aabb().unwrap();
        assert_eq!(
            (p.min, p.max),
            (Vec3::new(-2.0, 0.0, -1.0), Vec3::new(2.0, 0.0, 1.0))
        );
    }

    #[test]
    fn sphere_normals_agree_with_computed_ones() {
        let s = uv_sphere(1.0, 24, 12).unwrap();
        let mut c = s.clone();
        compute_normals(&mut c);
        for (i, (a, b)) in s.vertices.iter().zip(&c.vertices).enumerate() {
            // One copy of each pole is used by no triangle.
            if !s.indices.contains(&(i as u32)) {
                continue;
            }
            // Seam and pole vertices are not shared, so allow some slack.
            assert!(Vec3::from_array(a.normal).dot(Vec3::from_array(b.normal)) > 0.9);
        }
    }

    #[test]
    fn refuses_bad_parameters() {
        assert!(cuboid(Vec3::new(1.0, 0.0, 1.0)).is_err());
        assert!(uv_sphere(-1.0, 8, 8).is_err());
        assert!(uv_sphere(1.0, 2, 8).is_err());
        assert!(cylinder(1.0, 1.0, MAX_TESSELLATION + 1).is_err());
        assert!(capsule(1.0, -0.5, 8, 2).is_err());
        assert!(plane(1.0, f32::NAN, 1).is_err());
        assert_eq!(
            plane(1.0, 1.0, 0).unwrap_err().code(),
            crate::codes::INVALID_PRIMITIVE
        );
    }
}
