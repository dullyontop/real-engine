//! Cellular (Worley) noise with one jittered feature point per unit cell.

use crate::gradient::split;
use crate::hash::{lattice2, lattice3, mix64};

/// Result of a cellular query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    /// Euclidean distance to the nearest feature point (F1).
    pub f1: f32,
    /// Euclidean distance to the second-nearest feature point (F2), `>= f1`.
    pub f2: f32,
    /// Stable 32-bit id of the cell owning the nearest feature point.
    pub id: u32,
}

/// 16 hash bits -> jitter offset in `[0, 1)` (exact in f32).
#[inline]
fn unit16(h: u64, shift: u32) -> f32 {
    ((h >> shift) & 0xFFFF) as f32 * (1.0 / 65_536.0)
}

/// Running F1/F2 tracker over squared distances.
struct Nearest {
    d1: f32,
    d2: f32,
    h1: u64,
}

impl Nearest {
    const fn new() -> Self {
        Self {
            d1: f32::MAX,
            d2: f32::MAX,
            h1: 0,
        }
    }

    #[inline]
    fn push(&mut self, d: f32, h: u64) {
        if d < self.d1 {
            self.d2 = self.d1;
            self.d1 = d;
            self.h1 = h;
        } else if d < self.d2 {
            self.d2 = d;
        }
    }

    fn finish(self) -> Cell {
        // sqrt is correctly rounded by IEEE 754, so it is portable; it is
        // applied once per query, not per candidate.
        Cell {
            f1: self.d1.sqrt(),
            f2: self.d2.sqrt(),
            id: (mix64(self.h1) >> 32) as u32,
        }
    }
}

/// Worley noise, 2D, searching the 3x3 cell neighborhood.
///
/// Ranges: `f1` in about [0, 1.1], `f2` in about [0, 1.5] (with full jitter
/// a 3x3 search can, rarely, miss a closer point two cells away; this
/// matches common practice, e.g. FastNoise, and is deterministic).
pub fn cellular2(seed: u64, x: f32, y: f32) -> Cell {
    let key = mix64(seed);
    let ((xi, rx), (yi, ry)) = (split(x), split(y));
    let mut best = Nearest::new();
    for dy in -1i32..=1 {
        for dx in -1i32..=1 {
            let h = lattice2(key, xi.wrapping_add(dx), yi.wrapping_add(dy));
            let px = dx as f32 + unit16(h, 0) - rx;
            let py = dy as f32 + unit16(h, 16) - ry;
            best.push(px * px + py * py, h);
        }
    }
    best.finish()
}

/// Worley noise, 3D, searching the 3x3x3 cell neighborhood.
/// Ranges: `f1` in about [0, 1.2], `f2` in about [0, 1.6].
pub fn cellular3(seed: u64, x: f32, y: f32, z: f32) -> Cell {
    let key = mix64(seed);
    let ((xi, rx), (yi, ry), (zi, rz)) = (split(x), split(y), split(z));
    let mut best = Nearest::new();
    for dz in -1i32..=1 {
        for dy in -1i32..=1 {
            for dx in -1i32..=1 {
                let h = lattice3(
                    key,
                    xi.wrapping_add(dx),
                    yi.wrapping_add(dy),
                    zi.wrapping_add(dz),
                );
                let px = dx as f32 + unit16(h, 0) - rx;
                let py = dy as f32 + unit16(h, 16) - ry;
                let pz = dz as f32 + unit16(h, 32) - rz;
                best.push(px * px + py * py + pz * pz, h);
            }
        }
    }
    best.finish()
}
