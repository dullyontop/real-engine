//! Portable, deterministic math helpers.
//!
//! Generated geometry is hashed (golden tests, ProcGraph cache keys), so it
//! must come out bit-identical on every platform. This module follows the
//! rules of `re_noise`: no libm transcendental functions and no fused
//! multiply-add. Sines and cosines come from [`sin_cos`], a polynomial in
//! `f64` built from `+ - *` only, and rotations are applied with scalar
//! arithmetic ([`Rotation`]) rather than glam's SIMD paths.

use glam::{Quat, Vec3};

/// `sin(x)` and `cos(x)` for an angle in radians, accurate to about `1e-15`
/// for `|x| < 1e6`, and bit-identical on every platform.
///
/// The angle is reduced to `[-π/4, π/4]` with a two-part `π/2`, then both
/// functions are evaluated with their Taylor series to the 16th power.
pub fn sin_cos(x: f64) -> (f64, f64) {
    use std::f64::consts::{FRAC_2_PI, FRAC_PI_2 as PIO2_HI};
    // The part of π/2 that `FRAC_PI_2` rounds away.
    const PIO2_LO: f64 = 6.123_233_995_736_766e-17;
    let k = (x * FRAC_2_PI).round();
    let r = (x - k * PIO2_HI) - k * PIO2_LO;
    let r2 = r * r;
    let s = r
        * (1.0
            + r2 * (-1.0 / 6.0
                + r2 * (1.0 / 120.0
                    + r2 * (-1.0 / 5040.0
                        + r2 * (1.0 / 362_880.0
                            + r2 * (-1.0 / 39_916_800.0
                                + r2 * (1.0 / 6_227_020_800.0
                                    + r2 * (-1.0 / 1_307_674_368_000.0))))))));
    let c = 1.0
        + r2 * (-1.0 / 2.0
            + r2 * (1.0 / 24.0
                + r2 * (-1.0 / 720.0
                    + r2 * (1.0 / 40_320.0
                        + r2 * (-1.0 / 3_628_800.0
                            + r2 * (1.0 / 479_001_600.0
                                + r2 * (-1.0 / 87_178_291_200.0
                                    + r2 * (1.0 / 20_922_789_888_000.0))))))));
    // `k` is integral; only its value modulo 4 matters.
    match (k as i64).rem_euclid(4) {
        0 => (s, c),
        1 => (c, -s),
        2 => (-s, -c),
        _ => (-c, s),
    }
}

/// [`sin_cos`] in `f32`.
pub fn sin_cos_f32(x: f32) -> (f32, f32) {
    let (s, c) = sin_cos(f64::from(x));
    (s as f32, c as f32)
}

/// A rotation from Euler angles in degrees, applied in the order X, then Y,
/// then Z (so `R = Rz * Ry * Rx`). Built with [`sin_cos`].
pub fn quat_from_euler_deg(degrees: Vec3) -> Quat {
    let half = |d: f32| sin_cos(f64::from(d) * (std::f64::consts::PI / 360.0));
    let (sx, cx) = half(degrees.x);
    let (sy, cy) = half(degrees.y);
    let (sz, cz) = half(degrees.z);
    // q = qz * qy * qx, expanded.
    let w = cz * cy * cx + sz * sy * sx;
    let x = cz * cy * sx - sz * sy * cx;
    let y = cz * sy * cx + sz * cy * sx;
    let z = sz * cy * cx - cz * sy * sx;
    Quat::from_xyzw(x as f32, y as f32, z as f32, w as f32)
}

/// A rotation about the Y axis by `radians`. Built with [`sin_cos`].
pub fn quat_from_yaw(radians: f32) -> Quat {
    let (s, c) = sin_cos(f64::from(radians) * 0.5);
    Quat::from_xyzw(0.0, s as f32, 0.0, c as f32)
}

/// A 3×3 rotation matrix, applied with scalar arithmetic in a fixed order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rotation {
    rows: [[f32; 3]; 3],
}

impl Rotation {
    /// The rotation of a quaternion (normalised first; a zero quaternion is
    /// the identity).
    pub fn from_quat(q: Quat) -> Rotation {
        let (x, y, z, w) = (q.x, q.y, q.z, q.w);
        let len2 = x * x + y * y + z * z + w * w;
        if len2 <= 0.0 || !len2.is_finite() {
            return Rotation::IDENTITY;
        }
        let s = 2.0 / len2;
        let (xx, yy, zz) = (x * x * s, y * y * s, z * z * s);
        let (xy, xz, yz) = (x * y * s, x * z * s, y * z * s);
        let (wx, wy, wz) = (w * x * s, w * y * s, w * z * s);
        Rotation {
            rows: [
                [1.0 - (yy + zz), xy - wz, xz + wy],
                [xy + wz, 1.0 - (xx + zz), yz - wx],
                [xz - wy, yz + wx, 1.0 - (xx + yy)],
            ],
        }
    }

    /// No rotation.
    pub const IDENTITY: Rotation = Rotation {
        rows: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };

    /// Rotates `v`.
    pub fn apply(&self, v: Vec3) -> Vec3 {
        let r = &self.rows;
        Vec3::new(
            r[0][0] * v.x + r[0][1] * v.y + r[0][2] * v.z,
            r[1][0] * v.x + r[1][1] * v.y + r[1][2] * v.z,
            r[2][0] * v.x + r[2][1] * v.y + r[2][2] * v.z,
        )
    }
}

/// `v` scaled to unit length, or `fallback` when `v` is zero or not finite.
pub fn normalize_or(v: Vec3, fallback: Vec3) -> Vec3 {
    let len2 = v.x * v.x + v.y * v.y + v.z * v.z;
    if len2 > 0.0 && len2.is_finite() {
        let len = len2.sqrt();
        Vec3::new(v.x / len, v.y / len, v.z / len)
    } else {
        fallback
    }
}

/// The cross product, in a fixed scalar order.
pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    Vec3::new(
        a.y * b.z - a.z * b.y,
        a.z * b.x - a.x * b.z,
        a.x * b.y - a.y * b.x,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_cos_matches_std_closely() {
        for i in -2000..=2000 {
            let x = f64::from(i) * 0.0137;
            let (s, c) = sin_cos(x);
            assert!((s - x.sin()).abs() < 1e-14, "sin({x})");
            assert!((c - x.cos()).abs() < 1e-14, "cos({x})");
        }
    }

    #[test]
    fn exact_quadrants() {
        let (s, c) = sin_cos(0.0);
        assert_eq!((s, c), (0.0, 1.0));
        let (s, c) = sin_cos(std::f64::consts::FRAC_PI_2);
        assert!((s - 1.0).abs() < 1e-16 && c.abs() < 1e-16);
    }

    #[test]
    fn euler_matches_glam() {
        let d = Vec3::new(30.0, -45.0, 60.0);
        let q = quat_from_euler_deg(d);
        let g = Quat::from_euler(
            glam::EulerRot::ZYX,
            d.z.to_radians(),
            d.y.to_radians(),
            d.x.to_radians(),
        );
        assert!(q.dot(g).abs() > 1.0 - 1e-6, "{q:?} vs {g:?}");
    }

    #[test]
    fn rotation_matches_glam() {
        let q = Quat::from_euler(glam::EulerRot::XYZ, 0.3, 1.1, -0.7);
        let v = Vec3::new(1.0, -2.0, 0.5);
        let a = Rotation::from_quat(q).apply(v);
        assert!((a - q * v).length() < 1e-5);
    }
}
