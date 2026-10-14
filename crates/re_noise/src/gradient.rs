//! Gradient noise: improved Perlin and classic (Perlin/Gustavson) simplex.
//!
//! Lattice gradients are chosen by hashing `(seed, lattice coordinate)`
//! (see [`crate::hash`]), not by a 256-entry permutation table, so there is
//! no 256-unit tiling period and every `u64` seed is valid.

use crate::hash::{lattice2, lattice3, mix64};

/// 8 unit gradients at 45 degree steps. sqrt(1/2) is written as a literal so
/// no runtime transcendental is involved.
const R: f32 = core::f32::consts::FRAC_1_SQRT_2;
const GRAD2: [(f32, f32); 8] = [
    (1.0, 0.0),
    (R, R),
    (0.0, 1.0),
    (-R, R),
    (-1.0, 0.0),
    (-R, -R),
    (0.0, -1.0),
    (R, -R),
];

/// Ken Perlin's 12 cube-edge gradients, padded to 16 with repeats
/// ("Improving Noise", SIGGRAPH 2002).
const GRAD3: [(f32, f32, f32); 16] = [
    (1.0, 1.0, 0.0),
    (-1.0, 1.0, 0.0),
    (1.0, -1.0, 0.0),
    (-1.0, -1.0, 0.0),
    (1.0, 0.0, 1.0),
    (-1.0, 0.0, 1.0),
    (1.0, 0.0, -1.0),
    (-1.0, 0.0, -1.0),
    (0.0, 1.0, 1.0),
    (0.0, -1.0, 1.0),
    (0.0, 1.0, -1.0),
    (0.0, -1.0, -1.0),
    (1.0, 1.0, 0.0),
    (-1.0, 1.0, 0.0),
    (0.0, -1.0, 1.0),
    (0.0, -1.0, -1.0),
];

/// Perlin 2D with unit gradients is bounded by sqrt(2)/2; scale to [-1, 1].
const PERLIN2_SCALE: f32 = core::f32::consts::SQRT_2;
/// Empirically calibrated (random sampling + hill climbing found a raw
/// maximum of ~1.0203) so 3D Perlin stays within [-1, 1].
const PERLIN3_SCALE: f32 = 0.98;
/// Empirically calibrated simplex scales (radius^2 = 0.5 kernels); the same
/// search found scaled maxima of ~0.99996 (2D) and ~0.99895 (3D).
const SIMPLEX2_SCALE: f32 = 99.2;
const SIMPLEX3_SCALE: f32 = 76.8;

#[inline]
fn grad2(h: u64, x: f32, y: f32) -> f32 {
    let (gx, gy) = GRAD2[(h >> 61) as usize];
    gx * x + gy * y
}

#[inline]
fn grad3(h: u64, x: f32, y: f32, z: f32) -> f32 {
    let (gx, gy, gz) = GRAD3[(h >> 60) as usize];
    gx * x + gy * y + gz * z
}

/// Floor to `i32` via truncating cast. Exact for all f32 within `i32`
/// range (saturates outside it), and inlines to a few instructions, whereas
/// `f32::floor` is a libm call on baseline x86_64 (no SSE4.1 `roundss`).
#[inline]
pub(crate) fn floor_i32(v: f32) -> i32 {
    let i = v as i32;
    if (i as f32) > v { i - 1 } else { i }
}

/// Splits `v` into lattice cell index and fractional part in `[0, 1)`.
#[inline]
pub(crate) fn split(v: f32) -> (i32, f32) {
    let i = floor_i32(v);
    (i, v - i as f32)
}

/// Quintic fade `6t^5 - 15t^4 + 10t^3`.
#[inline]
fn fade(t: f32) -> f32 {
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

#[inline]
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + t * (b - a)
}

/// Improved Perlin noise, 2D. Range: [-1, 1] (analytic bound). Zero at
/// integer lattice points.
pub fn perlin2(seed: u64, x: f32, y: f32) -> f32 {
    let key = mix64(seed);
    let (xi, xf) = split(x);
    let (yi, yf) = split(y);
    let (xi1, yi1) = (xi.wrapping_add(1), yi.wrapping_add(1));
    let n00 = grad2(lattice2(key, xi, yi), xf, yf);
    let n10 = grad2(lattice2(key, xi1, yi), xf - 1.0, yf);
    let n01 = grad2(lattice2(key, xi, yi1), xf, yf - 1.0);
    let n11 = grad2(lattice2(key, xi1, yi1), xf - 1.0, yf - 1.0);
    let u = fade(xf);
    let v = fade(yf);
    lerp(lerp(n00, n10, u), lerp(n01, n11, u), v) * PERLIN2_SCALE
}

