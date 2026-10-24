//! Animation: `Animator`, `Animation`, `AnimationTrack`, keyframe sequences
//! and `IKControl`, on `re_anim`'s keyframe mixer.
//!
//! An Animator drives the `Motor6D` joints of the rig it sits in (the model
//! holding its Humanoid or AnimationController), and the `Bone`s of its
//! skinned `MeshPart`s. A track's joints are named by each joint's `Part1`
//! (or the bone's own name), as a keyframe sequence's poses are. Each step,
//! [`step`] advances every track, fires the track events and writes the
//! blended pose to the joints' `Transform`, then applies the rig's IK
//! controls ([`ik`]).
//!
//! **The pose rule.** A script's write to a joint's `Transform` in a frame
//! wins over the Animator in that frame. Writes made after the Animator's step
//! (in `Stepped` or `Heartbeat`) win by coming later. For writes made earlier
//! in the frame (render-step bindings), [`begin_frame`] notes each driven
//! joint's value at the start of the frame and [`step`] leaves alone a joint
//! whose value changed since.
//!
//! Each VM animates its own world: tracks a client loads move the client's
//! copy of the rig.

pub mod clips;
pub mod ik;
pub mod pose;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use mlua::{Lua, MultiValue, Result, Value as LuaValue};
use re_anim::{KeyframeClip, Mixer, Priority, Track, TrackEvent, TrackId};
use re_reflect::{RecordId, Value};
use re_scene::ecs as bevy_ecs;
use re_scene::{Entity, World};

use crate::class::ClassRegistry;
use crate::codes;
use crate::datatypes::CFrame;
use crate::error::CompatError;
use crate::input::{get_own, set_own};
use crate::instance::methods_api::{args, ret};
use crate::instance::{arg_instance, push, push_list, tree};
use crate::runtime::{Bindings, compat_error, lua_error, state, with_world};
use crate::scene::spawn_instance;
use crate::signal;

/// An Animator's playing state: its tracks in load order and the value each
/// driven joint had at the start of the frame (or after the Animator's last
/// write).
#[derive(bevy_ecs::component::Component, Default)]
pub struct AnimatorState {
    mixer: Mixer,
    tracks: Vec<(TrackId, Entity)>,
    seen: HashMap<Entity, Value>,
    /// The joints this Animator posed last frame (tracks or IK): what it left
    /// in `Transform`, and what to put back when nothing poses the joint
    /// again (identity for a track, the pre-IK value for IK alone).
    owned: BTreeMap<Entity, (Value, Value)>,
}

/// Links an `AnimationTrack` instance to its Animator's mixer.
///
/// The track's properties are own properties, copied from the mixer after
/// every step and method call ([`publish`]). A script's write to `Looped`,
/// `Priority` or `TimePosition` is noticed against the last copy and passed
/// to the mixer before the next step or method call ([`pull`]).
#[derive(bevy_ecs::component::Component, Clone, Copy)]
pub struct TrackLink {
    animator: Entity,
    id: TrackId,
    synced: Synced,
}

/// The script-writable track properties as last copied out.
#[derive(Clone, Copy, PartialEq)]
struct Synced {
    looped: bool,
    priority: Priority,
    time: f64,
}

const TRACK: &str = "AnimationTrack";

/// The `CFrame` record of a CFrame.
pub fn cframe_value(c: &CFrame) -> Value {
    let id = RecordId::by_name("CFrame").expect("the CFrame record is registered");
    let fields = id
        .schema()
        .fields
        .iter()
        .map(|f| {
            Value::Vec3(glam::Vec3::from(match f.name.as_str() {
                "Position" => c.p,
                "XVector" => c.x_vector(),
                "YVector" => c.y_vector(),
                _ => c.z_vector(),
            }))
        })
        .collect();
    Value::Record(id, fields)
}

/// The CFrame of a `CFrame` record. A record whose rotation is all zeros (an
/// unset property) reads as unrotated.
pub fn value_cframe(v: &Value) -> Option<CFrame> {
    let Value::Record(id, values) = v else {
        return None;
    };
    if id.name() != "CFrame" {
        return None;
    }
    let mut parts = [[0.0f32; 3]; 4];
    for (f, v) in id.schema().fields.iter().zip(values) {
        let Value::Vec3(v) = v else { return None };
        let i = match f.name.as_str() {
            "Position" => 0,
            "XVector" => 1,
            "YVector" => 2,
            "ZVector" => 3,
            _ => continue,
        };
        parts[i] = v.to_array();
    }
    let [p, x, y, z] = parts;
    if [x, y, z].iter().flatten().all(|c| *c == 0.0) {
        // An unset CFrame property stores zeros: it reads as no rotation.
        return Some(CFrame::from_position(p));
    }
    Some(CFrame::from_parts(
        p,
        [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]],
    ))
}

