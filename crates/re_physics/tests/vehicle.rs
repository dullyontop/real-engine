//! Vehicles (ADR-0040): driving, steering and braking, lifecycle and errors, wheel
//! poses, the fixed-path acceptance drive and determinism.

mod common;

use common::*;
use re_physics::{Physics, codes};
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::{
    BodyDesc, BodyKind, PhysicsCmd, ShapeDesc, VehicleDesc, VehicleInput, VehicleKind, WheelDesc,
    WheelTester,
};

const CAR: EntityId = EntityId(10);

/// The chassis of the test car: a 1500 kg box of half extents (0.9, 0.3, 2.0).
fn chassis(pos: Vec3, rot: Quat) -> PhysicsCmd {
    PhysicsCmd::AddBody {
        id: CAR,
        desc: BodyDesc {
            kind: BodyKind::Dynamic,
            shape: ShapeDesc::Box {
                half_extents: Vec3::new(0.9, 0.3, 2.0),
            },
            pos,
            rot,
            mass: 1500.0,
            center_of_mass_offset: Vec3::new(0.0, -0.3, 0.0),
            ..BodyDesc::default()
        },
    }
}

fn car_desc(tester: WheelTester) -> VehicleDesc {
    VehicleDesc {
        tester,
        ..VehicleDesc::car(Vec3::new(0.9, -0.1, 1.4), 0.35)
    }
}

fn add_car(id: EntityId, desc: VehicleDesc) -> PhysicsCmd {
    PhysicsCmd::AddVehicle { id, desc }
}

fn input(throttle: f32, steer: f32, brake: f32) -> PhysicsCmd {
    PhysicsCmd::SetVehicleInput {
        id: CAR,
        input: VehicleInput {
            throttle,
            steer,
            brake,
            handbrake: 0.0,
        },
    }
}

/// A flat world with the car standing at the origin, facing +Z.
fn flat(threads: usize) -> Physics {
    let mut p = world(threads);
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 200.0),
            chassis(Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY),
            add_car(CAR, car_desc(WheelTester::default())),
        ],
    );
    p
}

fn drive(p: &mut Physics, cmd: PhysicsCmd, ticks: usize) {
    apply(p, vec![cmd]);
    steps(p, ticks);
}

fn errors(p: &mut Physics, cmds: Vec<PhysicsCmd>) -> Vec<String> {
    let mut cmds = cmds;
    p.apply(&mut cmds);
    p.take_diagnostics()
        .into_iter()
        .map(|d| format!("{}: {}", d.code, d.message))
        .collect()
}

#[test]
fn a_car_settles_on_its_wheels() {
    let mut p = flat(1);
    steps(&mut p, 120);
    let state = p.vehicle(CAR).expect("vehicle state");
    assert_eq!(state.wheels.len(), 4);
    assert!(state.wheels.iter().all(|w| w.on_ground), "{state:?}");
    assert!(state.wheels.iter().all(|w| w.contact == Some(EntityId(1))));
    let (pos, rot) = p.pose(CAR).unwrap();
    // The chassis hangs on its springs above the floor, level.
    assert!(pos.y > 0.4 && pos.y < 1.0, "chassis at {pos}");
    assert!(
        rot.angle_between(Quat::IDENTITY) < 0.02,
        "chassis tilted: {rot}"
    );
    assert!(state.speed.abs() < 0.05);
    assert_eq!(p.vehicle_count(), 1);
}

