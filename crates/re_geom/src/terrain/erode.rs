//! Hydraulic erosion on region tiles, blended into one seamless field.
//!
//! [`erode`] is droplet erosion (Hans Theobald Beyer, "Implementation of a
//! method for hydraulic erosion", 2015) with bilinear erosion and deposition,
//! in scalar `f32` with only `+ − × ÷` and `sqrt`, so it is bit-identical on
//! every platform. Droplet start points come from the caller, which draws
//! them from a seeded `re_noise::Pcg32`.
//!
//! A large terrain is eroded in **tiles** of [`TILE_CELLS`] cells plus an
//! [`APRON`] on every side ([`tile_window`]). Each tile erodes on its own, and
//! the field at a sample is the blend of the tiles around it with weights
//! that depend only on its global index ([`tile_blend`]), so the result is
//! one continuous function of position: no tile seams, and any window of it
//! agrees with any other.

/// Cells per tile side.
pub const TILE_CELLS: i64 = 256;

/// Samples of apron on each side of a tile. Droplets near a window's edge
/// leave it, so only the inner half of the apron is used for blending.
pub const APRON: i64 = 32;

/// Half the width of the band where neighbouring tiles blend.
const BLEND: i64 = APRON / 2;

/// The droplet parameters of [`erode`]. The defaults are the measured ones
/// of ADR-0042.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ErosionParams {
    /// Steps a droplet lives at most.
    pub steps: u32,
    /// How much a droplet keeps its direction (0 to 1).
    pub inertia: f32,
    /// Sediment a droplet can carry, per unit of slope, speed and water.
    pub capacity: f32,
    /// The smallest slope used for the capacity.
    pub min_slope: f32,
    /// Share of the excess sediment dropped per step.
    pub deposition: f32,
    /// Share of the free capacity eroded per step.
    pub erosion: f32,
    /// Share of the water that evaporates per step.
    pub evaporation: f32,
    /// Acceleration from height differences.
    pub gravity: f32,
}

impl Default for ErosionParams {
    fn default() -> Self {
        ErosionParams {
            steps: 48,
            inertia: 0.05,
            capacity: 4.0,
            min_slope: 0.01,
            deposition: 0.3,
            erosion: 0.3,
            evaporation: 0.02,
            gravity: 4.0,
        }
    }
}

/// Erodes the `n × n` grid `h` (row-major, heights in sample units of the
/// grid) with one droplet per start point, in order. Start points are in
/// sample coordinates and must lie in `[0, n − 2)`; others are skipped.
///
/// # Panics
///
/// Panics if `h.len() != n * n` or `n < 3`.
pub fn erode(
    h: &mut [f32],
    n: usize,
    p: &ErosionParams,
    starts: impl IntoIterator<Item = (f32, f32)>,
) {
    assert!(
        n >= 3 && h.len() == n * n,
        "erode needs an n x n grid, n >= 3"
    );
    let last = (n - 2) as f32;
    let inside = |x: f32, z: f32| x >= 0.0 && z >= 0.0 && x < last && z < last;
    for (sx, sz) in starts {
        if !inside(sx, sz) {
            continue;
        }
        let (mut px, mut pz) = (sx, sz);
        let (mut dx, mut dz) = (0f32, 0f32);
        let (mut speed, mut water, mut sediment) = (1f32, 1f32, 0f32);
        for _ in 0..p.steps {
            let (ix, iz) = (px as usize, pz as usize);
            let (fx, fz) = (px - ix as f32, pz - iz as f32);
            let i = iz * n + ix;
            let (h00, h10, h01, h11) = (h[i], h[i + 1], h[i + n], h[i + n + 1]);
            let gx = (h10 - h00) * (1.0 - fz) + (h11 - h01) * fz;
            let gz = (h01 - h00) * (1.0 - fx) + (h11 - h10) * fx;
            let old = bilinear(h00, h10, h01, h11, fx, fz);
            dx = dx * p.inertia - gx * (1.0 - p.inertia);
            dz = dz * p.inertia - gz * (1.0 - p.inertia);
            let len = (dx * dx + dz * dz).sqrt();
            if len == 0.0 {
                break;
            }
            dx /= len;
            dz /= len;
            px += dx;
            pz += dz;
            if !inside(px, pz) {
                break;
            }
            let (jx, jz) = (px as usize, pz as usize);
            let j = jz * n + jx;
            let new = bilinear(
                h[j],
                h[j + 1],
                h[j + n],
                h[j + n + 1],
                px - jx as f32,
                pz - jz as f32,
            );
            let dh = new - old;
            let capacity = (-dh).max(p.min_slope) * speed * water * p.capacity;
            let w = [
                (1.0 - fx) * (1.0 - fz),
                fx * (1.0 - fz),
                (1.0 - fx) * fz,
                fx * fz,
            ];
            let cells = [i, i + 1, i + n, i + n + 1];
            if sediment > capacity || dh > 0.0 {
                let amount = if dh > 0.0 {
                    dh.min(sediment)
                } else {
                    (sediment - capacity) * p.deposition
                };
                sediment -= amount;
                for k in 0..4 {
                    h[cells[k]] += amount * w[k];
                }
            } else {
                let amount = ((capacity - sediment) * p.erosion).min(-dh);
                for k in 0..4 {
                    h[cells[k]] -= amount * w[k];
                }
                sediment += amount;
            }
            let s2 = speed * speed + dh * p.gravity;
            speed = if s2 > 0.0 { s2.sqrt() } else { 0.0 };
            water *= 1.0 - p.evaporation;
        }
    }
}

