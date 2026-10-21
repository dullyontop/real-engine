//! Sampling clips: the value of every animated property at a time.

use glam::{Quat, Vec3};
use re_core::Diagnostic;
use re_types::anim::{Channel, ClipData, Interpolation};

use crate::codes;

/// The sampled transform parts of one animated target.
///
/// A part is `None` when the clip has no channel for it; the target keeps its
/// own value for that part.
#[derive(Clone, Debug, PartialEq)]
pub struct TargetPose {
    /// The target's index in [`ClipData::targets`].
    pub index: usize,
    /// The sampled translation.
    pub translation: Option<Vec3>,
    /// The sampled rotation (unit length).
    pub rotation: Option<Quat>,
    /// The sampled scale.
    pub scale: Option<Vec3>,
}

/// Evaluates a [`ClipData`] at any time.
///
/// The clip is checked once, in [`Sampler::new`]; sampling never fails. Times
/// before the first keyframe hold the first value and times after the last hold
/// the last value, as glTF specifies; a NaN time samples the first keyframe.
/// Use [`Sampler::wrap_time`] (or a [`Playhead`]) to loop.
#[derive(Clone, Debug)]
pub struct Sampler {
    clip: ClipData,
}

impl Sampler {
    /// Checks every channel of `clip` and wraps it.
    ///
    /// # Errors
    ///
    /// [`codes::CLIP_MALFORMED`] when a channel has times that are not finite or
    /// not strictly increasing, or a value count that does not match its
    /// interpolation (one value per key, three for cubic spline).
    pub fn new(clip: ClipData) -> Result<Sampler, Diagnostic> {
        for target in &clip.targets {
            check(
                &clip.name,
                &target.target,
                "translation",
                target.translation.as_ref(),
            )?;
            check(
                &clip.name,
                &target.target,
                "rotation",
                target.rotation.as_ref(),
            )?;
            check(&clip.name, &target.target, "scale", target.scale.as_ref())?;
        }
        Ok(Sampler { clip })
    }

    /// The clip.
    pub fn clip(&self) -> &ClipData {
        &self.clip
    }

    /// The clip length in seconds.
    pub fn duration(&self) -> f32 {
        self.clip.duration
    }

    /// Maps `time` into the clip: modulo the duration when `looping`, otherwise
    /// clamped to `[0, duration]`. A zero-length clip always maps to 0.
    pub fn wrap_time(&self, time: f32, looping: bool) -> f32 {
        wrap(time, self.clip.duration, looping)
    }

    /// Samples one target, by its index in [`ClipData::targets`].
    ///
    /// Returns `None` when there is no target with that index.
    pub fn sample_target(&self, index: usize, time: f32) -> Option<TargetPose> {
        let t = self.clip.targets.get(index)?;
        Some(TargetPose {
            index,
            translation: t.translation.as_ref().map(|c| sample_vec3(c, time)),
            rotation: t.rotation.as_ref().map(|c| sample_quat(c, time)),
            scale: t.scale.as_ref().map(|c| sample_vec3(c, time)),
        })
    }

    /// Samples every target at `time`, replacing the contents of `out` (one entry
    /// per target, in target order). Reuse `out` across frames to avoid
    /// allocating.
    pub fn sample_into(&self, time: f32, out: &mut Vec<TargetPose>) {
        out.clear();
        out.extend((0..self.clip.targets.len()).filter_map(|i| self.sample_target(i, time)));
    }

    /// Samples every target at `time`.
    pub fn sample(&self, time: f32) -> Vec<TargetPose> {
        let mut out = Vec::with_capacity(self.clip.targets.len());
        self.sample_into(time, &mut out);
        out
    }

    /// The index of the target with this name, if the clip animates it.
    pub fn target_index(&self, name: &str) -> Option<usize> {
        self.clip.targets.iter().position(|t| t.target == name)
    }
}

/// Playback state for one clip: what an `AnimPlayer` component describes.
///
/// `time` is the unwrapped play time in seconds; [`Playhead::clip_time`] maps it
/// into the clip.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Playhead {
    /// Seconds played (scaled by `speed`), unwrapped.
    pub time: f32,
    /// Playback rate: 1 is normal speed, negative plays backwards.
    pub speed: f32,
    /// Wrap at the end of the clip instead of holding the last frame.
    pub looping: bool,
    /// Whether [`Playhead::advance`] moves the time.
    pub playing: bool,
}

impl Default for Playhead {
    /// At the start, normal speed, looping and playing (the `AnimPlayer` defaults).
    fn default() -> Self {
        Playhead {
            time: 0.0,
            speed: 1.0,
            looping: true,
            playing: true,
        }
    }
}

impl Playhead {
    /// Moves the time forward by `dt * speed` seconds when playing. A non-looping
    /// playhead stops at the ends of a clip of length `duration`. A `dt` or
    /// `speed` that is not finite leaves the time unchanged.
    pub fn advance(&mut self, dt: f32, duration: f32) {
        if !self.playing || !dt.is_finite() || !self.speed.is_finite() {
            return;
        }
        self.time += dt * self.speed;
        if !self.looping {
            self.time = self.time.clamp(0.0, duration.max(0.0));
        } else if duration > 0.0 {
            // Keep the stored time small so f32 precision does not drift on long runs.
            self.time = self.time.rem_euclid(duration);
        }
    }

    /// The time inside a clip of length `duration`.
    pub fn clip_time(&self, duration: f32) -> f32 {
        wrap(self.time, duration, self.looping)
    }

