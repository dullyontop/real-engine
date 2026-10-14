//! Fractal combinations: fBm, ridged multifractal and domain warping.

use crate::gradient::{perlin2, perlin3, simplex2, simplex3};

/// Base gradient noise used by the fractal functions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Basis {
    /// Perlin gradient noise ([`perlin2`], [`perlin3`]).
    #[default]
    Perlin,
    /// Simplex gradient noise ([`simplex2`], [`simplex3`]).
    Simplex,
}

/// Fractal parameters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fractal {
    /// Base noise of every octave.
    pub basis: Basis,
    /// Number of octaves (0 yields 0.0).
    pub octaves: u32,
    /// Frequency multiplier per octave (typically 2.0).
    pub lacunarity: f32,
    /// Amplitude multiplier per octave (typically 0.5).
    pub gain: f32,
}

impl Default for Fractal {
    fn default() -> Self {
        Self {
            basis: Basis::Perlin,
            octaves: 6,
            lacunarity: 2.0,
            gain: 0.5,
        }
    }
}

/// Distinct, decorrelated seed per octave (mixed again inside the basis).
#[inline]
fn octave_seed(seed: u64, octave: u32) -> u64 {
    seed ^ u64::from(octave + 1).wrapping_mul(0xD6E8_FEB8_6659_FD93)
}

impl Fractal {
    #[inline]
    fn sample2(&self, seed: u64, x: f32, y: f32) -> f32 {
        match self.basis {
            Basis::Perlin => perlin2(seed, x, y),
            Basis::Simplex => simplex2(seed, x, y),
        }
    }

    #[inline]
    fn sample3(&self, seed: u64, x: f32, y: f32, z: f32) -> f32 {
        match self.basis {
            Basis::Perlin => perlin3(seed, x, y, z),
            Basis::Simplex => simplex3(seed, x, y, z),
        }
    }

    /// Sums octaves of `noise(octave_seed, frequency)`, normalised by the
    /// total amplitude so the output range equals the basis range.
    fn fbm(&self, seed: u64, mut noise: impl FnMut(u64, f32) -> f32) -> f32 {
        let (mut sum, mut norm, mut amp, mut freq) = (0.0f32, 0.0f32, 1.0f32, 1.0f32);
        for o in 0..self.octaves {
            sum += amp * noise(octave_seed(seed, o), freq);
            norm += amp;
            amp *= self.gain;
            freq *= self.lacunarity;
        }
        if norm > 0.0 { sum / norm } else { 0.0 }
    }

    /// Musgrave ridged multifractal (offset 1, weight gain 2), normalised by
    /// the amplitude sum. Range: [0, 1] given a basis in [-1, 1].
    fn ridged(&self, seed: u64, mut noise: impl FnMut(u64, f32) -> f32) -> f32 {
        let (mut sum, mut norm, mut amp, mut freq) = (0.0f32, 0.0f32, 1.0f32, 1.0f32);
        let mut weight = 1.0f32;
        for o in 0..self.octaves {
            let mut s = 1.0 - noise(octave_seed(seed, o), freq).abs();
            s = (s * s * weight).max(0.0);
            weight = (s * 2.0).clamp(0.0, 1.0);
            sum += s * amp;
            norm += amp;
            amp *= self.gain;
            freq *= self.lacunarity;
        }
        if norm > 0.0 { sum / norm } else { 0.0 }
    }
}

/// Fractal Brownian motion, 2D. Range: same as the basis, ~[-1, 1].
pub fn fbm2(seed: u64, f: &Fractal, x: f32, y: f32) -> f32 {
    f.fbm(seed, |s, k| f.sample2(s, x * k, y * k))
}

/// Fractal Brownian motion, 3D. Range: same as the basis, ~[-1, 1].
pub fn fbm3(seed: u64, f: &Fractal, x: f32, y: f32, z: f32) -> f32 {
    f.fbm(seed, |s, k| f.sample3(s, x * k, y * k, z * k))
}

/// Ridged multifractal, 2D. Range: [0, 1].
pub fn ridged2(seed: u64, f: &Fractal, x: f32, y: f32) -> f32 {
    f.ridged(seed, |s, k| f.sample2(s, x * k, y * k))
}

/// Ridged multifractal, 3D. Range: [0, 1].
pub fn ridged3(seed: u64, f: &Fractal, x: f32, y: f32, z: f32) -> f32 {
    f.ridged(seed, |s, k| f.sample3(s, x * k, y * k, z * k))
}

/// Per-axis seed offsets so warp components are independent fields.
const WARP_X: u64 = 0x5851_F42D_4C95_7F2D;
const WARP_Y: u64 = 0x1405_7B7E_F767_814F;
const WARP_Z: u64 = 0x2545_F491_4F6C_DD1D;

/// Domain warp, 2D: offsets `(x, y)` by `strength * fbm` per axis.
pub fn warp2(seed: u64, f: &Fractal, strength: f32, x: f32, y: f32) -> (f32, f32) {
    (
        x + strength * fbm2(seed ^ WARP_X, f, x, y),
        y + strength * fbm2(seed ^ WARP_Y, f, x, y),
    )
}

/// Domain warp, 3D: offsets `(x, y, z)` by `strength * fbm` per axis.
pub fn warp3(seed: u64, f: &Fractal, strength: f32, x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    (
        x + strength * fbm3(seed ^ WARP_X, f, x, y, z),
        y + strength * fbm3(seed ^ WARP_Y, f, x, y, z),
        z + strength * fbm3(seed ^ WARP_Z, f, x, y, z),
    )
}

/// fBm sampled at a domain-warped position, 2D. Range: ~[-1, 1].
pub fn warped_fbm2(seed: u64, f: &Fractal, strength: f32, x: f32, y: f32) -> f32 {
    let (wx, wy) = warp2(seed, f, strength, x, y);
    fbm2(seed, f, wx, wy)
}

/// fBm sampled at a domain-warped position, 3D. Range: ~[-1, 1].
pub fn warped_fbm3(seed: u64, f: &Fractal, strength: f32, x: f32, y: f32, z: f32) -> f32 {
    let (wx, wy, wz) = warp3(seed, f, strength, x, y, z);
    fbm3(seed, f, wx, wy, wz)
}
