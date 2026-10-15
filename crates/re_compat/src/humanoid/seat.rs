//! `Seat` and `VehicleSeat`: a character that touches a free seat sits in
//! it, held by a `SeatWeld`.
//!
//! Sitting takes the character controller away ([`body::release`]); while
//! it sits, the Humanoid places the root part on the seat through the weld
//! each frame, so the character rides the seat wherever it goes. Jumping,
//! setting `Sit` to false, destroying the weld or disabling the seat stands
//! it up, and it does not sit again on touch for [`REST`] seconds.
//!
//! An occupied `VehicleSeat` reports its occupant's walk direction as
//! `Throttle` and `Steer` (and their float forms); it does not drive
//! anything itself.

use glam::Vec3;
use mlua::{Lua, MultiValue, Result};
use re_reflect::Value;
use re_scene::{Entity, World};

use super::{HUM, body, get_bool, root_part, rt};
use crate::anim::cframe_value;
use crate::anim::pose::{self, cframe_prop};
use crate::class::{ClassMarker, ClassRegistry};
use crate::datatypes::CFrame;
use crate::input::{get_own, set_own};
use crate::instance::methods_api::{args, ret};
use crate::instance::{arg_instance, tree};
use crate::runtime::{Bindings, lua_error, state, with_world};
use crate::scene::spawn_instance;
use crate::signal;

/// The seat classes.
pub const SEATS: [&str; 2] = ["Seat", "VehicleSeat"];

/// How long after standing up a character does not sit on touch, in
/// seconds.
pub const REST: f32 = 1.0;

/// How close a character's capsule must come to a seat to touch it, in
/// studs.
const REACH: f32 = 0.25;

/// The seat class `e` is (or inherits from), when it is a seat.
fn seat_class(w: &World, classes: &ClassRegistry, e: Entity) -> Option<&'static str> {
    SEATS.into_iter().find(|c| tree::is_a(w, classes, e, c))
}

fn seat_bool(w: &World, classes: &ClassRegistry, seat: Entity, p: &str) -> bool {
    seat_class(w, classes, seat)
        .is_some_and(|c| matches!(get_own(w, c, seat, p), Some(Value::Bool(true))))
}

fn occupant(w: &World, classes: &ClassRegistry, seat: Entity) -> Option<Entity> {
    let c = seat_class(w, classes, seat)?;
    match get_own(w, c, seat, "Occupant") {
        Some(Value::Entity(Some(h))) if tree::alive(w, h) => Some(h),
        _ => None,
    }
}

fn set_seat(w: &mut World, classes: &ClassRegistry, seat: Entity, p: &str, v: Value) {
    if let Some(c) = seat_class(w, classes, seat) {
        set_own(w, c, seat, p, v);
    }
}

fn size(w: &World, classes: &ClassRegistry, e: Entity) -> Vec3 {
    match classes.get_property(w, e, "Size") {
        Ok(Value::Vec3(s)) => s,
        _ => Vec3::ONE,
    }
}

/// What a seat update does for one Humanoid.
enum Step {
    Stay,
    Leave,
    Sit(Entity),
}

/// Seats and unseats one Humanoid before its physics step: it stands up when
/// it jumps, stops sitting or loses its seat or weld, and sits in a free
/// seat it touches. Updates an occupied `VehicleSeat`'s throttle and steer.
pub(super) fn update(lua: &Lua, classes: &ClassRegistry, e: Entity, dt: f32) -> Result<()> {
    let game = state(lua)?.game;
    let step = with_world(lua, |w| decide(w, classes, game, e, dt))?;
    match step {
        Step::Stay => Ok(()),
        Step::Leave => leave(lua, classes, e),
        Step::Sit(seat) => with_world(lua, |w| sit(w, classes, e, seat)).map(|_| ()),
    }
}

fn decide(w: &mut World, classes: &ClassRegistry, game: Entity, e: Entity, dt: f32) -> Step {
    let (seat, dead, rest) = {
        let mut rt = rt(w, classes, e);
        rt.unseated += dt;
        (rt.seat, rt.state == "Dead", rt.unseated)
    };
    if let Some((seat, weld)) = seat {
        let gone = !tree::alive(w, seat)
            || !tree::alive(w, weld)
            || tree::parent(w, weld).is_none()
            || seat_bool(w, classes, seat, "Disabled");
        if dead || gone || !get_bool(w, e, "Sit") || get_bool(w, e, "Jump") {
            return Step::Leave;
        }
        vehicle_input(w, classes, e, seat);
        return Step::Stay;
    }
    if dead || rest < REST || rt(w, classes, e).disabled.contains("Seated") {
        return Step::Stay;
    }
    let Some(root) = root_part(w, classes, e) else {
        return Step::Stay;
    };
    touched_seat(w, classes, game, e, root).map_or(Step::Stay, Step::Sit)
}