#[test]
fn throttle_drives_steering_turns_right_and_brakes_stop() {
    let mut p = flat(1);
    steps(&mut p, 60);
    drive(&mut p, input(1.0, 0.0, 0.0), 180);
    let state = p.vehicle(CAR).unwrap();
    assert!(state.speed > 5.0, "speed {}", state.speed);
    assert!(state.rpm > 1000.0 && state.gear >= 1, "{state:?}");
    let (pos, _) = p.pose(CAR).unwrap();
    assert!(pos.z > 5.0 && pos.x.abs() < 0.5, "drove to {pos}");

    // Steering right: the front wheels turn right (positive steer_deg), and the car
    // heads toward −X (the vehicle's right in Jolt's frame).
    drive(&mut p, input(0.5, 1.0, 0.0), 60);
    let state = p.vehicle(CAR).unwrap();
    assert!(state.wheels[0].steer_deg > 10.0, "{:?}", state.wheels[0]);
    assert_eq!(state.wheels[2].steer_deg, 0.0);
    let (_, rot) = p.pose(CAR).unwrap();
    let heading = rot * Vec3::Z;
    assert!(heading.x < -0.2, "heading {heading}");
    assert_eq!(state.input.steer, 1.0);

    drive(&mut p, input(0.0, 0.0, 1.0), 240);
    let state = p.vehicle(CAR).unwrap();
    assert!(state.speed.abs() < 0.2, "still moving at {}", state.speed);
}

#[test]
fn reverse_throttle_backs_up() {
    let mut p = flat(1);
    steps(&mut p, 60);
    drive(&mut p, input(-1.0, 0.0, 0.0), 180);
    let state = p.vehicle(CAR).unwrap();
    assert!(state.speed < -1.0, "speed {}", state.speed);
    assert!(state.gear < 0, "gear {}", state.gear);
    assert!(p.pose(CAR).unwrap().0.z < -1.0);
}

#[test]
fn a_new_input_wakes_a_sleeping_chassis() {
    let mut p = flat(1);
    let asleep = (0..600).any(|_| {
        steps(&mut p, 1);
        p.is_awake(CAR) == Some(false)
    });
    assert!(asleep, "the chassis never fell asleep");
    drive(&mut p, input(1.0, 0.0, 0.0), 60);
    assert_eq!(p.is_awake(CAR), Some(true));
    assert!(p.vehicle(CAR).unwrap().speed > 1.0);
}

#[test]
fn wheel_poses_follow_the_wheels() {
    let mut p = flat(1);
    steps(&mut p, 120);
    let mut poses = Vec::new();
    p.vehicle_poses_into(&mut poses);
    assert_eq!(poses.len(), 4);
    let (chassis_pos, chassis_rot) = p.pose(CAR).unwrap();
    let desc = car_desc(WheelTester::default());
    for (i, pose) in poses.iter().enumerate() {
        assert_eq!((pose.id, pose.wheel), (CAR, i as u32));
        let state = p.vehicle(CAR).unwrap();
        // The centre hangs below the attachment point by the suspension length.
        let attach = chassis_pos + chassis_rot * desc.wheels[i].pos;
        let expected = attach - chassis_rot * Vec3::Y * state.wheels[i].suspension_length;
        assert!(
            pose.pos.distance(expected) < 1e-3,
            "wheel {i}: {} vs {expected}",
            pose.pos
        );
        // A wheel standing still (it may have rolled a little while settling) keeps its
        // axle along the chassis' X.
        let axle = pose.rot * Vec3::X;
        assert!(
            axle.dot(chassis_rot * Vec3::X) > 0.999,
            "wheel {i} axle {axle}"
        );
        // It touches the floor at its radius.
        assert!(
            (pose.pos.y - 0.35).abs() < 0.03,
            "wheel {i} at {}",
            pose.pos
        );
    }
}

