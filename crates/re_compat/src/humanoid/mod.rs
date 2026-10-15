//! `Humanoid` characters: walking, jumping, sitting, health and the state
//! machine; the R6 and R15 rigs ([`rig`]) and `HumanoidDescription`
//! ([`description`]).
//!
//! A Humanoid drives the model it sits in. The model's `HumanoidRootPart`
//! rides a character controller ([`body`]): a capsule as tall as twice the
//! root part's height above the floor, so the rig stands on the floor. The
//! limbs follow through their `Motor6D` joints ([`crate::anim::pose`]).
//!
//! Each frame the host calls, around its physics step:
//! - [`pre_simulation`]: health and death, the walk direction
//!   (`Move`, `MoveTo`), jumps, and the move request to the controller;
//! - [`post_simulation`]: the root part follows the controller, the limbs are
//!   posed, and the state machine moves on (Running, Jumping, Freefall,
//!   Landed and the rest), firing the state events.
//!
//! A jump leaves the ground at `JumpPower` studs/s, or, when `UseJumpPower`
//! is false, at `sqrt(2 g JumpHeight)` so it rises `JumpHeight` studs under
//! gravity `g`.

pub mod body;
pub mod description;
pub mod rig;
pub mod scripts;

use std::collections::BTreeSet;
use std::sync::Arc;

use glam::{Quat, Vec3};
use mlua::{Lua, MultiValue, Result, Value as LuaValue};
use re_reflect::Value;
use re_scene::ecs as bevy_ecs;
use re_scene::{Entity, World};

use crate::anim::cframe_value;
use crate::anim::pose::{self, cframe_prop};
use crate::class::{ClassMarker, ClassRegistry};
use crate::datatypes::CFrame;
use crate::enums::EnumItem;
use crate::input::{get_own, set_own};
use crate::instance::methods_api::{args, ret};
use crate::instance::{arg_instance, push, tree};
use crate::runtime::{Bindings, compat_error, lua_error, state, with_world};
use crate::scene::spawn_instance;
use crate::signal;

const HUM: &str = "Humanoid";

/// How long `MoveTo` walks before giving up, in seconds.
pub const MOVE_TO_TIMEOUT: f32 = 8.0;

/// How close to a `MoveTo` goal counts as arrived, in studs (horizontally).
pub const MOVE_TO_REACH: f32 = 1.0;

/// `Workspace.Gravity`'s default, used when no physics world says otherwise.
pub const DEFAULT_GRAVITY: f32 = 196.2;

/// A Humanoid's running state.
#[derive(bevy_ecs::component::Component, Debug)]
pub struct HumanoidRt {
    state: &'static str,
    state_time: f32,
    disabled: BTreeSet<&'static str>,
    /// The root part that has a character controller.
    root: Option<Entity>,
    /// The root part's frame as last written, to notice a script moving it.
    last_root: Option<CFrame>,
    move_dir: Vec3,
    move_to: Option<f32>,
    last_health: f64,
    last_speed: Option<f32>,
    had_neck: bool,
    rebuild: bool,
    jump_now: bool,
    on_ground: bool,
}

impl HumanoidRt {
    fn new(health: f64, had_neck: bool) -> HumanoidRt {
        HumanoidRt {
            state: "Running",
            state_time: 0.0,
            disabled: BTreeSet::new(),
            root: None,
            last_root: None,
            move_dir: Vec3::ZERO,
            move_to: None,
            last_health: health,
            last_speed: None,
            had_neck,
            rebuild: false,
            jump_now: false,
            on_ground: true,
        }
    }

    /// The current state's name (an `Enum.HumanoidStateType` item).
    pub fn state(&self) -> &'static str {
        self.state
    }
}

