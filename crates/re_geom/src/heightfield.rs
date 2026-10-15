//! [`Heightfield`]: a square grid of heights, laid out the way Jolt's
//! height-field shape reads it, so the same samples give the render mesh and
//! the collider.

use glam::Vec3;
use re_types::mesh::{MeshData, Vertex};
use re_types::physics::ShapeDesc;
use serde::{Deserialize, Serialize};

use crate::error::GeomError;
use crate::math::{cross, normalize_or};
use crate::ops::compute_normals;

/// A square grid of `n × n` heights in the local XZ plane.
///
/// Sample `(x, z)` is `samples[z * n + x]` and sits at
/// `offset + (x * spacing, samples[z * n + x], z * spacing)`. Each grid cell
/// is split into two triangles along the diagonal from `(x, z)` to
/// `(x + 1, z + 1)`, as Jolt splits it: [`Heightfield::to_mesh`] and
/// [`Heightfield::height_at`] both use that split, so what is drawn, what is
/// queried and what collides agree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Heightfield {
    /// Samples per side (at least 2).
    pub n: u32,
    /// Distance between neighbouring samples along X and Z, in metres.
    pub spacing: f32,
    /// Position of sample `(0, 0)`; heights are added to its Y.
    pub offset: Vec3,
    /// The heights, row by row (`n * n` of them).
    pub samples: Vec<f32>,
}

impl Heightfield {
    /// A heightfield from its parts, checked with [`Heightfield::validate`].
    pub fn new(n: u32, spacing: f32, offset: Vec3, samples: Vec<f32>) -> Result<Self, GeomError> {
        let h = Heightfield {
            n,
            spacing,
            offset,
            samples,
        };
        h.validate()?;
        Ok(h)
    }

    /// A heightfield centred on the origin in X and Z (its offset is
    /// `-(n - 1) * spacing / 2` on both axes, and 0 in Y).
    pub fn centered(n: u32, spacing: f32, samples: Vec<f32>) -> Result<Self, GeomError> {
        let half = Self::half_extent(n, spacing);
        Self::new(n, spacing, Vec3::new(-half, 0.0, -half), samples)
    }

    /// Half the side length of a centred grid of `n` samples.
    pub fn half_extent(n: u32, spacing: f32) -> f32 {
        n.saturating_sub(1) as f32 * spacing * 0.5
    }

    /// Checks `n >= 2`, `n * n` finite samples, a positive finite spacing
    /// and a finite offset (`RE4500`).
    pub fn validate(&self) -> Result<(), GeomError> {
        if self.n < 2 {
            return Err(GeomError::Heightfield(format!(
                "n is {}, at least 2 samples per side are needed",
                self.n
            )));
        }
        let want = u64::from(self.n) * u64::from(self.n);
        if self.samples.len() as u64 != want {
            return Err(GeomError::Heightfield(format!(
                "{} samples, expected {n} * {n} = {want}",
                self.samples.len(),
                n = self.n
            )));
        }
        if !(self.spacing.is_finite() && self.spacing > 0.0) {
            return Err(GeomError::Heightfield(format!(
                "spacing must be finite and positive, got {}",
                self.spacing
            )));
        }
        if !self.offset.is_finite() {
            return Err(GeomError::Heightfield("the offset is not finite".into()));
        }
        if let Some(i) = self.samples.iter().position(|h| !h.is_finite()) {
            return Err(GeomError::Heightfield(format!(
                "sample {i} is {}",
                self.samples[i]
            )));
        }
        Ok(())
    }

    /// The height of sample `(x, z)` (without the offset).
    ///
    /// # Panics
    ///
    /// Panics if `x` or `z` is not below `n`.
    pub fn sample(&self, x: u32, z: u32) -> f32 {
        assert!(
            x < self.n && z < self.n,
            "sample ({x}, {z}) outside {0}x{0}",
            self.n
        );
        self.samples[(z * self.n + x) as usize]
    }

