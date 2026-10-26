//! Vehicles (ADR-0040): oxijolt's wheeled and motorcycle controllers on a dynamic body
//! the entity already has, its chassis.
//!
//! A vehicle runs inside the Jolt step as a constraint: before the collision step its
//! wheels cast against the scene, then engine, brakes and tire friction act through
//! the constraint. `re_physics` adds what Jolt leaves to the caller:
//!
//! * differentials and anti-roll bars are derived from the wheels' axles, and the
//!   differentials are open, where Jolt's default limits their slip;
//! * the wheel tester queries as the chassis layer's moving object layer, so wheels
//!   touch what the chassis touches in the [`LayerTable`](crate::LayerTable);
//! * a new driver input wakes a sleeping chassis (Jolt gives a sleeping chassis no
//!   drive force);
//! * removing the chassis body removes its vehicle first.

use std::f32::consts::FRAC_PI_2;

use glam::{Quat, Vec3};
use oxijolt::{
    DriverInput, Motorcycle, MotorcycleSettings, ObjectLayer, SpringSettings, VehicleAntiRollBar,
    VehicleCollisionTester, VehicleDifferentialSettings, VehicleEngineSettings, VehicleId,
    VehicleTransmissionSettings, WheelSettings, WheeledVehicle, WheeledVehicleSettings,
};
use re_core::Diagnostic;
use re_types::EntityId;
use re_types::physics::{
    BodyKind, VehicleDesc, VehicleInput, VehicleKind, VehicleState, WheelDesc, WheelPose,
    WheelState, WheelTester,
};

use crate::codes;
use crate::convert::{gq, gv, jv};
use crate::world::{Physics, at, object_layer, unknown};

/// Driven wheels whose local `z` differ by at most this share an axle, metres.
const AXLE_TOLERANCE: f32 = 0.01;

/// The limited-slip ratio of an open differential, which splits its torque evenly
/// whatever its sides' speeds. Jolt's default of 1.4 sends more of a differential's
/// torque to its slower side the faster the other side turns, and all of it once that
/// side turns 1.4 times as fast. Jolt shifts up only on a step on which no wheel that
/// gets torque slips, so with limited slip within and between the differentials a car
/// with more torque than grip can put all of it on one wheel and shift up the first
/// step that wheel grips, even while the others spin: its first gear change hangs on
/// tiny differences (ADR-0040). With open differentials every driven wheel gets
/// torque, so a car stays in its gear while any of them spins.
const OPEN: f32 = f32::MAX;

/// The vehicle of one chassis entity.
#[derive(Clone, Copy, Debug)]
pub(crate) enum VehicleEntry {
    Wheeled(VehicleId<WheeledVehicle>),
    Motorcycle(VehicleId<Motorcycle>),
}

/// Runs `$body` with `$v` bound to the entry's typed vehicle read from `$world`.
macro_rules! with_vehicle {
    ($world:expr, $entry:expr, |$v:ident| $body:expr) => {
        match $entry {
            VehicleEntry::Wheeled(id) => $world.vehicle(id).ok().map(|$v| $body),
            VehicleEntry::Motorcycle(id) => $world.vehicle(id).ok().map(|$v| $body),
        }
    };
}

fn vehicle_invalid(id: EntityId, why: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::error(codes::VEHICLE_INVALID, format!("vehicle {id}: {why}"))
        .at(at(id))
        .with_hint(
            "a vehicle needs a dynamic body on the same entity, at least one wheel (a motorcycle \
         exactly two) and positive sizes, torques and gear ratios",
        )
}

fn driver_input(input: VehicleInput) -> DriverInput {
    DriverInput {
        forward: input.throttle,
        right: input.steer,
        brake: input.brake,
        hand_brake: input.handbrake,
    }
}

fn vehicle_input(input: DriverInput) -> VehicleInput {
    VehicleInput {
        throttle: input.forward,
        steer: input.right,
        brake: input.brake,
        handbrake: input.hand_brake,
    }
}

/// The wheels of `wheels` (indices) grouped into axles by local `z`, in the order of
/// each axle's first wheel.
fn axles(wheels: &[WheelDesc], pick: impl Fn(&WheelDesc) -> bool) -> Vec<Vec<u32>> {
    let mut out: Vec<Vec<u32>> = Vec::new();
    for (i, w) in wheels.iter().enumerate() {
        if !pick(w) {
            continue;
        }
        let i = i as u32;
        match out
            .iter_mut()
            .find(|axle| (wheels[axle[0] as usize].pos.z - w.pos.z).abs() <= AXLE_TOLERANCE)
        {
            Some(axle) => axle.push(i),
            None => out.push(vec![i]),
        }
    }
    out
}