/// Something that happened to a Humanoid, fired as an event once the world
/// is no longer borrowed.
#[derive(Debug)]
enum Ev {
    State(&'static str, &'static str),
    Flag(&'static str, bool),
    Seated(bool),
    Running(f32),
    Health(f64),
    Died,
    MoveToFinished(bool),
}

fn num(v: Option<Value>) -> Option<f64> {
    match v? {
        Value::Float(x) => Some(x),
        Value::Int(i) => Some(i as f64),
        _ => None,
    }
}

fn get_num(w: &World, e: Entity, p: &str) -> f64 {
    num(get_own(w, HUM, e, p)).unwrap_or(0.0)
}

fn get_bool(w: &World, e: Entity, p: &str) -> bool {
    matches!(get_own(w, HUM, e, p), Some(Value::Bool(true)))
}

fn get_vec(w: &World, e: Entity, p: &str) -> Vec3 {
    match get_own(w, HUM, e, p) {
        Some(Value::Vec3(v)) => v,
        _ => Vec3::ZERO,
    }
}

fn flat(v: Vec3) -> Vec3 {
    Vec3::new(v.x, 0.0, v.z)
}

/// The model's root part, if it has one.
pub fn root_part(w: &World, classes: &ClassRegistry, humanoid: Entity) -> Option<Entity> {
    let rig = tree::parent(w, humanoid)?;
    let r = tree::find_child(w, rig, "HumanoidRootPart", false)?;
    tree::is_a(w, classes, r, "BasePart").then_some(r)
}

fn has_neck(w: &World, classes: &ClassRegistry, rig: Entity) -> bool {
    tree::descendants(w, rig).into_iter().any(|d| {
        tree::name(w, d) == "Neck"
            && tree::is_a(w, classes, d, "Motor6D")
            && !tree::is_destroyed(w, d)
    })
}

/// The Humanoid's running state, made on first use.
fn rt<'w>(
    w: &'w mut World,
    classes: &ClassRegistry,
    e: Entity,
) -> bevy_ecs::world::Mut<'w, HumanoidRt> {
    if w.ecs.get::<HumanoidRt>(e).is_none() {
        let health = get_num(w, e, "Health");
        let neck = tree::parent(w, e).is_some_and(|rig| has_neck(w, classes, rig));
        w.ecs.entity_mut(e).insert(HumanoidRt::new(health, neck));
    }
    w.ecs.get_mut::<HumanoidRt>(e).expect("inserted above")
}

/// Moves to `new` unless it is disabled or current; records the events.
fn set_state(rt: &mut HumanoidRt, new: &'static str, evs: &mut Vec<Ev>) -> bool {
    let old = rt.state;
    if old == new || rt.disabled.contains(new) || old == "Dead" {
        return false;
    }
    rt.state = new;
    rt.state_time = 0.0;
    evs.push(Ev::State(old, new));
    for (s, on) in [(old, false), (new, true)] {
        match s {
            "Jumping" => evs.push(Ev::Flag("Jumping", on)),
            "Freefall" => evs.push(Ev::Flag("FreeFalling", on)),
            "Seated" => evs.push(Ev::Seated(on)),
            "PlatformStanding" => evs.push(Ev::Flag("PlatformStanding", on)),
            "FallingDown" => evs.push(Ev::Flag("FallingDown", on)),
            "GettingUp" => evs.push(Ev::Flag("GettingUp", on)),
            "Ragdoll" => evs.push(Ev::Flag("Ragdoll", on)),
            _ => {}
        }
    }
    if new == "Running" {
        rt.last_speed = None;
    }
    true
}

/// Health bookkeeping: clamps to `MaxHealth`, notices changes and death.
fn health(w: &mut World, classes: &ClassRegistry, e: Entity, evs: &mut Vec<Ev>) {
    let max = get_num(w, e, "MaxHealth");
    let h0 = get_num(w, e, "Health");
    let mut h = h0.clamp(0.0, max.max(0.0));
    if h != h0 {
        set_own(w, HUM, e, "Health", Value::Float(h));
    }
    // The neck counts once the rig has had one, whenever it was added.
    let neck_now = tree::parent(w, e).is_some_and(|rig| has_neck(w, classes, rig));
    let neck_lost = {
        let mut rt = rt(w, classes, e);
        rt.had_neck |= neck_now;
        rt.had_neck
    } && get_bool(w, e, "RequiresNeck")
        && !neck_now;
    if neck_lost && h > 0.0 {
        h = 0.0;
        set_own(w, HUM, e, "Health", Value::Float(0.0));
    }
    let mut rt = rt(w, classes, e);
    if h != rt.last_health {
        rt.last_health = h;
        evs.push(Ev::Health(h));
    }
    if h <= 0.0 && rt.state != "Dead" {
        let old = rt.state;
        rt.state = "Dead";
        rt.state_time = 0.0;
        evs.push(Ev::State(old, "Dead"));
        evs.push(Ev::Died);
    }
}