    /// The smallest and largest sample, plus the offset's Y.
    pub fn height_range(&self) -> (f32, f32) {
        let (lo, hi) = self
            .samples
            .iter()
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &h| {
                (lo.min(h), hi.max(h))
            });
        (lo + self.offset.y, hi + self.offset.y)
    }

    /// The cell holding local point `(x, z)` and the position inside it, or
    /// `None` outside the grid.
    fn locate(&self, x: f32, z: f32) -> Option<(u32, u32, f32, f32)> {
        let last = (self.n - 1) as f32;
        let fx = (x - self.offset.x) / self.spacing;
        let fz = (z - self.offset.z) / self.spacing;
        if !(fx >= 0.0 && fz >= 0.0 && fx <= last && fz <= last) {
            return None;
        }
        // `fx` is non-negative, so the cast floors.
        let ix = (fx as u32).min(self.n - 2);
        let iz = (fz as u32).min(self.n - 2);
        Some((ix, iz, fx - ix as f32, fz - iz as f32))
    }

    /// The surface height at local point `(x, z)` (offset included),
    /// interpolated on the triangle that contains it, or `None` outside the grid.
    pub fn height_at(&self, x: f32, z: f32) -> Option<f32> {
        let (ix, iz, tx, tz) = self.locate(x, z)?;
        let h00 = self.sample(ix, iz);
        let h10 = self.sample(ix + 1, iz);
        let h01 = self.sample(ix, iz + 1);
        let h11 = self.sample(ix + 1, iz + 1);
        let h = if tz >= tx {
            h00 + tz * (h01 - h00) + tx * (h11 - h01)
        } else {
            h00 + tx * (h10 - h00) + tz * (h11 - h10)
        };
        Some(h + self.offset.y)
    }

    /// The unit normal of the triangle containing local point `(x, z)`, or
    /// `None` outside the grid.
    pub fn normal_at(&self, x: f32, z: f32) -> Option<Vec3> {
        let (ix, iz, tx, tz) = self.locate(x, z)?;
        let s = self.spacing;
        let h00 = self.sample(ix, iz);
        let h11 = self.sample(ix + 1, iz + 1);
        let n = if tz >= tx {
            let h01 = self.sample(ix, iz + 1);
            // (0,0) -> (0,1) -> (1,1)
            cross(Vec3::new(0.0, h01 - h00, s), Vec3::new(s, h11 - h00, s))
        } else {
            let h10 = self.sample(ix + 1, iz);
            // (0,0) -> (1,1) -> (1,0)
            cross(Vec3::new(s, h11 - h00, s), Vec3::new(s, h10 - h00, 0.0))
        };
        Some(normalize_or(n, Vec3::Y))
    }

    /// The render mesh: one vertex per sample (UV 0 to 1 across the grid,
    /// white), two counter-clockwise triangles per cell split as Jolt splits
    /// them, and area-weighted normals.
    pub fn to_mesh(&self) -> MeshData {
        let n = self.n;
        let last = (n - 1) as f32;
        let mut vertices = Vec::with_capacity((n * n) as usize);
        for z in 0..n {
            for x in 0..n {
                vertices.push(Vertex {
                    pos: [
                        self.offset.x + x as f32 * self.spacing,
                        self.offset.y + self.sample(x, z),
                        self.offset.z + z as f32 * self.spacing,
                    ],
                    normal: [0.0, 1.0, 0.0],
                    uv: [x as f32 / last, z as f32 / last],
                    color: [1.0; 4],
                });
            }
        }
        let mut mesh = MeshData {
            vertices,
            indices: grid_indices(n, n),
        };
        compute_normals(&mut mesh);
        mesh
    }

    /// The collider shape for `re_physics` (`ShapeDesc::Heightfield`).
    pub fn to_shape(&self) -> ShapeDesc {
        ShapeDesc::Heightfield {
            n: self.n,
            samples: self.samples.clone(),
            spacing: self.spacing,
            offset: self.offset,
        }
    }
}

