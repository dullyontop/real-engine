//! Chunk meshes: indices with stitched edges, and vertices with normals that
//! agree across chunk borders.

use re_types::mesh::{MeshData, Vertex};

use super::lod::Edges;
use super::{CHUNK_CELLS, CHUNK_SAMPLES};
use crate::error::GeomError;
use crate::heightfield::grid_indices;

/// The index list of a chunk (`CHUNK_SAMPLES²` vertices, row-major, `x`
/// fastest), counter-clockwise seen from +Y.
///
/// Away from stitched edges each cell is split along the diagonal from
/// `(x, z)` to `(x + 1, z + 1)`, as [`Heightfield`](crate::Heightfield) and
/// Jolt split it. Each 2 × 2 block of cells touching a stitched edge is
/// instead a fan around its centre vertex that skips the odd vertices of the
/// stitched edges, so the edge is made of the coarser neighbour's vertices
/// only and no crack can open.
pub fn chunk_indices(edges: Edges) -> Vec<u32> {
    let n = CHUNK_SAMPLES;
    if edges.0 == 0 {
        return grid_indices(n, n);
    }
    let last = CHUNK_CELLS;
    let idx = |x: u32, z: u32| z * n + x;
    let mut out = Vec::with_capacity((CHUNK_CELLS * CHUNK_CELLS * 6) as usize);
    // A vertex on a stitched edge at an odd position is skipped.
    let skipped = |x: u32, z: u32| {
        (edges.has(Edges::NEG_X) && x == 0 && z % 2 == 1)
            || (edges.has(Edges::POS_X) && x == last && z % 2 == 1)
            || (edges.has(Edges::NEG_Z) && z == 0 && x % 2 == 1)
            || (edges.has(Edges::POS_Z) && z == last && x % 2 == 1)
    };
    for bz in (0..last).step_by(2) {
        for bx in (0..last).step_by(2) {
            let touches = (edges.has(Edges::NEG_X) && bx == 0)
                || (edges.has(Edges::POS_X) && bx + 2 == last)
                || (edges.has(Edges::NEG_Z) && bz == 0)
                || (edges.has(Edges::POS_Z) && bz + 2 == last);
            if !touches {
                for z in bz..bz + 2 {
                    for x in bx..bx + 2 {
                        let (i00, i10, i01, i11) =
                            (idx(x, z), idx(x + 1, z), idx(x, z + 1), idx(x + 1, z + 1));
                        out.extend_from_slice(&[i00, i01, i11, i00, i11, i10]);
                    }
                }
                continue;
            }
            // The block's boundary, walked so that each fan triangle is
            // counter-clockwise seen from +Y.
            let ring = [
                (0, 0),
                (0, 1),
                (0, 2),
                (1, 2),
                (2, 2),
                (2, 1),
                (2, 0),
                (1, 0),
            ];
            let ring: Vec<u32> = ring
                .iter()
                .map(|&(dx, dz)| (bx + dx, bz + dz))
                .filter(|&(x, z)| !skipped(x, z))
                .map(|(x, z)| idx(x, z))
                .collect();
            let centre = idx(bx + 1, bz + 1);
            for i in 0..ring.len() {
                out.extend_from_slice(&[centre, ring[i], ring[(i + 1) % ring.len()]]);
            }
        }
    }
    out
}

