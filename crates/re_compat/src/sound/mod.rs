//! `Sound` on `re_audio` (WP15): playback state, timing, events, 3D roll-off
//! and the audio commands that play it.
//!
//! The layer keeps each sound's clock itself, so `TimePosition`, `IsPlaying`,
//! `Ended` and `DidLoop` behave the same with and without an audio device.
//! [`step`] runs once per frame (after `Heartbeat`) in the VM of the world
//! that plays sounds (the client's): it advances every playing sound by the
//! frame's time step, fires the events that are due, and queues the
//! [`AudioCmd`]s that make the audio backend follow. The frame driver takes
//! them with [`take_commands`] and hands them to `re_audio`. Headless runs
//! read the play events from [`take_events`].
//!
//! * `SoundId` resolves through the substitute table ([`SoundSettings`]):
//!   online ids are not available offline, so `[compat] assets` in
//!   `real.toml` maps each id to a project file. An unknown id is silent and
//!   warns once with `RE9350`; it has a `TimeLength` of 0, so a sound that is
//!   not looped ends at once.
//! * A sound whose parent is a part or an attachment is 3D: it plays at the
//!   parent's position and its volume follows `RollOffMode` between
//!   `RollOffMinDistance` and `RollOffMaxDistance` from the listener. Any
//!   other sound plays at its own volume everywhere.
//! * A script that writes `Playing` or `TimePosition` starts, stops or seeks
//!   the sound, as `Play`, `Stop` and a seek would.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use glam::{Quat, Vec3};
use mlua::{Lua, MultiValue, Value as LuaValue};
use re_reflect::Value;
use re_scene::{Entity, GlobalTransform, World, entity_id};
use re_types::audio::AudioCmd;

use crate::instance::methods_api::ret;
use crate::instance::tree;
use crate::runtime::{Bindings, state, with_world};
use crate::scene::{CompatConfig, asset_key};
use crate::signal;
use crate::tween::set_own;

/// `Enum.RollOffMode`: how a 3D sound's volume falls with distance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RollOffMode {
    /// `min / distance`, silent beyond the maximum distance.
    #[default]
    Inverse,
    /// Full volume to the minimum distance, then a straight line to silence
    /// at the maximum distance.
    Linear,
    /// The square of `Linear`.
    LinearSquare,
    /// `Inverse` near the minimum distance, tapering to silence at the
    /// maximum distance: the smaller of `Inverse` and `LinearSquare`.
    InverseTapered,
}

impl RollOffMode {
    fn from_name(name: &str) -> RollOffMode {
        match name {
            "Linear" => RollOffMode::Linear,
            "LinearSquare" => RollOffMode::LinearSquare,
            "InverseTapered" => RollOffMode::InverseTapered,
            _ => RollOffMode::Inverse,
        }
    }
}

/// The gain (0 to 1) of a 3D sound `distance` studs from the listener.
pub fn roll_off(mode: RollOffMode, min: f32, max: f32, distance: f32) -> f32 {
    let min = if min.is_finite() { min.max(0.0) } else { 0.0 };
    let max = if max.is_finite() {
        max.max(min)
    } else {
        f32::MAX
    };
    let d = if distance.is_finite() {
        distance.max(0.0)
    } else {
        f32::MAX
    };
    if d <= min {
        return 1.0;
    }
    if d >= max {
        return 0.0;
    }
    let inverse = if d > 0.0 { (min / d).min(1.0) } else { 1.0 };
    let linear = if max > min {
        (max - d) / (max - min)
    } else {
        0.0
    };
    match mode {
        RollOffMode::Inverse => inverse,
        RollOffMode::Linear => linear,
        RollOffMode::LinearSquare => linear * linear,
        RollOffMode::InverseTapered => inverse.min(linear * linear),
    }
}

/// The length in seconds of a clip, by its file path; `None` when it cannot
/// be loaded.
pub type ClipSecondsFn = dyn Fn(&str) -> Option<f64> + Send + Sync;

/// How sound ids become clips: the project's substitute table and the clip
/// lengths. Install one per VM with [`configure`]; without one every id is
/// unknown.
#[derive(Clone, Default)]
pub struct SoundSettings {
    /// Asset id to project clip path, keyed as [`asset_key`] gives them (the
    /// `[compat] assets` table).
    pub assets: BTreeMap<String, String>,
    /// The length of a clip in seconds, `None` when it cannot be loaded.
    pub clip_seconds: Option<Arc<ClipSecondsFn>>,
}

