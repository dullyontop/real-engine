//! The libraries other than `world`: `input`, `physics`, `audio`, `anim`, `ui`, `time`,
//! `task`, `quat`, `color`, `proc`, `stream`, `command` (tools only), and the `print`,
//! `warn` and `script` globals.

use mlua::{Function, IntoLua, Lua, MultiValue, Result, Table, Value};
use re_command::Origin;
use re_core::Code;
use re_scene::{Entity, entity_from_id, entity_id};
use re_types::audio::{AudioCmd, EffectParam};
use re_types::host::HostError;
use re_types::physics::{GroundState, GroupMask, RagdollMode, VehicleInput};

use super::entity::{EntityRef, handle};
use super::math::{LQuat, V3};
use super::{Lib, json};
use crate::codes;
use crate::host::with_host;
use crate::scheduler::{self, Signal};
use crate::vm::ContextKind;

/// The declaration of `Connection`.
pub(crate) const CONNECTION_DECL: &str = "-- A connected callback.
declare extern type Connection with
    -- False once disconnected.
    read connected: boolean
    -- Stops further calls.
    function disconnect(self): ()
end";

/// Globals that are not library tables.
pub(crate) const GLOBAL_DECLS: &str = "-- Writes a warning to the log and the console.
declare function warn(...: any): ()

-- The running script: its project-relative path, its context, and the entity
-- whose ScriptRef started it (nil for modules and scripts started directly).
declare script: {
    path: string,
    context: string,
    entity: Entity?,
}";

/// Every library of the context `kind`, in a fixed order, finished (strict) with
/// its declaration.
pub(crate) fn all(lua: &Lua, kind: ContextKind) -> Result<Vec<(&'static str, Table, String)>> {
    type Builder = fn(&Lua, &mut Lib) -> Result<()>;
    let mut libs: Vec<(&'static str, &'static str, Builder)> = vec![
        (
            "world",
            "Entities, components and the scene.",
            super::world::lib,
        ),
        ("quat", "Rotations.", super::math::quat_lib),
        ("color", "Linear RGBA colours.", super::math::color_lib),
        ("input", "Input actions from real.toml [input].", input_lib),
        (
            "physics",
            "Ray casts, impulses, characters and contacts.",
            physics_lib,
        ),
        ("audio", "Sound playback.", audio_lib),
        (
            "anim",
            "Animation graphs: parameters, states and joints.",
            super::anim::lib,
        ),
        ("ui", "The heads-up display.", ui_lib),
        (
            "time",
            "Simulation time and per-frame and per-step callbacks.",
            time_lib,
        ),
        (
            "task",
            "Script threads: waiting, spawning and deferring.",
            scheduler::task_lib,
        ),
        (
            "proc",
            "ProcGraph builders and procedural regeneration.",
            super::proc::lib,
        ),
    ];
    // Only the game and tools contexts run the phases its callbacks fire in.
    let stream: Option<Builder> = match kind {
        ContextKind::Tools => Some(|lua, lib| super::stream::lib(lua, lib, true)),
        ContextKind::Game => Some(|lua, lib| super::stream::lib(lua, lib, false)),
        _ => None,
    };
    if let Some(stream) = stream {
        libs.push((
            "stream",
            "World streaming: the loaded cells and their load and unload events.",
            stream,
        ));
    }
    if kind == ContextKind::Tools {
        libs.push((
            "command",
            "Commands (tools context only): the same registry the CLI, editor and agents use.",
            command_lib,
        ));
    }
    let mut out = Vec::with_capacity(libs.len());
    for (name, doc, build) in libs {
        let mut lib = Lib::new(lua, name, doc)?;
        build(lua, &mut lib)?;
        let (table, decl) = lib.finish(lua)?;
        out.push((name, table, decl));
    }
    Ok(out)
}

/// A `HostError` as a Luau error carrying its code.
pub(crate) fn host_err(e: HostError) -> mlua::Error {
    match Code::parse(e.code) {
        Some(code) => super::error(code, e.message),
        None => mlua::Error::runtime(format!("{} ({})", e.message, e.code)),
    }
}

