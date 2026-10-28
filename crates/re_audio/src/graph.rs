//! The mixer graph (ADR-0041): named buses on kira sub-tracks, send tracks,
//! kira's built-in effects, meters, and snapshots.
//!
//! Every bus and send is a kira track whose effect chain is the listed effects,
//! then a volume control (the bus volume, so meters and sends see it), then a
//! [`Meter`]. The track's own volume stays at 0 dB. Effects keep their listed
//! index even when an invalid one is dropped, so commands and snapshots address
//! the same effect as `real.toml`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use kira::backend::Backend;
use kira::effect::compressor::{CompressorBuilder, CompressorHandle};
use kira::effect::delay::{DelayBuilder, DelayHandle};
use kira::effect::distortion::{DistortionBuilder, DistortionHandle, DistortionKind};
use kira::effect::eq_filter::{EqFilterBuilder, EqFilterHandle, EqFilterKind};
use kira::effect::filter::{FilterBuilder, FilterHandle};
use kira::effect::reverb::{ReverbBuilder, ReverbHandle};
use kira::effect::volume_control::{VolumeControlBuilder, VolumeControlHandle};
use kira::track::{SendTrackBuilder, SendTrackHandle, TrackBuilder, TrackHandle};
use kira::{AudioManager, Decibels, Mix, Tween};
use re_core::Diagnostic;
use re_types::audio::{
    AudioGraphDesc, BusLevel, DistortionMode, EffectDesc, EffectParam, EqShape, FilterMode,
    SnapshotDesc, SnapshotEffect,
};

use crate::codes;
use crate::meter::{Meter, MeterSlot};
use crate::mixer::MAX_FADE;

/// The most send tracks a graph may have: kira's 16, with one kept spare.
pub const MAX_SENDS: usize = 15;

/// Sub-tracks (spatial voices, child buses) and sounds each bus holds.
pub(crate) const BUS_CAPACITY: usize = 256;

/// The snapshot name that returns to the graph's own values.
pub const DEFAULT_SNAPSHOT: &str = "default";

/// The lowest and highest filter and EQ frequencies, in Hz: 20 kHz is below the
/// Nyquist frequency of every supported output rate (44.1 kHz and up).
const MIN_HZ: f32 = 10.0;
const MAX_HZ: f32 = 20_000.0;

/// The longest compressor attack or release, in milliseconds.
const MAX_ENVELOPE_MS: f32 = 10_000.0;

/// A tween of `seconds` (0 is at once), linear. Callers refuse a length past
/// [`MAX_FADE`]; one they missed is clamped to it rather than handed to kira
/// (a `Duration` overflows near 1.8e19 s).
pub(crate) fn tween(seconds: f32) -> Tween {
    Tween {
        start_time: kira::StartTime::Immediate,
        duration: seconds_to_duration(f64::from(seconds), MAX_FADE),
        easing: kira::Easing::Linear,
    }
}

/// `seconds` as a `Duration`, from 0 to `max` seconds. The callers have
/// checked the range already; this keeps a value they missed from panicking.
fn seconds_to_duration(seconds: f64, max: f32) -> Duration {
    Duration::try_from_secs_f64(seconds.clamp(0.0, f64::from(max))).unwrap_or(Duration::ZERO)
}

/// The kira handle of one built effect.
enum EffectHandle {
    Reverb(ReverbHandle),
    Filter(FilterHandle),
    Delay(DelayHandle),
    Compressor(CompressorHandle),
    Eq(EqFilterHandle),
    Distortion(DistortionHandle),
}

/// One effect slot: `None` when the listed effect was invalid and dropped.
struct Slot {
    /// The values from the graph.
    base: EffectDesc,
    /// The values last set.
    current: EffectDesc,
    handle: EffectHandle,
}

enum Track {
    Bus(Box<TrackHandle>),
    Send(SendTrackHandle),
}

/// A bus or a send.
struct Node {
    name: String,
    track: Track,
    volume: VolumeControlHandle,
    base_db: f32,
    effects: Vec<Option<Slot>>,
    meter: Arc<MeterSlot>,
}

/// The built graph.
pub(crate) struct Graph {
    desc: AudioGraphDesc,
    /// Sends first, then buses with every parent before its children.
    nodes: Vec<Node>,
    by_name: HashMap<String, usize>,
    default_bus: usize,
    snapshots: BTreeMap<String, SnapshotDesc>,
    /// Names already reported with RE7408.
    warned: HashSet<String>,
}

