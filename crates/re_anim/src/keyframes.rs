//! Keyframe pose animation for jointed rigs.
//!
//! A [`KeyframeClip`] is a list of [`Keyframe`]s, each holding a pose (a
//! [`JointKey`]) for some of a rig's joints by name, plus named [`Marker`]s.
//! A [`Track`] is one playing instance of a clip: it owns a time position, a
//! speed and a weight that fades in and out, and reports [`TrackEvent`]s as it
//! moves. A [`Mixer`] holds the tracks of one rig and blends their poses by
//! [`Priority`] into one pose per joint.
//!
//! Everything here is plain computation over joint names: the caller decides
//! what a joint is and applies the evaluated offsets to it, typically once per
//! fixed step.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use glam::{Quat, Vec3, Vec4};
use re_core::Diagnostic;
use serde::{Deserialize, Serialize};

use crate::{Easing, codes};

/// Sampled joint poses by joint name: `(translation, rotation, weight)`, as
/// [`KeyframeClip::sample`] fills them.
pub type SampledPoses = BTreeMap<String, (Vec3, Quat, f32)>;

/// The fade time, in seconds, used when none is given and when a track that
/// does not loop reaches its end: 0.1 s (shown as `0.100000001` when the `f32`
/// is printed with nine digits).
pub const DEFAULT_FADE_TIME: f32 = 0.1;

/// The weight a track plays at when none is given: 1.
pub const DEFAULT_WEIGHT: f32 = 1.0;

/// The speed a track plays at when none is given: 1 (real time).
pub const DEFAULT_SPEED: f32 = 1.0;

// ---------------------------------------------------------------------------
// Easing
// ---------------------------------------------------------------------------

/// The shape of a pose easing curve.
///
/// `Cubic`, `Elastic` and `Bounce` are the legacy styles: their `In` and `Out`
/// directions are swapped compared with the usual convention (and with
/// [`Easing`]), so `Cubic` + `In` starts fast and ends slow. Content authored
/// for those styles relies on that. `CubicV2` is the same cubic curve with the
/// usual directions (`In` starts slow).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PoseStyle {
    /// Constant speed.
    #[default]
    Linear,
    /// Holds the earlier pose for the whole segment, then jumps.
    Constant,
    /// Cubic curve, legacy (swapped) directions.
    Cubic,
    /// Spring-like overshoot and wobble, legacy (swapped) directions.
    Elastic,
    /// Bounces like a dropped ball, legacy (swapped) directions.
    Bounce,
    /// Cubic curve with the usual directions.
    CubicV2,
}

impl PoseStyle {
    /// Every style, in declaration order.
    pub const ALL: [PoseStyle; 6] = [
        PoseStyle::Linear,
        PoseStyle::Constant,
        PoseStyle::Cubic,
        PoseStyle::Elastic,
        PoseStyle::Bounce,
        PoseStyle::CubicV2,
    ];

    /// The name, for example `CubicV2`.
    pub const fn as_str(self) -> &'static str {
        match self {
            PoseStyle::Linear => "Linear",
            PoseStyle::Constant => "Constant",
            PoseStyle::Cubic => "Cubic",
            PoseStyle::Elastic => "Elastic",
            PoseStyle::Bounce => "Bounce",
            PoseStyle::CubicV2 => "CubicV2",
        }
    }

    /// Parses a name (exact case, as [`PoseStyle::as_str`] gives it).
    ///
    /// # Errors
    ///
    /// [`codes::UNKNOWN_POSE_EASING`], listing the valid names.
    pub fn from_name(name: &str) -> Result<PoseStyle, Diagnostic> {
        PoseStyle::ALL
            .into_iter()
            .find(|s| s.as_str() == name)
            .ok_or_else(|| {
                let names: Vec<&str> = PoseStyle::ALL.iter().map(|s| s.as_str()).collect();
                unknown_pose_easing("style", name, &names)
            })
    }
}

/// Which end of a [`PoseStyle`] curve is slow.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PoseDirection {
    /// The curve as written (for the usual convention: slow start).
    #[default]
    In,
    /// The curve mirrored (for the usual convention: slow end).
    Out,
    /// The `In` curve over the first half and the `Out` curve over the second.
    InOut,
}

impl PoseDirection {
    /// Every direction, in declaration order.
    pub const ALL: [PoseDirection; 3] =
        [PoseDirection::In, PoseDirection::Out, PoseDirection::InOut];