pub(crate) fn entity_arg(v: &Value, what: &str) -> Result<Entity> {
    if let Value::UserData(ud) = v
        && let Ok(e) = ud.borrow::<EntityRef>()
    {
        return Ok(e.0);
    }
    Err(super::error(
        codes::BAD_ARGUMENT,
        format!("{what} expects an Entity, got {}", v.type_name()),
    ))
}

fn input_lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "down",
        "(action: string) -> boolean",
        "True while the button action is held.",
        |lua, a: String| with_host(lua, |h| Ok(h.services().actions().is_down(&a))),
    )?;
    lib.func(
        lua,
        "pressed",
        "(action: string) -> boolean",
        "True on the frame the button action went down.",
        |lua, a: String| with_host(lua, |h| Ok(h.services().actions().pressed(&a))),
    )?;
    lib.func(
        lua,
        "released",
        "(action: string) -> boolean",
        "True on the frame the button action went up.",
        |lua, a: String| with_host(lua, |h| Ok(h.services().actions().button(&a).released)),
    )?;
    lib.func(
        lua,
        "axis",
        "(action: string) -> vector",
        "An axis action as vector(x, y, 0); zero when the action is unknown.",
        |lua, a: String| {
            let v = with_host(lua, |h| {
                Ok(h.services()
                    .actions()
                    .axes
                    .get(&a)
                    .copied()
                    .unwrap_or_default())
            })?;
            Ok(V3(glam::Vec3::new(v.x, v.y, 0.0)))
        },
    )?;
    lib.func(
        lua,
        "on_pressed",
        "(action: string, f: () -> ()) -> Connection",
        "Calls `f` (in its own thread) each frame the button action goes down.",
        |lua, (a, f): (String, Function)| scheduler::connect(lua, Signal::Pressed(a), f),
    )?;
    lib.func(
        lua,
        "on_released",
        "(action: string, f: () -> ()) -> Connection",
        "Calls `f` (in its own thread) each frame the button action goes up.",
        |lua, (a, f): (String, Function)| scheduler::connect(lua, Signal::Released(a), f),
    )?;
    Ok(())
}

fn ground_name(g: GroundState) -> &'static str {
    match g {
        GroundState::OnGround => "on_ground",
        GroundState::OnSteepGround => "on_steep_ground",
        GroundState::NotSupported => "not_supported",
        GroundState::InAir => "in_air",
    }
}

