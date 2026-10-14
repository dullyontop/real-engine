//! `re_noise`: deterministic, portable, seedable RNG and gradient noise.
//!
//! * [`Pcg32`] / [`SplitMix64`]: seedable generators; [`Pcg32::fork`] derives
//!   order-independent sub-streams for hierarchical procedural generation.
//! * [`hash2`] / [`hash3`]: stateless coordinate hashes (SplitMix64 finalizer).
//! * [`perlin2`], [`perlin3`], [`simplex2`], [`simplex3`]: gradient noise.
//! * [`fbm2`], [`ridged2`], [`warped_fbm2`] (and 3D): fractal combinations.
//! * [`cellular2`], [`cellular3`]: Worley F1 / F2 / cell id.
//!
//! Every noise function takes a `u64` seed. Lattice gradients come from
//! hashing `(seed, lattice coordinate)` rather than a permutation table, so
//! any seed works and there is no 256-unit tiling period.
//!
//! # Portable determinism rules
//!
//! Output must be bit-identical on every platform, compiler and opt level.
//! Code in this crate follows these rules; keep following them when editing:
//!
//! 1. **No transcendental float functions** (`sin`, `cos`, `exp`, `ln`,
//!    `powf`, ...): libm implementations differ between platforms. Constants
//!    such as `sqrt(3)` are written as literals. `sqrt` is IEEE-exact and is
//!    used only outside hot loops (once per cellular query).
//! 2. **No fused multiply-add**: never call `mul_add`. Rust does not contract
//!    `a * b + c` into FMA, so plain arithmetic stays portable.
//! 3. **Only `+ - * /`, `abs`, `min`/`max`/`clamp` and int<->float
//!    casts on floats**: all are exactly specified by IEEE 754 / Rust.
//!    Flooring uses a cast-based `floor_i32` (exact, and avoids a libm call
//!    on baseline x86_64).
//! 4. **Fixed-width integers in hashing**: `u32`/`u64`/`i32` with wrapping
//!    arithmetic; never `usize` (its width varies by target). `usize` is only
//!    used to index fixed-size tables with an already-reduced value.
//! 5. **No hash-map iteration order** (`HashMap`/`HashSet` are randomly
//!    seeded) and no reliance on thread scheduling.
//! 6. **Stable f32 accumulation order**: sums are accumulated in a fixed,
//!    sequential order (no parallel or SIMD reductions that reorder adds).
//! 7. Inputs are `f32`; precision degrades as `|coord|` grows beyond about
//!    2^20, and lattice indices saturate at the `i32` range.
//!
//! Golden-value tests (`tests/golden.rs`) pin exact output bits.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cellular;
pub mod fractal;
pub mod gradient;
pub mod hash;
pub mod rng;

pub use cellular::{Cell, cellular2, cellular3};
pub use fractal::{
    Basis, Fractal, fbm2, fbm3, ridged2, ridged3, warp2, warp3, warped_fbm2, warped_fbm3,
};
pub use gradient::{perlin2, perlin3, simplex2, simplex3};
pub use hash::{hash2, hash3, mix64};
pub use rng::{Pcg32, SplitMix64};
