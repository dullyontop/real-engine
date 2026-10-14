//! Zero-dependency micro-benchmark. Run with:
//! `cargo run --release --example bench`

use re_noise::*;
use std::hint::black_box;
use std::time::Instant;

const N: u32 = 512;
const FREQ: f32 = 1.0 / 37.0;

/// Times `f` over a 512x512 grid (best of 3 passes) and prints ns/sample.
fn grid(name: &str, f: impl Fn(f32, f32) -> f32) {
    let mut best = f64::MAX;
    for _ in 0..3 {
        let t = Instant::now();
        let mut acc = 0.0f32;
        for j in 0..N {
            for i in 0..N {
                acc += f(black_box(i as f32 * FREQ), black_box(j as f32 * FREQ));
            }
        }
        black_box(acc);
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!(
        "{name:<24} {:>8.2} ns/sample",
        best * 1e9 / f64::from(N * N)
    );
}

fn rng(name: &str, iters: u32, mut f: impl FnMut() -> u64) {
    let t = Instant::now();
    let mut acc = 0u64;
    for _ in 0..iters {
        acc = acc.wrapping_add(f());
    }
    black_box(acc);
    let s = t.elapsed().as_secs_f64();
    println!(
        "{name:<24} {:>8.2} ns/call  ({:.0} M/s)",
        s * 1e9 / f64::from(iters),
        f64::from(iters) / s / 1e6
    );
}

fn main() {
    let seed = black_box(1337u64);
    let f6 = Fractal::default();
    let s6 = Fractal {
        basis: Basis::Simplex,
        ..f6
    };
    println!("grid {N}x{N}, best of 3");
    grid("perlin2", |x, y| perlin2(seed, x, y));
    grid("perlin3", |x, y| perlin3(seed, x, y, 0.5 * x));
    grid("simplex2", |x, y| simplex2(seed, x, y));
    grid("simplex3", |x, y| simplex3(seed, x, y, 0.5 * x));
    grid("fbm2 perlin 6oct", |x, y| fbm2(seed, &f6, x, y));
    grid("fbm2 simplex 6oct", |x, y| fbm2(seed, &s6, x, y));
    grid("fbm3 perlin 6oct", |x, y| fbm3(seed, &f6, x, y, 0.5 * x));
    grid("ridged2 perlin 6oct", |x, y| ridged2(seed, &f6, x, y));
    grid("warped_fbm2 perlin 6oct", |x, y| {
        warped_fbm2(seed, &f6, 0.8, x, y)
    });
    grid("cellular2 (F1+F2+id)", |x, y| cellular2(seed, x, y).f1);
    grid("cellular3 (F1+F2+id)", |x, y| {
        cellular3(seed, x, y, 0.5 * x).f1
    });

    const ITERS: u32 = 50_000_000;
    let mut r = Pcg32::from_seed(seed);
    rng("pcg32 next_u32", ITERS, || u64::from(r.next_u32()));
    rng("pcg32 next_u64", ITERS, || r.next_u64());
    rng("pcg32 next_f32", ITERS, || {
        u64::from(r.next_f32().to_bits())
    });
    rng("pcg32 range_u32(0,1000)", ITERS, || {
        u64::from(r.range_u32(0, 1000))
    });
    let mut sm = SplitMix64::new(seed);
    rng("splitmix64 next_u64", ITERS, || sm.next_u64());
    let mut i = 0i32;
    rng("hash2", ITERS, || {
        i = i.wrapping_add(1);
        u64::from(hash2(seed, i, -i))
    });
}