fn physics_lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "raycast",
        "(origin: vector, direction: vector, max_distance: number, mask: number?) -> { entity: Entity?, point: vector, normal: vector, distance: number }?",
        "The closest hit along a ray against bodies whose groups intersect `mask` (default: all), or nil.",
        |lua, (origin, dir, max, mask): (V3, V3, f32, Option<u32>)| {
            let mask = GroupMask(mask.unwrap_or(u32::MAX));
            let hit = with_host(lua, |h| Ok(h.services().raycast(origin.0, dir.0, max, mask)))?;
            let Some(hit) = hit else { return Ok(Value::Nil) };
            let t = lua.create_table()?;
            if let Some(e) = entity_from_id(hit.entity) {
                t.raw_set("entity", handle(lua, e)?)?;
            }
            t.raw_set("point", V3(hit.point))?;
            t.raw_set("normal", V3(hit.normal))?;
            t.raw_set("distance", hit.distance)?;
            Ok(Value::Table(t))
        },
    )?;
    lib.func(
        lua,
        "impulse",
        "(e: Entity, impulse: vector) -> ()",
        "Applies an impulse (N·s) to a dynamic body at its centre of mass, at the next step.",
        |lua, (e, v): (Value, V3)| {
            let e = entity_arg(&e, "physics.impulse")?;
            with_host(lua, |h| {
                h.services()
                    .apply_impulse(entity_id(e), v.0)
                    .map_err(host_err)
            })
        },
    )?;
    lib.func(
        lua,
        "velocity",
        "(e: Entity) -> (vector, vector)",
        "A body's linear and angular velocity after the last step.",
        |lua, e: Value| {
            let e = entity_arg(&e, "physics.velocity")?;
            let (l, a) = with_host(lua, |h| {
                h.services().body_velocity(entity_id(e)).map_err(host_err)
            })?;
            Ok((V3(l), V3(a)))
        },
    )?;
    lib.func(
        lua,
        "move_character",
        "(e: Entity, velocity: vector, jump: boolean?) -> ()",
        "Sets a character's desired velocity for the next step; `jump` jumps when it is on the ground.",
        |lua, (e, v, jump): (Value, V3, Option<bool>)| {
            let e = entity_arg(&e, "physics.move_character")?;
            with_host(lua, |h| {
                h.services()
                    .character_move(entity_id(e), v.0, jump.unwrap_or(false))
                    .map_err(host_err)
            })
        },
    )?;
    lib.func(
        lua,
        "character",
        "(e: Entity) -> { position: vector, rotation: Quat, velocity: vector, on_ground: boolean, ground: string, ground_normal: vector, ground_entity: Entity? }",
        "A character's state after the last step.",
        |lua, e: Value| {
            let e = entity_arg(&e, "physics.character")?;
            let st = with_host(lua, |h| h.services().character_state(entity_id(e)).map_err(host_err))?;
            let t = lua.create_table()?;
            t.raw_set("position", V3(st.pos))?;
            t.raw_set("rotation", LQuat(st.rot))?;
            t.raw_set("velocity", V3(st.velocity))?;
            t.raw_set("on_ground", st.on_ground())?;
            t.raw_set("ground", ground_name(st.ground))?;
            t.raw_set("ground_normal", V3(st.ground_normal))?;
            if let Some(g) = st.ground_entity.and_then(entity_from_id) {
                t.raw_set("ground_entity", handle(lua, g)?)?;
            }
            Ok(t)
        },
    )?;
    lib.func(
        lua,
        "drive",
        "(e: Entity, input: { throttle: number?, steer: number?, brake: number?, handbrake: number? }) -> ()",
        "Sets a vehicle's driver input from the next step until the next call: `throttle` and `steer` -1 to 1 (negative reverses or steers left), `brake` and `handbrake` 0 to 1; missing fields are 0, and a value out of range or not finite is an error.",
        |lua, (e, input): (Value, Table)| {
            let e = entity_arg(&e, "physics.drive")?;
            let field = |name: &str| -> Result<f32> { Ok(input.get::<Option<f32>>(name)?.unwrap_or(0.0)) };
            let input = VehicleInput {
                throttle: field("throttle")?,
                steer: field("steer")?,
                brake: field("brake")?,
                handbrake: field("handbrake")?,
            };
            with_host(lua, |h| {
                h.services()
                    .vehicle_drive(entity_id(e), input)
                    .map_err(host_err)
            })
        },
    )?;
    lib.func(
        lua,
        "vehicle",
        "(e: Entity) -> { speed: number, rpm: number, gear: number, throttle: number, steer: number, brake: number, handbrake: number, wheels: { { on_ground: boolean, contact: Entity?, contact_point: vector, suspension_length: number, angular_velocity: number, steer_deg: number } } }",
        "A vehicle's state after the last step: forward speed (m/s, negative in reverse), engine rpm, gear (negative in reverse, 0 neutral), the input in effect, and each wheel in wheel order.",
        |lua, e: Value| {
            let e = entity_arg(&e, "physics.vehicle")?;
            let st = with_host(lua, |h| h.services().vehicle_state(entity_id(e)).map_err(host_err))?;
            let t = lua.create_table()?;
            t.raw_set("speed", st.speed)?;
            t.raw_set("rpm", st.rpm)?;
            t.raw_set("gear", st.gear)?;
            t.raw_set("throttle", st.input.throttle)?;
            t.raw_set("steer", st.input.steer)?;
            t.raw_set("brake", st.input.brake)?;
            t.raw_set("handbrake", st.input.handbrake)?;
            let wheels = lua.create_table()?;
            for (i, w) in st.wheels.iter().enumerate() {
                let wt = lua.create_table()?;
                wt.raw_set("on_ground", w.on_ground)?;
                if let Some(c) = w.contact.and_then(entity_from_id) {
                    wt.raw_set("contact", handle(lua, c)?)?;
                }
                wt.raw_set("contact_point", V3(w.contact_point))?;
                wt.raw_set("suspension_length", w.suspension_length)?;
                wt.raw_set("angular_velocity", w.angular_velocity)?;
                wt.raw_set("steer_deg", w.steer_deg)?;
                wheels.raw_set(i + 1, wt)?;
            }
            t.raw_set("wheels", wheels)?;
            Ok(t)
        },
    )?;
    lib.func(
        lua,
        "ragdoll",
        "(e: Entity, mode: \"animated\" | \"simulated\" | \"powered\", strength: number?) -> ()",
        "Switches the entity's ragdoll from the next step until the next call or an edit of its `Ragdoll`: `animated` follows the animation, `simulated` goes limp, keeping its motion, and `powered` holds the animation with joint motors at `strength` (0 to 1, default 1). The shown pose blends to the ragdoll and back over the component's `blend_seconds`.",
        |lua, (e, mode, strength): (Value, String, Option<f32>)| {
            let e = entity_arg(&e, "physics.ragdoll")?;
            let mode = match mode.as_str() {
                "animated" => RagdollMode::Animated,
                "simulated" => RagdollMode::Simulated,
                "powered" => RagdollMode::Powered {
                    strength: strength.unwrap_or(1.0),
                },
                other => {
                    return Err(super::error(
                        codes::BAD_ARGUMENT,
                        format!(
                            "physics.ragdoll: mode `{other}` is not animated, simulated or powered"
                        ),
                    ));
                }
            };
            with_host(lua, |h| {
                h.services()
                    .ragdoll_mode(entity_id(e), mode)
                    .map_err(host_err)
            })
        },
    )?;
    lib.func(
        lua,
        "ragdoll_state",
        "(e: Entity) -> { mode: string, strength: number, position: vector, rotation: Quat, root_position: vector, root_rotation: Quat, calm: boolean }",
        "The entity's ragdoll after the last step: its mode and powered strength (0 otherwise), the frame its pose is placed in (the entity's pose when last targeted), the root part's world pose, and `calm`, true once every part has stayed slower than 0.05 m/s and 0.1 rad/s for 30 steps.",
        |lua, e: Value| {
            let e = entity_arg(&e, "physics.ragdoll_state")?;
            let st = with_host(lua, |h| h.services().ragdoll_state(entity_id(e)).map_err(host_err))?;
            let t = lua.create_table()?;
            let (mode, strength) = match st.mode {
                RagdollMode::Animated => ("animated", 0.0),
                RagdollMode::Simulated => ("simulated", 0.0),
                RagdollMode::Powered { strength } => ("powered", strength),
            };
            t.raw_set("mode", mode)?;
            t.raw_set("strength", strength)?;
            t.raw_set("position", V3(st.pos))?;
            t.raw_set("rotation", LQuat(st.rot))?;
            t.raw_set("root_position", V3(st.root_pos))?;
            t.raw_set("root_rotation", LQuat(st.root_rot))?;
            t.raw_set("calm", st.calm)?;
            Ok(t)
        },
    )?;
    lib.func(
        lua,
        "ragdoll_impulse",
        "(e: Entity, joint: string, impulse: vector) -> ()",
        "Applies an impulse (N·s) at the next step to the ragdoll part that follows `joint` (or the joint's nearest ancestor with a part); the ragdoll must be simulated or powered.",
        |lua, (e, joint, v): (Value, String, V3)| {
            let e = entity_arg(&e, "physics.ragdoll_impulse")?;
            with_host(lua, |h| {
                h.services()
                    .ragdoll_impulse(entity_id(e), &joint, v.0)
                    .map_err(host_err)
            })
        },
    )?;
    lib.func(
        lua,
        "on_contact",
        "(f: (a: Entity, b: Entity, info: { point: vector, normal: vector, sensor: boolean }) -> ()) -> Connection",
        "Calls `f` when two bodies start touching (or a body enters a sensor), after each physics step.",
        |lua, f: Function| scheduler::connect(lua, Signal::Contact, f),
    )?;
    lib.func(
        lua,
        "on_contact_end",
        "(f: (a: Entity, b: Entity, sensor: boolean) -> ()) -> Connection",
        "Calls `f` when two bodies stop touching (one may already be gone).",
        |lua, f: Function| scheduler::connect(lua, Signal::ContactEnd, f),
    )?;
    lib.func(
        lua,
        "on_touch",
        "(e: Entity, f: (other: Entity, info: { point: vector, normal: vector, sensor: boolean }) -> ()) -> Connection",
        "Calls `f` when something starts touching `e` (or enters it, for a sensor).",
        |lua, (e, f): (Value, Function)| {
            let e = entity_arg(&e, "physics.on_touch")?;
            scheduler::connect(lua, Signal::Touch(e), f)
        },
    )?;
    Ok(())
}

