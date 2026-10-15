//! `TweenService` and `Tween`: property animation over time (WP15).
//!
//! `TweenService:Create(instance, info, goals)` makes a `Tween` instance
//! (with no parent) that animates the named properties of `instance` from
//! their values when the tween starts to `goals`, along an easing curve
//! ([`easing`]), with the delay, repeats and reversal of the `TweenInfo`.
//! [`step`] advances every playing tween; the frame driver calls it once per
//! frame with the frame's time step, before `Heartbeat`.
//!
//! Semantics, as Roblox-style scripts expect them:
//! * `Play` starts from the current property values (or resumes a paused
//!   tween). Starting a tween cancels any other playing tween of the same
//!   instance that animates one of the same properties.
//! * Each run waits `DelayTime`, then plays for `Time`, then plays back for
//!   another `Time` when `Reverses` is set. `RepeatCount` more runs follow (a
//!   negative count repeats until cancelled).
//! * At the end the properties hold the goals (or the start values when the
//!   tween reverses), `PlaybackState` is `Completed` and `Completed` fires
//!   with it. `Cancel` keeps the current values, sets `Cancelled` and fires
//!   `Completed` with `Cancelled`; the next `Play` starts over.
//! * A tween whose instance is destroyed stops without firing.
//! * A finished tween that no script can reach any more is destroyed.

pub mod easing;
pub mod lerp;

use std::collections::BTreeMap;
use std::sync::Arc;

use mlua::{Lua, MultiValue, Table, Value as LuaValue};
use re_reflect::Value;
use re_scene::{Entity, World};
use re_script::codes::BAD_ARGUMENT;

pub use easing::{EasingDirection, EasingStyle, ease};

use crate::class::{Member, own_component_name};
use crate::datatypes::TweenInfo;
use crate::enums::EnumItem;
use crate::error::CompatError;
use crate::instance::methods_api::{args, ret};
use crate::instance::{self, tree};
use crate::runtime::{Bindings, compat_error, lua_error, state, with_world};
use crate::signal;

/// The registry key of the weak table of tween handles.
const HANDLES: &str = "re_compat.tween.handles";

/// `Enum.PlaybackState`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    /// Created, never played.
    Begin,
    /// Waiting out `DelayTime`.
    Delayed,
    /// Animating.
    Playing,
    /// Stopped by `Pause`; `Play` resumes.
    Paused,
    /// Ran to the end.
    Completed,
    /// Stopped by `Cancel`, or by a newer tween of the same property.
    Cancelled,
}

impl PlaybackState {
    fn name(self) -> &'static str {
        match self {
            PlaybackState::Begin => "Begin",
            PlaybackState::Delayed => "Delayed",
            PlaybackState::Playing => "Playing",
            PlaybackState::Paused => "Paused",
            PlaybackState::Completed => "Completed",
            PlaybackState::Cancelled => "Cancelled",
        }
    }

    fn running(self) -> bool {
        matches!(self, PlaybackState::Delayed | PlaybackState::Playing)
    }
}

/// One tweened property.
#[derive(Clone, Debug)]
struct Track {
    name: String,
    goal: Value,
    start: Option<Value>,
}

/// The timing of a tween, from its `TweenInfo`.
#[derive(Clone, Copy, Debug)]
struct Timing {
    time: f64,
    style: EasingStyle,
    direction: EasingDirection,
    repeats: i32,
    reverses: bool,
    delay: f64,
}

/// One tween.
#[derive(Clone, Debug)]
struct Run {
    target: Entity,
    tracks: Vec<Track>,
    timing: Timing,
    state: PlaybackState,
    /// The state before `Pause`.
    paused_from: PlaybackState,
    /// Seconds into the current run (delay included).
    elapsed: f64,
    /// Runs finished.
    runs: i64,
}

/// What every tween of a VM is doing, keyed by tween entity, in creation
/// order.
#[derive(Default)]
struct Hub {
    runs: BTreeMap<u64, Entity>,
    by_entity: std::collections::HashMap<Entity, Run>,
    /// Steps since the last sweep of unreachable tweens.
    since_sweep: u32,
}