/// Improved Perlin noise, 3D. Range: approximately [-1, 1] (empirically
/// calibrated; tested over a large sample). Zero at integer lattice points.
pub fn perlin3(seed: u64, x: f32, y: f32, z: f32) -> f32 {
    let key = mix64(seed);
    let (xi, xf) = split(x);
    let (yi, yf) = split(y);
    let (zi, zf) = split(z);
    let c = |dx: i32, dy: i32, dz: i32| {
        let h = lattice3(
            key,
            xi.wrapping_add(dx),
            yi.wrapping_add(dy),
            zi.wrapping_add(dz),
        );
        grad3(h, xf - dx as f32, yf - dy as f32, zf - dz as f32)
    };
    let (u, v, w) = (fade(xf), fade(yf), fade(zf));
    let x00 = lerp(c(0, 0, 0), c(1, 0, 0), u);
    let x10 = lerp(c(0, 1, 0), c(1, 1, 0), u);
    let x01 = lerp(c(0, 0, 1), c(1, 0, 1), u);
    let x11 = lerp(c(0, 1, 1), c(1, 1, 1), u);
    lerp(lerp(x00, x10, v), lerp(x01, x11, v), w) * PERLIN3_SCALE
}

/// Skew/unskew factors (sqrt(3)-1)/2 and (3-sqrt(3))/6 as literals.
const F2: f32 = 0.366_025_42;
const G2: f32 = 0.211_324_87;
const F3: f32 = 1.0 / 3.0;
const G3: f32 = 1.0 / 6.0;

/// Radial kernel `max(0, 0.5 - d^2)^4`; radius^2 = 0.5 keeps the noise C1
/// continuous across simplex boundaries.
#[inline]
fn kernel(d2: f32) -> f32 {
    let t = 0.5 - d2;
    if t <= 0.0 {
        0.0
    } else {
        let t2 = t * t;
        t2 * t2
    }
}

/// Classic simplex noise, 2D (Perlin 2001; layout after S. Gustavson,
/// "Simplex noise demystified"). Range: approximately [-1, 1].
pub fn simplex2(seed: u64, x: f32, y: f32) -> f32 {
    let key = mix64(seed);
    let s = (x + y) * F2;
    let (ii, jj) = (floor_i32(x + s), floor_i32(y + s));
    let (i, j) = (ii as f32, jj as f32);
    let t = (i + j) * G2;
    let x0 = x - i + t;
    let y0 = y - j + t;
    let (i1, j1) = if x0 > y0 { (1, 0) } else { (0, 1) };
    let corner = |di: i32, dj: i32, dx: f32, dy: f32| {
        let k = kernel(dx * dx + dy * dy);
        if k == 0.0 {
            0.0
        } else {
            k * grad2(
                lattice2(key, ii.wrapping_add(di), jj.wrapping_add(dj)),
                dx,
                dy,
            )
        }
    };
    let n0 = corner(0, 0, x0, y0);
    let n1 = corner(i1, j1, x0 - i1 as f32 + G2, y0 - j1 as f32 + G2);
    let n2 = corner(1, 1, x0 - 1.0 + 2.0 * G2, y0 - 1.0 + 2.0 * G2);
    (n0 + n1 + n2) * SIMPLEX2_SCALE
}

/// Classic simplex noise, 3D (Perlin 2001; layout after S. Gustavson).
/// The 3D simplex patent (US 6,867,776) expired in 2022.
/// Range: approximately [-1, 1].
pub fn simplex3(seed: u64, x: f32, y: f32, z: f32) -> f32 {
    let key = mix64(seed);
    let s = (x + y + z) * F3;
    let (ii, jj, kk) = (floor_i32(x + s), floor_i32(y + s), floor_i32(z + s));
    let (i, j, k) = (ii as f32, jj as f32, kk as f32);
    let t = (i + j + k) * G3;
    let (x0, y0, z0) = (x - i + t, y - j + t, z - k + t);
    // Rank the offsets to find which simplex of the skewed cube we are in.
    let ((i1, j1, k1), (i2, j2, k2)) = if x0 >= y0 {
        if y0 >= z0 {
            ((1, 0, 0), (1, 1, 0))
        } else if x0 >= z0 {
            ((1, 0, 0), (1, 0, 1))
        } else {
            ((0, 0, 1), (1, 0, 1))
        }
    } else if y0 < z0 {
        ((0, 0, 1), (0, 1, 1))
    } else if x0 < z0 {
        ((0, 1, 0), (0, 1, 1))
    } else {
        ((0, 1, 0), (1, 1, 0))
    };
    let corner = |di: i32, dj: i32, dk: i32, g: f32| {
        let (dx, dy, dz) = (x0 - di as f32 + g, y0 - dj as f32 + g, z0 - dk as f32 + g);
        let w = kernel(dx * dx + dy * dy + dz * dz);
        if w == 0.0 {
            return 0.0;
        }
        let h = lattice3(
            key,
            ii.wrapping_add(di),
            jj.wrapping_add(dj),
            kk.wrapping_add(dk),
        );
        w * grad3(h, dx, dy, dz)
    };
    let n = corner(0, 0, 0, 0.0)
        + corner(i1, j1, k1, G3)
        + corner(i2, j2, k2, 2.0 * G3)
        + corner(1, 1, 1, 3.0 * G3);
    n * SIMPLEX3_SCALE
}