/// The CFrame of a position and rotation.
pub fn cframe_of(t: glam::Vec3, r: glam::Quat) -> CFrame {
    CFrame::from_quaternion(t.to_array(), [r.x, r.y, r.z, r.w])
}

/// The substitute table of animation ids for this VM (`[compat.animations]`
/// in `real.toml`): id or id number → `builtin:<clip>` or the dotted path of
/// a `KeyframeSequence`.
#[derive(Clone, Debug, Default)]
pub struct Substitutes(pub BTreeMap<String, String>);

/// Sets the animation substitute table of a VM.
pub fn set_substitutes(lua: &Lua, table: BTreeMap<String, String>) {
    lua.set_app_data(Substitutes(table));
}

/// The rig an Animator drives: the parent of its Humanoid or
/// AnimationController.
pub fn rig_of(w: &World, animator: Entity) -> Option<Entity> {
    let holder = tree::parent(w, animator)?;
    tree::parent(w, holder)
}

/// The joints of a rig a track's poses name: its `Motor6D`s by the name of
/// their `Part1`, then the `Bone`s of its skinned `MeshPart`s by their own
/// name (a `Motor6D` wins a shared name). Both are posed through
/// `Transform`.
pub fn joints(w: &World, classes: &ClassRegistry, rig: Entity) -> BTreeMap<String, Entity> {
    let mut out = BTreeMap::new();
    for j in pose::joints(w, classes, rig) {
        out.entry(tree::name(w, j.part1)).or_insert(j.motor);
    }
    for b in tree::descendants(w, rig) {
        if tree::is_a(w, classes, b, "Bone") && !tree::is_destroyed(w, b) {
            out.entry(tree::name(w, b)).or_insert(b);
        }
    }
    out
}

/// Notes the `Transform` of every joint an Animator drives, at the start of a
/// frame, so [`step`] can tell which ones a script wrote since.
pub fn begin_frame(lua: &Lua) -> Result<()> {
    let classes = state(lua)?.setup.classes.clone();
    with_world(lua, |w| {
        let animators: Vec<Entity> = w
            .ecs
            .query_filtered::<Entity, bevy_ecs::query::With<AnimatorState>>()
            .iter(&w.ecs)
            .collect();
        for a in animators {
            let Some(rig) = rig_of(w, a) else { continue };
            let seen: HashMap<Entity, Value> = joints(w, &classes, rig)
                .into_values()
                .filter_map(|m| Some((m, classes.get_property(w, m, "Transform").ok()?)))
                .collect();
            if let Some(mut st) = w.ecs.get_mut::<AnimatorState>(a) {
                st.seen = seen;
            }
        }
    })
}

/// Advances every Animator by `dt` seconds: track events fire, the blended
/// poses are written to the joints, then the IK controls apply, and rigs
/// under an `AnimationController` move their parts to the new pose.
pub fn step(lua: &Lua, dt: f64) -> Result<()> {
    let classes = state(lua)?.setup.classes.clone();
    let mut animators: Vec<Entity> = with_world(lua, |w| {
        w.ecs
            .query_filtered::<Entity, bevy_ecs::query::With<AnimatorState>>()
            .iter(&w.ecs)
            .collect()
    })?;
    animators.sort();
    for a in &animators {
        step_one(lua, &classes, *a, dt as f32)?;
    }
    ik::apply_all(lua, &classes)?;
    // A Humanoid's rig is posed after physics moves its root part; a rig
    // under an AnimationController is posed here.
    with_world(lua, |w| {
        for a in animators {
            let controlled = tree::parent(w, a)
                .is_some_and(|h| tree::is_a(w, &classes, h, "AnimationController"));
            if controlled
                && !tree::is_destroyed(w, a)
                && let Some(rig) = rig_of(w, a)
            {
                pose::apply(w, &classes, rig);
            }
        }
    })?;
    signal::events::after_write(lua)
}