fn invalid_graph(message: String) -> Diagnostic {
    Diagnostic::error(codes::INVALID_GRAPH, message)
        .with_hint("check the [audio] section of real.toml; the default buses music, sfx and ui are used instead")
}

fn invalid_effect(message: String) -> Diagnostic {
    Diagnostic::error(codes::INVALID_EFFECT, message)
}

/// The graph's structural problems (RE7407), or the buses in build order
/// (every parent before its children).
fn check_structure(desc: &AudioGraphDesc) -> Result<Vec<String>, Vec<Diagnostic>> {
    let mut problems = Vec::new();
    if desc.sends.len() > MAX_SENDS {
        problems.push(invalid_graph(format!(
            "the audio graph has {} sends; at most {MAX_SENDS} are allowed",
            desc.sends.len()
        )));
    }
    if desc.buses.is_empty() {
        problems.push(invalid_graph("the audio graph has no buses".into()));
    }
    for name in desc.buses.keys().chain(desc.sends.keys()) {
        if name.trim().is_empty() {
            problems.push(invalid_graph(
                "an audio bus or send has an empty name".into(),
            ));
        }
    }
    for name in desc.buses.keys() {
        if desc.sends.contains_key(name) {
            problems.push(invalid_graph(format!(
                "`{name}` is both a bus and a send; names must be unique"
            )));
        }
    }
    if !desc.buses.contains_key(&desc.default_bus) {
        problems.push(invalid_graph(format!(
            "default_bus `{}` is not a bus",
            desc.default_bus
        )));
    }
    for (name, bus) in &desc.buses {
        if !bus.volume_db.is_finite() {
            problems.push(invalid_graph(format!(
                "bus `{name}` has volume_db {}, which is not finite",
                bus.volume_db
            )));
        }
        if !bus.parent.is_empty() && !desc.buses.contains_key(&bus.parent) {
            problems.push(invalid_graph(format!(
                "bus `{name}` has parent `{}`, which is not a bus",
                bus.parent
            )));
        }
        for (send, level) in &bus.sends {
            if !desc.sends.contains_key(send) {
                problems.push(invalid_graph(format!(
                    "bus `{name}` sends to `{send}`, which is not a send"
                )));
            }
            if !level.is_finite() {
                problems.push(invalid_graph(format!(
                    "bus `{name}` sends to `{send}` at {level} dB, which is not finite"
                )));
            }
        }
    }
    for (name, send) in &desc.sends {
        if !send.volume_db.is_finite() {
            problems.push(invalid_graph(format!(
                "send `{name}` has volume_db {}, which is not finite",
                send.volume_db
            )));
        }
    }
    if !problems.is_empty() {
        return Err(problems);
    }

    // Parents first; a bus left over at the end is on a cycle.
    let mut order: Vec<String> = Vec::new();
    let mut placed: HashSet<&str> = HashSet::new();
    loop {
        let before = order.len();
        for (name, bus) in &desc.buses {
            if !placed.contains(name.as_str())
                && (bus.parent.is_empty() || placed.contains(bus.parent.as_str()))
            {
                placed.insert(name);
                order.push(name.clone());
            }
        }
        if order.len() == desc.buses.len() {
            return Ok(order);
        }
        if order.len() == before {
            let cycle: Vec<&str> = desc
                .buses
                .keys()
                .map(String::as_str)
                .filter(|n| !placed.contains(n))
                .collect();
            return Err(vec![invalid_graph(format!(
                "the audio buses {} have parents in a cycle",
                cycle.join(", ")
            ))]);
        }
    }
}

fn unit(name: &str, v: f32) -> Result<(), String> {
    if (0.0..=1.0).contains(&v) {
        Ok(())
    } else {
        Err(format!("{name} {v} is not between 0 and 1"))
    }
}

fn hz(name: &str, v: f32) -> Result<(), String> {
    if (MIN_HZ..=MAX_HZ).contains(&v) {
        Ok(())
    } else {
        Err(format!(
            "{name} {v} is not between {MIN_HZ} and {MAX_HZ} Hz"
        ))
    }
}

fn finite(name: &str, v: f32) -> Result<(), String> {
    if v.is_finite() {
        Ok(())
    } else {
        Err(format!("{name} {v} is not finite"))
    }
}