#[test]
fn removing_the_chassis_removes_the_vehicle_and_remove_vehicle_keeps_the_body() {
    let mut p = flat(1);
    steps(&mut p, 10);
    apply(&mut p, vec![PhysicsCmd::RemoveVehicle { id: CAR }]);
    assert!(p.vehicle(CAR).is_none());
    assert!(p.pose(CAR).is_some(), "the chassis body stays");
    assert_eq!(p.vehicle_count(), 0);
    // A new vehicle on the same body, then the body removed: both go.
    apply(&mut p, vec![add_car(CAR, car_desc(WheelTester::Ray))]);
    steps(&mut p, 10);
    apply(&mut p, vec![PhysicsCmd::RemoveBody { id: CAR }]);
    assert!(p.vehicle(CAR).is_none());
    assert_eq!(p.vehicle_count(), 0);
    steps(&mut p, 10);
    // Removing a body and its vehicle in one buffer is fine too.
    apply(
        &mut p,
        vec![
            chassis(Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY),
            add_car(CAR, car_desc(WheelTester::Ray)),
        ],
    );
    let left = errors(
        &mut p,
        vec![
            PhysicsCmd::RemoveVehicle { id: CAR },
            PhysicsCmd::RemoveBody { id: CAR },
        ],
    );
    // RemoveBody (rank 1) removes the vehicle first; the RemoveVehicle after it finds
    // nothing.
    assert_eq!(left.len(), 1, "{left:?}");
    assert!(left[0].starts_with("RE5004"), "{left:?}");
}

#[test]
fn a_body_and_its_vehicle_in_one_buffer_apply_in_any_order() {
    let run = |cmds: Vec<PhysicsCmd>| {
        let mut p = world(1);
        apply(&mut p, cmds);
        steps(&mut p, 120);
        p.pose(CAR).unwrap()
    };
    let mut cmds = vec![
        floor(1, 0.0, 0.0, 0.0, 200.0),
        chassis(Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY),
        add_car(CAR, car_desc(WheelTester::default())),
        input(1.0, 0.3, 0.0),
    ];
    let forward = run(cmds.clone());
    cmds.reverse();
    let reversed = run(cmds);
    assert_eq!(forward, reversed);
    assert!(forward.0.z > 1.0);
}

#[test]
fn invalid_vehicles_and_inputs_are_refused_with_re5018() {
    let mut p = world(1);
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 50.0),
            add(
                2,
                body(BodyKind::Dynamic, cube(0.5), Vec3::new(5.0, 1.0, 0.0)),
            ),
        ],
    );
    let refused = |p: &mut Physics, id: u64, desc: VehicleDesc| {
        let e = errors(p, vec![add_car(EntityId(id), desc)]);
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(
            e[0].starts_with(&codes::VEHICLE_INVALID.to_string()),
            "{e:?}"
        );
        e[0].clone()
    };
    let car = car_desc(WheelTester::Ray);
    assert!(refused(&mut p, 99, car.clone()).contains("no body"));
    assert!(refused(&mut p, 1, car.clone()).contains("must be dynamic"));
    assert!(refused(&mut p, 2, VehicleDesc::default()).contains("no wheels"));
    let mut undriven = car.clone();
    undriven.wheels.iter_mut().for_each(|w| w.driven = false);
    assert!(refused(&mut p, 2, undriven).contains("no wheel is driven"));
    let mut crowded = car.clone();
    crowded.wheels.push(WheelDesc {
        pos: Vec3::new(0.0, -0.1, 1.4),
        driven: true,
        ..WheelDesc::default()
    });
    assert!(refused(&mut p, 2, crowded).contains("share an axle"));
    let mut tiny = car.clone();
    tiny.wheels[0].radius = 0.0;
    refused(&mut p, 2, tiny);
    let mut no_gears = car.clone();
    no_gears.transmission.gears.clear();
    refused(&mut p, 2, no_gears);
    let bike = VehicleDesc {
        kind: VehicleKind::Motorcycle,
        ..car.clone()
    };
    assert!(refused(&mut p, 2, bike).contains("exactly two wheels"));
    assert_eq!(p.vehicle_count(), 0);

    apply(&mut p, vec![add_car(EntityId(2), car.clone())]);
    assert!(refused(&mut p, 2, car).contains("already has a vehicle"));
    let e = errors(
        &mut p,
        vec![PhysicsCmd::SetVehicleInput {
            id: EntityId(2),
            input: VehicleInput {
                throttle: 2.0,
                ..VehicleInput::default()
            },
        }],
    );
    assert!(e.len() == 1 && e[0].starts_with("RE5018"), "{e:?}");
    let e = errors(&mut p, vec![input(1.0, 0.0, 0.0)]);
    assert!(e.len() == 1 && e[0].starts_with("RE5004"), "{e:?}");
}