fn step_one(lua: &Lua, classes: &Arc<ClassRegistry>, a: Entity, dt: f32) -> Result<()> {
    let (events, poses) = with_world(lua, |w| {
        if !tree::alive(w, a) {
            return (Vec::new(), BTreeMap::new());
        }
        let Some(st) = w.ecs.get::<AnimatorState>(a) else {
            return (Vec::new(), BTreeMap::new());
        };
        // Tracks whose instance was destroyed leave the mixer.
        let gone: Vec<TrackId> = st
            .tracks
            .iter()
            .filter(|(_, inst)| tree::is_destroyed(w, *inst))
            .map(|(id, _)| *id)
            .collect();
        let ids: Vec<(TrackId, Entity)> = {
            let mut st = w.ecs.get_mut::<AnimatorState>(a).expect("checked above");
            for id in &gone {
                st.mixer.remove(*id);
            }
            st.tracks.retain(|(id, _)| !gone.contains(id));
            st.tracks.clone()
        };
        let mut events = Vec::new();
        for (_, inst) in &ids {
            pull(w, *inst);
        }
        let mut st = w.ecs.get_mut::<AnimatorState>(a).expect("checked above");
        for (id, inst) in &ids {
            if let Some(t) = st.mixer.get_mut(*id)
                && !t.is_finished()
            {
                for e in t.advance(dt) {
                    events.push((*inst, e));
                }
            }
        }
        let mut poses = BTreeMap::new();
        st.mixer.evaluate(&mut poses);
        for (_, inst) in &ids {
            publish(w, *inst);
        }
        (events, poses)
    })?;
    with_world(lua, |w| write_pose(w, classes, a, &poses))?;
    for (inst, e) in events {
        fire_event(lua, inst, e)?;
    }
    Ok(())
}

/// Writes `v` to a joint's `Transform` and returns what the field stored
/// (rounding included).
fn set_transform(w: &mut World, classes: &ClassRegistry, motor: Entity, v: Value) -> Option<Value> {
    classes.set_property(w, motor, "Transform", v).ok()?;
    classes.get_property(w, motor, "Transform").ok()
}

/// Notes that the Animator (its IK) changed a joint's `Transform` from
/// `before` to `after`, so the next frame starts from `before` again.
pub(crate) fn note_ik(w: &mut World, animator: Entity, motor: Entity, before: Value, after: Value) {
    if w.ecs.get::<AnimatorState>(animator).is_none() {
        w.ecs.entity_mut(animator).insert(AnimatorState::default());
    }
    if let Some(mut st) = w.ecs.get_mut::<AnimatorState>(animator) {
        let restore = st.owned.get(&motor).map_or(before, |(_, r)| r.clone());
        st.owned.insert(motor, (after.clone(), restore));
        st.seen.insert(motor, after);
    }
}

/// Puts back the joints this Animator posed last frame, where nothing wrote
/// them since, so a stopped track or a removed IK control leaves no pose.
fn restore_owned(w: &mut World, classes: &ClassRegistry, animator: Entity) {
    let owned = match w.ecs.get_mut::<AnimatorState>(animator) {
        Some(mut st) => std::mem::take(&mut st.owned),
        None => return,
    };
    let mut written = Vec::new();
    for (motor, (left, restore)) in owned {
        if !tree::alive(w, motor) {
            continue;
        }
        let current = classes.get_property(w, motor, "Transform").ok();
        if current.as_ref() == Some(&left)
            && let Some(stored) = set_transform(w, classes, motor, restore)
        {
            written.push((motor, stored));
        }
    }
    if let Some(mut st) = w.ecs.get_mut::<AnimatorState>(animator) {
        for (m, v) in written {
            st.seen.insert(m, v);
        }
    }
}

/// Writes blended poses to the rig's joints, skipping joints a script wrote
/// this frame. Joints posed last frame and not now go back to rest first.
fn write_pose(
    w: &mut World,
    classes: &ClassRegistry,
    animator: Entity,
    poses: &BTreeMap<String, (glam::Vec3, glam::Quat)>,
) {
    restore_owned(w, classes, animator);
    let Some(rig) = rig_of(w, animator) else {
        return;
    };
    let joints = joints(w, classes, rig);
    let mut written = Vec::new();
    let mut posed = Vec::new();
    for (name, (t, r)) in poses {
        let Some(&motor) = joints.get(name) else {
            continue;
        };
        let current = classes.get_property(w, motor, "Transform").ok();
        let seen = w
            .ecs
            .get::<AnimatorState>(animator)
            .and_then(|s| s.seen.get(&motor).cloned());
        if let (Some(cur), Some(seen)) = (&current, &seen)
            && cur != seen
        {
            // A script wrote it this frame: its value stands, and becomes the
            // reference for the rest of the frame.
            written.push((motor, cur.clone()));
            continue;
        }
        let v = cframe_value(&cframe_of(*t, *r));
        if let Some(stored) = set_transform(w, classes, motor, v) {
            written.push((motor, stored.clone()));
            posed.push((motor, stored));
        }
    }
    let rest = cframe_value(&CFrame::default());
    if let Some(mut st) = w.ecs.get_mut::<AnimatorState>(animator) {
        for (m, v) in written {
            st.seen.insert(m, v);
        }
        for (m, v) in posed {
            st.owned.insert(m, (v, rest.clone()));
        }
    }
}