fn hub_mut<R>(lua: &Lua, f: impl FnOnce(&mut Hub) -> R) -> R {
    if lua.app_data_ref::<Hub>().is_none() {
        lua.set_app_data(Hub::default());
    }
    let mut h = lua.app_data_mut::<Hub>().expect("installed above");
    f(&mut h)
}

/// Writes field `field` of `class`'s own component, ignoring read-only (the
/// layer's own bookkeeping, such as `PlaybackState`).
pub(crate) fn set_own(
    w: &mut World,
    e: Entity,
    class: &str,
    field: &str,
    v: Value,
) -> Result<(), CompatError> {
    let key = w.registry.key(&own_component_name(class))?;
    let index = w.registry.schema(key).field_index(field)?;
    w.registry.set_field(&mut w.ecs, e, key, index, v)?;
    Ok(())
}

/// Registers `TweenService` and `TweenBase` methods.
pub fn register(b: &mut Bindings) {
    b.method("TweenService", "Create", Arc::new(create));
    b.method("TweenService", "GetValue", Arc::new(get_value));
    b.method("TweenBase", "Play", Arc::new(play));
    b.method("TweenBase", "Pause", Arc::new(pause));
    b.method("TweenBase", "Cancel", Arc::new(cancel));
}

fn timing(info: &TweenInfo) -> Timing {
    Timing {
        time: if info.time.is_finite() {
            info.time.max(0.0)
        } else {
            0.0
        },
        style: EasingStyle::from_name(info.easing_style.name()).unwrap_or(EasingStyle::Linear),
        direction: EasingDirection::from_name(info.easing_direction.name())
            .unwrap_or(EasingDirection::Out),
        repeats: info.repeat_count,
        reverses: info.reverses,
        delay: if info.delay_time.is_finite() {
            info.delay_time.max(0.0)
        } else {
            0.0
        },
    }
}

fn create(lua: &Lua, _svc: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (target, info, goals): (LuaValue, LuaValue, Table) = args(lua, a, "TweenService:Create")?;
    let target = instance::arg_instance(&target, "TweenService:Create")?;
    let info = match &info {
        LuaValue::UserData(ud) => ud.borrow::<TweenInfo>().ok().map(|i| i.clone()),
        _ => None,
    }
    .ok_or_else(|| lua_error(BAD_ARGUMENT, "TweenService:Create expects a TweenInfo"))?;
    let st = state(lua)?;
    let classes = &st.setup.classes;
    let class =
        with_world(lua, |w| tree::class(w, target).map(str::to_string))?.ok_or_else(|| {
            lua_error(
                BAD_ARGUMENT,
                "TweenService:Create: the instance was destroyed",
            )
        })?;
    let mut tracks = Vec::new();
    for pair in goals.pairs::<LuaValue, LuaValue>() {
        let (k, v) = pair?;
        let LuaValue::String(k) = k else {
            return Err(lua_error(
                BAD_ARGUMENT,
                "TweenService:Create: goal keys must be property names",
            ));
        };
        let name = k.to_str()?.to_string();
        let m = classes.member(&class, &name).map_err(compat_error)?;
        let Member::Property(p) = m.member else {
            return Err(lua_error(
                BAD_ARGUMENT,
                format!("TweenService:Create: {name} is not a property of {class}"),
            ));
        };
        m.require_implemented().map_err(compat_error)?;
        if p.read_only {
            return Err(compat_error(CompatError::ReadOnly {
                class: class.clone(),
                property: name,
            }));
        }
        let goal = st.setup.codec.from_lua(lua, v, &p.ty).map_err(|msg| {
            lua_error(
                BAD_ARGUMENT,
                format!("TweenService:Create: bad goal for {class}.{name}: {msg}"),
            )
        })?;
        // The property's type decides: its current value must be tweenable,
        // and the goal must have the same shape.
        let current =
            with_world(lua, |w| classes.get_property(w, target, &name))?.map_err(compat_error)?;
        if !lerp::tweenable(&current) {
            return Err(lua_error(
                crate::codes::NOT_TWEENABLE,
                format!(
                    "{class}.{name} ({}) cannot be tweened: only numbers, booleans, Vector3, \
                     Vector2, CFrame, Color3, UDim, UDim2, Rect and NumberRange can",
                    p.ty
                ),
            ));
        }
        if lerp::lerp(&current, &goal, 0.0).is_none() {
            return Err(lua_error(
                BAD_ARGUMENT,
                format!(
                    "TweenService:Create: bad goal for {class}.{name}: expected {}",
                    p.ty
                ),
            ));
        }
        tracks.push(Track {
            name,
            goal,
            start: None,
        });
    }
    tracks.sort_by(|a, b| a.name.cmp(&b.name));
    let tween = with_world(lua, |w| {
        let e = crate::scene::spawn_instance(w, classes, "Tween", None, "Tween", None)?;
        set_own(w, e, "Tween", "Instance", Value::Entity(Some(target)))?;
        Ok::<_, CompatError>(e)
    })?
    .map_err(compat_error)?;
    let run = Run {
        target,
        tracks,
        timing: timing(&info),
        state: PlaybackState::Begin,
        paused_from: PlaybackState::Playing,
        elapsed: 0.0,
        runs: 0,
    };
    hub_mut(lua, |h| {
        let order = h.runs.last_key_value().map_or(0, |(k, _)| k + 1);
        h.runs.insert(order, tween);
        h.by_entity.insert(tween, run);
    });
    let handle = instance::handle(lua, tween)?;
    weak_handles(lua)?.raw_set(tween.to_bits() as i64, handle.clone())?;
    signal::events::after_write(lua)?;
    ret(lua, handle)
}

