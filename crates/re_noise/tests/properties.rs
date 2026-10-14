//! Statistical and structural property tests.

use re_noise::*;

type F2 = fn(u64, f32, f32) -> f32;
type F3 = fn(u64, f32, f32, f32) -> f32;
type DynF<'a> = dyn Fn(u64, f32, f32, f32) -> f32 + 'a;
const N2: [(&str, F2); 2] = [("perlin2", perlin2), ("simplex2", simplex2)];
const N3: [(&str, F3); 2] = [("perlin3", perlin3), ("simplex3", simplex3)];

fn points(seed: u64, n: usize, span: f32) -> Vec<(f32, f32, f32)> {
    let mut r = Pcg32::from_seed(seed);
    (0..n)
        .map(|_| {
            (
                r.range_f32(-span, span),
                r.range_f32(-span, span),
                r.range_f32(-span, span),
            )
        })
        .collect()
}

/// Asserts bounds and returns (mean, min, max, std-dev).
fn stats(name: &str, vals: impl Iterator<Item = f32>, lo: f32, hi: f32) -> (f64, f32, f32, f64) {
    let (mut s, mut s2, mut n, mut mn, mut mx) = (0f64, 0f64, 0f64, f32::MAX, f32::MIN);
    for v in vals {
        assert!(
            v.is_finite() && (lo..=hi).contains(&v),
            "{name}: {v} outside [{lo}, {hi}]"
        );
        s += f64::from(v);
        s2 += f64::from(v) * f64::from(v);
        n += 1.0;
        mn = mn.min(v);
        mx = mx.max(v);
    }
    let mean = s / n;
    (mean, mn, mx, (s2 / n - mean * mean).sqrt())
}

#[test]
fn gradient_noise_range_and_mean() {
    let pts = points(1, 200_000, 1000.0);
    for (name, f) in N2 {
        let (mean, mn, mx, sd) = stats(name, pts.iter().map(|p| f(9, p.0, p.1)), -1.0, 1.0);
        assert!(mean.abs() < 0.01, "{name} mean {mean}");
        assert!(
            mn < -0.75 && mx > 0.75 && sd > 0.1,
            "{name} poorly spread: {mn} {mx} {sd}"
        );
    }
    for (name, f) in N3 {
        let (mean, mn, mx, sd) = stats(name, pts.iter().map(|p| f(9, p.0, p.1, p.2)), -1.0, 1.0);
        assert!(mean.abs() < 0.01, "{name} mean {mean}");
        assert!(
            mn < -0.7 && mx > 0.7 && sd > 0.1,
            "{name} poorly spread: {mn} {mx} {sd}"
        );
    }
}

#[test]
fn fractal_ranges() {
    let pts = points(2, 50_000, 200.0);
    for basis in [Basis::Perlin, Basis::Simplex] {
        let f = Fractal {
            basis,
            ..Fractal::default()
        };
        let (m, ..) = stats("fbm2", pts.iter().map(|p| fbm2(3, &f, p.0, p.1)), -1.0, 1.0);
        assert!(m.abs() < 0.02, "fbm2 mean {m}");
        stats(
            "fbm3",
            pts.iter().map(|p| fbm3(3, &f, p.0, p.1, p.2)),
            -1.0,
            1.0,
        );
        stats(
            "ridged2",
            pts.iter().map(|p| ridged2(3, &f, p.0, p.1)),
            0.0,
            1.0,
        );
        stats(
            "ridged3",
            pts.iter().map(|p| ridged3(3, &f, p.0, p.1, p.2)),
            0.0,
            1.0,
        );
        stats(
            "warped2",
            pts.iter().map(|p| warped_fbm2(3, &f, 1.0, p.0, p.1)),
            -1.0,
            1.0,
        );
        stats(
            "warped3",
            pts.iter().map(|p| warped_fbm3(3, &f, 1.0, p.0, p.1, p.2)),
            -1.0,
            1.0,
        );
    }
    let zero = Fractal {
        octaves: 0,
        ..Fractal::default()
    };
    assert_eq!(fbm2(1, &zero, 0.3, 0.4), 0.0);
}

#[test]
fn cellular_invariants() {
    for p in points(4, 50_000, 500.0) {
        let c = cellular2(5, p.0, p.1);
        assert!(0.0 <= c.f1 && c.f1 <= c.f2 && c.f2 < 1.6, "cellular2 {c:?}");
        let c = cellular3(5, p.0, p.1, p.2);
        assert!(0.0 <= c.f1 && c.f1 <= c.f2 && c.f2 < 1.8, "cellular3 {c:?}");
    }
    // Points in the same Voronoi region share an id; nearby points mostly do.
    let a = cellular2(5, 10.3, 20.7);
    assert_eq!(a.id, cellular2(5, 10.3001, 20.7001).id);
    assert_ne!(a.id, cellular2(6, 10.3, 20.7).id);
}