fn fire_event(lua: &Lua, track: Entity, e: TrackEvent) -> Result<()> {
    match e {
        TrackEvent::KeyframeReached(name) => {
            let s = LuaValue::String(lua.create_string(&name)?);
            signal::fire(lua, track, "KeyframeReached", MultiValue::from_vec(vec![s]))
        }
        TrackEvent::MarkerReached { name, value } => {
            let v = LuaValue::String(lua.create_string(&value)?);
            signal::fire(
                lua,
                track,
                &format!("MarkerReached:{name}"),
                MultiValue::from_vec(vec![v]),
            )
        }
        TrackEvent::DidLoop => signal::fire(lua, track, "DidLoop", MultiValue::new()),
        TrackEvent::Ended => signal::fire(lua, track, "Ended", MultiValue::new()),
        TrackEvent::Stopped => signal::fire(lua, track, "Stopped", MultiValue::new()),
    }
}

/// The clip an `Animation` instance names.
fn load_clip(lua: &Lua, classes: &ClassRegistry, animation: Entity) -> Result<Arc<KeyframeClip>> {
    let game = state(lua)?.game;
    let subs = lua
        .app_data_ref::<Substitutes>()
        .map(|s| s.0.clone())
        .unwrap_or_default();
    let id = with_world(lua, |w| {
        match classes.get_property(w, animation, "AnimationId") {
            Ok(Value::Str(s)) => s,
            _ => String::new(),
        }
    })?;
    let warn = |why: String| {
        re_script::vm::output(
            lua,
            true,
            format!(
                "[{}] animation {id:?} {why}; it plays as an empty clip",
                codes::UNKNOWN_ANIMATION
            ),
        );
        clips::empty()
    };
    Ok(match clips::resolve(&id, &subs) {
        clips::Source::Builtin(name) => match clips::builtin(&name) {
            Some(c) => c,
            None => warn(format!("names the unknown built-in clip {name:?}")),
        },
        clips::Source::Path(path) => {
            let found = with_world(lua, |w| {
                let e = clips::find_path(w, game, &path)?;
                (tree::class(w, e) == Some("KeyframeSequence"))
                    .then(|| clips::from_sequence(w, classes, e))
            })?;
            match found {
                Some(Ok(c)) => Arc::new(c),
                Some(Err(e)) => warn(format!("is substituted by a bad KeyframeSequence ({e})")),
                None => warn(format!(
                    "is substituted by {path:?}, which is not a KeyframeSequence"
                )),
            }
        }
        clips::Source::Unknown => {
            warn("is not in the substitute table or the built-in clips".into())
        }
    })
}

pub(crate) fn load_animation(lua: &Lua, animator: Entity, a: MultiValue) -> Result<MultiValue> {
    let v: LuaValue = args(lua, a, "LoadAnimation")?;
    let animation = arg_instance(&v, "LoadAnimation")?;
    let classes = state(lua)?.setup.classes.clone();
    if !with_world(lua, |w| tree::is_a(w, &classes, animation, "Animation"))? {
        return Err(lua_error(
            re_script::codes::BAD_ARGUMENT,
            "bad argument to LoadAnimation: an Animation expected",
        ));
    }
    let clip = load_clip(lua, &classes, animation)?;
    let track = with_world(lua, |w| -> std::result::Result<Entity, CompatError> {
        let t = spawn_instance(w, &classes, TRACK, None, TRACK, None)?;
        let name = tree::name(w, animation);
        tree::set_name(w, t, &name);
        if w.ecs.get::<AnimatorState>(animator).is_none() {
            w.ecs.entity_mut(animator).insert(AnimatorState::default());
        }
        let track = Track::new(clip);
        let synced = Synced {
            looped: track.looped(),
            priority: track.priority(),
            time: 0.0,
        };
        let mut st = w
            .ecs
            .get_mut::<AnimatorState>(animator)
            .expect("inserted above");
        let id = st.mixer.add(track);
        st.tracks.push((id, t));
        w.ecs.entity_mut(t).insert(TrackLink {
            animator,
            id,
            synced,
        });
        set_own(w, TRACK, t, "Animation", Value::Entity(Some(animation)));
        publish(w, t);
        Ok(t)
    })?
    .map_err(compat_error)?;
    ret(lua, push(lua, track)?)
}

