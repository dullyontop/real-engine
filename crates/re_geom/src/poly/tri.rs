//! Triangulation of a [`PolyMesh`] into `MeshData` parts (ADR-0062 §5).

use std::collections::HashMap;

use glam::Vec3;
use re_types::mesh::{MeshData, Vertex};

use super::{COLOR, Column, NORMAL, PolyMesh, UV, length64};
use crate::GeomError;
use crate::math::normalize_or;

/// A colour column's value at `i` as linear RGBA.
fn rgba(col: Option<&Column>, i: usize) -> Option<[f32; 4]> {
    match col? {
        Column::Vec3(v) => Some([v[i].x, v[i].y, v[i].z, 1.0]),
        Column::Vec4(v) => Some(v[i]),
        _ => None,
    }
}

pub(super) fn to_meshes(m: &PolyMesh) -> Result<Vec<MeshData>, GeomError> {
    let materials = m.face_materials();
    let uv = m.corners.vec2s(UV);
    let normal = m.corners.vec3s(NORMAL);
    let (c_color, p_color, f_color) = (
        m.corners.get(COLOR),
        m.points.get(COLOR),
        m.faces.get(COLOR),
    );
    let mut out = vec![MeshData::default(); m.part_count()];
    let mut seen: Vec<HashMap<[u32; 12], u32>> = vec![HashMap::new(); m.part_count()];
    let mut ids: Vec<u32> = Vec::new();
    let mut tris: Vec<[usize; 3]> = Vec::new();
    for (f, &material) in materials.iter().enumerate() {
        let part = material as usize;
        let r = m.face(f);
        let n64 = m.face_area_vector(f);
        let len = length64(n64);
        let face_n = Vec3::new(
            (n64[0] / len) as f32,
            (n64[1] / len) as f32,
            (n64[2] / len) as f32,
        );
        triangulate(m, f, n64, &mut tris).ok_or_else(|| {
            GeomError::Triangulation(format!(
                "face {f} could not be triangulated (its boundary crosses itself)"
            ))
        })?;
        let mesh = &mut out[part];
        ids.clear();
        for c in r {
            let p = m.corner_point[c] as usize;
            let n = match normal {
                Some(ns) => normalize_or(ns[c], face_n),
                None => face_n,
            };
            let color = rgba(c_color, c)
                .or_else(|| rgba(p_color, p))
                .or_else(|| rgba(f_color, f))
                .unwrap_or([1.0; 4]);
            let v = Vertex {
                pos: m.positions[p].to_array(),
                normal: n.to_array(),
                uv: uv.map_or([0.0; 2], |u| u[c]),
                color,
            };
            let mut bits = [0u32; 12];
            for (b, x) in bits
                .iter_mut()
                .zip(v.pos.iter().chain(&v.normal).chain(&v.uv).chain(&v.color))
            {
                *b = x.to_bits();
            }
            let id = *seen[part].entry(bits).or_insert_with(|| {
                mesh.vertices.push(v);
                mesh.vertices.len() as u32 - 1
            });
            ids.push(id);
        }
        for t in &tris {
            mesh.indices
                .extend_from_slice(&[ids[t[0]], ids[t[1]], ids[t[2]]]);
        }
    }
    Ok(out)
}

/// `b - a` in `f64`.
fn sub64(a: Vec3, b: Vec3) -> [f64; 3] {
    [
        f64::from(b.x) - f64::from(a.x),
        f64::from(b.y) - f64::from(a.y),
        f64::from(b.z) - f64::from(a.z),
    ]
}