/// The height of the root part's centre above the rig's lowest point.
fn root_height(w: &World, classes: &ClassRegistry, humanoid: Entity, root: Entity) -> f32 {
    let half = match classes.get_property(w, root, "Size") {
        Ok(Value::Vec3(s)) => s.y / 2.0,
        _ => 1.0,
    };
    let hip = get_num(w, humanoid, "HipHeight") as f32;
    if hip > 0.0 {
        return hip + half;
    }
    let Some(rig) = tree::parent(w, humanoid) else {
        return half;
    };
    let frames = pose::solve(w, classes, rig);
    let Some(root_cf) = frames.get(&root) else {
        return half;
    };
    let mut lowest = root_cf.p[1] - half;
    for (part, cf) in &frames {
        if let Ok(Value::Vec3(s)) = classes.get_property(w, *part, "Size") {
            // The lowest corner of the part's box.
            let ext: f32 = [cf.x_vector(), cf.y_vector(), cf.z_vector()]
                .iter()
                .zip([s.x, s.y, s.z])
                .map(|(axis, len)| axis[1].abs() * len / 2.0)
                .sum();
            lowest = lowest.min(cf.p[1] - ext);
        }
    }
    root_cf.p[1] - lowest
}

/// A yaw-only rotation facing `dir` (the front is -Z).
fn facing(dir: Vec3) -> Quat {
    Quat::from_rotation_y(f32::atan2(-dir.x, -dir.z))
}

fn quat(c: &CFrame) -> Quat {
    let [x, y, z, w] = c.quaternion();
    Quat::from_xyzw(x, y, z, w).normalize()
}

fn gravity(w: &World) -> f32 {
    body::gravity(w).unwrap_or(DEFAULT_GRAVITY)
}

fn move_goal(w: &World, classes: &ClassRegistry, e: Entity) -> Vec3 {
    let point = get_vec(w, e, "WalkToPoint");
    match get_own(w, HUM, e, "WalkToPart") {
        Some(Value::Entity(Some(part))) if !tree::is_destroyed(w, part) => {
            match cframe_prop(w, classes, part, "CFrame") {
                Some(cf) => Vec3::from_array(cf.point_to_world(point.to_array())),
                None => point,
            }
        }
        _ => point,
    }
}

const STILL: &[&str] = &[
    "Seated",
    "PlatformStanding",
    "Ragdoll",
    "Physics",
    "FallingDown",
    "GettingUp",
    "Dead",
];