pub(crate) fn playing_tracks(lua: &Lua, animator: Entity, _: MultiValue) -> Result<MultiValue> {
    let list: Vec<Entity> = with_world(lua, |w| {
        let Some(st) = w.ecs.get::<AnimatorState>(animator) else {
            return Vec::new();
        };
        let order: Vec<TrackId> = st.mixer.playing().collect();
        order
            .into_iter()
            .filter_map(|id| st.tracks.iter().find(|(t, _)| *t == id).map(|(_, e)| *e))
            .collect()
    })?;
    ret(lua, push_list(lua, &list)?)
}

fn step_animations(lua: &Lua, animator: Entity, a: MultiValue) -> Result<MultiValue> {
    let dt: f64 = args(lua, a, "StepAnimations")?;
    let classes = state(lua)?.setup.classes.clone();
    step_one(lua, &classes, animator, dt as f32)?;
    signal::events::after_write(lua)?;
    ret(lua, ())
}

fn num(v: Option<Value>) -> Option<f64> {
    match v? {
        Value::Float(x) => Some(x),
        Value::Int(i) => Some(i as f64),
        _ => None,
    }
}

/// Passes a script's writes to the track's properties on to the mixer.
fn pull(w: &mut World, e: Entity) {
    let Some(link) = w.ecs.get::<TrackLink>(e).copied() else {
        return;
    };
    let looped = match get_own(w, TRACK, e, "Looped") {
        Some(Value::Bool(b)) => b,
        _ => link.synced.looped,
    };
    let priority = match get_own(w, TRACK, e, "Priority") {
        Some(Value::Enum(s) | Value::Str(s)) => {
            Priority::from_name(&s).unwrap_or(link.synced.priority)
        }
        _ => link.synced.priority,
    };
    let time = num(get_own(w, TRACK, e, "TimePosition")).unwrap_or(link.synced.time);
    let now = Synced {
        looped,
        priority,
        time,
    };
    if now == link.synced {
        return;
    }
    if let Some(mut st) = w.ecs.get_mut::<AnimatorState>(link.animator)
        && let Some(t) = st.mixer.get_mut(link.id)
    {
        if looped != link.synced.looped {
            t.set_looped(looped);
        }
        if priority != link.synced.priority {
            t.set_priority(priority);
        }
        if time != link.synced.time {
            t.set_time_position(time as f32);
        }
    }
    if let Some(mut l) = w.ecs.get_mut::<TrackLink>(e) {
        l.synced = now;
    }
}

/// Copies the mixer's track state to the track's properties.
fn publish(w: &mut World, e: Entity) {
    let Some(link) = w.ecs.get::<TrackLink>(e).copied() else {
        return;
    };
    let Some(snap) = w
        .ecs
        .get::<AnimatorState>(link.animator)
        .and_then(|st| st.mixer.get(link.id))
        .map(|t| {
            (
                t.is_playing(),
                t.length(),
                t.looped(),
                t.priority(),
                t.speed(),
                t.time_position(),
                t.weight_current(),
                t.weight_target(),
            )
        })
    else {
        return;
    };
    let (playing, length, looped, priority, speed, time, wc, wt) = snap;
    let f = |x: f32| Value::Float(f64::from(x));
    set_own(w, TRACK, e, "IsPlaying", Value::Bool(playing));
    set_own(w, TRACK, e, "Length", f(length));
    set_own(w, TRACK, e, "Looped", Value::Bool(looped));
    set_own(
        w,
        TRACK,
        e,
        "Priority",
        Value::Enum(priority.as_str().to_string()),
    );
    set_own(w, TRACK, e, "Speed", f(speed));
    set_own(w, TRACK, e, "TimePosition", f(time));
    set_own(w, TRACK, e, "WeightCurrent", f(wc));
    set_own(w, TRACK, e, "WeightTarget", f(wt));
    // Read back what the fields stored, so rounding is not taken for a write.
    let time = num(get_own(w, TRACK, e, "TimePosition")).unwrap_or(f64::from(time));
    if let Some(mut l) = w.ecs.get_mut::<TrackLink>(e) {
        l.synced = Synced {
            looped,
            priority,
            time,
        };
    }
}