/// Checks an effect's values before kira sees them.
pub(crate) fn validate(effect: &EffectDesc) -> Result<(), String> {
    match effect {
        EffectDesc::Reverb(r) => {
            unit("feedback", r.feedback)?;
            unit("damping", r.damping)?;
            unit("stereo_width", r.stereo_width)?;
            unit("mix", r.mix)
        }
        EffectDesc::Filter(f) => {
            hz("cutoff_hz", f.cutoff_hz)?;
            unit("resonance", f.resonance)?;
            unit("mix", f.mix)
        }
        EffectDesc::Delay(d) => {
            // kira panics on a zero delay and allocates the whole delay up front.
            if !(0.001..=10.0).contains(&d.seconds) {
                return Err(format!("seconds {} is not between 0.001 and 10", d.seconds));
            }
            finite("feedback_db", d.feedback_db)?;
            if d.feedback_db > 0.0 {
                return Err(format!(
                    "feedback_db {} is above 0, so the echo would grow without end",
                    d.feedback_db
                ));
            }
            unit("mix", d.mix)
        }
        EffectDesc::Compressor(c) => {
            finite("threshold_db", c.threshold_db)?;
            finite("makeup_db", c.makeup_db)?;
            if !(c.ratio.is_finite() && c.ratio >= 1.0) {
                return Err(format!(
                    "ratio {} is not a finite number of 1 or more",
                    c.ratio
                ));
            }
            for (name, ms) in [("attack_ms", c.attack_ms), ("release_ms", c.release_ms)] {
                if !(ms.is_finite() && (0.0..=MAX_ENVELOPE_MS).contains(&ms)) {
                    return Err(format!(
                        "{name} {ms} is not a number between 0 and {MAX_ENVELOPE_MS}"
                    ));
                }
            }
            unit("mix", c.mix)
        }
        EffectDesc::Eq(e) => {
            hz("hz", e.hz)?;
            finite("gain_db", e.gain_db)?;
            if !(e.q.is_finite() && e.q > 0.0) {
                return Err(format!("q {} is not a finite number above 0", e.q));
            }
            Ok(())
        }
        EffectDesc::Distortion(d) => {
            finite("drive_db", d.drive_db)?;
            unit("mix", d.mix)
        }
    }
}

fn number(param: &str, value: &EffectParam) -> Result<f32, String> {
    match value {
        EffectParam::Number(v) => Ok(*v),
        EffectParam::Choice(c) => Err(format!("{param} takes a number, not `{c}`")),
    }
}

fn choice<'a>(param: &str, value: &'a EffectParam) -> Result<&'a str, String> {
    match value {
        EffectParam::Choice(c) => Ok(c),
        EffectParam::Number(v) => Err(format!("{param} takes a name, not {v}")),
    }
}

/// The names of an effect's parameters, for messages.
fn params_of(effect: &EffectDesc) -> &'static str {
    match effect {
        EffectDesc::Reverb(_) => "feedback, damping, stereo_width, mix",
        EffectDesc::Filter(_) => "mode, cutoff_hz, resonance, mix",
        EffectDesc::Delay(_) => "feedback_db, mix (seconds is fixed once built)",
        EffectDesc::Compressor(_) => "threshold_db, ratio, attack_ms, release_ms, makeup_db, mix",
        EffectDesc::Eq(_) => "shape, hz, gain_db, q",
        EffectDesc::Distortion(_) => "mode, drive_db, mix",
    }
}