fn pre_one(w: &mut World, classes: &ClassRegistry, e: Entity, dt: f32, evs: &mut Vec<Ev>) {
    health(w, classes, e, evs);
    let found = root_part(w, classes, e);
    if get_own(w, HUM, e, "RootPart") != Some(Value::Entity(found)) {
        set_own(w, HUM, e, "RootPart", Value::Entity(found));
    }
    let Some(root) = found else {
        return;
    };
    let dead = rt(w, classes, e).state == "Dead";
    if !body::available(w) {
        return;
    }
    // The controller: made for a new root part, rebuilt after a rescale.
    let (has, rebuild) = {
        let rt = rt(w, classes, e);
        (rt.root, rt.rebuild)
    };
    if dead {
        if let Some(r) = has {
            body::remove(w, r);
            rt(w, classes, e).root = None;
        }
        return;
    }
    let root_cf = cframe_prop(w, classes, root, "CFrame").unwrap_or_default();
    if has != Some(root) || rebuild {
        if let Some(old) = has {
            body::remove(w, old);
        }
        let h = root_height(w, classes, e, root).max(0.5);
        let width = match classes.get_property(w, root, "Size") {
            Ok(Value::Vec3(s)) => s.x.max(s.z) / 2.0,
            _ => 1.0,
        };
        let radius = width.clamp(0.25, h);
        let rot = facing(-Vec3::from_array(root_cf.look_vector()));
        let added = body::add(w, root, 2.0 * h, radius, Vec3::from_array(root_cf.p), rot);
        let mut rt = rt(w, classes, e);
        rt.root = added.then_some(root);
        rt.rebuild = false;
        rt.last_root = Some(root_cf);
    } else if rt(w, classes, e)
        .last_root
        .is_some_and(|last| !last.fuzzy_eq(&root_cf, 1e-4))
    {
        // A script moved the root part: the controller jumps there.
        body::teleport(w, root, Vec3::from_array(root_cf.p), quat(&root_cf));
        rt(w, classes, e).last_root = Some(root_cf);
    }

    // Sitting and platform standing by property.
    let sit = get_bool(w, e, "Sit");
    let stand = get_bool(w, e, "PlatformStand");
    {
        let mut rt = rt(w, classes, e);
        let rt = &mut *rt;
        if sit && rt.state != "Seated" {
            set_state(rt, "Seated", evs);
        } else if !sit && rt.state == "Seated" {
            set_state(rt, "Running", evs);
        }
        if stand && rt.state != "PlatformStanding" {
            set_state(rt, "PlatformStanding", evs);
        } else if !stand && rt.state == "PlatformStanding" {
            set_state(rt, "Running", evs);
        }
    }

    // The walk direction.
    let (state, move_dir, moving_to) = {
        let rt = rt(w, classes, e);
        (rt.state, rt.move_dir, rt.move_to.is_some())
    };
    let dir = if STILL.contains(&state) {
        Vec3::ZERO
    } else if moving_to {
        flat(move_goal(w, classes, e) - Vec3::from_array(root_cf.p)).normalize_or_zero()
    } else {
        let d = flat(move_dir);
        if d.length() > 1.0 { d.normalize() } else { d }
    };
    set_own(w, HUM, e, "MoveDirection", Value::Vec3(dir));
    let speed = get_num(w, e, "WalkSpeed").max(0.0) as f32;
    body::walk(w, root, dir * speed);

    // Jumping.
    let wants = get_bool(w, e, "Jump") || rt(w, classes, e).jump_now;
    if wants {
        let can = {
            let mut rt = rt(w, classes, e);
            let can = rt.on_ground
                && !rt.disabled.contains("Jumping")
                && matches!(
                    rt.state,
                    "Running" | "Landed" | "Seated" | "RunningNoPhysics"
                );
            if can {
                set_state(&mut rt, "Jumping", evs);
                rt.jump_now = false;
                rt.on_ground = false;
            }
            can
        };
        if can {
            let g = gravity(w);
            let v = if get_bool(w, e, "UseJumpPower") {
                get_num(w, e, "JumpPower") as f32
            } else {
                (2.0 * g * get_num(w, e, "JumpHeight") as f32)
                    .max(0.0)
                    .sqrt()
            };
            // The controller takes a step of gravity off before each move, so
            // half a step more at launch makes the peak the continuous one,
            // v² / 2g.
            let v = if v > 0.0 { v + g * dt / 2.0 } else { 0.0 };
            set_own(w, HUM, e, "Jump", Value::Bool(false));
            if sit {
                set_own(w, HUM, e, "Sit", Value::Bool(false));
            }
            body::set_velocity(w, root, Vec3::Y * v);
        }
    }
}