/// Runs `f` on the track behind an `AnimationTrack` instance, with the
/// script's property writes passed in first and the result copied out after.
fn with_track<R>(w: &mut World, e: Entity, f: impl FnOnce(&mut Track) -> R) -> Option<R> {
    pull(w, e);
    let link = *w.ecs.get::<TrackLink>(e)?;
    let r = {
        let mut st = w.ecs.get_mut::<AnimatorState>(link.animator)?;
        st.mixer.get_mut(link.id).map(f)
    };
    publish(w, e);
    r
}

fn opt_num(v: Option<f64>, default: f32) -> f32 {
    v.map_or(default, |x| x as f32)
}

fn track_play(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (fade, weight, speed): (Option<f64>, Option<f64>, Option<f64>) =
        args(lua, a, "AnimationTrack:Play")?;
    let animator = with_world(lua, |w| {
        with_track(w, e, |t| {
            t.play(
                opt_num(fade, re_anim::DEFAULT_FADE_TIME),
                opt_num(weight, re_anim::DEFAULT_WEIGHT),
                opt_num(speed, re_anim::DEFAULT_SPEED),
            )
        })?;
        w.ecs.get::<TrackLink>(e).map(|l| l.animator)
    })?;
    if let Some(animator) = animator {
        signal::fire(
            lua,
            animator,
            "AnimationPlayed",
            MultiValue::from_vec(vec![push(lua, e)?]),
        )?;
    }
    ret(lua, ())
}

fn track_stop(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let fade: Option<f64> = args(lua, a, "AnimationTrack:Stop")?;
    with_world(lua, |w| {
        with_track(w, e, |t| t.stop(opt_num(fade, re_anim::DEFAULT_FADE_TIME)))
    })?;
    ret(lua, ())
}

fn track_adjust_speed(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let speed: Option<f64> = args(lua, a, "AnimationTrack:AdjustSpeed")?;
    with_world(lua, |w| {
        with_track(w, e, |t| {
            t.adjust_speed(opt_num(speed, re_anim::DEFAULT_SPEED))
        })
    })?;
    ret(lua, ())
}

fn track_adjust_weight(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (weight, fade): (Option<f64>, Option<f64>) = args(lua, a, "AnimationTrack:AdjustWeight")?;
    with_world(lua, |w| {
        with_track(w, e, |t| {
            t.adjust_weight(
                opt_num(weight, re_anim::DEFAULT_WEIGHT),
                opt_num(fade, re_anim::DEFAULT_FADE_TIME),
            )
        })
    })?;
    ret(lua, ())
}

fn track_marker_signal(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "AnimationTrack:GetMarkerReachedSignal")?;
    ret(lua, signal::push(lua, e, &format!("MarkerReached:{name}"))?)
}

fn track_time_of_keyframe(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let name: String = args(lua, a, "AnimationTrack:GetTimeOfKeyframe")?;
    let t = with_world(lua, |w| {
        with_track(w, e, |t| {
            t.clip()
                .keyframes()
                .iter()
                .find(|k| k.name == name)
                .map(|k| k.time)
        })
        .flatten()
    })?;
    match t {
        Some(t) => ret(lua, f64::from(t)),
        None => Err(lua_error(
            re_script::codes::BAD_ARGUMENT,
            format!("the animation has no keyframe named {name:?}"),
        )),
    }
}

type Imp = fn(&Lua, Entity, MultiValue) -> Result<MultiValue>;

/// Registers the Animator and track methods.
pub fn register(b: &mut Bindings) {
    let animator: &[(&str, Imp)] = &[
        ("LoadAnimation", load_animation),
        ("GetPlayingAnimationTracks", playing_tracks),
        ("StepAnimations", step_animations),
    ];
    for (name, f) in animator {
        b.method("Animator", name, Arc::new(*f));
    }
    let track: &[(&str, Imp)] = &[
        ("Play", track_play),
        ("Stop", track_stop),
        ("AdjustSpeed", track_adjust_speed),
        ("AdjustWeight", track_adjust_weight),
        ("GetMarkerReachedSignal", track_marker_signal),
        ("GetTimeOfKeyframe", track_time_of_keyframe),
    ];
    for (name, f) in track {
        b.method(TRACK, name, Arc::new(*f));
    }
}