    /// The name, for example `InOut`.
    pub const fn as_str(self) -> &'static str {
        match self {
            PoseDirection::In => "In",
            PoseDirection::Out => "Out",
            PoseDirection::InOut => "InOut",
        }
    }

    /// Parses a name (exact case, as [`PoseDirection::as_str`] gives it).
    ///
    /// # Errors
    ///
    /// [`codes::UNKNOWN_POSE_EASING`], listing the valid names.
    pub fn from_name(name: &str) -> Result<PoseDirection, Diagnostic> {
        PoseDirection::ALL
            .into_iter()
            .find(|d| d.as_str() == name)
            .ok_or_else(|| {
                let names: Vec<&str> = PoseDirection::ALL.iter().map(|d| d.as_str()).collect();
                unknown_pose_easing("direction", name, &names)
            })
    }

    const fn swapped(self) -> PoseDirection {
        match self {
            PoseDirection::In => PoseDirection::Out,
            PoseDirection::Out => PoseDirection::In,
            PoseDirection::InOut => PoseDirection::InOut,
        }
    }
}

fn unknown_pose_easing(what: &str, name: &str, names: &[&str]) -> Diagnostic {
    Diagnostic::error(
        codes::UNKNOWN_POSE_EASING,
        format!("unknown pose easing {what} `{name}`"),
    )
    .with_hint(format!("use one of: {}", names.join(", ")))
}

impl fmt::Display for PoseStyle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PoseStyle {
    type Err = Diagnostic;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PoseStyle::from_name(s)
    }
}

impl fmt::Display for PoseDirection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PoseDirection {
    type Err = Diagnostic;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        PoseDirection::from_name(s)
    }
}

/// How a joint moves from one key to the next: a style and a direction.
///
/// The easing belongs to the earlier key of a segment.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PoseEasing {
    /// The curve shape.
    pub style: PoseStyle,
    /// Which end of the curve is slow.
    pub direction: PoseDirection,
}

impl PoseEasing {
    /// Linear easing (the default).
    pub const LINEAR: PoseEasing = PoseEasing {
        style: PoseStyle::Linear,
        direction: PoseDirection::In,
    };

    /// An easing from its parts.
    pub const fn new(style: PoseStyle, direction: PoseDirection) -> PoseEasing {
        PoseEasing { style, direction }
    }