/// The mesh of one chunk.
///
/// `heights` holds `(CHUNK_SAMPLES + 2)²` samples: the chunk's samples with a
/// one-sample apron on every side, row-major. Sample `(i, j)` of the chunk
/// (apron sample `(i + 1, j + 1)`) sits at world position
/// `((origin[0] + i) as f32 × spacing, height, (origin[1] + j) as f32 × spacing)`,
/// the exact position rule of the module docs, so neighbouring chunks share
/// edge vertices bit for bit. Normals come from central differences over the
/// apron, so they agree across chunk borders too. UVs run from 0 to 1 across
/// the chunk.
pub fn chunk_mesh(
    heights: &[f32],
    origin: [i64; 2],
    spacing: f32,
    edges: Edges,
) -> Result<MeshData, GeomError> {
    let n = CHUNK_SAMPLES as usize;
    let w = n + 2;
    if heights.len() != w * w {
        return Err(GeomError::Heightfield(format!(
            "a chunk needs {w} * {w} samples (with its apron), got {}",
            heights.len()
        )));
    }
    if !(spacing.is_finite() && spacing > 0.0) {
        return Err(GeomError::Heightfield(format!(
            "spacing must be finite and positive, got {spacing}"
        )));
    }
    if let Some(i) = heights.iter().position(|h| !h.is_finite()) {
        return Err(GeomError::Heightfield(format!(
            "sample {i} is {}",
            heights[i]
        )));
    }
    let h = |x: usize, z: usize| heights[z * w + x];
    let last = (n - 1) as f32;
    let mut vertices = Vec::with_capacity(n * n);
    for j in 0..n {
        for i in 0..n {
            let (x, z) = (i + 1, j + 1);
            let nx = h(x - 1, z) - h(x + 1, z);
            let nz = h(x, z - 1) - h(x, z + 1);
            let ny = 2.0 * spacing;
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            vertices.push(Vertex {
                pos: [
                    (origin[0] + i as i64) as f32 * spacing,
                    h(x, z),
                    (origin[1] + j as i64) as f32 * spacing,
                ],
                normal: [nx / len, ny / len, nz / len],
                uv: [i as f32 / last, j as f32 / last],
                color: [1.0; 4],
            });
        }
    }
    Ok(MeshData {
        vertices,
        indices: chunk_indices(edges),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area_and_winding(indices: &[u32]) -> (f64, bool) {
        let n = CHUNK_SAMPLES;
        let p = |i: u32| ((i % n) as f64, (i / n) as f64);
        let mut area = 0.0;
        let mut ccw = true;
        for t in indices.chunks(3) {
            let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
            let (ux, uz) = (b.0 - a.0, b.1 - a.1);
            let (vx, vz) = (c.0 - a.0, c.1 - a.1);
            // The Y component of (b - a) × (c - a) in (x, y, z).
            let cy = uz * vx - ux * vz;
            ccw &= cy > 0.0;
            area += cy / 2.0;
        }
        (area, ccw)
    }

    #[test]
    fn every_pattern_covers_the_chunk_counter_clockwise() {
        let full = f64::from(CHUNK_CELLS * CHUNK_CELLS);
        for bits in 0..16u8 {
            let idx = chunk_indices(Edges(bits));
            let (area, ccw) = area_and_winding(&idx);
            assert!(ccw, "pattern {bits}");
            assert_eq!(area, full, "pattern {bits}");
        }
        // The plain pattern matches the heightfield's.
        assert_eq!(
            chunk_indices(Edges(0)),
            grid_indices(CHUNK_SAMPLES, CHUNK_SAMPLES)
        );
    }

    #[test]
    fn stitched_edges_use_even_vertices_only() {
        let n = CHUNK_SAMPLES;
        for bits in 1..16u8 {
            let e = Edges(bits);
            for &i in &chunk_indices(e) {
                let (x, z) = (i % n, i / n);
                let odd_on = |on: bool, coord: u32| on && coord % 2 == 1;
                assert!(
                    !odd_on(e.has(Edges::NEG_X) && x == 0, z),
                    "{bits}: ({x},{z})"
                );
                assert!(
                    !odd_on(e.has(Edges::POS_X) && x == n - 1, z),
                    "{bits}: ({x},{z})"
                );
                assert!(
                    !odd_on(e.has(Edges::NEG_Z) && z == 0, x),
                    "{bits}: ({x},{z})"
                );
                assert!(
                    !odd_on(e.has(Edges::POS_Z) && z == n - 1, x),
                    "{bits}: ({x},{z})"
                );
            }
        }
    }

    #[test]
    fn neighbouring_chunks_share_edge_vertices_and_normals() {
        // A height function of the global position, sampled with an apron.
        let f = |gx: i64, gz: i64| ((gx * 7 + gz * 13) % 17) as f32 * 0.25;
        let window = |o: [i64; 2]| {
            let w = CHUNK_SAMPLES as i64 + 2;
            let mut v = Vec::new();
            for z in 0..w {
                for x in 0..w {
                    v.push(f(o[0] + x - 1, o[1] + z - 1));
                }
            }
            v
        };
        let s = 0.7;
        let a = chunk_mesh(&window([-64, 128]), [-64, 128], s, Edges(0)).unwrap();
        let b = chunk_mesh(&window([0, 128]), [0, 128], s, Edges(0)).unwrap();
        let n = CHUNK_SAMPLES as usize;
        for z in 0..n {
            let va = a.vertices[z * n + n - 1];
            let vb = b.vertices[z * n];
            assert_eq!(va.pos, vb.pos);
            assert_eq!(va.normal, vb.normal);
        }
        assert!(a.is_well_formed());
        assert!(chunk_mesh(&[0.0; 4], [0, 0], s, Edges(0)).is_err());
    }
}