fn get_value(lua: &Lua, _svc: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (alpha, style, direction): (f64, LuaValue, LuaValue) =
        args(lua, a, "TweenService:GetValue")?;
    let style = EnumItem::coerce(&style, "EasingStyle")?;
    let direction = EnumItem::coerce(&direction, "EasingDirection")?;
    let v = ease(
        EasingStyle::from_name(style.name()).unwrap_or(EasingStyle::Linear),
        EasingDirection::from_name(direction.name()).unwrap_or(EasingDirection::Out),
        alpha,
    );
    ret(lua, v)
}

fn weak_handles(lua: &Lua) -> mlua::Result<Table> {
    if let Ok(t) = lua.named_registry_value::<Table>(HANDLES) {
        return Ok(t);
    }
    let t = lua.create_table()?;
    let mt = lua.create_table()?;
    mt.raw_set("__mode", "v")?;
    t.set_metatable(Some(mt))?;
    lua.set_named_registry_value(HANDLES, t.clone())?;
    Ok(t)
}

fn run_of(lua: &Lua, tween: Entity) -> mlua::Result<Run> {
    lua.app_data_ref::<Hub>()
        .and_then(|h| h.by_entity.get(&tween).cloned())
        .ok_or_else(|| {
            lua_error(
                BAD_ARGUMENT,
                "this Tween was not made by TweenService:Create in this VM",
            )
        })
}

fn put(lua: &Lua, tween: Entity, run: Run) {
    hub_mut(lua, |h| {
        h.by_entity.insert(tween, run);
    });
}

/// Writes `PlaybackState` and fires `Completed` when the state ends the
/// tween.
fn set_state(lua: &Lua, tween: Entity, s: PlaybackState) -> mlua::Result<()> {
    with_world(lua, |w| {
        if tree::alive(w, tween) {
            set_own(
                w,
                tween,
                "TweenBase",
                "PlaybackState",
                Value::Enum(s.name().into()),
            )
        } else {
            Ok(())
        }
    })?
    .map_err(compat_error)?;
    if matches!(s, PlaybackState::Completed | PlaybackState::Cancelled) {
        let item = EnumItem::named("PlaybackState", s.name()).to_lua(lua)?;
        signal::fire(lua, tween, "Completed", MultiValue::from_vec(vec![item]))?;
    }
    Ok(())
}

