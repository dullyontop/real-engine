//! Fixed-timestep simulation clock with render interpolation (ADR-0009).
//!
//! Gameplay and physics step at a constant `dt`, so results do not depend on the
//! frame rate; the renderer draws `lerp(previous, current, alpha)`. Time is kept in
//! integer nanoseconds, so the accumulator never drifts, and the number of steps
//! per frame is capped, so one long hitch (a breakpoint, a load) cannot cause a
//! "spiral of death": whole steps beyond the cap are dropped and reported.
//!
//! It also re-exports the wall clocks every crate measures with. `std::time::Instant::now`
//! panics on `wasm32-unknown-unknown`, so the engine uses web-time's, which is
//! `std::time`'s on every other target (ADR-0049 §5).

use std::time::Duration;

pub use web_time::{Instant, SystemTime, UNIX_EPOCH};

/// What one call to [`FixedClock::advance`] asks the frame to do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Steps {
    /// Fixed updates to run this frame (at most the clock's cap).
    pub count: u32,
    /// Blend factor in `[0, 1)` between the previous and the current simulation state.
    pub alpha: f32,
    /// Time discarded because the frame needed more than the cap allows.
    pub dropped: Duration,
}

/// The fixed-step clock: 60 Hz and at most 5 steps per frame by default.
///
/// ```
/// use std::time::Duration;
/// use re_core::FixedClock;
///
/// let mut clock = FixedClock::default();
/// let steps = clock.advance(Duration::from_millis(40));
/// assert_eq!(steps.count, 2); // 40 ms holds two whole 16.67 ms steps
/// assert!(steps.alpha > 0.0 && steps.alpha < 1.0);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedClock {
    dt_ns: u64,
    accumulator_ns: u64,
    max_steps_per_frame: u32,
    tick: u64,
}

impl FixedClock {
    /// The default simulation rate.
    pub const DEFAULT_HZ: u32 = 60;
    /// The default (and recommended) cap on catch-up steps per frame.
    pub const DEFAULT_MAX_STEPS_PER_FRAME: u32 = 5;

    /// A clock stepping `hz` times per second, running at most
    /// `max_steps_per_frame` steps per frame.
    ///
    /// The step is `1_000_000_000 / hz` nanoseconds, rounded down.
    ///
    /// # Panics
    ///
    /// Panics if `hz` or `max_steps_per_frame` is zero, or `hz` exceeds one billion.
    pub fn new(hz: u32, max_steps_per_frame: u32) -> FixedClock {
        assert!(
            hz > 0 && hz <= 1_000_000_000,
            "FixedClock: hz must be in 1..=1e9"
        );
        assert!(
            max_steps_per_frame > 0,
            "FixedClock: max_steps_per_frame must be at least 1"
        );
        FixedClock {
            dt_ns: 1_000_000_000 / u64::from(hz),
            accumulator_ns: 0,
            max_steps_per_frame,
            tick: 0,
        }
    }

    /// The fixed step.
    pub fn dt(&self) -> Duration {
        Duration::from_nanos(self.dt_ns)
    }

    /// The fixed step in seconds, for simulation code that works in `f32`.
    pub fn dt_secs(&self) -> f32 {
        self.dt_ns as f32 / 1e9
    }

    /// The cap on steps per frame.
    pub fn max_steps_per_frame(&self) -> u32 {
        self.max_steps_per_frame
    }

    /// The total number of fixed steps taken: the simulation tick.
    pub fn tick(&self) -> u64 {
        self.tick
    }

    /// Time carried over to the next frame (always less than one step after `advance`).
    pub fn accumulated(&self) -> Duration {
        Duration::from_nanos(self.accumulator_ns)
    }

    /// Adds `frame_time` and returns how many fixed steps to run now.
    pub fn advance(&mut self, frame_time: Duration) -> Steps {
        let nanos = u64::try_from(frame_time.as_nanos()).unwrap_or(u64::MAX);
        self.advance_nanos(nanos)
    }

    /// Like [`advance`](Self::advance), with the frame time in nanoseconds.
    pub fn advance_nanos(&mut self, frame_ns: u64) -> Steps {
        self.accumulator_ns = self.accumulator_ns.saturating_add(frame_ns);
        let whole = self.accumulator_ns / self.dt_ns;
        let count = whole.min(u64::from(self.max_steps_per_frame));
        self.accumulator_ns -= count * self.dt_ns;
        let mut dropped_ns = 0;
        if self.accumulator_ns >= self.dt_ns {
            // Keep the fractional remainder so alpha stays meaningful; drop whole steps.
            let rem = self.accumulator_ns % self.dt_ns;
            dropped_ns = self.accumulator_ns - rem;
            self.accumulator_ns = rem;
        }
        self.tick += count;
        let alpha = (self.accumulator_ns as f64 / self.dt_ns as f64) as f32;
        Steps {
            // `count` is at most `max_steps_per_frame`, a u32.
            count: count as u32,
            alpha,
            dropped: Duration::from_nanos(dropped_ns),
        }
    }

    /// Forgets the carried-over time and the tick count (for a restart).
    pub fn reset(&mut self) {
        self.accumulator_ns = 0;
        self.tick = 0;
    }
}

impl Default for FixedClock {
    /// 60 Hz, at most 5 steps per frame.
    fn default() -> FixedClock {
        FixedClock::new(Self::DEFAULT_HZ, Self::DEFAULT_MAX_STEPS_PER_FRAME)
    }
}

/// Linear interpolation: `a` at `t = 0`, `b` at `t = 1`.
pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