fn audio_lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "play",
        "(clip: string, at: vector?, volume: number?, bus: string?) -> ()",
        "Plays a sound asset once, at a world position or (without `at`) non-spatially; volume defaults to 1, and `bus` to the project's default bus.",
        |lua, (clip, at, volume, bus): (String, Option<V3>, Option<f32>, Option<String>)| {
            let (at, volume) = (at.map(|v| v.0), volume.unwrap_or(1.0));
            with_host(lua, |h| {
                let s = h.services();
                match bus {
                    None => s.play_sound(&clip, at, volume),
                    Some(bus) => s.audio_mix(AudioCmd::PlayOneShotOnBus {
                        clip,
                        position: at,
                        volume,
                        bus,
                    }),
                }
                .map_err(host_err)
            })
        },
    )?;
    lib.func(
        lua,
        "set_bus_volume",
        "(bus: string, db: number, seconds: number?) -> ()",
        "Tweens a mixer bus's (or send's) volume to `db` decibels (0 is unchanged) over `seconds`, at once by default.",
        |lua, (bus, volume_db, seconds): (String, f32, Option<f32>)| {
            let cmd = AudioCmd::SetBusVolume {
                bus,
                volume_db,
                seconds: seconds.unwrap_or(0.0),
            };
            with_host(lua, |h| h.services().audio_mix(cmd).map_err(host_err))
        },
    )?;
    lib.func(
        lua,
        "set_effect",
        "(bus: string, index: number, params: { [string]: number | string }, seconds: number?) -> ()",
        "Tweens parameters of a bus's effect over `seconds` (at once by default). `index` is the effect's place in the bus's `effects` list in real.toml, 1 for the first; `params` are its fields, such as `{ cutoff_hz = 600 }` or `{ mode = \"high_pass\" }`.",
        |lua, (bus, index, params, seconds): (String, f64, Table, Option<f32>)| {
            if !(index.fract() == 0.0 && (1.0..=f64::from(u32::MAX)).contains(&index)) {
                return Err(super::error(
                    codes::BAD_ARGUMENT,
                    format!("audio.set_effect: index {index} is not a whole number from 1"),
                ));
            }
            let effect = index as u32 - 1;
            let mut values: Vec<(String, EffectParam)> = Vec::new();
            for pair in params.pairs::<String, Value>() {
                let (param, value) = pair?;
                let number = match value {
                    Value::Integer(i) => Some(crate::lua_int(i) as f32),
                    Value::Number(n) => Some(n as f32),
                    Value::String(s) => {
                        values.push((param, EffectParam::Choice(s.to_str()?.to_string())));
                        continue;
                    }
                    other => {
                        return Err(super::error(
                            codes::BAD_ARGUMENT,
                            format!(
                                "audio.set_effect: `{param}` must be a number or a string, not {}",
                                other.type_name()
                            ),
                        ));
                    }
                };
                let n = number.unwrap_or(f32::NAN);
                // Every value is checked before any command is sent.
                if !n.is_finite() {
                    return Err(super::error(
                        codes::BAD_ARGUMENT,
                        format!("audio.set_effect: `{param}` is {n}, which is not finite"),
                    ));
                }
                values.push((param, EffectParam::Number(n)));
            }
            // Table order is not defined; the commands go in name order.
            values.sort_by(|a, b| a.0.cmp(&b.0));
            let seconds = seconds.unwrap_or(0.0);
            with_host(lua, |h| {
                for (param, value) in values {
                    h.services()
                        .audio_mix(AudioCmd::SetEffectParam {
                            bus: bus.clone(),
                            effect,
                            param,
                            value,
                            seconds,
                        })
                        .map_err(host_err)?;
                }
                Ok(())
            })
        },
    )?;
    lib.func(
        lua,
        "snapshot",
        "(name: string, seconds: number?) -> ()",
        "Tweens the mixer to a snapshot from real.toml [audio.snapshots] over `seconds` (at once by default); `\"default\"` returns to the graph's own values.",
        |lua, (name, seconds): (String, Option<f32>)| {
            let cmd = AudioCmd::ApplySnapshot {
                name,
                seconds: seconds.unwrap_or(0.0),
            };
            with_host(lua, |h| h.services().audio_mix(cmd).map_err(host_err))
        },
    )?;
    lib.func(
        lua,
        "levels",
        "(bus: string) -> { peak_db: number, rms_db: number }",
        "A mixer bus's (or send's) output over the last frame, after its effects and volume, in decibels (-60 is silence). It measures live audio, so it is frame-only: calling it during a fixed step (`time.on_step`, `time.on_post_step`, or a thread resumed there) is an error, because the simulation would no longer replay.",
        |lua, bus: String| {
            if scheduler::with(lua, |s| s.in_step)? {
                return Err(super::error(
                    codes::SERVICE_UNAVAILABLE,
                    "audio.levels is not available during a fixed step: it reads live audio, which a replay cannot reproduce; call it from time.on_frame",
                ));
            }
            let level = with_host(lua, |h| h.services().audio_level(&bus).map_err(host_err))?;
            let t = lua.create_table()?;
            t.raw_set("peak_db", level.peak_db)?;
            t.raw_set("rms_db", level.rms_db)?;
            Ok(t)
        },
    )
}

