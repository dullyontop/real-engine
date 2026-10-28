//! PerfReport (ADR-0022): per-phase frame times (mean, p95, max), counts and,
//! when a renderer draws, what it drew and what that cost (the amendment for
//! WP32).

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

/// The index of the 95th percentile of `n` sorted samples (nearest rank).
fn p95_index(n: usize) -> usize {
    ((0.95 * n as f64).ceil() as usize).clamp(1, n) - 1
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
        TimeStats {
            samples: n as u64,
            mean_ms: sorted.iter().sum::<f64>() / n as f64,
            p95_ms: sorted[p95_index(n)],
            max_ms: sorted[n - 1],
        }
    }
}

/// Mean, 95th percentile and maximum of a count over frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CountStats {
    /// Mean.
    pub mean: f64,
    /// 95th percentile (nearest rank).
    pub p95: u64,
    /// Maximum.
    pub max: u64,
}

impl CountStats {
    /// The statistics of `counts` (one a frame; any order).
    pub fn of(counts: &[u64]) -> CountStats {
        // As TimeStats: counts are exact in an f64 up to 2^53.
        let t = TimeStats::of(&counts.iter().map(|&c| c as f64).collect::<Vec<_>>());
        CountStats {
            mean: t.mean_ms,
            p95: t.p95_ms as u64,
            max: t.max_ms as u64,
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

/// The live count and GPU bytes of one kind of renderer resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ResourceCount {
    /// Live resources.
    pub count: u64,
    /// GPU memory they use, in bytes.
    pub bytes: u64,
}

impl From<re_render::ResourceStats> for ResourceCount {
    fn from(s: re_render::ResourceStats) -> ResourceCount {
        ResourceCount {
            count: s.count as u64,
            bytes: s.bytes,
        }
    }
}

/// The renderer's live resources at the end of the run (`RenderStats`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RendererResources {
    /// Uploaded meshes.
    pub meshes: ResourceCount,
    /// Uploaded textures.
    pub textures: ResourceCount,
    /// Created materials.
    pub materials: ResourceCount,
}

/// The GPU time of a timed frame's passes, by group, over the timed frames.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GpuPassTimes {
    /// The shadow cascades.
    pub shadow: TimeStats,
    /// The scene pass.
    pub scene: TimeStats,
    /// The post chain.
    pub post: TimeStats,
    /// The highlights and the world quads drawn on top.
    pub overlay: TimeStats,
    /// The UI painted over the frame.
    pub ui: TimeStats,
    /// All of a frame's passes.
    pub total: TimeStats,
}

/// What the renderer drew during the run and what it cost (ADR-0022's
/// amendment for WP32).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RendererReport {
    /// The wgpu backend (`vulkan`, `gl`, `metal`, `dx12`, `webgpu`).
    pub backend: String,
    /// The adapter: its name, backend, type and driver.
    pub adapter: String,
    /// The frames whose main view the renderer drew: a window's frames and
    /// captures.
    pub frames: u64,
    /// The live resources at the end of the run.
    pub resources: RendererResources,
    /// Each count of the renderer's `FrameStats` over the main views drawn,
    /// by name (`draw_calls`, `triangles`, `lights_dropped`, …).
    pub per_frame: BTreeMap<String, CountStats>,
    /// The other views drawn: a compat game's surfaces and viewports, an
    /// editor's scene view. Their counts are not in `per_frame`.
    pub extra_views: u64,
    /// The GPU time of the passes of timed frames, where the device can time
    /// them (not on GL). Measurements, not checks: times arrive a frame or
    /// two after their frame, and a frame whose readback is still in use is
    /// not timed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_ms: Option<GpuPassTimes>,
    /// Every renderer warning of the views drawn, counted by code.
    pub warnings: BTreeMap<String, u64>,
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
    /// One physics step: `Physics::step`, and in a compat game the server's parts
    /// and characters with their touch events, which step with it.
    pub physics_step: TimeStats,
    /// Drawing one frame on the CPU (ADR-0038): the renderer preparing,
    /// encoding and finishing the command buffers of the 3D view, a compat
    /// game's surfaces and viewports, and the UI, which is what a render
    /// thread would take over. Waits for the GPU are not in it: presentation,
    /// readback, and `Queue::submit` (`queue_submit`). Taken when frames are
    /// drawn (in a window, and for captures), after the frame the phases
    /// measure.
    #[serde(default)]
    pub render_submit: TimeStats,
    /// The frame's time inside `Queue::submit`. Software rasterisers and some
    /// GL drivers wait there until the GPU is done with the previous frame, so
    /// there it mostly measures the GPU.
    #[serde(default)]
    pub queue_submit: TimeStats,
    /// Counts at the end of the run.
    pub counts: PerfCounts,
    /// What the renderer drew, when it drew a view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<RendererReport>,
}

/// Collects timing samples during a run.
#[derive(Clone, Debug, Default)]
pub struct PerfRecorder {
    frame: Vec<f64>,
    current: BTreeMap<Phase, f64>,
    phases: BTreeMap<Phase, Vec<f64>>,
    physics_step: Vec<f64>,
    render_submit: Vec<f64>,
    queue_submit: Vec<f64>,
    renderer: RendererSamples,
}

/// What the renderer drew, from [`PerfRecorder::drawn`].
#[derive(Clone, Debug, Default)]
struct RendererSamples {
    frames: Vec<re_render::FrameStats>,
    extra_views: u64,
    gpu: Vec<re_render::GpuTimes>,
    warnings: BTreeMap<String, u64>,
}

