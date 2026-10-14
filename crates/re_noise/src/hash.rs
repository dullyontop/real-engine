//! Stateless integer hashing for lattice coordinates.
//!
//! The core mixer is the **SplitMix64 finalizer** (Sebastiano Vigna,
//! `splitmix64.c`, 2015), which is David Stafford's "Mix13" variant of the
//! MurmurHash3 `fmix64` finalizer:
//!
//! ```text
//! z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9
//! z = (z ^ (z >> 27)) * 0x94d049bb133111eb
//! z =  z ^ (z >> 31)
//! ```
//!
//! It is a bijection on `u64` with full avalanche, costs two multiplies, and
//! uses only fixed-width 64-bit wrapping integer arithmetic, so it is
//! bit-identical on every platform.
//!
//! Lattice coordinates are combined as `key ^ x*PX ^ y*PY ^ z*PZ` (wrapping
//! 64-bit multiplies by large odd constants) and then mixed once. `key` is
//! `mix64(seed)`, so every `u64` seed gives an independent, non-tiling field.

/// Odd multiplier for the x coordinate: the 64-bit golden-ratio constant
/// `0x9E3779B97F4A7C15` (also SplitMix64's increment).
const PX: u64 = 0x9E37_79B9_7F4A_7C15;
/// Odd multiplier for the y coordinate: xxHash64 `PRIME64_2`.
const PY: u64 = 0xC2B2_AE3D_27D4_EB4F;
/// Odd multiplier for the z coordinate: xxHash64 `PRIME64_3`.
const PZ: u64 = 0x1656_67B1_9E37_79F9;

/// SplitMix64 finalizer (Stafford Mix13). Bijective 64-bit avalanche mix.
#[inline]
pub const fn mix64(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Sign-extends an `i32` lattice coordinate to a fixed-width `u64`.
#[inline]
const fn coord(v: i32) -> u64 {
    v as i64 as u64
}

/// 64-bit hash of a 2D lattice point under a pre-mixed key (`mix64(seed)`).
#[inline]
pub(crate) const fn lattice2(key: u64, x: i32, y: i32) -> u64 {
    mix64(key ^ coord(x).wrapping_mul(PX) ^ coord(y).wrapping_mul(PY))
}

/// 64-bit hash of a 3D lattice point under a pre-mixed key (`mix64(seed)`).
#[inline]
pub(crate) const fn lattice3(key: u64, x: i32, y: i32, z: i32) -> u64 {
    mix64(key ^ coord(x).wrapping_mul(PX) ^ coord(y).wrapping_mul(PY) ^ coord(z).wrapping_mul(PZ))
}

/// Public 32-bit coordinate hash: `hash2(seed, x, y)`.
#[inline]
pub const fn hash2(seed: u64, x: i32, y: i32) -> u32 {
    (lattice2(mix64(seed), x, y) >> 32) as u32
}

/// Public 32-bit coordinate hash: `hash3(seed, x, y, z)`.
#[inline]
pub const fn hash3(seed: u64, x: i32, y: i32, z: i32) -> u32 {
    (lattice3(mix64(seed), x, y, z) >> 32) as u32
}