/// An axle's `(left, right)` wheels: left is the one at larger `x` (Jolt's frame).
fn sides(wheels: &[WheelDesc], axle: &[u32]) -> Result<(Option<u32>, Option<u32>), String> {
    match *axle {
        [only] if wheels[only as usize].pos.x >= 0.0 => Ok((Some(only), None)),
        [only] => Ok((None, Some(only))),
        [a, b] if wheels[a as usize].pos.x >= wheels[b as usize].pos.x => Ok((Some(a), Some(b))),
        [a, b] => Ok((Some(b), Some(a))),
        _ => Err(format!(
            "wheels {axle:?} share an axle (the same local z within {AXLE_TOLERANCE} m); an \
             axle has at most two"
        )),
    }
}

fn tester(t: WheelTester, layer: ObjectLayer) -> VehicleCollisionTester {
    match t {
        WheelTester::Ray => VehicleCollisionTester::ray(layer),
        WheelTester::CastSphere { radius } => VehicleCollisionTester::cast_sphere(layer, radius),
        WheelTester::CastCylinder => VehicleCollisionTester::cast_cylinder(layer),
    }
}

/// The oxijolt settings of `desc`; oxijolt validates the values when the vehicle is
/// created.
fn settings(desc: &VehicleDesc, layer: ObjectLayer) -> Result<WheeledVehicleSettings, String> {
    if desc.wheels.is_empty() {
        return Err("it has no wheels".into());
    }
    let wheels = desc
        .wheels
        .iter()
        .map(|w| {
            WheelSettings::new(jv(w.pos))
                .radius(w.radius)
                .width(w.width)
                .suspension_min_length(w.suspension_min)
                .suspension_max_length(w.suspension_max)
                .suspension_spring(SpringSettings::FrequencyAndDamping {
                    frequency: w.suspension_frequency,
                    damping: w.suspension_damping,
                })
                .max_steer_angle(w.max_steer_deg.to_radians())
                .max_brake_torque(w.max_brake_torque)
                .max_hand_brake_torque(w.max_hand_brake_torque)
        })
        .collect();
    let driven = axles(&desc.wheels, |w| w.driven);
    if driven.is_empty() {
        return Err("no wheel is driven".into());
    }
    let share = 1.0 / driven.len() as f32;
    let mut differentials = Vec::with_capacity(driven.len());
    for (n, axle) in driven.iter().enumerate() {
        let (left, right) = sides(&desc.wheels, axle)?;
        // The torque ratios must sum to 1; the last axle takes the rounding.
        let ratio = if n + 1 == driven.len() {
            1.0 - share * n as f32
        } else {
            share
        };
        differentials.push(
            VehicleDifferentialSettings::new(left, right)
                .differential_ratio(desc.transmission.differential_ratio)
                .engine_torque_ratio(ratio)
                .limited_slip_ratio(OPEN),
        );
    }
    let mut bars = Vec::new();
    if desc.anti_roll_stiffness > 0.0 {
        for axle in axles(&desc.wheels, |_| true) {
            if let (Some(l), Some(r)) = sides(&desc.wheels, &axle)? {
                bars.push(VehicleAntiRollBar::new(l, r).stiffness(desc.anti_roll_stiffness));
            }
        }
    }
    let t = &desc.transmission;
    Ok(
        WheeledVehicleSettings::new(wheels, differentials, tester(desc.tester, layer))
            .max_pitch_roll_angle(desc.max_pitch_roll_deg.to_radians())
            .differential_limited_slip_ratio(OPEN)
            .anti_roll_bars(bars)
            .engine(
                VehicleEngineSettings::default()
                    .max_torque(desc.engine.max_torque)
                    .min_rpm(desc.engine.min_rpm)
                    .max_rpm(desc.engine.max_rpm),
            )
            .transmission(
                VehicleTransmissionSettings::default()
                    .gear_ratios(t.gears.clone())
                    .reverse_gear_ratios(t.reverse_gears.clone())
                    .shift_up_rpm(t.shift_up_rpm)
                    .shift_down_rpm(t.shift_down_rpm),
            ),
    )
}

/// Turns oxijolt's wheel rotation, which maps a model with its axle along Y and its up
/// along X into the wheel, into one for a model built in the chassis' frame.
fn wheel_rotation(jolt: Quat) -> Quat {
    (jolt * Quat::from_rotation_z(-FRAC_PI_2)).normalize()
}