    /// True when a non-looping playhead has reached the end it plays towards.
    pub fn is_finished(&self, duration: f32) -> bool {
        !self.looping
            && ((self.speed >= 0.0 && self.time >= duration)
                || (self.speed < 0.0 && self.time <= 0.0))
    }
}

fn wrap(time: f32, duration: f32, looping: bool) -> f32 {
    // A NaN duration counts as empty: `clamp` panics on a NaN bound.
    if !time.is_finite() || duration.is_nan() || duration <= 0.0 {
        return 0.0;
    }
    if looping {
        time.rem_euclid(duration)
    } else {
        time.clamp(0.0, duration)
    }
}

fn check<T>(
    clip: &str,
    target: &str,
    property: &str,
    channel: Option<&Channel<T>>,
) -> Result<(), Diagnostic> {
    let Some(channel) = channel else {
        return Ok(());
    };
    if channel.times.is_empty()
        || !channel.is_well_formed()
        || channel.times.iter().any(|t| !t.is_finite())
    {
        let per_key = if channel.interpolation == Interpolation::CubicSpline {
            3
        } else {
            1
        };
        return Err(Diagnostic::error(
            codes::CLIP_MALFORMED,
            format!(
                "clip `{clip}`: the {property} channel of `{target}` has {} key times and {} values \
                 ({:?} needs {per_key} value(s) per key, times finite and strictly increasing)",
                channel.times.len(),
                channel.values.len(),
                channel.interpolation,
            ),
        )
        .with_hint("re-export the animation, or run `real explain RE7600`"));
    }
    Ok(())
}

/// Finds the keyframe interval containing `time`: `(i, s)` with `times[i] <= time
/// < times[i + 1]` and `s` the fraction across it. Outside the keys (infinities
/// included), clamps; NaN holds the first key.
enum Span {
    /// Before the first key, after the last, or a single key: hold this key.
    Hold(usize),
    /// Between keys `i` and `i + 1`, `s` in `[0, 1)`, over `dt` seconds.
    Between { i: usize, s: f32, dt: f32 },
}

fn span(times: &[f32], time: f32) -> Span {
    let last = times.len() - 1;
    if time.is_nan() || time <= times[0] || last == 0 {
        return Span::Hold(0);
    }
    if time >= times[last] {
        return Span::Hold(last);
    }
    // First key strictly after `time`; `time` is inside (times[0], times[last]).
    let next = times.partition_point(|&t| t <= time);
    let i = next - 1;
    let dt = times[next] - times[i];
    Span::Between {
        i,
        s: (time - times[i]) / dt,
        dt,
    }
}

/// A value type keyframes can interpolate.
trait Key: Copy {
    fn lerp_key(a: Self, b: Self, s: f32) -> Self;
    fn hermite(p0: Self, m0: Self, p1: Self, m1: Self, s: f32) -> Self;
}

impl Key for Vec3 {
    fn lerp_key(a: Self, b: Self, s: f32) -> Self {
        a.lerp(b, s)
    }
    fn hermite(p0: Self, m0: Self, p1: Self, m1: Self, s: f32) -> Self {
        let [h00, h10, h01, h11] = hermite_basis(s);
        p0 * h00 + m0 * h10 + p1 * h01 + m1 * h11
    }
}

impl Key for Quat {
    fn lerp_key(a: Self, b: Self, s: f32) -> Self {
        a.slerp(b, s)
    }
    fn hermite(p0: Self, m0: Self, p1: Self, m1: Self, s: f32) -> Self {
        let [h00, h10, h01, h11] = hermite_basis(s);
        let v = glam::Vec4::from(p0) * h00
            + glam::Vec4::from(m0) * h10
            + glam::Vec4::from(p1) * h01
            + glam::Vec4::from(m1) * h11;
        Quat::from_vec4(v).normalize()
    }
}

fn hermite_basis(s: f32) -> [f32; 4] {
    let s2 = s * s;
    let s3 = s2 * s;
    [
        2.0 * s3 - 3.0 * s2 + 1.0,
        s3 - 2.0 * s2 + s,
        -2.0 * s3 + 3.0 * s2,
        s3 - s2,
    ]
}

fn sample<T: Key + std::ops::Mul<f32, Output = T>>(c: &Channel<T>, time: f32) -> T {
    let cubic = c.interpolation == Interpolation::CubicSpline;
    // For cubic spline, values are (in-tangent, value, out-tangent) per key.
    let value = |k: usize| {
        if cubic {
            c.values[3 * k + 1]
        } else {
            c.values[k]
        }
    };
    match span(&c.times, time) {
        Span::Hold(k) => value(k),
        Span::Between { i, s, dt } => match c.interpolation {
            Interpolation::Step => value(i),
            Interpolation::Linear => T::lerp_key(value(i), value(i + 1), s),
            Interpolation::CubicSpline => {
                let out_tangent = c.values[3 * i + 2] * dt;
                let in_tangent = c.values[3 * (i + 1)] * dt;
                T::hermite(value(i), out_tangent, value(i + 1), in_tangent, s)
            }
        },
    }
}

pub(crate) fn sample_vec3(c: &Channel<Vec3>, time: f32) -> Vec3 {
    sample(c, time)
}

pub(crate) fn sample_quat(c: &Channel<Quat>, time: f32) -> Quat {
    sample(c, time).normalize()
}