/// Face `f`'s triangles as corner positions within the face, counter-
/// clockwise about its Newell vector `n`: a fan from the first corner when
/// every fan triangle faces `n`'s way, else ear clipping on the projection
/// along `n`'s largest axis. `None` when ear clipping finds no ear.
pub(super) fn triangulate(
    m: &PolyMesh,
    f: usize,
    n: [f64; 3],
    out: &mut Vec<[usize; 3]>,
) -> Option<()> {
    out.clear();
    let pts = m.face_points(f);
    let k = pts.len();
    if k == 3 {
        out.push([0, 1, 2]);
        return Some(());
    }
    let p = |i: usize| m.positions[pts[i] as usize];
    let fan = (1..k - 1).all(|i| {
        let (a, b) = (sub64(p(0), p(i)), sub64(p(0), p(i + 1)));
        let c = [
            a[1] * b[2] - a[2] * b[1],
            a[2] * b[0] - a[0] * b[2],
            a[0] * b[1] - a[1] * b[0],
        ];
        c[0] * n[0] + c[1] * n[1] + c[2] * n[2] > 0.0
    });
    if fan {
        out.extend((1..k - 1).map(|i| [0, i, i + 1]));
        return Some(());
    }
    // Project along the largest axis, keeping the polygon counter-clockwise.
    let (ax, ay, az) = (n[0].abs(), n[1].abs(), n[2].abs());
    let flat: Vec<[f64; 2]> = (0..k)
        .map(|i| {
            let q = p(i);
            let (x, y, z) = (f64::from(q.x), f64::from(q.y), f64::from(q.z));
            if ax >= ay && ax >= az {
                if n[0] >= 0.0 { [y, z] } else { [z, y] }
            } else if ay >= az {
                if n[1] >= 0.0 { [z, x] } else { [x, z] }
            } else if n[2] >= 0.0 {
                [x, y]
            } else {
                [y, x]
            }
        })
        .collect();
    ear_clip(&flat, out)
}