impl Physics {
    pub(crate) fn add_vehicle(
        &mut self,
        id: EntityId,
        desc: &VehicleDesc,
    ) -> Result<(), Diagnostic> {
        if self.vehicles.contains_key(&id) {
            return Err(vehicle_invalid(id, "the entity already has a vehicle"));
        }
        let entry = self
            .bodies
            .get(&id)
            .ok_or_else(|| vehicle_invalid(id, "the entity has no body to be its chassis"))?;
        if entry.kind != BodyKind::Dynamic {
            return Err(vehicle_invalid(
                id,
                format!("its chassis is a {:?} body; it must be dynamic", entry.kind),
            ));
        }
        let (chassis, layer) = (entry.id, entry.layer);
        let settings =
            settings(desc, object_layer(layer, true)).map_err(|why| vehicle_invalid(id, why))?;
        let vehicle = match desc.kind {
            VehicleKind::Wheeled => self
                .world
                .create_wheeled_vehicle(chassis, &settings)
                .map(VehicleEntry::Wheeled),
            VehicleKind::Motorcycle => self
                .world
                .create_motorcycle(chassis, &MotorcycleSettings::new(settings))
                .map(VehicleEntry::Motorcycle),
        }
        .map_err(|e| vehicle_invalid(id, e))?;
        self.vehicles.insert(id, vehicle);
        // A vehicle replaced in this apply (a changed description, a replaced
        // chassis body) keeps its driver's input.
        if let Some(input) = self.dropped_inputs.remove(&id) {
            self.set_vehicle_input(id, input)?;
        }
        Ok(())
    }

    /// Removes `id`'s vehicle, if it has one; its chassis body stays.
    pub(crate) fn drop_vehicle(&mut self, id: EntityId) -> bool {
        let Some(entry) = self.vehicles.remove(&id) else {
            return false;
        };
        if let Some(input) = with_vehicle!(self.world, entry, |v| vehicle_input(v.driver_input())) {
            self.dropped_inputs.insert(id, input);
        }
        // The entry names a vehicle of this world, so this cannot fail.
        let _ = match entry {
            VehicleEntry::Wheeled(v) => self.world.remove_vehicle(v),
            VehicleEntry::Motorcycle(v) => self.world.remove_vehicle(v),
        };
        true
    }

    /// Removes `id`'s vehicle. Without one (it went with its chassis body, or its
    /// description was refused) this is a no-op (ADR-0040).
    pub(crate) fn remove_vehicle(&mut self, id: EntityId) -> Result<(), Diagnostic> {
        self.drop_vehicle(id);
        Ok(())
    }

    pub(crate) fn set_vehicle_input(
        &mut self,
        id: EntityId,
        input: VehicleInput,
    ) -> Result<(), Diagnostic> {
        let entry = *self
            .vehicles
            .get(&id)
            .ok_or_else(|| unknown(id, "vehicle"))?;
        if !input.is_valid() {
            return Err(vehicle_invalid(
                id,
                format!(
                    "input {input:?} is out of range: throttle and steer must be in [-1, 1], \
                     brake and handbrake in [0, 1]"
                ),
            ));
        }
        let previous = match entry {
            VehicleEntry::Wheeled(v) => self.world.vehicle(v).map(|v| v.driver_input()),
            VehicleEntry::Motorcycle(v) => self.world.vehicle(v).map(|v| v.driver_input()),
        }
        .map_err(|e| vehicle_invalid(id, e))?;
        let wanted = driver_input(input);
        match entry {
            VehicleEntry::Wheeled(v) => self
                .world
                .vehicle_mut(v)
                .and_then(|mut v| v.set_driver_input(wanted)),
            VehicleEntry::Motorcycle(v) => self
                .world
                .vehicle_mut(v)
                .and_then(|mut v| v.set_driver_input(wanted)),
        }
        .map_err(|e| vehicle_invalid(id, e))?;
        // Jolt gives a sleeping chassis no drive force, and a new input does not wake
        // it.
        if wanted != previous
            && let Some(body) = self.bodies.get(&id).map(|b| b.id)
            && let Ok(mut b) = self.world.body_mut(body)
        {
            b.activate();
        }
        Ok(())
    }