/// The first free seat (in instance order) the character's capsule touches.
fn touched_seat(
    w: &mut World,
    classes: &ClassRegistry,
    game: Entity,
    hum: Entity,
    root: Entity,
) -> Option<Entity> {
    let root_cf = cframe_prop(w, classes, root, "CFrame")?;
    let half = super::root_height(w, classes, hum, root).max(0.5);
    let radius = (size(w, classes, root).x.max(size(w, classes, root).z) / 2.0).clamp(0.25, half);
    let centre = Vec3::from_array(root_cf.p);
    let mut seats: Vec<Entity> = w
        .ecs
        .query::<(Entity, &ClassMarker)>()
        .iter(&w.ecs)
        .filter(|(_, m)| SEATS.iter().any(|s| classes.is_a(&m.class, s)))
        .map(|(e, _)| e)
        .collect();
    seats.sort();
    seats.into_iter().find(|&s| {
        tree::in_game(w, game, s)
            && !tree::is_destroyed(w, s)
            && !seat_bool(w, classes, s, "Disabled")
            && occupant(w, classes, s).is_none()
            && cframe_prop(w, classes, s, "CFrame").is_some_and(|cf| {
                // The capsule's axis against the seat's box, both widened
                // by the radius and the reach.
                let half_size = size(w, classes, s) / 2.0 + Vec3::splat(radius + REACH);
                let local = Vec3::from_array(cf.point_to_object(centre.to_array()));
                let up =
                    Vec3::from_array(cf.point_to_object((centre + Vec3::Y).to_array())) - local;
                let half_axis = (half - radius).max(0.0);
                let (a, b) = (local - up * half_axis, local + up * half_axis);
                // The closest point of the axis segment to the box centre.
                let t = if (b - a).length_squared() > 0.0 {
                    ((-a).dot(b - a) / (b - a).length_squared()).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let p = a + (b - a) * t;
                p.abs().cmple(half_size).all()
            })
    })
}

/// Sits Humanoid `e` in `seat` now: the controller goes, a `SeatWeld` joins
/// the seat's top to the root part's bottom, and `Occupant`, `SeatPart` and
/// `Sit` are set. False when the character cannot sit.
fn sit(w: &mut World, classes: &ClassRegistry, e: Entity, seat: Entity) -> bool {
    let Some(root) = root_part(w, classes, e) else {
        return false;
    };
    if occupant(w, classes, seat).is_some() || rt(w, classes, e).state == "Dead" {
        return false;
    }
    let Ok(weld) = spawn_instance(w, classes, "Weld", None, "SeatWeld", Some(seat)) else {
        return false;
    };
    let c0 = CFrame::from_position([0.0, size(w, classes, seat).y / 2.0, 0.0]);
    let c1 = CFrame::from_position([0.0, -size(w, classes, root).y / 2.0, 0.0]);
    let _ = classes.set_property(w, weld, "C0", cframe_value(&c0));
    let _ = classes.set_property(w, weld, "C1", cframe_value(&c1));
    let _ = classes.set_property(w, weld, "Part0", Value::Entity(Some(seat)));
    let _ = classes.set_property(w, weld, "Part1", Value::Entity(Some(root)));
    if rt(w, classes, e).root == Some(root) {
        body::release(w, root);
    }
    set_seat(w, classes, seat, "Occupant", Value::Entity(Some(e)));
    set_own(w, HUM, e, "SeatPart", Value::Entity(Some(seat)));
    set_own(w, HUM, e, "Sit", Value::Bool(true));
    {
        let mut rt = rt(w, classes, e);
        rt.root = None;
        rt.seat = Some((seat, weld));
        rt.move_to = None;
    }
    place(w, classes, e);
    true
}

/// Stands Humanoid `e` up from its seat: the weld is destroyed, the seat
/// freed, and the controller comes back at the next step (with a jump, if
/// one was asked for).
pub(super) fn leave(lua: &Lua, classes: &ClassRegistry, e: Entity) -> Result<()> {
    let weld = with_world(lua, |w| {
        let (seat, weld) = rt(w, classes, e).seat.take()?;
        if tree::alive(w, seat) {
            if occupant(w, classes, seat) == Some(e) {
                set_seat(w, classes, seat, "Occupant", Value::Entity(None));
            }
            if tree::is_a(w, classes, seat, "VehicleSeat") {
                for (p, v) in [
                    ("Throttle", Value::Int(0)),
                    ("Steer", Value::Int(0)),
                    ("ThrottleFloat", Value::Float(0.0)),
                    ("SteerFloat", Value::Float(0.0)),
                ] {
                    set_seat(w, classes, seat, p, v);
                }
            }
        }
        // The character stands on the seat's top, clear of it.
        if let (Some(root), Some(cf)) = (
            root_part(w, classes, e),
            cframe_prop(w, classes, seat, "CFrame"),
        ) {
            let top =
                Vec3::from_array(cf.point_to_world([0.0, size(w, classes, seat).y / 2.0, 0.0]));
            let h = super::root_height(w, classes, e, root);
            let old = cframe_prop(w, classes, root, "CFrame").unwrap_or_default();
            let at = top + Vec3::Y * (h + 0.05);
            let q = super::quat(&old);
            let cf = CFrame::from_quaternion(at.to_array(), [q.x, q.y, q.z, q.w]);
            let _ = classes.set_property(w, root, "CFrame", cframe_value(&cf));
        }
        set_own(w, HUM, e, "SeatPart", Value::Entity(None));
        set_own(w, HUM, e, "Sit", Value::Bool(false));
        let mut rt = rt(w, classes, e);
        rt.unseated = 0.0;
        // A jump out of the seat leaves from where it sits.
        rt.on_ground = true;
        Some(weld)
    })?;
    if let Some(weld) = weld {
        let alive = with_world(lua, |w| {
            tree::alive(w, weld) && !tree::is_destroyed(w, weld)
        })?;
        if alive {
            crate::instance::destroy(lua, weld)?;
        }
    }
    Ok(())
}

/// Places a seated character's root part where its weld holds it, and poses
/// the limbs. False when it is not seated.
pub(super) fn place(w: &mut World, classes: &ClassRegistry, e: Entity) -> bool {
    let Some((seat, weld)) = w.ecs.get::<super::HumanoidRt>(e).and_then(|rt| rt.seat) else {
        return false;
    };
    let (Some(root), Some(rig)) = (root_part(w, classes, e), tree::parent(w, e)) else {
        return false;
    };
    let joint = |p: &str| cframe_prop(w, classes, weld, p).unwrap_or(CFrame::IDENTITY);
    let (c0, c1) = (joint("C0"), joint("C1"));
    let Some(seat_cf) = cframe_prop(w, classes, seat, "CFrame") else {
        return false;
    };
    let cf = seat_cf * c0 * c1.inverse();
    let _ = classes.set_property(w, root, "CFrame", cframe_value(&cf));
    let stored = cframe_prop(w, classes, root, "CFrame").unwrap_or(cf);
    pose::apply(w, classes, rig);
    if let Some(mut rt) = w.ecs.get_mut::<super::HumanoidRt>(e) {
        rt.last_root = Some(stored);
    }
    true
}

/// A `VehicleSeat`'s `Throttle` and `Steer` from its occupant's walk
/// direction, in the seat's frame (forward is -Z, right is +X).
fn vehicle_input(w: &mut World, classes: &ClassRegistry, e: Entity, seat: Entity) {
    if !tree::is_a(w, classes, seat, "VehicleSeat") {
        return;
    }
    let dir = rt(w, classes, e).move_dir;
    let Some(cf) = cframe_prop(w, classes, seat, "CFrame") else {
        return;
    };
    let forward = Vec3::from_array(cf.look_vector());
    let right = Vec3::from_array(cf.x_vector());
    let throttle = dir.dot(forward).clamp(-1.0, 1.0);
    let steer = dir.dot(right).clamp(-1.0, 1.0);
    let step = |x: f32| -> i64 {
        if x > 0.1 {
            1
        } else if x < -0.1 {
            -1
        } else {
            0
        }
    };
    for (p, v) in [
        ("Throttle", Value::Int(step(throttle))),
        ("Steer", Value::Int(step(steer))),
        ("ThrottleFloat", Value::Float(f64::from(throttle))),
        ("SteerFloat", Value::Float(f64::from(steer))),
    ] {
        let class = seat_class(w, classes, seat).unwrap_or("VehicleSeat");
        if get_own(w, class, seat, p) != Some(v.clone()) {
            set_own(w, class, seat, p, v);
        }
    }
}

/// `Seat:Sit(humanoid)` and `VehicleSeat:Sit(humanoid)`: sits the Humanoid
/// in the seat now, standing it up from any other seat first.
fn s_sit(lua: &Lua, seat: Entity, a: MultiValue) -> Result<MultiValue> {
    const WHAT: &str = "Seat:Sit";
    let h: mlua::Value = args(lua, a, WHAT)?;
    let h = arg_instance(&h, WHAT)?;
    let classes = state(lua)?.setup.classes.clone();
    let is_hum = with_world(lua, |w| tree::is_a(w, &classes, h, HUM))?;
    if !is_hum {
        return Err(lua_error(
            re_script::codes::BAD_ARGUMENT,
            format!("bad argument to {WHAT}: a Humanoid expected"),
        ));
    }
    let current = with_world(lua, |w| rt(w, &classes, h).seat)?;
    if current.is_some_and(|(s, _)| s == seat) {
        return ret(lua, ());
    }
    if current.is_some() {
        leave(lua, &classes, h)?;
    }
    with_world(lua, |w| sit(w, &classes, h, seat))?;
    signal::events::after_write(lua)?;
    ret(lua, ())
}

/// Registers the seat methods.
pub fn register(b: &mut Bindings) {
    for class in SEATS {
        b.method(class, "Sit", std::sync::Arc::new(s_sit));
    }
}