/// `effect` with `param` set to `value`, checked.
pub(crate) fn with_param(
    effect: &EffectDesc,
    param: &str,
    value: &EffectParam,
) -> Result<EffectDesc, String> {
    let mut e = effect.clone();
    let unknown = || {
        format!(
            "a {} has no parameter `{param}` that can change (it has {}); an effect's kind cannot change",
            effect.kind(),
            params_of(effect)
        )
    };
    match &mut e {
        EffectDesc::Reverb(r) => match param {
            "feedback" => r.feedback = number(param, value)?,
            "damping" => r.damping = number(param, value)?,
            "stereo_width" => r.stereo_width = number(param, value)?,
            "mix" => r.mix = number(param, value)?,
            _ => return Err(unknown()),
        },
        EffectDesc::Filter(f) => match param {
            "mode" => {
                f.mode = match choice(param, value)? {
                    "low_pass" => FilterMode::LowPass,
                    "high_pass" => FilterMode::HighPass,
                    "band_pass" => FilterMode::BandPass,
                    "notch" => FilterMode::Notch,
                    other => {
                        return Err(format!(
                            "mode `{other}` is not low_pass, high_pass, band_pass or notch"
                        ));
                    }
                }
            }
            "cutoff_hz" => f.cutoff_hz = number(param, value)?,
            "resonance" => f.resonance = number(param, value)?,
            "mix" => f.mix = number(param, value)?,
            _ => return Err(unknown()),
        },
        EffectDesc::Delay(d) => match param {
            "feedback_db" => d.feedback_db = number(param, value)?,
            "mix" => d.mix = number(param, value)?,
            _ => return Err(unknown()),
        },
        EffectDesc::Compressor(c) => match param {
            "threshold_db" => c.threshold_db = number(param, value)?,
            "ratio" => c.ratio = number(param, value)?,
            "attack_ms" => c.attack_ms = number(param, value)?,
            "release_ms" => c.release_ms = number(param, value)?,
            "makeup_db" => c.makeup_db = number(param, value)?,
            "mix" => c.mix = number(param, value)?,
            _ => return Err(unknown()),
        },
        EffectDesc::Eq(q) => match param {
            "shape" => {
                q.shape = match choice(param, value)? {
                    "bell" => EqShape::Bell,
                    "low_shelf" => EqShape::LowShelf,
                    "high_shelf" => EqShape::HighShelf,
                    other => {
                        return Err(format!(
                            "shape `{other}` is not bell, low_shelf or high_shelf"
                        ));
                    }
                }
            }
            "hz" => q.hz = number(param, value)?,
            "gain_db" => q.gain_db = number(param, value)?,
            "q" => q.q = number(param, value)?,
            _ => return Err(unknown()),
        },
        EffectDesc::Distortion(d) => match param {
            "mode" => {
                d.mode = match choice(param, value)? {
                    "hard_clip" => DistortionMode::HardClip,
                    "soft_clip" => DistortionMode::SoftClip,
                    other => return Err(format!("mode `{other}` is not hard_clip or soft_clip")),
                }
            }
            "drive_db" => d.drive_db = number(param, value)?,
            "mix" => d.mix = number(param, value)?,
            _ => return Err(unknown()),
        },
    }
    validate(&e)?;
    Ok(e)
}

fn filter_mode(m: FilterMode) -> kira::effect::filter::FilterMode {
    use kira::effect::filter::FilterMode as K;
    match m {
        FilterMode::LowPass => K::LowPass,
        FilterMode::HighPass => K::HighPass,
        FilterMode::BandPass => K::BandPass,
        FilterMode::Notch => K::Notch,
    }
}

fn eq_kind(s: EqShape) -> EqFilterKind {
    match s {
        EqShape::Bell => EqFilterKind::Bell,
        EqShape::LowShelf => EqFilterKind::LowShelf,
        EqShape::HighShelf => EqFilterKind::HighShelf,
    }
}

fn distortion_kind(m: DistortionMode) -> DistortionKind {
    match m {
        DistortionMode::HardClip => DistortionKind::HardClip,
        DistortionMode::SoftClip => DistortionKind::SoftClip,
    }
}

fn ms(v: f32) -> Duration {
    seconds_to_duration(f64::from(v) / 1000.0, MAX_ENVELOPE_MS / 1000.0)
}

/// Something effects can be added to.
trait Chain {
    fn effect<E: kira::effect::EffectBuilder>(&mut self, builder: E) -> E::Handle;
    fn built(&mut self, effect: Box<dyn kira::effect::Effect>);
}

impl Chain for TrackBuilder {
    fn effect<E: kira::effect::EffectBuilder>(&mut self, builder: E) -> E::Handle {
        self.add_effect(builder)
    }
    fn built(&mut self, effect: Box<dyn kira::effect::Effect>) {
        self.add_built_effect(effect);
    }
}

impl Chain for SendTrackBuilder {
    fn effect<E: kira::effect::EffectBuilder>(&mut self, builder: E) -> E::Handle {
        self.add_effect(builder)
    }
    fn built(&mut self, effect: Box<dyn kira::effect::Effect>) {
        self.add_built_effect(effect);
    }
}