#[test]
fn a_motorcycle_rides_upright() {
    let mut p = world(1);
    let bike = EntityId(20);
    let wheel = |z: f32, front: bool| WheelDesc {
        pos: Vec3::new(0.0, -0.27, z),
        radius: 0.31,
        width: 0.05,
        suspension_min: 0.3,
        suspension_max: 0.5,
        max_steer_deg: if front { 30.0 } else { 0.0 },
        max_brake_torque: if front { 500.0 } else { 250.0 },
        driven: !front,
        ..WheelDesc::default()
    };
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 200.0),
            PhysicsCmd::AddBody {
                id: bike,
                desc: BodyDesc {
                    kind: BodyKind::Dynamic,
                    shape: ShapeDesc::Box {
                        half_extents: Vec3::new(0.2, 0.3, 0.4),
                    },
                    pos: Vec3::new(0.0, 1.2, 0.0),
                    mass: 240.0,
                    center_of_mass_offset: Vec3::new(0.0, -0.3, 0.0),
                    ..BodyDesc::default()
                },
            },
            PhysicsCmd::AddVehicle {
                id: bike,
                desc: VehicleDesc {
                    kind: VehicleKind::Motorcycle,
                    wheels: vec![wheel(0.75, true), wheel(-0.75, false)],
                    max_pitch_roll_deg: 60.0,
                    ..VehicleDesc::default()
                },
            },
            PhysicsCmd::SetVehicleInput {
                id: bike,
                input: VehicleInput {
                    throttle: 0.5,
                    ..VehicleInput::default()
                },
            },
        ],
    );
    steps(&mut p, 240);
    let (pos, rot) = p.pose(bike).unwrap();
    assert!(pos.z > 3.0, "rode to {pos}");
    let up = rot * Vec3::Y;
    assert!(up.y > 0.8, "fell over: up {up}");
    assert_eq!(p.vehicle(bike).unwrap().wheels.len(), 2);
}

// ---- The fixed-path acceptance drive (M3_PLAN WP22; ADR-0040 "Acceptance tests").

const PATH_TICKS: usize = 720;

/// The open-loop inputs of the gentle path: throttle, a gentle right turn, brake.
fn path_input(tick: usize) -> Option<PhysicsCmd> {
    match tick {
        0 => Some(input(0.7, 0.0, 0.0)),
        300 => Some(input(0.4, 0.15, 0.0)),
        540 => Some(input(0.0, 0.0, 1.0)),
        _ => None,
    }
}

/// A flat floor, a 10° ramp rising along +Z from z = 25 m for 24 m, and a plateau at
/// its top.
fn path_world(threads: usize) -> Physics {
    let mut p = world(threads);
    let rot = Quat::from_rotation_x(-10f32.to_radians());
    let half = Vec3::new(30.0, 0.5, 12.0);
    // The ramp's top face starts at y = 0, z = 25.
    let centre = Vec3::new(0.0, 0.0, 25.0) - rot * Vec3::new(0.0, half.y, -half.z);
    let top = centre + rot * Vec3::new(0.0, half.y, half.z);
    let plateau = Vec3::new(100.0, 0.5, 60.0);
    apply(
        &mut p,
        vec![
            floor(1, 0.0, 0.0, 0.0, 200.0),
            add(
                2,
                BodyDesc {
                    rot,
                    ..body(
                        BodyKind::Static,
                        ShapeDesc::Box { half_extents: half },
                        centre,
                    )
                },
            ),
            add(
                3,
                body(
                    BodyKind::Static,
                    ShapeDesc::Box {
                        half_extents: plateau,
                    },
                    Vec3::new(0.0, top.y - plateau.y, top.z + plateau.z - 0.05),
                ),
            ),
            chassis(Vec3::new(0.0, 1.0, 0.0), Quat::IDENTITY),
            add_car(CAR, car_desc(WheelTester::Ray)),
        ],
    );
    p
}