fn play(lua: &Lua, tween: Entity, _a: MultiValue) -> mlua::Result<MultiValue> {
    let mut run = run_of(lua, tween)?;
    match run.state {
        PlaybackState::Delayed | PlaybackState::Playing => return ret(lua, ()),
        PlaybackState::Paused => {
            run.state = run.paused_from;
            let s = run.state;
            put(lua, tween, run);
            set_state(lua, tween, s)?;
        }
        PlaybackState::Begin | PlaybackState::Completed | PlaybackState::Cancelled => {
            // Newer tweens win: cancel the other running tweens of the same
            // instance that share a property with this one.
            let names: Vec<String> = run.tracks.iter().map(|t| t.name.clone()).collect();
            let rivals: Vec<Entity> = lua
                .app_data_ref::<Hub>()
                .map(|h| {
                    h.runs
                        .values()
                        .filter(|&&e| e != tween)
                        .filter(|e| {
                            h.by_entity.get(e).is_some_and(|r| {
                                r.target == run.target
                                    && (r.state.running() || r.state == PlaybackState::Paused)
                                    && r.tracks.iter().any(|t| names.contains(&t.name))
                            })
                        })
                        .copied()
                        .collect()
                })
                .unwrap_or_default();
            for r in rivals {
                stop(lua, r, PlaybackState::Cancelled)?;
            }
            let st = state(lua)?;
            let starts = with_world(lua, |w| {
                run.tracks
                    .iter()
                    .map(|t| st.setup.classes.get_property(w, run.target, &t.name).ok())
                    .collect::<Vec<_>>()
            })?;
            for (t, s) in run.tracks.iter_mut().zip(starts) {
                t.start = s;
            }
            run.elapsed = 0.0;
            run.runs = 0;
            run.state = if run.timing.delay > 0.0 {
                PlaybackState::Delayed
            } else {
                PlaybackState::Playing
            };
            let s = run.state;
            put(lua, tween, run);
            set_state(lua, tween, s)?;
            // A zero-length tween finishes at once; the rest move on the
            // next step.
            advance(lua, tween, 0.0)?;
        }
    }
    signal::events::after_write(lua)?;
    ret(lua, ())
}

fn pause(lua: &Lua, tween: Entity, _a: MultiValue) -> mlua::Result<MultiValue> {
    let mut run = run_of(lua, tween)?;
    if run.state.running() {
        run.paused_from = run.state;
        run.state = PlaybackState::Paused;
        put(lua, tween, run);
        set_state(lua, tween, PlaybackState::Paused)?;
        signal::events::after_write(lua)?;
    }
    ret(lua, ())
}

/// Ends a tween where it is, with `Cancelled`.
fn stop(lua: &Lua, tween: Entity, s: PlaybackState) -> mlua::Result<()> {
    let mut run = run_of(lua, tween)?;
    if matches!(
        run.state,
        PlaybackState::Begin | PlaybackState::Completed | PlaybackState::Cancelled
    ) {
        return Ok(());
    }
    run.state = s;
    run.elapsed = 0.0;
    run.runs = 0;
    put(lua, tween, run);
    set_state(lua, tween, s)
}

fn cancel(lua: &Lua, tween: Entity, _a: MultiValue) -> mlua::Result<MultiValue> {
    run_of(lua, tween)?;
    stop(lua, tween, PlaybackState::Cancelled)?;
    signal::events::after_write(lua)?;
    ret(lua, ())
}

/// The eased progress of a run `elapsed` seconds in (delay included), and
/// whether the run is over. Reversing runs go 0 → 1 → 0.
fn progress(t: &Timing, elapsed: f64) -> (Option<f64>, bool) {
    let active = elapsed - t.delay;
    if active < 0.0 {
        return (None, false);
    }
    let span = if t.reverses { 2.0 * t.time } else { t.time };
    if active >= span {
        let end = if t.reverses { 0.0 } else { 1.0 };
        return (Some(end), true);
    }
    let linear = if active <= t.time {
        active / t.time
    } else {
        (span - active) / t.time
    };
    (Some(ease(t.style, t.direction, linear)), false)
}

