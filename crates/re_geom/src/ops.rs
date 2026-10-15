//! Operations on whole meshes: normals, transforms and merging.
//!
//! Every function processes vertices and triangles in index order with
//! scalar arithmetic, so the result is bit-identical on every platform.

use glam::{Quat, Vec3};
use re_types::mesh::MeshData;

use crate::error::GeomError;
use crate::math::{Rotation, cross, normalize_or};

/// Recomputes every vertex normal as the area-weighted average of the normals
/// of the triangles that use it. A vertex used by no triangle (or only by
/// degenerate ones) gets `+Y`. Indices out of range are skipped.
pub fn compute_normals(mesh: &mut MeshData) {
    let mut acc = vec![Vec3::ZERO; mesh.vertices.len()];
    for tri in mesh.indices.as_chunks::<3>().0 {
        let [a, b, c] = [tri[0] as usize, tri[1] as usize, tri[2] as usize];
        if a >= acc.len() || b >= acc.len() || c >= acc.len() {
            continue;
        }
        let pa = Vec3::from_array(mesh.vertices[a].pos);
        let pb = Vec3::from_array(mesh.vertices[b].pos);
        let pc = Vec3::from_array(mesh.vertices[c].pos);
        // Twice the triangle's area times its unit normal.
        let n = cross(pb - pa, pc - pa);
        for i in [a, b, c] {
            acc[i] += n;
        }
    }
    for (v, n) in mesh.vertices.iter_mut().zip(acc) {
        v.normal = normalize_or(n, Vec3::Y).to_array();
    }
}

/// Scales, then rotates, then translates every vertex. Normals are
/// transformed by the inverse transpose and renormalised. A scale with an odd
/// number of negative axes mirrors the mesh, so the triangle winding is
/// flipped to keep front faces counter-clockwise.
pub fn transform(mesh: &mut MeshData, translation: Vec3, rotation: Quat, scale: Vec3) {
    let r = Rotation::from_quat(rotation);
    let inv = |s: f32| if s == 0.0 { 0.0 } else { 1.0 / s };
    let inv_scale = Vec3::new(inv(scale.x), inv(scale.y), inv(scale.z));
    for v in &mut mesh.vertices {
        let p = Vec3::from_array(v.pos) * scale;
        v.pos = (r.apply(p) + translation).to_array();
        let n = r.apply(Vec3::from_array(v.normal) * inv_scale);
        v.normal = normalize_or(n, Vec3::Y).to_array();
    }
    let negative = [scale.x, scale.y, scale.z]
        .iter()
        .filter(|s| **s < 0.0)
        .count();
    if negative % 2 == 1 {
        for tri in mesh.indices.as_chunks_mut::<3>().0 {
            tri.swap(1, 2);
        }
    }
}

/// One mesh holding every input's vertices and triangles, in input order.
pub fn merge<'a>(meshes: impl IntoIterator<Item = &'a MeshData>) -> Result<MeshData, GeomError> {
    let meshes: Vec<&MeshData> = meshes.into_iter().collect();
    let total: u64 = meshes.iter().map(|m| m.vertices.len() as u64).sum();
    if total > u64::from(u32::MAX) {
        return Err(GeomError::TooLarge(total));
    }
    let mut out = MeshData {
        vertices: Vec::with_capacity(total as usize),
        indices: Vec::with_capacity(meshes.iter().map(|m| m.indices.len()).sum()),
    };
    for m in meshes {
        let base = out.vertices.len() as u32;
        out.vertices.extend_from_slice(&m.vertices);
        out.indices.extend(m.indices.iter().map(|i| i + base));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives;

    #[test]
    fn merge_offsets_indices() {
        let a = primitives::cuboid(Vec3::ONE).unwrap();
        let b = primitives::plane(2.0, 2.0, 1).unwrap();
        let m = merge([&a, &b]).unwrap();
        assert_eq!(m.vertices.len(), a.vertices.len() + b.vertices.len());
        assert!(m.is_well_formed());
        assert_eq!(m.indices[a.indices.len()], 24);
    }

    #[test]
    fn transform_moves_and_scales() {
        let mut m = primitives::cuboid(Vec3::splat(0.5)).unwrap();
        transform(
            &mut m,
            Vec3::new(10.0, 0.0, 0.0),
            Quat::IDENTITY,
            Vec3::new(2.0, 1.0, 1.0),
        );
        let b = m.aabb().unwrap();
        assert_eq!(b.min, Vec3::new(9.0, -0.5, -0.5));
        assert_eq!(b.max, Vec3::new(11.0, 0.5, 0.5));
    }

    #[test]
    fn mirroring_flips_winding() {
        let mut m = primitives::plane(1.0, 1.0, 1).unwrap();
        transform(
            &mut m,
            Vec3::ZERO,
            Quat::IDENTITY,
            Vec3::new(-1.0, 1.0, 1.0),
        );
        let mut again = m.clone();
        compute_normals(&mut again);
        // Normals from the flipped winding still point up.
        assert!(again.vertices.iter().all(|v| v.normal[1] > 0.99));
    }

    #[test]
    fn normals_of_a_flat_grid_point_up() {
        let mut m = primitives::plane(4.0, 4.0, 3).unwrap();
        for v in &mut m.vertices {
            v.normal = [0.0; 3];
        }
        compute_normals(&mut m);
        for v in &m.vertices {
            assert!((Vec3::from_array(v.normal) - Vec3::Y).length() < 1e-6);
        }
    }
}