    /// The eased fraction at segment progress `t` (clamped to `[0, 1]`; NaN
    /// counts as 0).
    ///
    /// Every curve gives 0 at `t = 0` and 1 at `t = 1`. `Constant` gives 0 for
    /// every `t < 1`. `Elastic` overshoots on the way.
    pub fn apply(self, t: f32) -> f32 {
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        match self.style {
            PoseStyle::Linear => t,
            PoseStyle::Constant => {
                if t >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            PoseStyle::Cubic => directed(cubic_in, self.direction.swapped(), t),
            PoseStyle::Elastic => directed(elastic_in, self.direction.swapped(), t),
            PoseStyle::Bounce => directed(bounce_in, self.direction.swapped(), t),
            PoseStyle::CubicV2 => directed(cubic_in, self.direction, t),
        }
    }
}

fn cubic_in(t: f32) -> f32 {
    t * t * t
}

fn elastic_in(t: f32) -> f32 {
    1.0 - Easing::ElasticOut.apply(1.0 - t)
}

fn bounce_in(t: f32) -> f32 {
    1.0 - Easing::BounceOut.apply(1.0 - t)
}

/// Builds the `direction` variant of a curve given as its slow-start form.
fn directed(slow_start: fn(f32) -> f32, direction: PoseDirection, t: f32) -> f32 {
    match direction {
        PoseDirection::In => slow_start(t),
        PoseDirection::Out => 1.0 - slow_start(1.0 - t),
        PoseDirection::InOut => {
            if t < 0.5 {
                slow_start(2.0 * t) / 2.0
            } else {
                1.0 - slow_start(2.0 - 2.0 * t) / 2.0
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Priority
// ---------------------------------------------------------------------------

/// Which tracks win when several animate the same joint: a higher priority
/// blends over the result of all lower ones. Ordered low to high.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum Priority {
    /// The lowest level, under everything (the default).
    #[default]
    Core,
    /// Standing-still loops.
    Idle,
    /// Walking, running, climbing.
    Movement,
    /// One-off actions such as waves and attacks.
    Action,
    /// Above `Action`.
    Action2,
    /// Above `Action2`.
    Action3,
    /// The highest level.
    Action4,
}

impl Priority {
    /// Every priority, low to high.
    pub const ALL: [Priority; 7] = [
        Priority::Core,
        Priority::Idle,
        Priority::Movement,
        Priority::Action,
        Priority::Action2,
        Priority::Action3,
        Priority::Action4,
    ];

    /// The name, for example `Action2`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Priority::Core => "Core",
            Priority::Idle => "Idle",
            Priority::Movement => "Movement",
            Priority::Action => "Action",
            Priority::Action2 => "Action2",
            Priority::Action3 => "Action3",
            Priority::Action4 => "Action4",
        }
    }

    /// Parses a name (exact case, as [`Priority::as_str`] gives it).
    ///
    /// # Errors
    ///
    /// [`codes::UNKNOWN_PRIORITY`], listing the valid names.
    pub fn from_name(name: &str) -> Result<Priority, Diagnostic> {
        Priority::ALL
            .into_iter()
            .find(|p| p.as_str() == name)
            .ok_or_else(|| {
                let names: Vec<&str> = Priority::ALL.iter().map(|p| p.as_str()).collect();
                Diagnostic::error(
                    codes::UNKNOWN_PRIORITY,
                    format!("unknown animation priority `{name}`"),
                )
                .with_hint(format!("use one of: {}", names.join(", ")))
            })
    }
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Priority {
    type Err = Diagnostic;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Priority::from_name(s)
    }
}

// ---------------------------------------------------------------------------
// Clip data
// ---------------------------------------------------------------------------

/// One joint's pose in a [`Keyframe`]: an offset from the joint's rest
/// transform, how strongly it applies, and how it eases toward the joint's next
/// key.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct JointKey {
    /// Translation offset.
    pub translation: Vec3,
    /// Rotation offset (normalised by [`KeyframeClip::new`]).
    pub rotation: Quat,
    /// How strongly this pose applies, usually in `[0, 1]`; multiplied with the
    /// track's weight when blending.
    pub weight: f32,
    /// The easing from this key to the joint's next key.
    pub easing: PoseEasing,
}

impl Default for JointKey {
    /// The rest pose at full weight, linear easing.
    fn default() -> Self {
        JointKey {
            translation: Vec3::ZERO,
            rotation: Quat::IDENTITY,
            weight: 1.0,
            easing: PoseEasing::LINEAR,
        }
    }
}

/// The poses of some joints at one time.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Seconds from the start of the clip.
    pub time: f32,
    /// A name; a non-empty name is reported by [`TrackEvent::KeyframeReached`].
    pub name: String,
    /// Joint poses by joint name. Joints not listed are interpolated between
    /// the neighbouring keyframes that do list them.
    pub joints: BTreeMap<String, JointKey>,
}

/// A named point in time carrying a value, reported by
/// [`TrackEvent::MarkerReached`].
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    /// Seconds from the start of the clip.
    pub time: f32,
    /// The marker name.
    pub name: String,
    /// A free-form value passed along with the event.
    pub value: String,
}

#[derive(Clone, Debug)]
enum ClipEvent {
    Keyframe(String),
    Marker { name: String, value: String },
}

/// A checked keyframe clip, ready to sample.
///
/// Keyframes are sorted by time on construction (keyframes with equal times
/// keep their order). The clip is immutable afterwards; share it between
/// tracks with an [`Arc`].
#[derive(Clone, Debug)]
pub struct KeyframeClip {
    keyframes: Vec<Keyframe>,
    markers: Vec<Marker>,
    looped: bool,
    priority: Priority,
    /// Per joint: indices into `keyframes` of the keys that pose it, by time.
    joint_keys: BTreeMap<String, Vec<usize>>,
    /// Named keyframes and markers, by time.
    events: Vec<(f32, ClipEvent)>,
}