fn post_one(w: &mut World, classes: &ClassRegistry, e: Entity, dt: f32, evs: &mut Vec<Ev>) {
    let Some(root) = root_part(w, classes, e) else {
        return;
    };
    let Some(rig) = tree::parent(w, e) else {
        return;
    };
    if rt(w, classes, e).root != Some(root) {
        return;
    }
    let Some(st) = body::state(w, root) else {
        return;
    };
    // The root part follows the controller, turned to face the walk.
    let old = cframe_prop(w, classes, root, "CFrame").unwrap_or_default();
    let dir = get_vec(w, e, "MoveDirection");
    let rot = if get_bool(w, e, "AutoRotate") && dir.length_squared() > 1e-6 {
        facing(dir)
    } else {
        quat(&old)
    };
    let cf = CFrame::from_quaternion(st.pos.to_array(), [rot.x, rot.y, rot.z, rot.w]);
    let _ = classes.set_property(w, root, "CFrame", cframe_value(&cf));
    let stored = cframe_prop(w, classes, root, "CFrame").unwrap_or(cf);
    pose::apply(w, classes, rig);

    let on_ground = st.on_ground();
    let floor = if on_ground {
        st.ground_entity
            .and_then(|id| {
                w.ecs
                    .get_entity(Entity::from_bits(id.0))
                    .ok()
                    .map(|x| x.id())
            })
            .and_then(|g| match classes.get_property(w, g, "Material") {
                Ok(Value::Enum(m)) => Some(m),
                _ => None,
            })
            .unwrap_or_else(|| "Plastic".to_string())
    } else {
        "Air".to_string()
    };
    set_own(w, HUM, e, "FloorMaterial", Value::Enum(floor));

    let evaluate = get_bool(w, e, "EvaluateStateMachine");
    let goal = move_goal(w, classes, e);
    let mut rt = rt(w, classes, e);
    let rt = &mut *rt;
    rt.last_root = Some(stored);
    rt.on_ground = on_ground;
    rt.state_time += dt;
    let vy = st.velocity.y;
    if evaluate {
        let next = match rt.state {
            "Running" | "RunningNoPhysics" if !on_ground => Some("Freefall"),
            "Jumping" if vy <= 0.0 => Some(if on_ground { "Landed" } else { "Freefall" }),
            "Freefall" if on_ground => Some("Landed"),
            "Landed" => Some(if on_ground { "Running" } else { "Freefall" }),
            "FallingDown" if rt.state_time >= 1.0 => Some("GettingUp"),
            "GettingUp" if rt.state_time >= 0.5 => Some("Running"),
            _ => None,
        };
        if let Some(n) = next {
            set_state(rt, n, evs);
        }
    }
    if rt.state == "Running" {
        let speed = flat(st.velocity).length();
        let speed = if speed < 0.01 { 0.0 } else { speed };
        if rt.last_speed.is_none_or(|s| (s - speed).abs() > 0.01) {
            rt.last_speed = Some(speed);
            evs.push(Ev::Running(speed));
        }
    }
    if let Some(t) = rt.move_to.as_mut() {
        *t += dt;
        let left = flat(goal - st.pos).length();
        if left <= MOVE_TO_REACH {
            rt.move_to = None;
            evs.push(Ev::MoveToFinished(true));
        } else if *t >= MOVE_TO_TIMEOUT {
            rt.move_to = None;
            evs.push(Ev::MoveToFinished(false));
        }
    }
}

fn state_item(lua: &Lua, name: &str) -> Result<LuaValue> {
    EnumItem::named("HumanoidStateType", name).to_lua(lua)
}

fn fire(lua: &Lua, e: Entity, evs: Vec<Ev>) -> Result<()> {
    for ev in evs {
        match ev {
            Ev::State(old, new) => signal::fire(
                lua,
                e,
                "StateChanged",
                MultiValue::from_vec(vec![state_item(lua, old)?, state_item(lua, new)?]),
            )?,
            Ev::Flag(name, on) => signal::fire(
                lua,
                e,
                name,
                MultiValue::from_vec(vec![LuaValue::Boolean(on)]),
            )?,
            Ev::Seated(on) => {
                let seat = with_world(lua, |w| match get_own(w, HUM, e, "SeatPart") {
                    Some(Value::Entity(s)) => s,
                    _ => None,
                })?;
                let seat = match seat {
                    Some(s) if on => push(lua, s)?,
                    _ => LuaValue::Nil,
                };
                signal::fire(
                    lua,
                    e,
                    "Seated",
                    MultiValue::from_vec(vec![LuaValue::Boolean(on), seat]),
                )?
            }
            Ev::Running(speed) => signal::fire(
                lua,
                e,
                "Running",
                MultiValue::from_vec(vec![LuaValue::Number(f64::from(speed))]),
            )?,
            Ev::Health(h) => signal::fire(
                lua,
                e,
                "HealthChanged",
                MultiValue::from_vec(vec![LuaValue::Number(h)]),
            )?,
            Ev::Died => {
                die(lua, e)?;
                signal::fire(lua, e, "Died", MultiValue::new())?;
            }
            Ev::MoveToFinished(ok) => signal::fire(
                lua,
                e,
                "MoveToFinished",
                MultiValue::from_vec(vec![LuaValue::Boolean(ok)]),
            )?,
        }
    }
    Ok(())
}

