//! `FixedClock` tests, ported from the platform spike's timestep tests, plus the
//! M1 acceptance checks (at most five steps per frame, no drift over 10,000 frames).

use std::time::Duration;

use re_core::FixedClock;
use re_core::time::lerp;

#[test]
fn steps_match_wall_time_under_jittery_frames() {
    let mut clock = FixedClock::new(60, 8);
    // 1 s of jittery frames: 7 ms, 23 ms, 16 ms repeating, plus remainder.
    let pattern = [7u64, 23, 16];
    let mut elapsed = 0u64;
    let mut steps = 0;
    let mut i = 0;
    while elapsed < 1000 {
        let ms = pattern[i % 3].min(1000 - elapsed);
        elapsed += ms;
        i += 1;
        let s = clock.advance(Duration::from_millis(ms));
        assert!((0.0..1.0).contains(&s.alpha), "alpha {}", s.alpha);
        assert_eq!(s.dropped, Duration::ZERO);
        steps += s.count;
    }
    println!(
        "{i} frames over 1 s -> {steps} fixed steps, tick {}",
        clock.tick()
    );
    assert_eq!(steps, 60);
}

#[test]
fn hitch_is_capped_and_time_is_dropped() {
    let mut clock = FixedClock::new(60, 5);
    let s = clock.advance(Duration::from_secs(2));
    println!("2 s hitch -> {s:?}");
    assert_eq!(s.count, 5);
    assert!(s.dropped > Duration::from_millis(1800));
    let next = clock.advance(Duration::from_millis(16));
    assert!(next.count <= 1, "recovers immediately: {next:?}");
}

#[test]
fn same_frame_times_give_same_ticks() {
    let frames: Vec<Duration> = (0..500)
        .map(|i| Duration::from_micros(5_000 + (i * 7919 % 20_000)))
        .collect();
    let run = || {
        let mut clock = FixedClock::new(120, 8);
        frames
            .iter()
            .map(|f| clock.advance(*f).count)
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn interpolation_hides_step_rate() {
    // A body moving 1 unit per step, rendered at 144 Hz from a 60 Hz simulation:
    // the rendered position must be monotonic and never jump more than one step.
    let mut clock = FixedClock::new(60, 8);
    let (mut prev, mut curr) = (0.0f32, 0.0f32);
    let mut last_drawn = 0.0f32;
    for _ in 0..288 {
        let s = clock.advance(Duration::from_nanos(1_000_000_000 / 144));
        for _ in 0..s.count {
            prev = curr;
            curr += 1.0;
        }
        let drawn = lerp(prev, curr, s.alpha);
        assert!(drawn >= last_drawn && drawn - last_drawn <= 1.0);
        last_drawn = drawn;
    }
    println!(
        "after 2 s at 144 Hz: tick {}, drawn {last_drawn:.3}",
        clock.tick()
    );
}

#[test]
fn default_is_60_hz_with_a_cap_of_5() {
    let clock = FixedClock::default();
    assert_eq!(clock.dt(), Duration::from_nanos(16_666_666));
    assert_eq!(clock.max_steps_per_frame(), 5);
    assert_eq!(clock.tick(), 0);
}

/// Deterministic frame times between 0 and 120 ms (xorshift), including hitches
/// longer than five steps.
fn frame_times(count: usize) -> Vec<u64> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    (0..count)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % 120_000_000
        })
        .collect()
}

#[test]
fn at_most_five_steps_per_frame_and_no_drift_over_10000_frames() {
    let mut clock = FixedClock::default();
    let dt = clock.dt().as_nanos() as u64;
    let mut elapsed = 0u128;
    let mut dropped = 0u128;
    let mut steps = 0u64;
    for ns in frame_times(10_000) {
        let s = clock.advance_nanos(ns);
        assert!(s.count <= 5, "{} steps in one frame", s.count);
        assert!((0.0..1.0).contains(&s.alpha), "alpha {}", s.alpha);
        elapsed += u128::from(ns);
        dropped += s.dropped.as_nanos();
        steps += u64::from(s.count);
        // Exact accounting after every frame: no nanosecond is lost or invented.
        assert_eq!(
            u128::from(clock.tick()) * u128::from(dt) + clock.accumulated().as_nanos() + dropped,
            elapsed
        );
        assert!(clock.accumulated().as_nanos() < u128::from(dt));
    }
    assert_eq!(steps, clock.tick());
    assert!(dropped > 0, "the sample includes hitches over the cap");
}

#[test]
fn steady_frames_never_drift_over_10000_frames() {
    // Frames of exactly one step, and of 1.5 steps: the tick count stays exactly
    // `floor(elapsed / dt)`, with nothing dropped.
    for frame in [16_666_666u64, 25_000_000] {
        let mut clock = FixedClock::default();
        let dt = clock.dt().as_nanos() as u64;
        for n in 1..=10_000u64 {
            let s = clock.advance_nanos(frame);
            assert_eq!(s.dropped, Duration::ZERO);
            assert_eq!(clock.tick(), n * frame / dt, "frame {n}");
        }
    }
}