fn bilinear(h00: f32, h10: f32, h01: f32, h11: f32, fx: f32, fz: f32) -> f32 {
    h00 * (1.0 - fx) * (1.0 - fz) + h10 * fx * (1.0 - fz) + h01 * (1.0 - fx) * fz + h11 * fx * fz
}

/// The window tile `(tx, tz)` erodes: the global index of its sample
/// `(0, 0)` and its samples per side (core plus apron on both sides).
pub fn tile_window(tx: i64, tz: i64) -> ([i64; 2], usize) {
    (
        [tx * TILE_CELLS - APRON, tz * TILE_CELLS - APRON],
        (TILE_CELLS + 2 * APRON + 1) as usize,
    )
}

/// The tiles whose eroded values make up global sample `(gx, gz)`, with
/// their weights: one to four `(tile x, tile z, weight)`, in tile order,
/// weights summing to 1.
pub fn tile_blend(gx: i64, gz: i64) -> Vec<(i64, i64, f32)> {
    let xs = axis(gx);
    let zs = axis(gz);
    let mut out = Vec::with_capacity(4);
    for &(tz, wz) in &zs {
        for &(tx, wx) in &xs {
            out.push((tx, tz, wx * wz));
        }
    }
    out
}

/// The tiles along one axis covering index `g`, with 1D weights. Inside a
/// tile's core, away from its edges, the weight is 1; within `BLEND` of an
/// edge it ramps linearly to the neighbour.
fn axis(g: i64) -> Vec<(i64, f32)> {
    let t = g.div_euclid(TILE_CELLS);
    let r = g - t * TILE_CELLS;
    let ramp = |d: i64| (d + BLEND) as f32 / (2 * BLEND) as f32;
    if r < BLEND {
        // Blending with the tile before: own weight rises from 0.5.
        let w = ramp(r);
        vec![(t - 1, 1.0 - w), (t, w)]
    } else if r > TILE_CELLS - BLEND {
        let w = ramp(TILE_CELLS - r);
        vec![(t, w), (t + 1, 1.0 - w)]
    } else {
        vec![(t, 1.0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn starts(n: usize, count: usize) -> Vec<(f32, f32)> {
        // A fixed scatter (the engine draws these from a seeded Pcg32).
        let mut s = 0x9e37_79b9_u32;
        let mut next = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s >> 8) as f32 / (1u32 << 24) as f32 * (n - 2) as f32
        };
        (0..count).map(|_| (next(), next())).collect()
    }

    fn hill(n: usize) -> Vec<f32> {
        let c = (n / 2) as f32;
        (0..n * n)
            .map(|i| {
                let (x, z) = ((i % n) as f32 - c, (i / n) as f32 - c);
                (c * c - (x * x + z * z) * 0.5).max(0.0) / c
            })
            .collect()
    }

    #[test]
    fn erosion_moves_material_and_repeats_exactly() {
        let n = 65;
        let base = hill(n);
        let mut a = base.clone();
        erode(&mut a, n, &ErosionParams::default(), starts(n, 4000));
        let mut b = base.clone();
        erode(&mut b, n, &ErosionParams::default(), starts(n, 4000));
        assert_eq!(
            a.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            b.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
        let changed = a.iter().zip(&base).filter(|(x, y)| x != y).count();
        assert!(changed > n * n / 4, "{changed}");
        assert!(a.iter().all(|v| v.is_finite()));
        // Starts outside the grid are skipped.
        let mut c = base.clone();
        erode(
            &mut c,
            n,
            &ErosionParams::default(),
            [(-1.0, 3.0), (64.0, 1.0)],
        );
        assert_eq!(c, base);
    }

    #[test]
    fn tile_weights_are_a_partition_of_unity() {
        for g in -600..600i64 {
            let ws = tile_blend(g, 77);
            let sum: f32 = ws.iter().map(|w| w.2).sum();
            assert!((sum - 1.0).abs() < 1e-6, "{g}: {ws:?}");
            for (tx, tz, w) in ws {
                assert!(w > 0.0);
                // Every tile used covers the sample with at least BLEND to spare.
                let ([ox, oz], n) = tile_window(tx, tz);
                let n = n as i64;
                assert!(
                    g - ox >= BLEND && ox + n - 1 - g >= BLEND,
                    "{g} in tile {tx}"
                );
                assert!(77 - oz >= BLEND && oz + n - 1 - 77 >= BLEND);
            }
        }
        // Weights change continuously: one step never moves a weight by more
        // than one ramp step.
        let step = 1.0 / (2 * BLEND) as f32;
        let w_of = |g: i64, t: i64| {
            tile_blend(g, 0)
                .iter()
                .filter(|w| w.0 == t)
                .map(|w| w.2)
                .sum::<f32>()
        };
        for g in -300..300 {
            for t in -3..3 {
                assert!(
                    (w_of(g + 1, t) - w_of(g, t)).abs() <= step + 1e-6,
                    "{g} {t}"
                );
            }
        }
    }
}