impl KeyframeClip {
    /// Checks and sorts the clip data.
    ///
    /// # Errors
    ///
    /// [`codes::KEYFRAME_CLIP_MALFORMED`] when a keyframe or marker time is
    /// negative or not finite, or a joint key has a non-finite translation,
    /// rotation or weight, a negative weight, or a zero rotation.
    pub fn new(
        keyframes: Vec<Keyframe>,
        markers: Vec<Marker>,
        looped: bool,
        priority: Priority,
    ) -> Result<KeyframeClip, Diagnostic> {
        let mut keyframes = keyframes;
        let mut markers = markers;
        for (i, kf) in keyframes.iter_mut().enumerate() {
            if !kf.time.is_finite() || kf.time < 0.0 {
                return Err(malformed(format!(
                    "keyframe {i} (`{}`) has time {}; times must be finite and >= 0",
                    kf.name, kf.time
                )));
            }
            for (joint, key) in &mut kf.joints {
                if !key.translation.is_finite() {
                    return Err(malformed(format!(
                        "keyframe {i} (`{}`): joint `{joint}` has a non-finite translation",
                        kf.name
                    )));
                }
                let len = key.rotation.length();
                if !key.rotation.is_finite() || len < 1e-6 {
                    return Err(malformed(format!(
                        "keyframe {i} (`{}`): joint `{joint}` has a zero or non-finite rotation",
                        kf.name
                    )));
                }
                key.rotation = Quat::from_vec4(Vec4::from(key.rotation) / len);
                if !key.weight.is_finite() || key.weight < 0.0 {
                    return Err(malformed(format!(
                        "keyframe {i} (`{}`): joint `{joint}` has weight {}; weights must be finite and >= 0",
                        kf.name, key.weight
                    )));
                }
            }
        }
        for (i, m) in markers.iter().enumerate() {
            if !m.time.is_finite() || m.time < 0.0 {
                return Err(malformed(format!(
                    "marker {i} (`{}`) has time {}; times must be finite and >= 0",
                    m.name, m.time
                )));
            }
        }
        keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        markers.sort_by(|a, b| a.time.total_cmp(&b.time));

        let mut joint_keys: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (i, kf) in keyframes.iter().enumerate() {
            for joint in kf.joints.keys() {
                joint_keys.entry(joint.clone()).or_default().push(i);
            }
        }
        let mut events: Vec<(f32, ClipEvent)> = keyframes
            .iter()
            .filter(|kf| !kf.name.is_empty())
            .map(|kf| (kf.time, ClipEvent::Keyframe(kf.name.clone())))
            .chain(markers.iter().map(|m| {
                (
                    m.time,
                    ClipEvent::Marker {
                        name: m.name.clone(),
                        value: m.value.clone(),
                    },
                )
            }))
            .collect();
        events.sort_by(|a, b| a.0.total_cmp(&b.0));

        Ok(KeyframeClip {
            keyframes,
            markers,
            looped,
            priority,
            joint_keys,
            events,
        })
    }

    /// The keyframes, sorted by time.
    pub fn keyframes(&self) -> &[Keyframe] {
        &self.keyframes
    }

    /// The markers, sorted by time.
    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }

    /// Whether tracks of this clip loop unless they say otherwise.
    pub fn looped(&self) -> bool {
        self.looped
    }

    /// The priority tracks of this clip use unless they say otherwise.
    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// The names of the joints some keyframe poses, sorted.
    pub fn joints(&self) -> impl Iterator<Item = &str> {
        self.joint_keys.keys().map(String::as_str)
    }

    /// The clip length in seconds: the time of the last keyframe (0 when there
    /// are none).
    pub fn length(&self) -> f32 {
        self.keyframes.last().map_or(0.0, |kf| kf.time)
    }

    /// Samples every joint at time `t` (NaN counts as 0) into `out` as
    /// `(translation, rotation, weight)`, adding to or replacing entries.
    ///
    /// For each joint, the last key at or before `t` and the next key after
    /// `t` that pose that joint are interpolated (translation and weight
    /// linearly, rotation spherically), with the earlier key's easing applied
    /// to the fraction of the segment. Before the joint's first key the first
    /// is held; after its last key the last is held.
    pub fn sample(&self, t: f32, out: &mut SampledPoses) {
        for joint in self.joint_keys.keys() {
            if let Some(pose) = self.sample_joint(joint, t) {
                out.insert(joint.clone(), pose);
            }
        }
    }

    /// Samples one joint at time `t`; `None` when no keyframe poses it.
    pub fn sample_joint(&self, joint: &str, t: f32) -> Option<(Vec3, Quat, f32)> {
        let t = if t.is_nan() { 0.0 } else { t };
        let keys = self.joint_keys.get(joint)?;
        let key_at = |i: usize| -> (f32, &JointKey) {
            let kf = &self.keyframes[i];
            (kf.time, &kf.joints[joint])
        };
        let after = keys.partition_point(|&i| self.keyframes[i].time <= t);
        let pose = |k: &JointKey| (k.translation, k.rotation, k.weight);
        if after == 0 {
            return Some(pose(key_at(keys[0]).1));
        }
        let (t0, k0) = key_at(keys[after - 1]);
        if after == keys.len() {
            return Some(pose(k0));
        }
        let (t1, k1) = key_at(keys[after]);
        let span = t1 - t0;
        let f = if span > 0.0 { (t - t0) / span } else { 1.0 };
        let s = k0.easing.apply(f);
        Some((
            k0.translation.lerp(k1.translation, s),
            k0.rotation.slerp(k1.rotation, s).normalize(),
            k0.weight + (k1.weight - k0.weight) * s,
        ))
    }
}

