//! Seedable pseudo-random generators: PCG32 and SplitMix64.

use crate::hash::mix64;

/// SplitMix64 (Vigna, 2015). Used to expand one `u64` seed into many
/// well-distributed `u64`s (e.g. a PCG32 state and stream).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    /// Golden-ratio increment from the reference implementation.
    pub const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

    /// A generator starting from `seed`.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// The next 64-bit output.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(Self::GAMMA);
        mix64(self.state)
    }
}

/// LCG multiplier from the PCG reference (`PCG_DEFAULT_MULTIPLIER_64`).
const PCG_MULT: u64 = 6_364_136_223_846_793_005;

/// PCG32: `pcg_setseq_64_xsh_rr_32` from M. E. O'Neill's PCG reference.
///
/// 64-bit LCG state, selectable stream (odd increment), 32-bit XSH-RR output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pcg32 {
    state: u64,
    inc: u64,
}

impl Pcg32 {
    /// Equivalent to the reference `pcg32_srandom_r(rng, seed, stream)`.
    pub const fn new(seed: u64, stream: u64) -> Self {
        let inc = (stream << 1) | 1;
        let mut state = 0u64.wrapping_mul(PCG_MULT).wrapping_add(inc);
        state = state.wrapping_add(seed);
        state = state.wrapping_mul(PCG_MULT).wrapping_add(inc);
        Self { state, inc }
    }

    /// Builds a generator from a single seed, expanding it with SplitMix64.
    pub fn from_seed(seed: u64) -> Self {
        let mut sm = SplitMix64::new(seed);
        let s = sm.next_u64();
        Self::new(s, sm.next_u64())
    }

    /// The next 32-bit output.
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(PCG_MULT).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Two consecutive outputs, first one in the high half.
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let hi = u64::from(self.next_u32());
        (hi << 32) | u64::from(self.next_u32())
    }

    /// Uniform in `[0, 1)`: top 24 bits scaled by 2^-24 (exact, no rounding).
    #[inline]
    pub fn next_f32(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }

    /// Uniform in `[0, 1)`: top 53 bits scaled by 2^-53 (exact, no rounding).
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
    }

    /// Unbiased uniform integer in `[lo, hi)` using Lemire's
    /// multiply-shift method with rejection ("Fast Random Integer Generation
    /// in an Interval", 2019). Panics if `lo >= hi`.
    #[inline]
    pub fn range_u32(&mut self, lo: u32, hi: u32) -> u32 {
        assert!(lo < hi, "range_u32: empty range {lo}..{hi}");
        let range = hi - lo;
        let mut m = u64::from(self.next_u32()) * u64::from(range);
        if (m as u32) < range {
            let threshold = range.wrapping_neg() % range;
            while (m as u32) < threshold {
                m = u64::from(self.next_u32()) * u64::from(range);
            }
        }
        lo + (m >> 32) as u32
    }

    /// `lo + (hi - lo) * next_f32()`. Nominally `[lo, hi)`; for some spans
    /// f32 rounding of the final add can produce exactly `hi`.
    #[inline]
    pub fn range_f32(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next_f32()
    }

    /// Derives an independent child generator for `key` without advancing
    /// `self`. Because the parent is not mutated, forking chunk `(x, y)` gives
    /// the same child regardless of which other forks happened before it, so
    /// procedural generation is independent of generation order.
    ///
    /// Takes `&self` (not `&mut self`) precisely so it cannot perturb the
    /// parent stream.
    pub fn fork(&self, key: u64) -> Pcg32 {
        let mut sm = SplitMix64::new(self.state ^ mix64(key ^ self.inc.rotate_left(32)));
        let s = sm.next_u64();
        Pcg32::new(s, sm.next_u64())
    }

    /// Convenience: fork keyed by a 2D integer coordinate (e.g. a chunk).
    pub fn fork2(&self, x: i32, y: i32) -> Pcg32 {
        self.fork(u64::from(x as u32) | (u64::from(y as u32) << 32))
    }
}