/// What dying does besides the event: the controller goes, and with
/// `BreakJointsOnDeath` the rig's joints are destroyed.
fn die(lua: &Lua, e: Entity) -> Result<()> {
    let classes = state(lua)?.setup.classes.clone();
    let joints = with_world(lua, |w| {
        if let Some(r) = w.ecs.get::<HumanoidRt>(e).and_then(|rt| rt.root) {
            body::remove(w, r);
        }
        if let Some(mut rt) = w.ecs.get_mut::<HumanoidRt>(e) {
            rt.root = None;
            rt.move_to = None;
        }
        set_own(w, HUM, e, "MoveDirection", Value::Vec3(Vec3::ZERO));
        if !get_bool(w, e, "BreakJointsOnDeath") {
            return Vec::new();
        }
        let Some(rig) = tree::parent(w, e) else {
            return Vec::new();
        };
        tree::descendants(w, rig)
            .into_iter()
            .filter(|d| tree::is_a(w, &classes, *d, "JointInstance"))
            .collect()
    })?;
    for j in joints {
        crate::instance::destroy(lua, j)?;
    }
    Ok(())
}

/// The live Humanoids in the place.
fn humanoids(lua: &Lua) -> Result<Vec<Entity>> {
    let st = state(lua)?;
    let (classes, game) = (st.setup.classes.clone(), st.game);
    with_world(lua, |w| {
        let mut out: Vec<Entity> = w
            .ecs
            .query::<(Entity, &ClassMarker)>()
            .iter(&w.ecs)
            .filter(|(_, m)| classes.is_a(&m.class, HUM))
            .map(|(e, _)| e)
            .collect();
        out.retain(|e| !tree::is_destroyed(w, *e) && tree::in_game(w, game, *e));
        out.sort();
        out
    })
}

/// Before the physics step: health and death, the walk direction, jumps and
/// the move requests.
pub fn pre_simulation(lua: &Lua, dt: f64) -> Result<()> {
    let classes = state(lua)?.setup.classes.clone();
    for e in humanoids(lua)? {
        let mut evs = Vec::new();
        with_world(lua, |w| pre_one(w, &classes, e, dt as f32, &mut evs))?;
        fire(lua, e, evs)?;
    }
    signal::events::after_write(lua)
}

/// After the physics step: the root parts follow their controllers, the
/// limbs are posed and the state machine moves on.
pub fn post_simulation(lua: &Lua, dt: f64) -> Result<()> {
    let classes = state(lua)?.setup.classes.clone();
    for e in humanoids(lua)? {
        let mut evs = Vec::new();
        with_world(lua, |w| post_one(w, &classes, e, dt as f32, &mut evs))?;
        fire(lua, e, evs)?;
    }
    signal::events::after_write(lua)
}

/// Runs health bookkeeping for one Humanoid now (after `TakeDamage`).
fn check_health(lua: &Lua, e: Entity) -> Result<()> {
    let classes = state(lua)?.setup.classes.clone();
    let mut evs = Vec::new();
    with_world(lua, |w| health(w, &classes, e, &mut evs))?;
    fire(lua, e, evs)?;
    signal::events::after_write(lua)
}

fn vec_arg(v: &LuaValue, what: &str) -> Result<Vec3> {
    match v {
        LuaValue::Vector(x) => Ok(Vec3::new(x.x(), x.y(), x.z())),
        _ => Err(lua_error(
            re_script::codes::BAD_ARGUMENT,
            format!("bad argument to {what}: a Vector3 expected"),
        )),
    }
}

fn h_move(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (v, relative): (LuaValue, Option<bool>) = args(lua, a, "Humanoid:Move")?;
    let mut dir = vec_arg(&v, "Humanoid:Move")?;
    let classes = state(lua)?.setup.classes.clone();
    if relative == Some(true) {
        // Turned by the current camera's yaw, when there is one.
        let cam = with_world(lua, |w| {
            let ws = crate::services::core::find_service(w, state_game(lua)?, "Workspace")?;
            match classes.get_property(w, ws, "CurrentCamera") {
                Ok(Value::Entity(Some(c))) => cframe_prop(w, &classes, c, "CFrame"),
                _ => None,
            }
        })?;
        if let Some(cf) = cam {
            let look = flat(Vec3::from_array(cf.look_vector())).normalize_or_zero();
            if look != Vec3::ZERO {
                dir = facing(look) * dir;
            }
        }
    }
    with_world(lua, |w| {
        let mut rt = rt(w, &classes, e);
        rt.move_dir = flat(dir);
        if flat(dir) != Vec3::ZERO {
            rt.move_to = None;
        }
    })?;
    ret(lua, ())
}

fn state_game(lua: &Lua) -> Option<Entity> {
    state(lua).ok().map(|s| s.game)
}