/// Drives the path; returns the end pose and the exact bits of every tick's chassis
/// pose and wheel state.
fn drive_path(threads: usize) -> ((Vec3, Quat), Vec<u32>, f32) {
    let mut p = path_world(threads);
    let mut bits = Vec::new();
    let mut highest = 0f32;
    for tick in 0..PATH_TICKS {
        if let Some(cmd) = path_input(tick) {
            apply(&mut p, vec![cmd]);
        }
        p.step(DT).unwrap();
        let (pos, rot) = p.pose(CAR).unwrap();
        highest = highest.max(pos.y);
        bits.extend(pos.to_array().map(f32::to_bits));
        bits.extend(rot.to_array().map(f32::to_bits));
        for w in p.vehicle(CAR).unwrap().wheels {
            bits.extend([w.suspension_length, w.angular_velocity, w.steer_deg].map(f32::to_bits));
        }
    }
    assert!(p.take_diagnostics().is_empty());
    (p.pose(CAR).unwrap(), bits, highest)
}

/// The end pose of the path, recorded in the reference container (Linux x86_64) with
/// Jolt's default build. Jolt's cross-platform-deterministic build (WP18's PR "d2")
/// ends 4 cm from it; re-record it then (the test prints the pose).
const REFERENCE_POS: Vec3 = Vec3::new(-50.13417, 4.995464, 59.87319);
const REFERENCE_ROT: Quat = Quat::from_xyzw(3.212139e-5, -0.7627902, 3.7909456e-5, 0.6466461);

/// Position and rotation tolerance before D2 (ADR-0040). Measured on this path with
/// the ray tester: the cross-platform-deterministic build moves the end 4 cm, a start
/// 1 µm to the side 3.4 cm and a start turned by 0.001° 7 cm, each under 0.1°.
const TOLERANCE_M: f32 = 0.15;
const TOLERANCE_DEG: f32 = 1.0;

#[test]
fn a_car_drives_a_fixed_path_over_flat_and_sloped_ground_to_the_reference_pose() {
    let ((pos, rot), _, highest) = drive_path(1);
    println!("end pose {pos:?} {rot:?}, highest {highest}");
    // It climbed the ramp onto the plateau, and ends there upright.
    assert!(highest > 4.0, "never climbed the ramp: highest {highest}");
    assert!((rot * Vec3::Y).y > 0.95, "ends tilted: {rot}");
    let d = pos.distance(REFERENCE_POS);
    let a = rot.angle_between(REFERENCE_ROT).to_degrees();
    assert!(
        d <= TOLERANCE_M && a <= TOLERANCE_DEG,
        "end pose {pos} {rot} is {d} m and {a}° from the reference {REFERENCE_POS} {REFERENCE_ROT}"
    );
}

#[test]
fn the_path_is_bit_identical_across_runs_and_thread_counts() {
    let (_, one, _) = drive_path(1);
    let (_, again, _) = drive_path(1);
    let (_, four, _) = drive_path(4);
    assert!(!one.is_empty());
    assert!(one == again, "two runs differ");
    assert!(one == four, "1 and 4 threads differ");
}

#[test]
fn a_restored_snapshot_replays_the_drive_exactly() {
    let mut p = path_world(1);
    apply(&mut p, vec![input(0.8, 0.2, 0.0)]);
    steps(&mut p, 60);
    let saved = p.save_state();
    steps(&mut p, 90);
    let first = (p.pose(CAR).unwrap(), p.vehicle(CAR).unwrap());
    p.restore_state(&saved).unwrap();
    steps(&mut p, 90);
    let second = (p.pose(CAR).unwrap(), p.vehicle(CAR).unwrap());
    assert_eq!(first, second);
}