/// Advances one tween by `dt` and writes its properties.
fn advance(lua: &Lua, tween: Entity, dt: f64) -> mlua::Result<()> {
    let Ok(mut run) = run_of(lua, tween) else {
        return Ok(());
    };
    if !run.state.running() {
        return Ok(());
    }
    let st = state(lua)?;
    let alive = with_world(lua, |w| tree::alive(w, run.target) && tree::alive(w, tween))?;
    if !alive {
        run.state = PlaybackState::Cancelled;
        put(lua, tween, run);
        return Ok(());
    }
    run.elapsed += dt;
    let mut finished = false;
    let mut alpha = None;
    let mut delayed;
    loop {
        let (a, over) = progress(&run.timing, run.elapsed);
        delayed = a.is_none();
        // A run that ended this frame still writes its end value, even when
        // the next one is in its delay.
        alpha = a.or(alpha);
        if !over {
            break;
        }
        let span = run.timing.delay
            + if run.timing.reverses {
                2.0 * run.timing.time
            } else {
                run.timing.time
            };
        run.runs += 1;
        let more = run.timing.repeats < 0 || run.runs <= i64::from(run.timing.repeats);
        if !more {
            finished = true;
            break;
        }
        if span <= 0.0 {
            // A zero-length repeating tween cannot advance: it ends.
            finished = true;
            break;
        }
        run.elapsed -= span;
    }
    let new_state = if finished {
        PlaybackState::Completed
    } else if delayed {
        PlaybackState::Delayed
    } else {
        PlaybackState::Playing
    };
    if let Some(alpha) = alpha {
        let writes: Vec<(String, Value)> = run
            .tracks
            .iter()
            .filter_map(|t| {
                let start = t.start.as_ref()?;
                let v = if finished {
                    if run.timing.reverses {
                        start.clone()
                    } else {
                        t.goal.clone()
                    }
                } else {
                    lerp::lerp(start, &t.goal, alpha)?
                };
                Some((t.name.clone(), v))
            })
            .collect();
        with_world(lua, |w| {
            for (name, v) in writes {
                // A value that no longer fits (a converter refusing it) is
                // skipped rather than stopping the frame.
                let _ = st.setup.classes.set_property(w, run.target, &name, v);
            }
        })?;
    }
    let old = run.state;
    run.state = new_state;
    if finished {
        run.elapsed = 0.0;
        run.runs = 0;
    }
    put(lua, tween, run);
    if old != new_state {
        set_state(lua, tween, new_state)?;
    }
    Ok(())
}

/// Advances every playing tween by `dt` seconds, in creation order, then
/// fires the change events of what they wrote. Call it once per frame.
pub fn step(lua: &Lua, dt: f64) -> mlua::Result<()> {
    let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
    let order: Vec<Entity> = lua
        .app_data_ref::<Hub>()
        .map(|h| h.runs.values().copied().collect())
        .unwrap_or_default();
    for tween in order {
        advance(lua, tween, dt)?;
    }
    sweep(lua)?;
    signal::events::after_write(lua)
}

/// Destroys tweens that are not running and that no script can reach (their
/// handle was collected), and forgets destroyed ones. Runs every 120 steps.
fn sweep(lua: &Lua) -> mlua::Result<()> {
    let due = hub_mut(lua, |h| {
        h.since_sweep += 1;
        if h.since_sweep >= 120 {
            h.since_sweep = 0;
            true
        } else {
            false
        }
    });
    if !due {
        return Ok(());
    }
    lua.gc_collect()?;
    let handles = weak_handles(lua)?;
    let candidates: Vec<(u64, Entity, bool)> = lua
        .app_data_ref::<Hub>()
        .map(|h| {
            h.runs
                .iter()
                .map(|(&k, &e)| {
                    let idle = h.by_entity.get(&e).is_none_or(|r| !r.state.running());
                    (k, e, idle)
                })
                .collect()
        })
        .unwrap_or_default();
    for (k, e, idle) in candidates {
        let alive = with_world(lua, |w| tree::alive(w, e))?;
        let reachable = !matches!(
            handles.raw_get::<LuaValue>(e.to_bits() as i64)?,
            LuaValue::Nil
        );
        if !alive || (idle && !reachable) {
            hub_mut(lua, |h| {
                h.runs.remove(&k);
                h.by_entity.remove(&e);
            });
            if alive {
                crate::instance::destroy(lua, e)?;
            }
        }
    }
    Ok(())
}

/// The number of tweens a VM tracks (for tests and diagnostics).
pub fn count(lua: &Lua) -> usize {
    lua.app_data_ref::<Hub>().map_or(0, |h| h.runs.len())
}