/// Adds a checked effect to a track builder.
fn add_effect(chain: &mut impl Chain, effect: &EffectDesc) -> EffectHandle {
    match *effect {
        EffectDesc::Reverb(r) => EffectHandle::Reverb(
            chain.effect(
                ReverbBuilder::new()
                    .feedback(f64::from(r.feedback))
                    .damping(f64::from(r.damping))
                    .stereo_width(f64::from(r.stereo_width))
                    .mix(Mix(r.mix)),
            ),
        ),
        EffectDesc::Filter(f) => EffectHandle::Filter(
            chain.effect(
                FilterBuilder::new()
                    .mode(filter_mode(f.mode))
                    .cutoff(f64::from(f.cutoff_hz))
                    .resonance(f64::from(f.resonance))
                    .mix(Mix(f.mix)),
            ),
        ),
        EffectDesc::Delay(d) => EffectHandle::Delay(
            chain.effect(
                DelayBuilder::new()
                    .delay_time(seconds_to_duration(f64::from(d.seconds), 10.0))
                    .feedback(Decibels(d.feedback_db))
                    .mix(Mix(d.mix)),
            ),
        ),
        EffectDesc::Compressor(c) => EffectHandle::Compressor(
            chain.effect(
                CompressorBuilder::new()
                    .threshold(f64::from(c.threshold_db))
                    .ratio(f64::from(c.ratio))
                    .attack_duration(ms(c.attack_ms))
                    .release_duration(ms(c.release_ms))
                    .makeup_gain(Decibels(c.makeup_db))
                    .mix(Mix(c.mix)),
            ),
        ),
        EffectDesc::Eq(e) => EffectHandle::Eq(chain.effect(EqFilterBuilder::new(
            eq_kind(e.shape),
            f64::from(e.hz),
            Decibels(e.gain_db),
            f64::from(e.q),
        ))),
        EffectDesc::Distortion(d) => EffectHandle::Distortion(
            chain.effect(
                DistortionBuilder::new()
                    .kind(distortion_kind(d.mode))
                    .drive(Decibels(d.drive_db))
                    .mix(Mix(d.mix)),
            ),
        ),
    }
}

/// Sends one checked parameter of `effect` to its kira handle.
fn push_param(handle: &mut EffectHandle, effect: &EffectDesc, param: &str, t: Tween) {
    match (handle, effect) {
        (EffectHandle::Reverb(h), EffectDesc::Reverb(r)) => match param {
            "feedback" => h.set_feedback(f64::from(r.feedback), t),
            "damping" => h.set_damping(f64::from(r.damping), t),
            "stereo_width" => h.set_stereo_width(f64::from(r.stereo_width), t),
            "mix" => h.set_mix(Mix(r.mix), t),
            _ => {}
        },
        (EffectHandle::Filter(h), EffectDesc::Filter(f)) => match param {
            "mode" => h.set_mode(filter_mode(f.mode)),
            "cutoff_hz" => h.set_cutoff(f64::from(f.cutoff_hz), t),
            "resonance" => h.set_resonance(f64::from(f.resonance), t),
            "mix" => h.set_mix(Mix(f.mix), t),
            _ => {}
        },
        (EffectHandle::Delay(h), EffectDesc::Delay(d)) => match param {
            "feedback_db" => h.set_feedback(Decibels(d.feedback_db), t),
            "mix" => h.set_mix(Mix(d.mix), t),
            _ => {}
        },
        (EffectHandle::Compressor(h), EffectDesc::Compressor(c)) => match param {
            "threshold_db" => h.set_threshold(f64::from(c.threshold_db), t),
            "ratio" => h.set_ratio(f64::from(c.ratio), t),
            "attack_ms" => h.set_attack_duration(ms(c.attack_ms), t),
            "release_ms" => h.set_release_duration(ms(c.release_ms), t),
            "makeup_db" => h.set_makeup_gain(Decibels(c.makeup_db), t),
            "mix" => h.set_mix(Mix(c.mix), t),
            _ => {}
        },
        (EffectHandle::Eq(h), EffectDesc::Eq(e)) => match param {
            "shape" => h.set_kind(eq_kind(e.shape)),
            "hz" => h.set_frequency(f64::from(e.hz), t),
            "gain_db" => h.set_gain(Decibels(e.gain_db), t),
            "q" => h.set_q(f64::from(e.q), t),
            _ => {}
        },
        (EffectHandle::Distortion(h), EffectDesc::Distortion(d)) => match param {
            "mode" => h.set_kind(distortion_kind(d.mode)),
            "drive_db" => h.set_drive(Decibels(d.drive_db), t),
            "mix" => h.set_mix(Mix(d.mix), t),
            _ => {}
        },
        _ => {}
    }
}