#[test]
fn seed_sensitivity() {
    let pts = points(6, 1000, 100.0);
    let f = Fractal::default();
    let differ = |g: &DynF<'_>| {
        pts.iter()
            .filter(|p| g(1, p.0, p.1, p.2).to_bits() != g(2, p.0, p.1, p.2).to_bits())
            .count()
    };
    let fns: [(&str, &DynF<'_>); 8] = [
        ("perlin2", &|s, x, y, _| perlin2(s, x, y)),
        ("perlin3", &|s, x, y, z| perlin3(s, x, y, z)),
        ("simplex2", &|s, x, y, _| simplex2(s, x, y)),
        ("simplex3", &|s, x, y, z| simplex3(s, x, y, z)),
        ("fbm2", &|s, x, y, _| fbm2(s, &f, x, y)),
        ("ridged2", &|s, x, y, _| ridged2(s, &f, x, y)),
        ("cellular2", &|s, x, y, _| cellular2(s, x, y).f1),
        ("cellular3", &|s, x, y, z| cellular3(s, x, y, z).f1),
    ];
    for (name, g) in fns {
        // With 8 discrete gradients, simplex 2D (often only 1-2 corners in
        // range) matches across seeds at ~1-2% of points by chance.
        let d = differ(g);
        assert!(d > 950, "{name} insensitive to seed: {d}");
    }
    assert_ne!(
        Pcg32::from_seed(1).next_u64(),
        Pcg32::from_seed(2).next_u64()
    );
}

#[test]
fn no_short_period_tiling() {
    // A permutation-table implementation repeats every 256 units.
    let same = (0..1000)
        .filter(|&i| {
            let x = i as f32 * 0.137 + 0.5;
            perlin2(1, x, 0.3) == perlin2(1, x + 256.0, 0.3)
        })
        .count();
    assert!(same < 5);
}

#[test]
fn continuity() {
    // Gradient noise is C1/C2-smooth: |df| <= L * |dx| for a modest L.
    let (eps, lipschitz) = (1.0e-3f32, 8.0f32);
    for p in points(7, 20_000, 300.0) {
        for (name, f) in N2 {
            let d = (f(3, p.0 + eps, p.1) - f(3, p.0, p.1)).abs();
            let d2 = (f(3, p.0, p.1 + eps) - f(3, p.0, p.1)).abs();
            assert!(d.max(d2) < lipschitz * eps, "{name} jump {d} at {p:?}");
        }
        for (name, f) in N3 {
            let d = (f(3, p.0, p.1, p.2 + eps) - f(3, p.0, p.1, p.2)).abs();
            assert!(d < lipschitz * eps, "{name} jump {d} at {p:?}");
        }
    }
}

#[test]
fn fork_is_order_independent() {
    let parent = Pcg32::new(99, 7);
    let keys = [3u64, 1, 4, 1_000_000, 5, 9];
    let forward: Vec<u64> = keys.iter().map(|&k| parent.fork(k).next_u64()).collect();
    let backward: Vec<u64> = keys
        .iter()
        .rev()
        .map(|&k| parent.fork(k).next_u64())
        .collect();
    assert!(forward.iter().eq(backward.iter().rev()));
    // Chunk generation from a world generator, in two different orders.
    let world = Pcg32::from_seed(2024);
    let gen_chunk = |x: i32, y: i32| {
        let mut r = world.fork2(x, y);
        (0..4).map(|_| r.next_u32()).collect::<Vec<_>>()
    };
    let a = gen_chunk(-5, 17);
    let _ = (gen_chunk(0, 0), gen_chunk(1, 0));
    assert_eq!(a, gen_chunk(-5, 17));
    assert_ne!(a, gen_chunk(17, -5));
    // Distinct keys give distinct streams; forking does not advance parent.
    assert_ne!(forward[0], forward[1]);
    let p2 = parent.clone();
    let _ = p2.fork(1);
    assert_eq!(p2, parent);
}

#[test]
fn range_u32_unbiased() {
    // Range 3 * 2^30 is the worst case for modulo bias (values < 2^30 would be
    // hit twice as often by `x % range`). Lemire with rejection must be flat.
    let mut r = Pcg32::from_seed(11);
    let range = 3u32 << 30;
    let n = 300_000;
    let mut thirds = [0u32; 3];
    for _ in 0..n {
        let v = r.range_u32(0, range);
        assert!(v < range);
        thirds[(v >> 30) as usize] += 1;
    }
    let expect = n as f64 / 3.0;
    for t in thirds {
        assert!(
            (f64::from(t) - expect).abs() < 0.02 * expect,
            "biased: {thirds:?}"
        );
    }
    // Small range: chi-squared over 10 buckets (df = 9, p=0.001 crit ~27.9).
    let mut b = [0u32; 10];
    for _ in 0..100_000 {
        b[(r.range_u32(5, 15) - 5) as usize] += 1;
    }
    let chi: f64 = b
        .iter()
        .map(|&c| (f64::from(c) - 10_000.0).powi(2) / 10_000.0)
        .sum();
    assert!(chi < 27.9, "chi2 {chi} {b:?}");
    assert_eq!(r.range_u32(7, 8), 7);
}

#[test]
fn float_ranges() {
    let mut r = Pcg32::from_seed(12);
    for _ in 0..100_000 {
        let (a, b) = (r.next_f32(), r.next_f64());
        assert!((0.0..1.0).contains(&a) && (0.0..1.0).contains(&b));
        let c = r.range_f32(-2.0, 3.0);
        assert!((-2.0..=3.0).contains(&c));
    }
}