fn h_move_to(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (v, part): (LuaValue, LuaValue) = args(lua, a, "Humanoid:MoveTo")?;
    let point = vec_arg(&v, "Humanoid:MoveTo")?;
    let part = match &part {
        LuaValue::Nil => None,
        p => Some(arg_instance(p, "Humanoid:MoveTo")?),
    };
    let classes = state(lua)?.setup.classes.clone();
    with_world(lua, |w| {
        set_own(w, HUM, e, "WalkToPoint", Value::Vec3(point));
        set_own(w, HUM, e, "WalkToPart", Value::Entity(part));
        let mut rt = rt(w, &classes, e);
        rt.move_to = Some(0.0);
    })?;
    signal::events::after_write(lua)?;
    ret(lua, ())
}

fn h_take_damage(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let amount: f64 = args(lua, a, "Humanoid:TakeDamage")?;
    let classes = state(lua)?.setup.classes.clone();
    with_world(lua, |w| {
        let shielded = tree::parent(w, e).is_some_and(|rig| {
            tree::children(w, rig)
                .into_iter()
                .any(|c| tree::is_a(w, &classes, c, "ForceField"))
        });
        if !shielded {
            let max = get_num(w, e, "MaxHealth").max(0.0);
            let h = (get_num(w, e, "Health") - amount).clamp(0.0, max);
            set_own(w, HUM, e, "Health", Value::Float(h));
        }
    })?;
    check_health(lua, e)?;
    ret(lua, ())
}

fn h_get_state(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let classes = state(lua)?.setup.classes.clone();
    let s = with_world(lua, |w| rt(w, &classes, e).state)?;
    ret(lua, state_item(lua, s)?)
}

fn state_arg(v: &LuaValue) -> Result<&'static str> {
    Ok(EnumItem::coerce(v, "HumanoidStateType")?.name())
}

fn h_change_state(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let v: LuaValue = args(lua, a, "Humanoid:ChangeState")?;
    let s = state_arg(&v)?;
    let classes = state(lua)?.setup.classes.clone();
    if s == "Dead" {
        with_world(lua, |w| set_own(w, HUM, e, "Health", Value::Float(0.0)))?;
        check_health(lua, e)?;
        return ret(lua, ());
    }
    let mut evs = Vec::new();
    with_world(lua, |w| {
        let mut rt = rt(w, &classes, e);
        if s == "Jumping" {
            // A jump starts from the ground; in the air it is ignored.
            if !rt.disabled.contains("Jumping") && rt.on_ground {
                rt.jump_now = true;
            }
        } else {
            set_state(&mut rt, s, &mut evs);
        }
    })?;
    fire(lua, e, evs)?;
    ret(lua, ())
}

fn h_set_state_enabled(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let (v, on): (LuaValue, bool) = args(lua, a, "Humanoid:SetStateEnabled")?;
    let s = state_arg(&v)?;
    let classes = state(lua)?.setup.classes.clone();
    let changed = with_world(lua, |w| {
        let mut rt = rt(w, &classes, e);
        if on {
            rt.disabled.remove(s)
        } else {
            rt.disabled.insert(s)
        }
    })?;
    if changed {
        signal::fire(
            lua,
            e,
            "StateEnabledChanged",
            MultiValue::from_vec(vec![state_item(lua, s)?, LuaValue::Boolean(on)]),
        )?;
    }
    ret(lua, ())
}

fn h_get_state_enabled(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let v: LuaValue = args(lua, a, "Humanoid:GetStateEnabled")?;
    let s = state_arg(&v)?;
    let classes = state(lua)?.setup.classes.clone();
    let on = with_world(lua, |w| !rt(w, &classes, e).disabled.contains(s))?;
    ret(lua, on)
}

/// The Humanoid's Animator, made if it has none.
fn animator(lua: &Lua, e: Entity) -> Result<Entity> {
    let classes = state(lua)?.setup.classes.clone();
    with_world(
        lua,
        |w| -> std::result::Result<Entity, crate::error::CompatError> {
            if let Some(a) = tree::children(w, e)
                .into_iter()
                .find(|c| tree::is_a(w, &classes, *c, "Animator"))
            {
                return Ok(a);
            }
            spawn_instance(w, &classes, "Animator", None, "Animator", Some(e))
        },
    )?
    .map_err(compat_error)
}