fn ui_lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "set_text",
        "(e: Entity, text: string) -> ()",
        "Sets the text of an entity's HudText component.",
        |lua, (e, text): (Value, String)| {
            let e = entity_arg(&e, "ui.set_text")?;
            with_host(lua, |h| {
                h.services()
                    .set_hud_text(entity_id(e), &text)
                    .map_err(host_err)
            })
        },
    )
}

fn time_lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "now",
        "() -> number",
        "Simulation time in seconds (advances by the fixed step).",
        |lua, ()| scheduler::with(lua, |s| s.now),
    )?;
    lib.func(
        lua,
        "delta",
        "() -> number",
        "The time of the current frame in seconds (variable).",
        |lua, ()| scheduler::with(lua, |s| s.frame_dt),
    )?;
    lib.func(
        lua,
        "frame",
        "() -> number",
        "The number of frames run so far.",
        |lua, ()| scheduler::with(lua, |s| s.frame as f64),
    )?;
    lib.func(
        lua,
        "on_frame",
        "(f: (dt: number) -> ()) -> Connection",
        "Calls `f` once per frame, before simulation, with the frame time.",
        |lua, f: Function| scheduler::connect(lua, Signal::Frame, f),
    )?;
    lib.func(
        lua,
        "on_step",
        "(f: (dt: number) -> ()) -> Connection",
        "Calls `f` on every fixed step, before physics, with the step time.",
        |lua, f: Function| scheduler::connect(lua, Signal::Step, f),
    )?;
    lib.func(
        lua,
        "on_post_step",
        "(f: (dt: number) -> ()) -> Connection",
        "Calls `f` on every fixed step, after physics and contacts, with the step time.",
        |lua, f: Function| scheduler::connect(lua, Signal::PostStep, f),
    )?;
    Ok(())
}