fn malformed(message: String) -> Diagnostic {
    Diagnostic::error(codes::KEYFRAME_CLIP_MALFORMED, message)
}

// ---------------------------------------------------------------------------
// Track
// ---------------------------------------------------------------------------

/// Something that happened while a [`Track`] advanced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrackEvent {
    /// The time of a keyframe with a non-empty name was reached or crossed.
    KeyframeReached(String),
    /// The time of a marker was reached or crossed.
    MarkerReached {
        /// The marker name.
        name: String,
        /// The marker value.
        value: String,
    },
    /// A looped track wrapped around (from the end to the start, or from the
    /// start to the end when running backward).
    DidLoop,
    /// A track that does not loop reached its end (the start, when running
    /// backward). It holds its last pose and fades out over
    /// [`DEFAULT_FADE_TIME`]. Reported once.
    Ended,
    /// A stopped track's weight reached 0. The track is finished. Reported once.
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Never played.
    Idle,
    Playing,
    /// Stopped (or ended), fading out.
    Stopping,
    Finished,
}

/// Process-wide counter giving each `play` a start order.
static PLAY_SEQ: AtomicU64 = AtomicU64::new(1);

/// One playing instance of a [`KeyframeClip`].
///
/// A new track is not playing, at time 0, weight 0 and speed
/// [`DEFAULT_SPEED`]. [`Track::play`] fades its weight in; [`Track::advance`]
/// moves its time and fades; [`Track::stop`] fades it out, after which it is
/// finished. Weights fade linearly from the current weight to the target over
/// the fade time given.
///
/// Events (keyframes, markers) fire when their time is crossed: moving forward
/// from `a` to `b`, the ones with `a < time <= b`; backward, `b <= time < a`.
/// Right after playing from the start, after [`Track::set_time_position`] and
/// after a loop wrap, an event exactly at the starting time fires too.
#[derive(Clone, Debug)]
pub struct Track {
    clip: Arc<KeyframeClip>,
    looped: bool,
    priority: Priority,
    time: f32,
    speed: f32,
    weight_current: f32,
    weight_target: f32,
    /// Weight change per second while fading.
    fade_rate: f32,
    phase: Phase,
    /// Reached the end without looping: holds its time.
    ended: bool,
    /// Events exactly at `time` still fire on the next move.
    include_start: bool,
    start_seq: u64,
}

impl Track {
    /// A track of `clip`, not playing, taking its looping and priority from
    /// the clip.
    pub fn new(clip: Arc<KeyframeClip>) -> Track {
        Track {
            looped: clip.looped(),
            priority: clip.priority(),
            clip,
            time: 0.0,
            speed: DEFAULT_SPEED,
            weight_current: 0.0,
            weight_target: 0.0,
            fade_rate: 0.0,
            phase: Phase::Idle,
            ended: false,
            include_start: true,
            start_seq: 0,
        }
    }

    /// The clip.
    pub fn clip(&self) -> &Arc<KeyframeClip> {
        &self.clip
    }

    /// The clip length in seconds.
    pub fn length(&self) -> f32 {
        self.clip.length()
    }

    /// Starts (or keeps) playing, fading the weight to `weight` over
    /// `fade_time` seconds, at `speed`.
    ///
    /// A track that has ended or finished restarts from its start (time 0, or
    /// the end for a negative speed); a fresh track starts from its time
    /// position; a playing or stopping track keeps its time. Non-finite or
    /// negative `fade_time` counts as 0 (instant); a non-finite `weight` or
    /// `speed` counts as the default; a negative weight counts as 0.
    pub fn play(&mut self, fade_time: f32, weight: f32, speed: f32) {
        let speed = if speed.is_finite() {
            speed
        } else {
            DEFAULT_SPEED
        };
        let weight = if weight.is_finite() {
            weight.max(0.0)
        } else {
            DEFAULT_WEIGHT
        };
        if self.ended || self.phase == Phase::Finished {
            self.time = if speed < 0.0 { self.length() } else { 0.0 };
            self.include_start = true;
            self.ended = false;
        }
        if self.phase != Phase::Playing {
            self.start_seq = PLAY_SEQ.fetch_add(1, Ordering::Relaxed);
        }
        self.phase = Phase::Playing;
        self.speed = speed;
        self.fade_to(weight, fade_time);
    }