fn h_load_animation(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let an = animator(lua, e)?;
    crate::anim::load_animation(lua, an, a)
}

fn h_playing_tracks(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let an = animator(lua, e)?;
    crate::anim::playing_tracks(lua, an, a)
}

fn h_apply_description(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let v: LuaValue = args(lua, a, "Humanoid:ApplyDescription")?;
    let d = arg_instance(&v, "Humanoid:ApplyDescription")?;
    let classes = state(lua)?.setup.classes.clone();
    with_world(
        lua,
        |w| -> std::result::Result<(), crate::error::CompatError> {
            if !tree::is_a(w, &classes, d, "HumanoidDescription") {
                return Err(crate::error::CompatError::InvalidData(
                    "bad argument to Humanoid:ApplyDescription: a HumanoidDescription expected"
                        .into(),
                ));
            }
            if description::apply(w, &classes, e, d)? {
                rt(w, &classes, e).rebuild = true;
            }
            Ok(())
        },
    )?
    .map_err(compat_error)?;
    signal::events::after_write(lua)?;
    ret(lua, ())
}

fn h_get_applied_description(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let classes = state(lua)?.setup.classes.clone();
    let d = with_world(
        lua,
        |w| -> std::result::Result<Entity, crate::error::CompatError> {
            match description::applied(w, &classes, e) {
                Some(d) => description::copy(w, &classes, d),
                None => spawn_instance(
                    w,
                    &classes,
                    "HumanoidDescription",
                    None,
                    "HumanoidDescription",
                    None,
                ),
            }
        },
    )?
    .map_err(compat_error)?;
    ret(lua, push(lua, d)?)
}

const R6_LIMBS: &[(&str, &str)] = &[
    ("Head", "Head"),
    ("Torso", "Torso"),
    ("Left Arm", "LeftArm"),
    ("Right Arm", "RightArm"),
    ("Left Leg", "LeftLeg"),
    ("Right Leg", "RightLeg"),
];

fn part_name_arg(lua: &Lua, e: Entity, a: MultiValue, what: &str) -> Result<String> {
    let v: LuaValue = args(lua, a, what)?;
    let p = arg_instance(&v, what)?;
    with_world(lua, |w| {
        if tree::parent(w, p) == tree::parent(w, e) {
            tree::name(w, p)
        } else {
            String::new()
        }
    })
}

fn h_get_limb(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let name = part_name_arg(lua, e, a, "Humanoid:GetLimb")?;
    let limb = R6_LIMBS
        .iter()
        .find(|(n, _)| *n == name)
        .map_or("Unknown", |(_, l)| *l);
    ret(lua, EnumItem::named("Limb", limb).to_lua(lua)?)
}

fn h_get_body_part_r15(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let name = part_name_arg(lua, e, a, "Humanoid:GetBodyPartR15")?;
    let item = match name.as_str() {
        "HumanoidRootPart" => "RootPart",
        n if EnumItem::lookup("BodyPartR15", n).is_some() && n != "Unknown" => n,
        _ => "Unknown",
    };
    ret(lua, EnumItem::named("BodyPartR15", item).to_lua(lua)?)
}

type Imp = fn(&Lua, Entity, MultiValue) -> Result<MultiValue>;

/// Registers the Humanoid's methods.
pub fn register(b: &mut Bindings) {
    let methods: &[(&str, Imp)] = &[
        ("Move", h_move),
        ("MoveTo", h_move_to),
        ("TakeDamage", h_take_damage),
        ("GetState", h_get_state),
        ("ChangeState", h_change_state),
        ("SetStateEnabled", h_set_state_enabled),
        ("GetStateEnabled", h_get_state_enabled),
        ("LoadAnimation", h_load_animation),
        ("GetPlayingAnimationTracks", h_playing_tracks),
        ("ApplyDescription", h_apply_description),
        ("GetAppliedDescription", h_get_applied_description),
        ("GetLimb", h_get_limb),
        ("GetBodyPartR15", h_get_body_part_r15),
    ];
    for (name, f) in methods {
        b.method(HUM, name, Arc::new(*f));
    }
}

/// The state of a Humanoid, for hosts and tests.
pub fn current_state(w: &World, e: Entity) -> Option<&'static str> {
    w.ecs.get::<HumanoidRt>(e).map(|rt| rt.state)
}