fn command_lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "run",
        "(name: string, args: { [string]: any }?, dry_run: boolean?) -> { ok: boolean, data: any, changes: number, diagnostics: { { code: string, message: string } } }",
        "Runs a registered command as this script (undoable, logged with the script's path).",
        |lua, (name, args, dry_run): (String, Option<Value>, Option<bool>)| {
            let args = match args {
                Some(v) => json::to_json(lua, &v)?,
                None => serde_json::Value::Object(Default::default()),
            };
            let origin = Origin::Script(scheduler::caller_script(lua).unwrap_or_else(|| "tools".into()));
            let outcome = with_host(lua, |h| {
                Ok(h.run_command(&name, args, dry_run.unwrap_or(false), origin))
            })?
            .ok_or_else(|| {
                super::error(
                    codes::COMMANDS_UNAVAILABLE,
                    format!("command.run({name:?}): no command registry is attached to this context"),
                )
            })?;
            let t = lua.create_table()?;
            t.raw_set("ok", outcome.ok)?;
            t.raw_set("data", json::from_json(lua, &outcome.data)?)?;
            t.raw_set("changes", outcome.changes.len())?;
            let diags = lua.create_table()?;
            for d in &outcome.diagnostics {
                let dt = lua.create_table()?;
                dt.raw_set("code", d.code.to_string())?;
                dt.raw_set("message", d.message.as_str())?;
                diags.raw_push(dt)?;
            }
            t.raw_set("diagnostics", diags)?;
            Ok(t)
        },
    )
}