    /// Plays with [`DEFAULT_FADE_TIME`], [`DEFAULT_WEIGHT`] and
    /// [`DEFAULT_SPEED`].
    pub fn play_default(&mut self) {
        self.play(DEFAULT_FADE_TIME, DEFAULT_WEIGHT, DEFAULT_SPEED);
    }

    /// Stops playing, fading the weight to 0 over `fade_time` seconds
    /// (non-finite or negative counts as 0). Time keeps moving during the
    /// fade. Once the weight is 0, the next [`Track::advance`] reports
    /// [`TrackEvent::Stopped`]. Does nothing on a finished track.
    pub fn stop(&mut self, fade_time: f32) {
        if self.phase == Phase::Finished {
            return;
        }
        self.phase = Phase::Stopping;
        self.fade_to(0.0, fade_time);
    }

    /// Sets the speed (1 is real time, 0 holds, negative runs backward).
    /// Non-finite values are ignored.
    pub fn adjust_speed(&mut self, speed: f32) {
        if speed.is_finite() {
            self.speed = speed;
        }
    }

    /// Fades the weight to `weight` (negative counts as 0) over `fade_time`
    /// seconds. Only a playing track changes; a stopping track keeps fading
    /// out. Non-finite weights are ignored.
    pub fn adjust_weight(&mut self, weight: f32, fade_time: f32) {
        if self.phase == Phase::Playing && weight.is_finite() {
            self.fade_to(weight.max(0.0), fade_time);
        }
    }

    /// Jumps to `time` (clamped to the clip, or wrapped into it when looped)
    /// without reporting events; an event exactly at the new time fires on the
    /// next move. A track that has ended keeps holding until played again.
    /// Non-finite values are ignored.
    pub fn set_time_position(&mut self, time: f32) {
        if !time.is_finite() {
            return;
        }
        let len = self.length();
        self.time = if len <= 0.0 {
            0.0
        } else if self.looped {
            time.rem_euclid(len)
        } else {
            time.clamp(0.0, len)
        };
        self.include_start = true;
    }

    /// Seconds from the start of the clip.
    pub fn time_position(&self) -> f32 {
        self.time
    }

    /// True between [`Track::play`] and [`Track::stop`] (or the end, for a
    /// track that does not loop).
    pub fn is_playing(&self) -> bool {
        self.phase == Phase::Playing
    }

    /// True once a stopped track has faded out and reported
    /// [`TrackEvent::Stopped`]. A finished track contributes nothing.
    pub fn is_finished(&self) -> bool {
        self.phase == Phase::Finished
    }

    /// The weight now.
    pub fn weight_current(&self) -> f32 {
        self.weight_current
    }

    /// The weight being faded to.
    pub fn weight_target(&self) -> f32 {
        self.weight_target
    }

    /// The speed.
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Whether this track loops.
    pub fn looped(&self) -> bool {
        self.looped
    }

    /// Overrides whether this track loops.
    pub fn set_looped(&mut self, looped: bool) {
        self.looped = looped;
    }

    /// This track's priority.
    pub fn priority(&self) -> Priority {
        self.priority
    }

    /// Overrides this track's priority.
    pub fn set_priority(&mut self, priority: Priority) {
        self.priority = priority;
    }

    /// Samples the clip at this track's time (see [`KeyframeClip::sample`]).
    /// The weights are the keys' weights, not multiplied by the track weight.
    pub fn sample(&self, out: &mut SampledPoses) {
        self.clip.sample(self.time, out);
    }

    /// Moves time by `dt * speed` seconds and fades the weight by `dt` seconds
    /// (negative or non-finite `dt` counts as 0), returning what happened in
    /// order.
    pub fn advance(&mut self, dt: f32) -> Vec<TrackEvent> {
        let dt = if dt.is_finite() && dt > 0.0 { dt } else { 0.0 };
        let mut events = Vec::new();
        if self.phase == Phase::Idle || self.phase == Phase::Finished {
            return events;
        }
        // Fade first, so a fade started this step (by an end) begins next step.
        self.step_fade(dt);
        if !self.ended {
            let d = dt * self.speed;
            if d != 0.0 {
                self.move_time(d, &mut events);
            }
        }
        if self.phase == Phase::Stopping && self.weight_current <= 0.0 {
            self.weight_current = 0.0;
            self.phase = Phase::Finished;
            events.push(TrackEvent::Stopped);
        }
        events
    }