impl std::fmt::Debug for SoundSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SoundSettings")
            .field("assets", &self.assets.len())
            .finish_non_exhaustive()
    }
}

impl SoundSettings {
    /// The settings of a project: its `[compat] assets` table, and the clip
    /// lengths (usually `re_audio::Audio::clip_seconds`).
    pub fn from_config(config: &CompatConfig, clip_seconds: Option<Arc<ClipSecondsFn>>) -> Self {
        SoundSettings {
            assets: config.assets.clone(),
            clip_seconds,
        }
    }

    /// The clip path of `id` (`rbxassetid://123`, `123`, or the asset URL).
    pub fn resolve(&self, id: &str) -> Option<&str> {
        self.assets.get(asset_key(id)).map(String::as_str)
    }
}

/// What happened to a sound, for headless runs and tests.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundEvent {
    /// Seconds since the VM's first sound step.
    pub time: f64,
    /// The sound instance.
    pub sound: Entity,
    /// What happened.
    pub kind: SoundEventKind,
    /// Its `SoundId`.
    pub sound_id: String,
}

/// The kinds of [`SoundEvent`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundEventKind {
    /// Started (`Play`, or `Playing` set to true).
    Played,
    /// Paused.
    Paused,
    /// Resumed after a pause.
    Resumed,
    /// Stopped by a script.
    Stopped,
    /// Reached its end and stopped.
    Ended,
    /// Reached its end and started again (`Looped`).
    Looped,
    /// Its id resolved to a clip that can play (fires `Loaded`).
    Loaded,
}

/// The state of one sound as last applied.
#[derive(Clone, Debug, Default)]
struct Voice {
    playing: bool,
    /// The position the layer last wrote (to tell script writes apart).
    position: f64,
    /// The position to restart from on `Play`: the last one a script set.
    script_position: f64,
    length: f64,
    loops: i64,
    sound_id: String,
    clip: Option<String>,
    /// True while the backend has the sound started.
    started: bool,
    gain: f32,
    rate: f32,
    looped: bool,
    at: Option<Vec3>,
    /// True once `sound_id` has been looked up.
    resolved: bool,
}

#[derive(Default)]
struct Hub {
    settings: SoundSettings,
    voices: BTreeMap<u64, (Entity, Voice)>,
    commands: Vec<AudioCmd>,
    events: Vec<SoundEvent>,
    warned: HashSet<String>,
    time: f64,
    listener: Option<(Vec3, Quat)>,
    /// Events to fire once the voices are stored, so a handler that runs
    /// at once (`SignalBehavior::Immediate`) sees, and keeps, the new state.
    pending: Vec<(Entity, SoundEventKind, String, i64)>,
}

fn hub_mut<R>(lua: &Lua, f: impl FnOnce(&mut Hub) -> R) -> R {
    if lua.app_data_ref::<Hub>().is_none() {
        lua.set_app_data(Hub::default());
    }
    let mut h = lua.app_data_mut::<Hub>().expect("installed above");
    f(&mut h)
}

/// Installs the substitute table and clip lengths for this VM.
pub fn configure(lua: &Lua, settings: SoundSettings) {
    hub_mut(lua, |h| h.settings = settings);
}

/// The audio commands queued since the last call, in order.
pub fn take_commands(lua: &Lua) -> Vec<AudioCmd> {
    hub_mut(lua, |h| std::mem::take(&mut h.commands))
}

/// The sound events since the last call, in order.
pub fn take_events(lua: &Lua) -> Vec<SoundEvent> {
    hub_mut(lua, |h| std::mem::take(&mut h.events))
}