/// Indices of a `cols × rows` vertex grid (row-major, `x` fastest), two
/// triangles per cell split along the `(x, z)`–`(x + 1, z + 1)` diagonal,
/// counter-clockwise seen from +Y.
pub(crate) fn grid_indices(cols: u32, rows: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity(((cols - 1) * (rows - 1) * 6) as usize);
    for z in 0..rows - 1 {
        for x in 0..cols - 1 {
            let i00 = z * cols + x;
            let i10 = i00 + 1;
            let i01 = i00 + cols;
            let i11 = i01 + 1;
            out.extend_from_slice(&[i00, i01, i11, i00, i11, i10]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp() -> Heightfield {
        // h = x * 1 + z * 2 (in sample units), spacing 2.
        let samples = (0..3)
            .flat_map(|z| (0..3).map(move |x| x as f32 + 2.0 * z as f32))
            .collect();
        Heightfield::new(3, 2.0, Vec3::new(-2.0, 1.0, -2.0), samples).unwrap()
    }

    #[test]
    fn validates() {
        assert!(Heightfield::new(1, 1.0, Vec3::ZERO, vec![0.0]).is_err());
        assert!(Heightfield::new(2, 1.0, Vec3::ZERO, vec![0.0; 3]).is_err());
        assert!(Heightfield::new(2, 0.0, Vec3::ZERO, vec![0.0; 4]).is_err());
        assert!(Heightfield::new(2, 1.0, Vec3::ZERO, vec![0.0, f32::NAN, 0.0, 0.0]).is_err());
        assert_eq!(
            Heightfield::new(2, 1.0, Vec3::ZERO, vec![0.0; 3])
                .unwrap_err()
                .code(),
            crate::codes::INVALID_HEIGHTFIELD
        );
    }

    #[test]
    fn height_at_hits_samples_and_planes() {
        let h = ramp();
        assert_eq!(h.height_at(-2.0, -2.0), Some(1.0));
        assert_eq!(h.height_at(2.0, 2.0), Some(1.0 + 2.0 + 4.0));
        // A plane is reproduced exactly by either triangle.
        assert_eq!(h.height_at(-1.0, 0.0), Some(1.0 + 0.5 + 2.0));
        assert_eq!(h.height_at(2.5, 0.0), None);
        assert_eq!(h.height_at(f32::NAN, 0.0), None);
    }

    #[test]
    fn height_at_uses_the_jolt_diagonal() {
        // One raised corner at (1, 0): only the lower-right triangle sees it.
        let h = Heightfield::new(2, 1.0, Vec3::ZERO, vec![0.0, 1.0, 0.0, 0.0]).unwrap();
        assert_eq!(h.height_at(0.25, 0.75), Some(0.0));
        assert_eq!(h.height_at(0.75, 0.25), Some(0.5));
    }

    #[test]
    fn mesh_matches_samples() {
        let h = ramp();
        let m = h.to_mesh();
        assert_eq!(m.vertices.len(), 9);
        assert_eq!(m.triangle_count(), 8);
        assert!(m.is_well_formed());
        for v in &m.vertices {
            assert_eq!(h.height_at(v.pos[0], v.pos[2]), Some(v.pos[1]));
            assert!(v.normal[1] > 0.0);
        }
        let n = h.normal_at(0.0, 0.0).unwrap();
        assert!((Vec3::from_array(m.vertices[4].normal) - n).length() < 1e-6);
    }

    #[test]
    fn centered_and_shape() {
        let h = Heightfield::centered(5, 0.5, vec![0.0; 25]).unwrap();
        assert_eq!(h.offset, Vec3::new(-1.0, 0.0, -1.0));
        assert!(h.to_shape().is_well_formed());
        assert_eq!(h.height_range(), (0.0, 0.0));
    }
}