fn cross2(o: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Ear clipping of a counter-clockwise polygon, starting at its second
/// corner and stepping back after each ear, so the result depends only on
/// the corner order.
fn ear_clip(p: &[[f64; 2]], out: &mut Vec<[usize; 3]>) -> Option<()> {
    let k = p.len();
    let mut next: Vec<usize> = (0..k).map(|i| (i + 1) % k).collect();
    let mut prev: Vec<usize> = (0..k).map(|i| (i + k - 1) % k).collect();
    let mut left = k;
    let mut v = 1;
    let mut misses = 0;
    while left > 3 {
        let (a, c) = (prev[v], next[v]);
        if is_ear(p, &next, a, v, c) {
            out.push([a, v, c]);
            next[a] = c;
            prev[c] = a;
            left -= 1;
            v = a;
            misses = 0;
        } else {
            v = c;
            misses += 1;
            if misses > left {
                return None;
            }
        }
    }
    // The last triangle must face the polygon's way too: one that does not
    // means the boundary crosses itself.
    let (a, c) = (prev[v], next[v]);
    if cross2(p[a], p[v], p[c]) <= 0.0 {
        return None;
    }
    out.push([a, v, c]);
    Some(())
}

/// True if `a, b, c` is convex and no other remaining corner lies inside it
/// or on its edges.
fn is_ear(p: &[[f64; 2]], next: &[usize], a: usize, b: usize, c: usize) -> bool {
    if cross2(p[a], p[b], p[c]) <= 0.0 {
        return false;
    }
    let mut i = next[c];
    while i != a {
        let q = p[i];
        if cross2(p[a], p[b], q) >= 0.0
            && cross2(p[b], p[c], q) >= 0.0
            && cross2(p[c], p[a], q) >= 0.0
        {
            return false;
        }
        i = next[i];
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::poly::Column;

    /// An L-shaped hexagon in the XZ plane, facing +Y: its fan from the
    /// first corner folds over, so it is ear-clipped.
    fn ell(first: usize) -> PolyMesh {
        let ring = [
            [0.0, 0.0],
            [2.0, 0.0],
            [2.0, -1.0],
            [1.0, -1.0],
            [1.0, -2.0],
            [0.0, -2.0],
        ];
        let mut m = PolyMesh::new();
        for i in 0..6 {
            let [x, z] = ring[(i + first) % 6];
            m.positions.push(Vec3::new(x, 0.0, z));
        }
        // Counter-clockwise seen from +Y (x to the right, -z up the page).
        m.corner_point.extend([0, 1, 2, 3, 4, 5]);
        m.face_start.push(6);
        m
    }

    fn area(mesh: &MeshData) -> f32 {
        mesh.indices
            .chunks(3)
            .map(|t| {
                let p = |i: u32| Vec3::from_array(mesh.vertices[i as usize].pos);
                let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
                let n = crate::math::cross(b - a, c - a);
                assert!(n.y > 0.0, "every triangle faces up");
                n.length() * 0.5
            })
            .sum()
    }

    #[test]
    fn non_convex_faces_are_ear_clipped_facing_their_way() {
        for first in 0..6 {
            let m = ell(first);
            m.validate().unwrap();
            let parts = m.to_meshes().unwrap();
            assert_eq!(parts.len(), 1);
            assert_eq!(parts[0].triangle_count(), 4, "start {first}");
            assert_eq!(area(&parts[0]), 3.0, "start {first}");
        }
    }

    #[test]
    fn faces_split_by_material_and_share_vertices() {
        let mut m = crate::poly::tests::cube();
        m.materials = vec!["a".into(), "b".into()];
        m.faces
            .insert(super::super::MATERIAL, Column::Int(vec![0, 1, 0, 1, 0, 1]))
            .unwrap();
        m.validate().unwrap();
        let parts = m.to_meshes().unwrap();
        assert_eq!(parts.len(), 2);
        // Flat shading: each face's four corners are distinct vertices.
        assert_eq!(parts[0].vertices.len(), 12);
        assert_eq!(parts[0].triangle_count(), 6);
        assert_eq!(parts[1].triangle_count(), 6);
        // A smooth normal per point instead merges the corners of a point.
        let mut m = crate::poly::tests::cube();
        let normals: Vec<Vec3> = m
            .corner_point
            .iter()
            .map(|&p| (m.positions[p as usize] - Vec3::splat(0.5)).normalize())
            .collect();
        m.corners.insert(NORMAL, Column::Vec3(normals)).unwrap();
        let mesh = m.to_mesh().unwrap();
        assert_eq!(mesh.vertices.len(), 8);
        assert!(mesh.is_well_formed());
    }

    #[test]
    fn colours_prefer_corners_then_points_then_faces() {
        let mut m = crate::poly::tests::cube();
        m.faces
            .insert(COLOR, Column::Vec3(vec![Vec3::new(1.0, 0.0, 0.0); 6]))
            .unwrap();
        let mut pc = vec![[0.0, 1.0, 0.0, 1.0]; 8];
        pc[0] = [0.0, 0.0, 1.0, 0.5];
        m.points.insert(COLOR, Column::Vec4(pc)).unwrap();
        m.validate().unwrap();
        let mesh = m.to_mesh().unwrap();
        assert!(
            mesh.vertices
                .iter()
                .any(|v| v.color == [0.0, 0.0, 1.0, 0.5])
        );
        assert!(
            mesh.vertices
                .iter()
                .all(|v| v.color != [1.0, 0.0, 0.0, 1.0])
        );
        m.points.remove(COLOR);
        let mesh = m.to_mesh().unwrap();
        assert!(
            mesh.vertices
                .iter()
                .all(|v| v.color == [1.0, 0.0, 0.0, 1.0])
        );
    }

    #[test]
    fn a_self_crossing_face_is_refused() {
        // A lopsided bow tie: its first and third edges cross, and its two
        // halves wind opposite ways, so it still has an area.
        let mut m = PolyMesh::new();
        for p in [[0.0, 0.0], [2.0, 2.0], [2.0, 0.0], [0.0, 1.0]] {
            m.positions.push(Vec3::new(p[0], p[1], 0.0));
        }
        m.corner_point.extend_from_slice(&[0, 1, 2, 3]);
        m.face_start.push(4);
        m.validate().unwrap();
        assert!(matches!(m.to_meshes(), Err(GeomError::Triangulation(_))));
    }
}