impl RendererSamples {
    /// `base` ([`Graphics::renderer_report`](crate::gfx::Graphics::renderer_report))
    /// with these samples.
    fn report(&self, base: RendererReport) -> RendererReport {
        let counts: Vec<_> = self
            .frames
            .iter()
            .map(re_render::FrameStats::counts)
            .collect();
        let per_frame = re_render::FrameStats::default()
            .counts()
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                let column: Vec<u64> = counts.iter().map(|c| c[i].1).collect();
                (name.to_string(), CountStats::of(&column))
            })
            .collect();
        let gpu = |part: fn(&re_render::GpuTimes) -> std::time::Duration| {
            TimeStats::of(&self.gpu.iter().map(|t| ms(part(t))).collect::<Vec<_>>())
        };
        RendererReport {
            frames: self.frames.len() as u64,
            per_frame,
            extra_views: self.extra_views,
            gpu_ms: (!self.gpu.is_empty()).then(|| GpuPassTimes {
                shadow: gpu(|t| t.shadow),
                scene: gpu(|t| t.scene),
                post: gpu(|t| t.post),
                overlay: gpu(|t| t.overlay),
                ui: gpu(|t| t.ui),
                total: gpu(|t| t.total),
            }),
            warnings: self.warnings.clone(),
            ..base
        }
    }
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

    /// Records one physics step ([`PerfReport::physics_step`]).
    pub fn physics_step(&mut self, d: Duration) {
        self.physics_step.push(ms(d));
    }

    /// Records the renderer's time for drawing one frame.
    pub fn draw(&mut self, t: re_render::DrawTime) {
        self.render_submit.push(ms(t.encode));
        self.queue_submit.push(ms(t.submit));
    }

    /// Records what the renderer drew ([`Graphics::take_drawn`](crate::gfx::Graphics::take_drawn)).
    pub fn drawn(&mut self, drawn: &crate::gfx::Drawn) {
        let r = &mut self.renderer;
        r.frames.extend_from_slice(&drawn.frames);
        r.extra_views += drawn.extra_views;
        r.gpu.extend_from_slice(&drawn.gpu);
        for code in &drawn.warnings {
            *r.warnings.entry(code.to_string()).or_default() += 1;
        }
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

    /// The report so far, with the given counts. `renderer`, the app's
    /// [`Graphics::renderer_report`](crate::gfx::Graphics::renderer_report),
    /// gets the views recorded, and is left out when none was drawn.
    pub fn report(&self, counts: PerfCounts, renderer: Option<RendererReport>) -> PerfReport {
        PerfReport {
            frames: self.frames(),
            frame: TimeStats::of(&self.frame),
            phases: self
                .phases
                .iter()
                .map(|(p, v)| (p.as_str().to_string(), TimeStats::of(v)))
                .collect(),
            physics_step: TimeStats::of(&self.physics_step),
            render_submit: TimeStats::of(&self.render_submit),
            queue_submit: TimeStats::of(&self.queue_submit),
            counts,
            renderer: renderer
                .filter(|_| !self.renderer.frames.is_empty() || self.renderer.extra_views > 0)
                .map(|base| self.renderer.report(base)),
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
    fn counts_use_nearest_rank() {
        let c = CountStats::of(&(1..=100).rev().collect::<Vec<u64>>());
        assert_eq!((c.p95, c.max), (95, 100));
        assert!((c.mean - 50.5).abs() < 1e-9);
        assert_eq!(CountStats::of(&[]), CountStats::default());
    }

    #[test]
    fn the_renderer_part_counts_main_views_and_warnings() {
        use re_render::{FrameStats, GpuTimes};
        let base = RendererReport {
            backend: "vulkan".into(),
            adapter: "lavapipe".into(),
            ..RendererReport::default()
        };
        let mut r = PerfRecorder::new();
        let report =
            |r: &PerfRecorder| r.report(PerfCounts::default(), Some(base.clone())).renderer;
        assert_eq!(report(&r), None, "no view drawn");
        let frame = |draw_calls| FrameStats {
            draw_calls,
            lights_dropped: 2,
            ..FrameStats::default()
        };
        r.drawn(&crate::gfx::Drawn {
            frames: vec![frame(4), frame(6)],
            extra_views: 3,
            warnings: vec![re_render::codes::TOO_MANY_LIGHTS; 3],
            ..Default::default()
        });
        let renderer = report(&r).unwrap();
        assert_eq!(
            (
                renderer.backend.as_str(),
                renderer.frames,
                renderer.extra_views
            ),
            ("vulkan", 2, 3)
        );
        assert_eq!(
            renderer.per_frame.len(),
            FrameStats::default().counts().len(),
            "every count"
        );
        assert_eq!(
            renderer.per_frame["draw_calls"],
            CountStats {
                mean: 5.0,
                p95: 6,
                max: 6
            }
        );
        assert_eq!(renderer.per_frame["lights_dropped"].max, 2);
        let code = re_render::codes::TOO_MANY_LIGHTS.to_string();
        assert_eq!(renderer.warnings[&code], 3);
        assert_eq!(renderer.gpu_ms, None, "nothing timed");

        let ms = Duration::from_millis;
        r.drawn(&crate::gfx::Drawn {
            gpu: vec![GpuTimes {
                shadow: ms(1),
                scene: ms(2),
                total: ms(3),
                ..GpuTimes::default()
            }],
            ..Default::default()
        });
        let gpu = report(&r).unwrap().gpu_ms.unwrap();
        assert_eq!((gpu.total.samples, gpu.total.mean_ms), (1, 3.0));
        assert_eq!(
            (gpu.shadow.max_ms, gpu.scene.max_ms, gpu.ui.max_ms),
            (1.0, 2.0, 0.0)
        );
        // Without a renderer, or after a reset, there is no renderer part.
        assert_eq!(r.report(PerfCounts::default(), None).renderer, None);
        r.clear();
        assert_eq!(report(&r), None);
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
