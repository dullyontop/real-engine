//! PerfReport (ADR-0022): per-phase frame times (mean, p95, max), counts and,
//! when a renderer runs, its resource statistics.

use std::collections::BTreeMap;
use std::time::Duration;

use re_core::Phase;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Mean, 95th percentile and maximum of a set of durations, in milliseconds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TimeStats {
    /// The number of samples.
    pub samples: u64,
    /// Mean, in ms.
    pub mean_ms: f64,
    /// 95th percentile (nearest rank), in ms.
    pub p95_ms: f64,
    /// Maximum, in ms.
    pub max_ms: f64,
}

impl TimeStats {
    /// The statistics of `ms` (milliseconds; any order).
    pub fn of(ms: &[f64]) -> TimeStats {
        if ms.is_empty() {
            return TimeStats::default();
        }
        let mut sorted = ms.to_vec();
        sorted.sort_by(f64::total_cmp);
        let n = sorted.len();
        let rank = ((0.95 * n as f64).ceil() as usize).clamp(1, n);
        TimeStats {
            samples: n as u64,
            mean_ms: sorted.iter().sum::<f64>() / n as f64,
            p95_ms: sorted[rank - 1],
            max_ms: sorted[n - 1],
        }
    }
}

/// Counts taken at the end of the measured run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PerfCounts {
    /// Entities in the world.
    pub entities: u64,
    /// Items drawn in the last frame.
    pub draw_items: u64,
    /// Physics bodies (character inner bodies included).
    pub bodies: u64,
    /// Physics characters.
    pub characters: u64,
}

/// The performance report of a run.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PerfReport {
    /// Frames measured.
    pub frames: u64,
    /// Whole frames.
    pub frame: TimeStats,
    /// Each phase of ADR-0009, by name (`input`, `pre_render`, … `end_frame`).
    /// Fixed-step phases are summed over the frame's steps.
    pub phases: BTreeMap<String, TimeStats>,
    /// One physics step (`Physics::step` alone).
    pub physics_step: TimeStats,
    /// Counts at the end of the run.
    pub counts: PerfCounts,
    /// The renderer's resource statistics, when a renderer ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<serde_json::Value>,
}

/// Collects timing samples during a run.
#[derive(Clone, Debug, Default)]
pub struct PerfRecorder {
    frame: Vec<f64>,
    current: BTreeMap<Phase, f64>,
    phases: BTreeMap<Phase, Vec<f64>>,
    physics_step: Vec<f64>,
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

impl PerfRecorder {
    /// An empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds time spent in `phase` during the current frame.
    pub fn add(&mut self, phase: Phase, d: Duration) {
        *self.current.entry(phase).or_default() += ms(d);
    }

    /// Records one `Physics::step`.
    pub fn physics_step(&mut self, d: Duration) {
        self.physics_step.push(ms(d));
    }

    /// Ends a frame that took `d` in total.
    pub fn end_frame(&mut self, d: Duration) {
        self.frame.push(ms(d));
        for phase in Phase::ALL {
            let v = self.current.remove(&phase).unwrap_or(0.0);
            self.phases.entry(phase).or_default().push(v);
        }
    }

    /// Frames recorded so far.
    pub fn frames(&self) -> u64 {
        self.frame.len() as u64
    }

    /// Forgets every sample (for example after warm-up frames).
    pub fn clear(&mut self) {
        *self = PerfRecorder::default();
    }

    /// The report so far, with the given counts and renderer statistics.
    pub fn report(&self, counts: PerfCounts, renderer: Option<serde_json::Value>) -> PerfReport {
        PerfReport {
            frames: self.frames(),
            frame: TimeStats::of(&self.frame),
            phases: self
                .phases
                .iter()
                .map(|(p, v)| (p.as_str().to_string(), TimeStats::of(v)))
                .collect(),
            physics_step: TimeStats::of(&self.physics_step),
            counts,
            renderer,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_use_nearest_rank() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = TimeStats::of(&v);
        assert_eq!(s.samples, 100);
        assert_eq!(s.p95_ms, 95.0);
        assert_eq!(s.max_ms, 100.0);
        assert!((s.mean_ms - 50.5).abs() < 1e-9);
        assert_eq!(TimeStats::of(&[]), TimeStats::default());
        assert_eq!(TimeStats::of(&[3.0]).p95_ms, 3.0);
    }

    #[test]
    fn every_phase_is_reported() {
        let mut r = PerfRecorder::new();
        r.add(Phase::Physics, Duration::from_millis(2));
        r.end_frame(Duration::from_millis(5));
        let rep = r.report(PerfCounts::default(), None);
        assert_eq!(rep.frames, 1);
        assert_eq!(rep.phases.len(), Phase::ALL.len());
        assert_eq!(rep.phases[Phase::Physics.as_str()].max_ms, 2.0);
    }
}