/// Every parameter that can change, by effect kind, for returning to the
/// graph's values.
fn changeable(effect: &EffectDesc) -> &'static [&'static str] {
    match effect {
        EffectDesc::Reverb(_) => &["feedback", "damping", "stereo_width", "mix"],
        EffectDesc::Filter(_) => &["mode", "cutoff_hz", "resonance", "mix"],
        EffectDesc::Delay(_) => &["feedback_db", "mix"],
        EffectDesc::Compressor(_) => &[
            "threshold_db",
            "ratio",
            "attack_ms",
            "release_ms",
            "makeup_db",
            "mix",
        ],
        EffectDesc::Eq(_) => &["shape", "hz", "gain_db", "q"],
        EffectDesc::Distortion(_) => &["mode", "drive_db", "mix"],
    }
}

/// Builds the effects of one bus or send onto `chain`, then its volume and meter.
fn build_chain(
    chain: &mut impl Chain,
    owner: &str,
    effects: &[EffectDesc],
    volume_db: f32,
    problems: &mut Vec<Diagnostic>,
) -> (Vec<Option<Slot>>, VolumeControlHandle, Arc<MeterSlot>) {
    let slots = effects
        .iter()
        .enumerate()
        .map(|(i, e)| match validate(e) {
            Ok(()) => Some(Slot {
                base: e.clone(),
                current: e.clone(),
                handle: add_effect(chain, e),
            }),
            Err(why) => {
                problems.push(invalid_effect(format!(
                    "effect {i} ({}) on `{owner}` is dropped: {why}",
                    e.kind()
                )));
                None
            }
        })
        .collect();
    let volume = chain.effect(VolumeControlBuilder::new(Decibels(volume_db)));
    let meter = Arc::new(MeterSlot::default());
    chain.built(Box::new(Meter::new(meter.clone())));
    (slots, volume, meter)
}

impl Graph {
    /// Builds `desc` on `manager`. A graph with structural problems (RE7407) is
    /// replaced by the default graph; an invalid effect is dropped (RE7406); a
    /// snapshot entry naming something unknown is dropped (RE7408).
    pub(crate) fn build<B: Backend>(
        manager: &mut AudioManager<B>,
        desc: &AudioGraphDesc,
    ) -> (Graph, Vec<Diagnostic>) {
        let (desc, order, mut problems) = match check_structure(desc) {
            Ok(order) => (desc.clone(), order, Vec::new()),
            Err(problems) => {
                let fallback = AudioGraphDesc::default();
                let order = check_structure(&fallback).expect("the default graph is valid");
                (fallback, order, problems)
            }
        };
        let mut nodes: Vec<Node> = Vec::new();
        let mut by_name = HashMap::new();
        for (name, send) in &desc.sends {
            let mut builder = SendTrackBuilder::new();
            let (effects, volume, meter) = build_chain(
                &mut builder,
                name,
                &send.effects,
                send.volume_db,
                &mut problems,
            );
            match manager.add_send_track(builder) {
                Ok(track) => {
                    by_name.insert(name.clone(), nodes.len());
                    nodes.push(Node {
                        name: name.clone(),
                        track: Track::Send(track),
                        volume,
                        base_db: send.volume_db,
                        effects,
                        meter,
                    });
                }
                Err(e) => problems.push(invalid_graph(format!("send `{name}` was not built: {e}"))),
            }
        }
        for name in &order {
            let bus = &desc.buses[name];
            let mut builder = TrackBuilder::new()
                .sub_track_capacity(BUS_CAPACITY)
                .sound_capacity(BUS_CAPACITY);
            for (send, level) in &bus.sends {
                if let Some(Track::Send(s)) = by_name.get(send).map(|&i| &nodes[i].track) {
                    builder = builder.with_send(s, Decibels(*level));
                }
            }
            let (effects, volume, meter) = build_chain(
                &mut builder,
                name,
                &bus.effects,
                bus.volume_db,
                &mut problems,
            );
            let track = if bus.parent.is_empty() {
                manager.add_sub_track(builder)
            } else {
                match by_name.get(&bus.parent).map(|&i| &mut nodes[i].track) {
                    Some(Track::Bus(parent)) => parent.add_sub_track(builder),
                    // A parent that failed to build was reported already.
                    _ => continue,
                }
            };
            match track {
                Ok(track) => {
                    by_name.insert(name.clone(), nodes.len());
                    nodes.push(Node {
                        name: name.clone(),
                        track: Track::Bus(Box::new(track)),
                        volume,
                        base_db: bus.volume_db,
                        effects,
                        meter,
                    });
                }
                Err(e) => problems.push(invalid_graph(format!("bus `{name}` was not built: {e}"))),
            }
        }
        let default_bus = by_name
            .get(&desc.default_bus)
            .copied()
            .or_else(|| nodes.iter().position(|n| matches!(n.track, Track::Bus(_))))
            .expect("the graph has at least one bus");
        let mut graph = Graph {
            snapshots: BTreeMap::new(),
            desc,
            nodes,
            by_name,
            default_bus,
            warned: HashSet::new(),
        };
        graph.snapshots = graph.check_snapshots(&mut problems);
        (graph, problems)
    }

