//! Checks every finite f32 (a manual run; the test suite samples 1M+ bit
//! patterns): written as JSON text (serde_json, as scene files do), parsed
//! back as f64 (as `Doc` does), then rounded to f32, both directly and with
//! [`re_reflect::f32_from_parsed_f64`].
//!
//! ```sh
//! cargo run --locked -p re_reflect --release --example f32_exhaustive
//! ```
//!
//! Expected: 4,278,190,080 values checked, 2 direct mismatches
//! (±7.038531e-26), 0 with `f32_from_parsed_f64`.

use std::thread;

fn main() {
    let threads = thread::available_parallelism().map_or(4, |n| n.get()) as u64;
    let t0 = std::time::Instant::now();
    let handles: Vec<_> = (0..threads)
        .map(|t| {
            thread::spawn(move || {
                let (mut checked, mut bad_direct, mut bad_fixed) = (0u64, 0u64, 0u64);
                let mut bits = t;
                while bits <= u32::MAX as u64 {
                    let f = f32::from_bits(bits as u32);
                    if f.is_finite() {
                        checked += 1;
                        let text = serde_json::to_string(&f).expect("finite f32 serializes");
                        let back: f64 = serde_json::from_str(&text).expect("JSON number parses");
                        if re_reflect::f32_from_parsed_f64(back).to_bits() != bits as u32 {
                            eprintln!("f32_from_parsed_f64 mismatch: {f:e}");
                            bad_fixed += 1;
                        }
                        if (back as f32).to_bits() != bits as u32 {
                            eprintln!(
                                "direct mismatch: bits {bits:#010x} = {f:e} -> text {text} -> f64 {back:e} -> f32 {:e}",
                                back as f32
                            );
                            bad_direct += 1;
                        }
                    }
                    bits += threads;
                }
                (checked, bad_direct, bad_fixed)
            })
        })
        .collect();
    let (mut checked, mut bad_direct, mut bad_fixed) = (0, 0, 0);
    for h in handles {
        let (c, d, x) = h.join().expect("worker thread");
        checked += c;
        bad_direct += d;
        bad_fixed += x;
    }
    println!(
        "finite f32 checked: {checked}; text->f64->f32 mismatches: {bad_direct}; with f32_from_parsed_f64: {bad_fixed}; {:.1?}",
        t0.elapsed()
    );
    if bad_fixed != 0 {
        std::process::exit(1);
    }
}