    fn fade_to(&mut self, weight: f32, fade_time: f32) {
        self.weight_target = weight;
        if fade_time.is_finite() && fade_time > 0.0 {
            self.fade_rate = (weight - self.weight_current).abs() / fade_time;
        } else {
            self.weight_current = weight;
            self.fade_rate = 0.0;
        }
    }

    fn step_fade(&mut self, dt: f32) {
        let gap = self.weight_target - self.weight_current;
        if gap == 0.0 {
            return;
        }
        let step = self.fade_rate * dt;
        // A small tolerance so fades made of many fixed steps land exactly.
        if gap.abs() <= step + 1e-5 {
            self.weight_current = self.weight_target;
        } else {
            self.weight_current += step.copysign(gap);
        }
    }

    fn move_time(&mut self, d: f32, events: &mut Vec<TrackEvent>) {
        let len = self.length();
        let forward = d > 0.0;
        let mut t0 = self.time;
        let mut remaining = d.abs();
        let mut include = self.include_start;
        let mut wraps = 0u32;
        loop {
            if len <= 0.0 {
                // A zero-length clip: everything happens at time 0.
                self.collect(0.0, 0.0, include, forward, events);
                self.time = 0.0;
                self.include_start = false;
                if !self.looped {
                    self.end(events);
                }
                return;
            }
            let t1 = if forward {
                t0 + remaining
            } else {
                t0 - remaining
            };
            let inside = if forward { t1 <= len } else { t1 >= 0.0 };
            if inside {
                self.collect(t0, t1, include, forward, events);
                self.time = t1;
                self.include_start = false;
                return;
            }
            let edge = if forward { len } else { 0.0 };
            self.collect(t0, edge, include, forward, events);
            if !self.looped {
                self.time = edge;
                self.include_start = false;
                self.end(events);
                return;
            }
            events.push(TrackEvent::DidLoop);
            remaining = (t1 - edge).abs();
            t0 = if forward { 0.0 } else { len };
            include = true;
            wraps += 1;
            if wraps >= 64 {
                // Skip whole loops when one step spans very many of them.
                remaining %= len;
            }
        }
    }

    fn end(&mut self, events: &mut Vec<TrackEvent>) {
        self.ended = true;
        events.push(TrackEvent::Ended);
        if self.phase == Phase::Playing {
            self.stop(DEFAULT_FADE_TIME);
        }
    }