    /// The graph's snapshots without the entries that cannot apply.
    fn check_snapshots(&self, problems: &mut Vec<Diagnostic>) -> BTreeMap<String, SnapshotDesc> {
        let mut out = BTreeMap::new();
        for (name, snap) in &self.desc.snapshots {
            if name == DEFAULT_SNAPSHOT {
                problems.push(invalid_graph(format!(
                    "snapshot `{DEFAULT_SNAPSHOT}` is reserved for the graph's own values; rename it"
                )));
                continue;
            }
            let mut kept = SnapshotDesc::default();
            for (bus, db) in &snap.buses {
                if !self.by_name.contains_key(bus) {
                    problems.push(unknown_bus(bus, &format!("snapshot `{name}`")));
                } else if !db.is_finite() {
                    problems.push(invalid_effect(format!(
                        "snapshot `{name}` sets `{bus}` to {db} dB, which is not finite"
                    )));
                } else {
                    kept.buses.insert(bus.clone(), *db);
                }
            }
            for entry in &snap.effects {
                let Some(&node) = self.by_name.get(&entry.bus) else {
                    problems.push(unknown_bus(&entry.bus, &format!("snapshot `{name}`")));
                    continue;
                };
                let mut entry_kept = SnapshotEffect {
                    bus: entry.bus.clone(),
                    effect: entry.effect,
                    params: BTreeMap::new(),
                };
                match self.slot(node, entry.effect) {
                    Ok(slot) => {
                        for (param, value) in &entry.params {
                            match with_param(&slot.base, param, value) {
                                Ok(_) => {
                                    entry_kept.params.insert(param.clone(), value.clone());
                                }
                                Err(why) => problems.push(invalid_effect(format!(
                                    "snapshot `{name}`, effect {} on `{}`: {why}",
                                    entry.effect, entry.bus
                                ))),
                            }
                        }
                    }
                    Err(why) => problems.push(invalid_effect(format!("snapshot `{name}`: {why}"))),
                }
                if !entry_kept.params.is_empty() {
                    kept.effects.push(entry_kept);
                }
            }
            out.insert(name.clone(), kept);
        }
        out
    }

    /// The description the graph was built from (the default graph when the
    /// requested one had structural problems).
    pub(crate) fn desc(&self) -> &AudioGraphDesc {
        &self.desc
    }

    fn slot(&self, node: usize, effect: u32) -> Result<&Slot, String> {
        let n = &self.nodes[node];
        match n.effects.get(effect as usize) {
            Some(Some(slot)) => Ok(slot),
            Some(None) => Err(format!(
                "effect {effect} on `{}` was dropped when the graph was built",
                n.name
            )),
            None => Err(format!(
                "`{}` has {} effects; there is no effect {effect}",
                n.name,
                n.effects.len()
            )),
        }
    }

    /// The bus or send called `name`; RE7408 the first time an unknown name is used.
    fn node(&mut self, name: &str) -> Result<usize, Option<Diagnostic>> {
        if let Some(&i) = self.by_name.get(name) {
            return Ok(i);
        }
        if self.warned.insert(name.to_string()) {
            Err(Some(unknown_bus(name, "a command")))
        } else {
            Err(None)
        }
    }

    /// The track a sound on bus `name` plays on (the default bus when `name` is
    /// empty or not a bus), and the warning for an unknown name the first time.
    pub(crate) fn bus_track(&mut self, name: &str) -> (&mut TrackHandle, Option<Diagnostic>) {
        let mut warning = None;
        let index = if name.is_empty() {
            self.default_bus
        } else {
            match self.by_name.get(name) {
                Some(&i) if matches!(self.nodes[i].track, Track::Bus(_)) => i,
                Some(_) => {
                    if self.warned.insert(name.to_string()) {
                        warning = Some(
                            Diagnostic::warning(
                                codes::UNKNOWN_BUS,
                                format!(
                                    "`{name}` is a send, and sounds play on buses; it plays on `{}`",
                                    self.desc.default_bus
                                ),
                            )
                            .with_hint("route a bus to the send with `sends` in real.toml"),
                        );
                    }
                    self.default_bus
                }
                None => {
                    if self.warned.insert(name.to_string()) {
                        warning = Some(unknown_bus(name, "a sound"));
                    }
                    self.default_bus
                }
            }
        };
        match &mut self.nodes[index].track {
            Track::Bus(t) => (t, warning),
            Track::Send(_) => unreachable!("sounds only resolve to buses"),
        }
    }