/// Builds `print` or `warn`: each writes its arguments, tab-separated, to the
/// context's output buffer and to the log.
pub(crate) fn output_fn(lua: &Lua, warn: bool) -> Result<Function> {
    lua.create_function(move |lua, args: MultiValue| {
        let mut line = String::new();
        for (i, v) in args.iter().enumerate() {
            if i > 0 {
                line.push('\t');
            }
            match v {
                Value::UserData(_) | Value::Table(_) => {
                    // tostring honours __tostring.
                    // From the shared globals: a sandboxed thread's own
                    // global table is an empty proxy.
                    let globals: mlua::Table = lua.named_registry_value(crate::vm::GLOBALS_KEY)?;
                    let ts: Function = globals.raw_get("tostring")?;
                    line.push_str(&ts.call::<String>(v.clone())?);
                }
                other => line.push_str(&other.to_string()?),
            }
        }
        crate::vm::push_output(lua, warn, line);
        Ok(())
    })
}

/// Builds the per-script `script` table.
pub(crate) fn script_table(
    lua: &Lua,
    path: &str,
    kind: ContextKind,
    entity: Option<Entity>,
) -> Result<Table> {
    let t = lua.create_table()?;
    t.raw_set("path", path)?;
    t.raw_set("context", kind.name())?;
    if let Some(e) = entity {
        t.raw_set("entity", handle(lua, e)?)?;
    }
    t.set_readonly(true);
    Ok(t)
}

/// Fires contact signals for one event.
pub(crate) fn fire_contact(lua: &Lua, ev: &re_types::physics::ContactEvent) -> Result<()> {
    use re_types::physics::ContactEvent;
    let (a, b) = ev.pair();
    let (Some(a), Some(b)) = (entity_from_id(a), entity_from_id(b)) else {
        return Ok(());
    };
    let ha = handle(lua, a)?.into_lua(lua)?;
    let hb = handle(lua, b)?.into_lua(lua)?;
    match *ev {
        ContactEvent::Begin {
            point,
            normal,
            sensor,
            ..
        } => {
            let info = lua.create_table()?;
            info.raw_set("point", V3(point))?;
            info.raw_set("normal", V3(normal))?;
            info.raw_set("sensor", sensor)?;
            let info = Value::Table(info);
            scheduler::fire(
                lua,
                |s| *s == Signal::Contact,
                MultiValue::from_iter([ha.clone(), hb.clone(), info.clone()]),
            )?;
            scheduler::fire(
                lua,
                |s| *s == Signal::Touch(a),
                MultiValue::from_iter([hb, info.clone()]),
            )?;
            scheduler::fire(
                lua,
                |s| *s == Signal::Touch(b),
                MultiValue::from_iter([ha, info]),
            )?;
        }
        ContactEvent::End { sensor, .. } => {
            scheduler::fire(
                lua,
                |s| *s == Signal::ContactEnd,
                MultiValue::from_iter([ha, hb, Value::Boolean(sensor)]),
            )?;
        }
    }
    Ok(())
}