/// Registers the `Sound` methods.
pub fn register(b: &mut Bindings) {
    b.method(
        "Sound",
        "Play",
        Arc::new(|lua, e, _| control(lua, e, Op::Play)),
    );
    b.method(
        "Sound",
        "Stop",
        Arc::new(|lua, e, _| control(lua, e, Op::Stop)),
    );
    b.method(
        "Sound",
        "Pause",
        Arc::new(|lua, e, _| control(lua, e, Op::Pause)),
    );
    b.method(
        "Sound",
        "Resume",
        Arc::new(|lua, e, _| control(lua, e, Op::Resume)),
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Play,
    Stop,
    Pause,
    Resume,
}

fn get(w: &World, e: Entity, prop: &str) -> Option<Value> {
    let key = w
        .registry
        .try_key(&crate::class::own_component_name("Sound"))?;
    let index = w.registry.schema(key).field_index(prop).ok()?;
    w.registry.get_field(&w.ecs, e, key, index)
}

fn num(v: Option<Value>, default: f64) -> f64 {
    match v {
        Some(Value::Float(x)) => x,
        Some(Value::Int(i)) => i as f64,
        _ => default,
    }
}

fn flag(v: Option<Value>) -> bool {
    matches!(v, Some(Value::Bool(true)))
}

fn text(v: Option<Value>) -> String {
    match v {
        Some(Value::Str(s)) => s,
        _ => String::new(),
    }
}

fn set(w: &mut World, e: Entity, prop: &str, v: Value) {
    // The fields exist on every Sound; a failure means the entity is gone.
    let _ = set_own(w, e, "Sound", prop, v);
}

/// The Sound's values the step reads.
#[derive(Clone, Debug)]
struct Props {
    sound_id: String,
    volume: f32,
    speed: f32,
    looped: bool,
    playing: bool,
    position: f64,
    mode: RollOffMode,
    min: f32,
    max: f32,
    at: Option<Vec3>,
}

fn props(w: &World, classes: &crate::ClassRegistry, e: Entity) -> Props {
    let mode = match get(w, e, "RollOffMode") {
        Some(Value::Enum(m)) => RollOffMode::from_name(&m),
        _ => RollOffMode::Inverse,
    };
    Props {
        sound_id: text(get(w, e, "SoundId")),
        volume: num(get(w, e, "Volume"), 0.5).clamp(0.0, 10.0) as f32,
        speed: num(get(w, e, "PlaybackSpeed"), 1.0) as f32,
        looped: flag(get(w, e, "Looped")),
        playing: flag(get(w, e, "Playing")),
        position: num(get(w, e, "TimePosition"), 0.0),
        mode,
        min: num(get(w, e, "RollOffMinDistance"), 10.0) as f32,
        max: num(get(w, e, "RollOffMaxDistance"), 10_000.0) as f32,
        at: emitter_position(w, classes, e),
    }
}

/// Where a 3D sound plays: its parent's position when the parent is a part
/// or an attachment, else `None` (the sound is not 3D).
fn emitter_position(w: &World, classes: &crate::ClassRegistry, e: Entity) -> Option<Vec3> {
    let parent = tree::parent(w, e)?;
    let is_3d =
        tree::is_a(w, classes, parent, "BasePart") || tree::is_a(w, classes, parent, "Attachment");
    if !is_3d {
        return None;
    }
    let at = |x: Entity| {
        w.ecs
            .get::<GlobalTransform>(x)
            .map(|g| Vec3::from(g.0.translation))
    };
    at(parent).or_else(|| tree::parent(w, parent).and_then(at))
}

/// Applies `Play`, `Stop`, `Pause` or `Resume` to a sound.
fn control(lua: &Lua, e: Entity, op: Op) -> mlua::Result<MultiValue> {
    let st = state(lua)?;
    let p = with_world(lua, |w| props(w, &st.setup.classes, e))?;
    let voice = hub_mut(lua, |h| h.voices.get(&e.to_bits()).map(|(_, v)| v.clone()));
    let mut v = voice.unwrap_or_else(|| fresh_voice(&p));
    match op {
        Op::Play => {
            v.position = v.script_position.max(0.0);
            v.loops = 0;
            begin(lua, e, &mut v, &p, SoundEventKind::Played)?;
        }
        Op::Resume => {
            if !v.playing {
                begin(lua, e, &mut v, &p, SoundEventKind::Resumed)?;
            }
        }
        Op::Pause => {
            if v.playing {
                halt(lua, e, &mut v, SoundEventKind::Paused)?;
            }
        }
        Op::Stop => {
            // Stopping a stopped sound still rewinds it, quietly.
            v.position = 0.0;
            v.script_position = 0.0;
            if v.playing {
                halt(lua, e, &mut v, SoundEventKind::Stopped)?;
            }
        }
    }
    write_back(lua, e, &v)?;
    hub_mut(lua, |h| {
        h.voices.insert(e.to_bits(), (e, v));
    });
    flush(lua)?;
    signal::events::after_write(lua)?;
    ret(lua, ())
}

fn fresh_voice(p: &Props) -> Voice {
    Voice {
        position: p.position,
        script_position: p.position,
        sound_id: p.sound_id.clone(),
        gain: -1.0,
        rate: 1.0,
        ..Voice::default()
    }
}

/// Resolves the voice's clip and length from its `SoundId`, warning once for
/// an unknown id.
fn resolve(lua: &Lua, e: Entity, v: &mut Voice, sound_id: &str) -> mlua::Result<()> {
    v.sound_id = sound_id.to_string();
    let (clip, length, warn) = hub_mut(lua, |h| {
        let clip = h.settings.resolve(sound_id).map(str::to_string);
        let length = clip
            .as_deref()
            .and_then(|c| h.settings.clip_seconds.as_ref().and_then(|f| f(c)));
        let warn = (clip.is_none() || length.is_none())
            && !sound_id.is_empty()
            && h.warned.insert(sound_id.to_string());
        (clip, length, warn)
    });
    let was_loaded = v.clip.is_some();
    v.clip = clip.filter(|_| length.is_some());
    v.length = length.unwrap_or(0.0);
    v.resolved = true;
    if v.clip.is_some() && !was_loaded {
        hub_mut(lua, |h| {
            h.pending
                .push((e, SoundEventKind::Loaded, v.sound_id.clone(), 0))
        });
    }
    if warn {
        let why = if v.clip.is_none() && length.is_none() {
            "is not in the [compat.assets] table of real.toml (or its file cannot be loaded)"
        } else {
            "cannot be loaded"
        };
        let w: mlua::Function = lua.globals().get("warn")?;
        w.call::<()>(format!(
            "[{}] sound {sound_id} {why}; it is silent",
            crate::codes::UNKNOWN_ASSET
        ))?;
    }
    Ok(())
}

fn begin(lua: &Lua, e: Entity, v: &mut Voice, p: &Props, kind: SoundEventKind) -> mlua::Result<()> {
    if v.clip.is_none() || v.sound_id != p.sound_id {
        resolve(lua, e, v, &p.sound_id)?;
    }
    v.playing = true;
    v.looped = p.looped;
    start_backend(lua, e, v, p);
    record(lua, e, v, kind);
    fire(lua, e, kind, v)
}

fn halt(lua: &Lua, e: Entity, v: &mut Voice, kind: SoundEventKind) -> mlua::Result<()> {
    v.playing = false;
    stop_backend(lua, e, v);
    record(lua, e, v, kind);
    fire(lua, e, kind, v)
}

fn start_backend(lua: &Lua, e: Entity, v: &mut Voice, p: &Props) {
    let Some(clip) = v.clip.clone() else {
        return;
    };
    let entity = entity_id(e);
    let gain = gain_of(lua, p);
    hub_mut(lua, |h| {
        if v.started {
            h.commands.push(AudioCmd::StopSource { entity });
        }
        if p.at.is_some() {
            h.commands.push(AudioCmd::SetSourceAttenuation {
                entity,
                enabled: false,
            });
        }
        h.commands.push(AudioCmd::StartSource {
            entity,
            clip,
            volume: gain,
            looping: p.looped,
            spatial: p.at.is_some(),
            position: p.at.unwrap_or(Vec3::ZERO),
        });
        let rate = speed(p.speed);
        if rate != 1.0 {
            h.commands.push(AudioCmd::SetSourceRate { entity, rate });
        }
        if v.position > 0.0 {
            h.commands.push(AudioCmd::SeekSource {
                entity,
                seconds: v.position,
            });
        }
    });
    v.started = true;
    v.gain = gain;
    v.rate = speed(p.speed);
    v.at = p.at;
}

fn stop_backend(lua: &Lua, e: Entity, v: &mut Voice) {
    if v.started {
        hub_mut(lua, |h| {
            h.commands.push(AudioCmd::StopSource {
                entity: entity_id(e),
            });
        });
        v.started = false;
    }
}

/// A usable playback rate: `PlaybackSpeed`, at least 0.01.
fn speed(s: f32) -> f32 {
    if s.is_finite() { s.max(0.01) } else { 1.0 }
}

fn gain_of(lua: &Lua, p: &Props) -> f32 {
    // Until set_listener is called the listener is at the origin, as the
    // audio backend's is.
    let listener = lua
        .app_data_ref::<Hub>()
        .and_then(|h| h.listener.map(|(at, _)| at))
        .unwrap_or(Vec3::ZERO);
    match p.at {
        Some(at) => p.volume * roll_off(p.mode, p.min, p.max, at.distance(listener)),
        None => p.volume,
    }
}

fn record(lua: &Lua, e: Entity, v: &Voice, kind: SoundEventKind) {
    hub_mut(lua, |h| {
        let time = h.time;
        h.events.push(SoundEvent {
            time,
            sound: e,
            kind,
            sound_id: v.sound_id.clone(),
        });
    });
}

/// Queues an event; [`flush`] fires it.
fn fire(lua: &Lua, e: Entity, kind: SoundEventKind, v: &Voice) -> mlua::Result<()> {
    hub_mut(lua, |h| {
        h.pending.push((e, kind, v.sound_id.clone(), v.loops))
    });
    Ok(())
}

/// Fires the queued events, after the voices that raised them are stored.
fn flush(lua: &Lua) -> mlua::Result<()> {
    loop {
        let pending = hub_mut(lua, |h| std::mem::take(&mut h.pending));
        if pending.is_empty() {
            return Ok(());
        }
        for (e, kind, sound_id, loops) in pending {
            let id = LuaValue::String(lua.create_string(&sound_id)?);
            let (name, args) = match kind {
                SoundEventKind::Played => ("Played", vec![id]),
                SoundEventKind::Paused => ("Paused", vec![id]),
                SoundEventKind::Resumed => ("Resumed", vec![id]),
                SoundEventKind::Stopped => ("Stopped", vec![id]),
                SoundEventKind::Ended => ("Ended", vec![id]),
                SoundEventKind::Looped => ("DidLoop", vec![id, LuaValue::Integer(loops)]),
                SoundEventKind::Loaded => ("Loaded", vec![id]),
            };
            signal::fire(lua, e, name, MultiValue::from_vec(args))?;
        }
    }
}

/// Writes the layer's view of a sound back to its properties.
fn write_back(lua: &Lua, e: Entity, v: &Voice) -> mlua::Result<()> {
    with_world(lua, |w| {
        if !tree::alive(w, e) {
            return;
        }
        set(w, e, "Playing", Value::Bool(v.playing));
        set(w, e, "IsPlaying", Value::Bool(v.playing));
        set(
            w,
            e,
            "IsPaused",
            Value::Bool(!v.playing && v.position > 0.0),
        );
        set(w, e, "TimePosition", Value::Float(v.position));
        set(w, e, "TimeLength", Value::Float(v.length));
        set(w, e, "IsLoaded", Value::Bool(v.clip.is_some()));
    })
}

/// Sets where the listener is (usually the camera). Sounds that are 3D take
/// their roll-off from the distance to it, and the backend pans by it.
pub fn set_listener(lua: &Lua, position: Vec3, rotation: Quat) {
    hub_mut(lua, |h| {
        if h.listener != Some((position, rotation)) {
            h.listener = Some((position, rotation));
            h.commands
                .push(AudioCmd::SetListener { position, rotation });
        }
    });
}

/// Advances every sound by `dt` seconds: picks up script writes to
/// `Playing`, `TimePosition`, `SoundId`, `Volume`, `PlaybackSpeed` and
/// `Looped`, moves the clocks, fires `Ended` and `DidLoop`, and queues the
/// audio commands. Call it once per frame, after [`set_listener`] for that
/// frame's camera.
pub fn step(lua: &Lua, dt: f64) -> mlua::Result<()> {
    let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
    let st = state(lua)?;
    let classes = st.setup.classes.clone();
    let sounds: Vec<(Entity, Props)> = with_world(lua, |w| {
        let mut out = Vec::new();
        let mut q = w.ecs.query::<(Entity, &crate::ClassMarker)>();
        for (e, m) in q.iter(&w.ecs) {
            if m.class == "Sound" && !tree::is_destroyed(w, e) {
                out.push(e);
            }
        }
        out.sort_by_key(|e| e.to_bits());
        out.into_iter()
            .map(|e| (e, props(w, &classes, e)))
            .collect()
    })?;
    hub_mut(lua, |h| h.time += dt);
    let live: HashSet<u64> = sounds.iter().map(|(e, _)| e.to_bits()).collect();
    // Destroyed sounds stop.
    hub_mut(lua, |h| {
        let gone: Vec<u64> = h
            .voices
            .keys()
            .filter(|k| !live.contains(k))
            .copied()
            .collect();
        for k in gone {
            if let Some((e, v)) = h.voices.remove(&k)
                && v.started
            {
                h.commands.push(AudioCmd::StopSource {
                    entity: entity_id(e),
                });
            }
        }
    });
    for (e, p) in sounds {
        let known = hub_mut(lua, |h| h.voices.get(&e.to_bits()).map(|(_, v)| v.clone()));
        let mut v = match known {
            Some(v) => v,
            None => {
                if !p.playing && p.sound_id.is_empty() {
                    // A sound with no id that nobody played needs no state.
                    continue;
                }
                let mut v = fresh_voice(&p);
                v.playing = false;
                v
            }
        };
        // Script writes since the last step.
        if (p.position - v.position).abs() > 1e-9 {
            v.position = p.position.max(0.0);
            v.script_position = v.position;
            if v.started {
                hub_mut(lua, |h| {
                    h.commands.push(AudioCmd::SeekSource {
                        entity: entity_id(e),
                        seconds: v.position,
                    });
                });
            }
        }
        // A new id resolves at once, so TimeLength, IsLoaded and Loaded
        // follow it before any Play.
        if !v.resolved || p.sound_id != v.sound_id {
            resolve(lua, e, &mut v, &p.sound_id)?;
            if v.playing {
                start_backend(lua, e, &mut v, &p);
            }
        }
        if p.playing != v.playing {
            if p.playing {
                begin(lua, e, &mut v, &p, SoundEventKind::Played)?;
            } else {
                // Playing set to false keeps the position, like Pause.
                halt(lua, e, &mut v, SoundEventKind::Paused)?;
            }
        }
        if v.playing {
            if p.looped != v.looped && v.started {
                v.looped = p.looped;
                start_backend(lua, e, &mut v, &p);
            }
            // Moving between a 2D and a 3D parent restarts the source where
            // it is, as the other kind.
            if v.started && p.at.is_some() != v.at.is_some() {
                start_backend(lua, e, &mut v, &p);
            }
            v.looped = p.looped;
            // The clock.
            v.position += dt * f64::from(speed(p.speed));
            if v.position >= v.length {
                if p.looped && v.length > 0.0 {
                    while v.position >= v.length {
                        v.position -= v.length;
                        v.loops += 1;
                    }
                    record(lua, e, &v, SoundEventKind::Looped);
                    fire(lua, e, SoundEventKind::Looped, &v)?;
                } else if !p.looped {
                    v.position = 0.0;
                    v.script_position = 0.0;
                    v.playing = false;
                    stop_backend(lua, e, &mut v);
                    record(lua, e, &v, SoundEventKind::Ended);
                    fire(lua, e, SoundEventKind::Ended, &v)?;
                } else {
                    // A looped sound with no clip plays silently on.
                    v.position = 0.0;
                }
            }
            if v.started {
                let entity = entity_id(e);
                let gain = gain_of(lua, &p);
                let rate = speed(p.speed);
                hub_mut(lua, |h| {
                    if (gain - v.gain).abs() > 1e-4 {
                        h.commands.push(AudioCmd::SetSourceVolume {
                            entity,
                            volume: gain,
                        });
                    }
                    if (rate - v.rate).abs() > 1e-6 {
                        h.commands.push(AudioCmd::SetSourceRate { entity, rate });
                    }
                    if let Some(at) = p.at
                        && v.at != Some(at)
                    {
                        h.commands.push(AudioCmd::SetSourcePosition {
                            entity,
                            position: at,
                        });
                    }
                });
                v.gain = gain;
                v.rate = rate;
                v.at = p.at;
            }
        }
        write_back(lua, e, &v)?;
        hub_mut(lua, |h| {
            h.voices.insert(e.to_bits(), (e, v));
        });
    }
    flush(lua)?;
    signal::events::after_write(lua)
}