    /// Tweens a bus's or send's volume.
    pub(crate) fn set_volume(
        &mut self,
        name: &str,
        volume_db: f32,
        seconds: f32,
    ) -> Result<(), Option<Diagnostic>> {
        let i = self.node(name)?;
        self.nodes[i]
            .volume
            .set_volume(Decibels(volume_db), tween(seconds));
        Ok(())
    }

    /// Sets a bus's or send's own track volume, which the graph leaves at 0 dB
    /// and no fader or snapshot touches: the player's volume
    /// (`Audio::set_user_volume`).
    pub(crate) fn set_user_volume(&mut self, name: &str, db: Decibels) -> Result<(), Diagnostic> {
        let Some(&i) = self.by_name.get(name) else {
            return Err(unknown_bus(name, "a user volume"));
        };
        match &mut self.nodes[i].track {
            Track::Bus(t) => t.set_volume(db, tween(0.0)),
            Track::Send(t) => t.set_volume(db, tween(0.0)),
        }
        Ok(())
    }

    /// Tweens one effect parameter.
    pub(crate) fn set_param(
        &mut self,
        name: &str,
        effect: u32,
        param: &str,
        value: &EffectParam,
        seconds: f32,
    ) -> Result<(), Option<Diagnostic>> {
        let i = self.node(name)?;
        let next = self
            .slot(i, effect)
            .and_then(|slot| with_param(&slot.current, param, value))
            .map_err(|why| {
                Some(invalid_effect(format!(
                    "effect {effect} on `{name}` is unchanged: {why}"
                )))
            })?;
        let Some(slot) = self.nodes[i].effects[effect as usize].as_mut() else {
            unreachable!("slot() found the effect");
        };
        push_param(&mut slot.handle, &next, param, tween(seconds));
        slot.current = next;
        Ok(())
    }

    /// Tweens to a snapshot, or to the graph's own values for `default`.
    pub(crate) fn apply_snapshot(
        &mut self,
        name: &str,
        seconds: f32,
    ) -> Result<(), Option<Diagnostic>> {
        let t = tween(seconds);
        if name == DEFAULT_SNAPSHOT {
            for node in &mut self.nodes {
                node.volume.set_volume(Decibels(node.base_db), t);
                for slot in node.effects.iter_mut().flatten() {
                    for param in changeable(&slot.base) {
                        push_param(&mut slot.handle, &slot.base, param, t);
                    }
                    slot.current = slot.base.clone();
                }
            }
            return Ok(());
        }
        let Some(snap) = self.snapshots.get(name).cloned() else {
            return Err(self.warned.insert(format!("snapshot:{name}")).then(|| {
                Diagnostic::warning(
                    codes::UNKNOWN_BUS,
                    format!("there is no audio snapshot `{name}`; nothing changes"),
                )
                .with_hint("snapshots are declared under [audio.snapshots] in real.toml")
            }));
        };
        for (bus, db) in &snap.buses {
            self.set_volume(bus, *db, seconds)?;
        }
        for entry in &snap.effects {
            for (param, value) in &entry.params {
                self.set_param(&entry.bus, entry.effect, param, value, seconds)?;
            }
        }
        Ok(())
    }

    /// Every bus's and send's level since the last call, by name.
    pub(crate) fn levels(&self) -> Vec<BusLevel> {
        let mut levels: Vec<BusLevel> = self
            .nodes
            .iter()
            .map(|n| {
                let w = n.meter.take();
                BusLevel {
                    bus: n.name.clone(),
                    peak_db: crate::linear_to_decibels(w.peak).0,
                    rms_db: crate::linear_to_decibels(w.rms).0,
                }
            })
            .collect();
        levels.sort_by(|a, b| a.bus.cmp(&b.bus));
        levels
    }
}

fn unknown_bus(name: &str, user: &str) -> Diagnostic {
    Diagnostic::warning(
        codes::UNKNOWN_BUS,
        format!("{user} names audio bus `{name}`, which the graph does not have"),
    )
    .with_hint(
        "buses and sends are declared under [audio] in real.toml; sounds fall back to default_bus",
    )
}