    /// Pushes the events crossed moving from `from` to `to`.
    fn collect(
        &self,
        from: f32,
        to: f32,
        include_from: bool,
        forward: bool,
        out: &mut Vec<TrackEvent>,
    ) {
        let ev = &self.clip.events;
        let to_event = |e: &ClipEvent| match e {
            ClipEvent::Keyframe(name) => TrackEvent::KeyframeReached(name.clone()),
            ClipEvent::Marker { name, value } => TrackEvent::MarkerReached {
                name: name.clone(),
                value: value.clone(),
            },
        };
        if forward {
            let lo = if include_from {
                ev.partition_point(|(t, _)| *t < from)
            } else {
                ev.partition_point(|(t, _)| *t <= from)
            };
            let hi = ev.partition_point(|(t, _)| *t <= to);
            if lo < hi {
                out.extend(ev[lo..hi].iter().map(|(_, e)| to_event(e)));
            }
        } else {
            let lo = ev.partition_point(|(t, _)| *t < to);
            let hi = if include_from {
                ev.partition_point(|(t, _)| *t <= from)
            } else {
                ev.partition_point(|(t, _)| *t < from)
            };
            if lo < hi {
                out.extend(ev[lo..hi].iter().rev().map(|(_, e)| to_event(e)));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mixer
// ---------------------------------------------------------------------------

/// Identifies a track in a [`Mixer`]. Ids count up from 1 and are never
/// reused by the same mixer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TrackId(pub u64);

/// The tracks of one rig, advanced together and blended by [`Priority`].
#[derive(Clone, Debug, Default)]
pub struct Mixer {
    next_id: u64,
    tracks: BTreeMap<TrackId, Track>,
}

impl Mixer {
    /// An empty mixer.
    pub fn new() -> Mixer {
        Mixer::default()
    }

    /// Adds a track and returns its id.
    pub fn add(&mut self, track: Track) -> TrackId {
        self.next_id += 1;
        let id = TrackId(self.next_id);
        self.tracks.insert(id, track);
        id
    }

    /// A track by id.
    pub fn get(&self, id: TrackId) -> Option<&Track> {
        self.tracks.get(&id)
    }

    /// A track by id, to change.
    pub fn get_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.get_mut(&id)
    }

    /// Removes a track at once (no fade) and returns it.
    pub fn remove(&mut self, id: TrackId) -> Option<Track> {
        self.tracks.remove(&id)
    }

    /// The number of tracks held.
    pub fn len(&self) -> usize {
        self.tracks.len()
    }

    /// True when no tracks are held.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Every track with its id, in id order.
    pub fn iter(&self) -> impl Iterator<Item = (TrackId, &Track)> {
        self.tracks.iter().map(|(id, t)| (*id, t))
    }

    /// The ids of the playing tracks, in the order they were started.
    pub fn playing(&self) -> impl Iterator<Item = TrackId> {
        let mut ids: Vec<(u64, TrackId)> = self
            .tracks
            .iter()
            .filter(|(_, t)| t.is_playing())
            .map(|(id, t)| (t.start_seq, *id))
            .collect();
        ids.sort();
        ids.into_iter().map(|(_, id)| id)
    }

    /// Advances every track by `dt` (see [`Track::advance`]) and returns their
    /// events, by track in id order. Tracks that report
    /// [`TrackEvent::Stopped`] are removed.
    pub fn advance(&mut self, dt: f32) -> Vec<(TrackId, TrackEvent)> {
        let mut out = Vec::new();
        for (id, track) in &mut self.tracks {
            out.extend(track.advance(dt).into_iter().map(|e| (*id, e)));
        }
        self.tracks.retain(|_, t| !t.is_finished());
        out
    }

    /// Blends every unfinished track into one pose per joint, replacing the
    /// contents of `out`.
    ///
    /// For each joint some track animates, starting from the rest pose
    /// (`Vec3::ZERO`, `Quat::IDENTITY`), each priority level from low to high
    /// contributes: the tracks of that level posing the joint with effective
    /// weight `w` (track weight times key weight) above 0 are averaged,
    /// weighted by `w`; the running pose then moves toward that average by
    /// `min(W, 1)`, where `W` is the level's total weight. A full-weight track
    /// so overrides lower levels on the joints it poses, and a fading one
    /// blends over them.
    pub fn evaluate(&self, out: &mut BTreeMap<String, (Vec3, Quat)>) {
        out.clear();
        let mut sampled: Vec<(Priority, f32, SampledPoses)> = Vec::new();
        for track in self.tracks.values().filter(|t| !t.is_finished()) {
            let mut poses = BTreeMap::new();
            track.sample(&mut poses);
            for joint in poses.keys() {
                if !out.contains_key(joint) {
                    out.insert(joint.clone(), (Vec3::ZERO, Quat::IDENTITY));
                }
            }
            if track.weight_current > 0.0 {
                sampled.push((track.priority, track.weight_current, poses));
            }
        }
        sampled.sort_by_key(|(p, _, _)| *p);

        for (joint, result) in out.iter_mut() {
            let mut i = 0;
            while i < sampled.len() {
                let level = sampled[i].0;
                let mut total = 0.0f32;
                let mut translation = Vec3::ZERO;
                let mut rotation = Vec4::ZERO;
                let mut first: Option<Quat> = None;
                while i < sampled.len() && sampled[i].0 == level {
                    let (_, track_weight, poses) = &sampled[i];
                    i += 1;
                    let Some(&(t, r, key_weight)) = poses.get(joint) else {
                        continue;
                    };
                    let w = track_weight * key_weight;
                    if w <= 0.0 {
                        continue;
                    }
                    let reference = *first.get_or_insert(r);
                    let r = if reference.dot(r) < 0.0 { -r } else { r };
                    total += w;
                    translation += t * w;
                    rotation += Vec4::from(r) * w;
                }
                if total <= 0.0 {
                    continue;
                }
                let level_t = translation / total;
                let level_r = Quat::from_vec4(rotation).normalize();
                let k = total.min(1.0);
                result.0 = result.0.lerp(level_t, k);
                result.1 = result.1.slerp(level_r, k).normalize();
            }
        }
    }
}