    /// A vehicle's state after the last step, or `None` if the entity has no vehicle.
    pub fn vehicle(&self, id: EntityId) -> Option<VehicleState> {
        let entry = *self.vehicles.get(&id)?;
        let chassis = self.world.body(self.bodies.get(&id)?.id).ok()?;
        let forward = gq(chassis.rotation()) * Vec3::Z;
        let speed = gv(chassis.linear_velocity()).dot(forward);
        let input = match entry {
            VehicleEntry::Wheeled(v) => self.world.vehicle(v).ok()?.driver_input(),
            VehicleEntry::Motorcycle(v) => self.world.vehicle(v).ok()?.driver_input(),
        };
        with_vehicle!(self.world, entry, |v| VehicleState {
            speed,
            rpm: v.engine_rpm(),
            gear: v.current_gear(),
            input: vehicle_input(input),
            wheels: v
                .wheels()
                .iter()
                .map(|w| WheelState {
                    contact: w
                        .contact
                        .as_ref()
                        .and_then(|c| self.entity_of.get(&c.body).copied()),
                    on_ground: w.contact.is_some(),
                    contact_point: w
                        .contact
                        .as_ref()
                        .map_or(Vec3::ZERO, |c| self.world_point(c.position)),
                    suspension_length: w.suspension_length,
                    angular_velocity: w.angular_velocity,
                    // Jolt's steer angle is about the steering axis (up): a right turn
                    // is negative.
                    steer_deg: -w.steer_angle.to_degrees(),
                })
                .collect(),
        })
    }

    /// Writes the world pose of every vehicle wheel into `out` (cleared first), by
    /// chassis entity then wheel index, for drawing the wheels after a step.
    pub fn vehicle_poses_into(&self, out: &mut Vec<WheelPose>) {
        out.clear();
        for (&id, &entry) in &self.vehicles {
            with_vehicle!(self.world, entry, |v| {
                for wheel in 0..v.wheel_count() {
                    if let Some((pos, rot)) = v.wheel_world_transform(wheel) {
                        out.push(WheelPose {
                            id,
                            wheel,
                            pos: self.world_point(pos),
                            rot: wheel_rotation(gq(rot)),
                        });
                    }
                }
            });
        }
    }

    /// True when `id` has a vehicle.
    pub fn has_vehicle(&self, id: EntityId) -> bool {
        self.vehicles.contains_key(&id)
    }

    /// The number of vehicles.
    pub fn vehicle_count(&self) -> usize {
        self.vehicles.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wheel(x: f32, z: f32, driven: bool) -> WheelDesc {
        WheelDesc {
            pos: Vec3::new(x, 0.0, z),
            driven,
            ..WheelDesc::default()
        }
    }

    #[test]
    fn axles_pair_wheels_by_z_with_left_at_larger_x() {
        let wheels = [
            wheel(-0.9, 1.4, true),
            wheel(0.9, 1.405, true),
            wheel(0.9, -1.4, false),
            wheel(-0.9, -1.4, false),
        ];
        let driven = axles(&wheels, |w| w.driven);
        assert_eq!(driven, vec![vec![0, 1]]);
        assert_eq!(sides(&wheels, &driven[0]).unwrap(), (Some(1), Some(0)));
        assert_eq!(axles(&wheels, |_| true), vec![vec![0, 1], vec![2, 3]]);
        assert_eq!(sides(&wheels, &[3]).unwrap(), (None, Some(3)));
        assert!(sides(&wheels, &[0, 1, 2]).is_err());
    }

    #[test]
    fn differentials_are_open_on_each_axle_and_between_axles() {
        // The settings' fields are private, so read the ratios from their debug text:
        // each axle's `limited_slip_ratio` and the `differential_limited_slip_ratio`
        // between the axles.
        let mut desc = VehicleDesc::car(Vec3::new(0.9, -0.1, 1.4), 0.35);
        for wheel in &mut desc.wheels {
            wheel.driven = true;
        }
        let text = format!("{:?}", settings(&desc, ObjectLayer::new(1)).unwrap());
        let open = format!("limited_slip_ratio: {:?}", f32::MAX);
        assert_eq!(text.matches("limited_slip_ratio").count(), 3, "{text}");
        assert_eq!(text.matches(&open).count(), 3, "{text}");
    }

    #[test]
    fn wheel_rotation_at_rest_is_the_chassis_rotation() {
        // At rest Jolt's wheel rotation is the chassis' times a quarter turn about Z
        // (model X to up, model Y to the vehicle's right, −X).
        let chassis = Quat::from_rotation_y(0.7);
        let jolt = chassis * Quat::from_rotation_z(FRAC_PI_2);
        assert!(wheel_rotation(jolt).angle_between(chassis) < 1e-5);
    }
}
