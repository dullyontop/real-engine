//! What crosses the physics boundary (ADR-0006, ADR-0008).
//!
//! `re_app` turns World changes into a buffer of [`PhysicsCmd`]s, sorted by
//! [`PhysicsCmd::sort_key`] so that the order is deterministic, and hands it to
//! `re_physics`. After the step it pulls [`BodyPose`]s (active bodies only, position
//! and rotation, no velocities) and [`ContactEvent`]s back.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::ids::EntityId;

mod ragdoll;
pub use ragdoll::*;

/// How a body moves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BodyKind {
    /// Never moves; infinite mass.
    #[default]
    Static,
    /// Moved by the simulation.
    Dynamic,
    /// Moved by the game through targets; pushes dynamic bodies, is not pushed.
    Kinematic,
}

/// An object layer: decides which layers collide with which (the layer table is
/// owned by `re_physics`). Comes from the `Collider.layer` field.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Layer(pub u16);

/// A set of collision groups (bit `g` is group `g`, for groups 0 to 31).
///
/// A body belongs to the groups of its mask; a query or character only sees bodies
/// whose mask intersects the query's mask. Serialized as its bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GroupMask(pub u32);

impl GroupMask {
    /// No group.
    pub const NONE: GroupMask = GroupMask(0);
    /// Every group.
    pub const ALL: GroupMask = GroupMask(u32::MAX);

    /// The mask holding only `group` (0 to 31); `None` for a larger group.
    pub const fn group(group: u32) -> Option<GroupMask> {
        if group < 32 {
            Some(GroupMask(1 << group))
        } else {
            None
        }
    }

    /// True when the masks share at least one group.
    pub const fn intersects(self, other: GroupMask) -> bool {
        self.0 & other.0 != 0
    }

    /// True when `group` is in the mask.
    pub const fn contains_group(self, group: u32) -> bool {
        group < 32 && self.0 & (1 << group) != 0
    }
}

impl Default for GroupMask {
    /// Group 0 only.
    fn default() -> Self {
        GroupMask(1)
    }
}

/// A collision shape, in the body's local space.
///
/// Serialized with a `shape` tag, for example `{"shape": "sphere", "radius": 0.5}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum ShapeDesc {
    /// A box centred on the origin.
    Box {
        /// Half the size along each axis.
        half_extents: Vec3,
    },
    /// A sphere centred on the origin.
    Sphere {
        /// The radius.
        radius: f32,
    },
    /// A capsule along the local Y axis, centred on the origin.
    Capsule {
        /// Half the length of the cylinder part (total height is `2 * (half_height + radius)`).
        half_height: f32,
        /// The radius.
        radius: f32,
    },
    /// The convex hull of a point cloud.
    ConvexHull {
        /// The points.
        points: Vec<Vec3>,
    },
    /// A static triangle mesh (for static bodies only).
    TriangleMesh {
        /// The vertices.
        vertices: Vec<Vec3>,
        /// Three vertex indices per triangle, counter-clockwise seen from outside.
        triangles: Vec<[u32; 3]>,
    },
    /// A height field on a square grid of `n × n` samples in the local XZ plane.
    Heightfield {
        /// Samples per side.
        n: u32,
        /// Heights, row by row: the sample at column `x`, row `z` is `samples[z * n + x]`.
        samples: Vec<f32>,
        /// Distance between neighbouring samples along X and Z.
        spacing: f32,
        /// Position of sample `(0, 0)` relative to the body (heights are added to its Y).
        offset: Vec3,
        /// How finely the heights are stored; `None` keeps the physics backend's
        /// defaults (Jolt: 8 bits per sample, block size 2).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        precision: Option<HeightfieldPrecision>,
    },
}

/// How finely a heightfield shape stores its heights (ADR-0042). Each block of
/// `block_size × block_size` samples is quantised to `bits_per_sample` bits
/// over its own height range: more bits follow the samples more closely, and
/// larger blocks use less memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct HeightfieldPrecision {
    /// Bits per sample, 1 to 16.
    pub bits_per_sample: u8,
    /// Samples per block side, 2 to 8.
    pub block_size: u8,
}

impl HeightfieldPrecision {
    /// 16 bits per sample in blocks of 4: neighbouring chunks built from the
    /// same edge samples agree to 0.1 mm (ADR-0042, §3).
    pub const FINE: HeightfieldPrecision = HeightfieldPrecision {
        bits_per_sample: 16,
        block_size: 4,
    };

    /// True when both values are in range.
    pub fn is_valid(&self) -> bool {
        (1..=16).contains(&self.bits_per_sample) && (2..=8).contains(&self.block_size)
    }
}

impl ShapeDesc {
    /// True when the shape's numbers make sense: positive sizes, indices in range,
    /// `n * n` height samples and finite values.
    pub fn is_well_formed(&self) -> bool {
        let finite = |v: Vec3| v.is_finite();
        match self {
            ShapeDesc::Box { half_extents } => {
                finite(*half_extents) && half_extents.min_element() > 0.0
            }
            ShapeDesc::Sphere { radius } => radius.is_finite() && *radius > 0.0,
            ShapeDesc::Capsule {
                half_height,
                radius,
            } => {
                half_height.is_finite()
                    && radius.is_finite()
                    && *half_height >= 0.0
                    && *radius > 0.0
            }
            ShapeDesc::ConvexHull { points } => {
                points.len() >= 4 && points.iter().all(|p| finite(*p))
            }
            ShapeDesc::TriangleMesh {
                vertices,
                triangles,
            } => {
                !triangles.is_empty()
                    && vertices.iter().all(|p| finite(*p))
                    && triangles
                        .iter()
                        .flatten()
                        .all(|&i| (i as usize) < vertices.len())
            }
            ShapeDesc::Heightfield {
                n,
                samples,
                spacing,
                offset,
                precision,
            } => {
                *n >= 2
                    && precision.is_none_or(|p| p.is_valid())
                    && samples.len() as u64 == u64::from(*n) * u64::from(*n)
                    && samples.iter().all(|h| h.is_finite())
                    && spacing.is_finite()
                    && *spacing > 0.0
                    && finite(*offset)
            }
        }
    }
}

/// Everything needed to create a body.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BodyDesc {
    /// How it moves.
    pub kind: BodyKind,
    /// Its shape.
    pub shape: ShapeDesc,
    /// Initial world position.
    pub pos: Vec3,
    /// Initial world rotation.
    pub rot: Quat,
    /// Mass in kilograms (dynamic bodies).
    pub mass: f32,
    /// Friction coefficient.
    pub friction: f32,
    /// Restitution (bounciness) in `[0, 1]`.
    pub restitution: f32,
    /// Linear damping per second.
    pub linear_damping: f32,
    /// Angular damping per second.
    pub angular_damping: f32,
    /// The object layer.
    pub layer: Layer,
    /// The groups the body belongs to.
    pub groups: GroupMask,
    /// A sensor reports contacts but does not collide.
    pub sensor: bool,
    /// Moves the centre of mass away from the shape's own, in the body's local space
    /// (dynamic bodies; ignored otherwise). A vehicle chassis with its centre of mass
    /// lowered toward the wheels does not roll over (ADR-0040).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub center_of_mass_offset: Vec3,
}

fn is_zero(v: &Vec3) -> bool {
    *v == Vec3::ZERO
}

impl Default for BodyDesc {
    /// A static unit box at the origin with the scene's `RigidBody` defaults.
    fn default() -> Self {
        BodyDesc {
            kind: BodyKind::Static,
            shape: ShapeDesc::Box {
                half_extents: Vec3::splat(0.5),
            },
            pos: Vec3::ZERO,
            rot: Quat::IDENTITY,
            mass: 1.0,
            friction: 0.5,
            restitution: 0.0,
            linear_damping: 0.05,
            angular_damping: 0.05,
            layer: Layer(0),
            groups: GroupMask::default(),
            sensor: false,
            center_of_mass_offset: Vec3::ZERO,
        }
    }
}

/// Everything needed to create a character (a capsule moved by desired velocity).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CharacterDesc {
    /// Total height in metres, including the capsule's caps.
    pub height: f32,
    /// Capsule radius.
    pub radius: f32,
    /// The steepest walkable slope, in degrees.
    pub max_slope_deg: f32,
    /// The highest step the character climbs without jumping.
    pub step_height: f32,
    /// Mass in kilograms (used when pushing bodies).
    pub mass: f32,
    /// Also create an inner rigid body, so the character takes part in contact
    /// events and is hit by raycasts.
    pub inner_body: bool,
    /// The object layer.
    pub layer: Layer,
    /// The groups the character belongs to.
    pub groups: GroupMask,
}

impl Default for CharacterDesc {
    /// The scene's `CharacterController` defaults.
    fn default() -> Self {
        CharacterDesc {
            height: 1.8,
            radius: 0.4,
            max_slope_deg: 45.0,
            step_height: 0.3,
            mass: 70.0,
            inner_body: true,
            layer: Layer(0),
            groups: GroupMask::default(),
        }
    }
}

/// The kind of a [`ConstraintDesc`]. Only fixed, hinge and distance constraints
/// are exposed in M1.
///
/// Serialized with a `type` tag, for example `{"type": "fixed"}`. Points and axes are
/// in world space at the moment the constraint is created; physics keeps them
/// attached to the bodies afterwards.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConstraintKind {
    /// Keeps the two bodies' current relative pose (a weld).
    Fixed,
    /// Lets the bodies rotate relative to each other about one axis.
    Hinge {
        /// The pivot point, in world space.
        pivot: Vec3,
        /// The hinge axis, in world space (normalised by physics).
        axis: Vec3,
        /// Optional angle limits in radians, `[min, max]` with `min <= max`.
        limits: Option<[f32; 2]>,
    },
    /// Keeps the distance between two attachment points within `[min, max]`.
    Distance {
        /// The attachment point on body `a`, in world space.
        point_a: Vec3,
        /// The attachment point on body `b`, in world space.
        point_b: Vec3,
        /// The smallest allowed distance (`>= 0`).
        min: f32,
        /// The largest allowed distance (`>= min`).
        max: f32,
    },
}

/// Everything needed to create a constraint between two bodies.
///
/// A constraint belongs to its own entity (a child entity, as a weld is in
/// Roblox), which is the `id` of [`PhysicsCmd::AddConstraint`]; `a` and `b` are the
/// constrained bodies' entities.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConstraintDesc {
    /// The first body's entity.
    pub a: EntityId,
    /// The second body's entity.
    pub b: EntityId,
    /// What the constraint does.
    pub kind: ConstraintKind,
}

impl ConstraintDesc {
    /// True when the numbers make sense: two different bodies, finite values, a
    /// non-zero hinge axis and ordered limits.
    pub fn is_well_formed(&self) -> bool {
        if self.a == self.b {
            return false;
        }
        match self.kind {
            ConstraintKind::Fixed => true,
            ConstraintKind::Hinge {
                pivot,
                axis,
                limits,
            } => {
                pivot.is_finite()
                    && axis.is_finite()
                    && axis.length_squared() > 0.0
                    && limits.is_none_or(|[lo, hi]| lo.is_finite() && hi.is_finite() && lo <= hi)
            }
            ConstraintKind::Distance {
                point_a,
                point_b,
                min,
                max,
            } => {
                point_a.is_finite()
                    && point_b.is_finite()
                    && min.is_finite()
                    && max.is_finite()
                    && min >= 0.0
                    && min <= max
            }
        }
    }
}

/// The kind of a vehicle: the controller Jolt runs on it (ADR-0040).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VehicleKind {
    /// A wheeled vehicle: any number of wheels, driven through differentials.
    #[default]
    Wheeled,
    /// A motorcycle: exactly two wheels (front first) and a lean controller that
    /// keeps it upright and leans it into turns.
    Motorcycle,
}

/// How a vehicle's wheels find the ground. Each wheel casts along its suspension at
/// the start of every step, against the bodies the chassis' layer collides with.
///
/// Serialized with a `tester` tag, for example `{"tester": "ray"}`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tester", rename_all = "snake_case")]
pub enum WheelTester {
    /// A ray from the attachment point: cheapest and the steadiest (the default). It
    /// misses steps and edges narrower than the tire.
    #[default]
    Ray,
    /// A sphere of `radius` metres cast from the attachment point; the radius must be
    /// smaller than every wheel's `suspension_max + radius`.
    CastSphere {
        /// The sphere's radius, metres.
        radius: f32,
    },
    /// A cylinder of the wheel's radius and width cast along the suspension: closest
    /// to the tire's shape, but a run reacts far more to tiny differences, at seams
    /// between bodies most (ADR-0040's measurements).
    CastCylinder,
}

/// One wheel of a [`VehicleDesc`]. Lengths are metres, torques N·m, in the chassis'
/// local space, where up is +Y, forward +Z and the vehicle's left +X (Jolt's frame).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WheelDesc {
    /// Where the suspension is attached to the chassis. The wheel's centre hangs
    /// below it by the suspension length.
    pub pos: Vec3,
    /// Wheel radius.
    pub radius: f32,
    /// Wheel width.
    pub width: f32,
    /// Shortest suspension length (fully compressed).
    pub suspension_min: f32,
    /// Longest suspension length (fully extended).
    pub suspension_max: f32,
    /// Suspension spring frequency, Hz.
    pub suspension_frequency: f32,
    /// Suspension damping ratio (0 is undamped, 1 critically damped).
    pub suspension_damping: f32,
    /// How far the wheel steers at full steering input, degrees; 0 does not steer.
    pub max_steer_deg: f32,
    /// Brake torque at full brake.
    pub max_brake_torque: f32,
    /// Hand-brake torque at full hand brake; 0 for wheels the hand brake leaves free.
    pub max_hand_brake_torque: f32,
    /// True when the engine drives this wheel.
    pub driven: bool,
}

impl Default for WheelDesc {
    /// Jolt's wheel defaults, at the chassis origin, not steering and not driven.
    fn default() -> Self {
        WheelDesc {
            pos: Vec3::ZERO,
            radius: 0.3,
            width: 0.1,
            suspension_min: 0.3,
            suspension_max: 0.5,
            suspension_frequency: 1.5,
            suspension_damping: 0.5,
            max_steer_deg: 0.0,
            max_brake_torque: 1500.0,
            max_hand_brake_torque: 0.0,
            driven: false,
        }
    }
}

/// A vehicle's engine. The defaults are Jolt's.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineDesc {
    /// Peak torque, N·m.
    pub max_torque: f32,
    /// Idle speed, rpm.
    pub min_rpm: f32,
    /// Highest speed, rpm.
    pub max_rpm: f32,
}

impl Default for EngineDesc {
    fn default() -> Self {
        EngineDesc {
            max_torque: 500.0,
            min_rpm: 1000.0,
            max_rpm: 6000.0,
        }
    }
}

/// A vehicle's automatic transmission. The defaults are Jolt's.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransmissionDesc {
    /// Forward gear ratios, first gear first; at least one, each positive.
    pub gears: Vec<f32>,
    /// Reverse gear ratios; at least one, each negative.
    pub reverse_gears: Vec<f32>,
    /// Engine speed at which it shifts up, rpm.
    pub shift_up_rpm: f32,
    /// Engine speed at which it shifts down, rpm.
    pub shift_down_rpm: f32,
    /// Ratio of the final drive (each differential), above 0.
    pub differential_ratio: f32,
}

impl Default for TransmissionDesc {
    fn default() -> Self {
        TransmissionDesc {
            gears: vec![2.66, 1.78, 1.3, 1.0, 0.74],
            reverse_gears: vec![-2.9],
            shift_up_rpm: 4000.0,
            shift_down_rpm: 2000.0,
            differential_ratio: 3.42,
        }
    }
}

/// Everything needed to put a vehicle on a dynamic body (its chassis), from
/// [`PhysicsCmd::AddVehicle`].
///
/// Differentials are derived from the driven wheels: driven wheels with the same
/// local `z` (within 1 cm) form an axle, the wheel at larger `x` being its left
/// wheel; a lone driven wheel is an axle of its own. Each axle with two wheels
/// also gets an anti-roll bar of `anti_roll_stiffness` (0 turns them off).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VehicleDesc {
    /// The controller kind.
    pub kind: VehicleKind,
    /// The wheels, in the order [`VehicleState::wheels`] and [`WheelPose::wheel`]
    /// report them. A motorcycle has exactly two, front first.
    pub wheels: Vec<WheelDesc>,
    /// The engine.
    pub engine: EngineDesc,
    /// The transmission.
    pub transmission: TransmissionDesc,
    /// Anti-roll bar stiffness between the two wheels of each axle, N/m.
    pub anti_roll_stiffness: f32,
    /// Largest pitch or roll before a constraint stops the vehicle tipping further,
    /// degrees; 180 turns the limit off.
    pub max_pitch_roll_deg: f32,
    /// How the wheels find the ground.
    pub tester: WheelTester,
}

impl Default for VehicleDesc {
    /// A wheeled vehicle without wheels (which [`PhysicsCmd::AddVehicle`] refuses);
    /// see [`VehicleDesc::car`] for a working one.
    fn default() -> Self {
        VehicleDesc {
            kind: VehicleKind::Wheeled,
            wheels: Vec::new(),
            engine: EngineDesc::default(),
            transmission: TransmissionDesc::default(),
            anti_roll_stiffness: 1000.0,
            max_pitch_roll_deg: 180.0,
            tester: WheelTester::default(),
        }
    }
}

impl VehicleDesc {
    /// A four-wheeled front-wheel-drive car: the front wheels at `front_left` and
    /// `(-x, y, z)`, the rear ones mirrored at `-z`, all of `wheel_radius` and width
    /// 0.2, with suspension travel 0.05 to 0.5 m. The front wheels steer up to 30°
    /// and are driven; the hand brake holds the rear wheels.
    ///
    /// It suits a chassis like a 1500 kg box of half extents (0.9, 0.3, 2.0) with
    /// wheels at (±0.9, −0.1, ±1.4) of radius 0.35.
    pub fn car(front_left: Vec3, wheel_radius: f32) -> VehicleDesc {
        let wheel = |x: f32, z: f32, front: bool| WheelDesc {
            pos: Vec3::new(x, front_left.y, z),
            radius: wheel_radius,
            width: 0.2,
            suspension_min: 0.05,
            suspension_max: 0.5,
            max_steer_deg: if front { 30.0 } else { 0.0 },
            max_hand_brake_torque: if front { 0.0 } else { 4000.0 },
            driven: front,
            ..WheelDesc::default()
        };
        let (x, z) = (front_left.x, front_left.z);
        VehicleDesc {
            wheels: vec![
                wheel(x, z, true),
                wheel(-x, z, true),
                wheel(x, -z, false),
                wheel(-x, -z, false),
            ],
            ..VehicleDesc::default()
        }
    }
}

/// What the driver asks of a vehicle, from [`PhysicsCmd::SetVehicleInput`]. It
/// holds until the next one. The default is no input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VehicleInput {
    /// Throttle in `[-1, 1]`; negative drives backwards (braking first when moving
    /// forward).
    pub throttle: f32,
    /// Steering in `[-1, 1]`; positive steers right.
    pub steer: f32,
    /// Brake in `[0, 1]`.
    pub brake: f32,
    /// Hand brake in `[0, 1]`.
    pub handbrake: f32,
}

impl VehicleInput {
    /// True when every value is finite and in its range.
    pub fn is_valid(&self) -> bool {
        let within = |v: f32, lo: f32| v.is_finite() && (lo..=1.0).contains(&v);
        within(self.throttle, -1.0)
            && within(self.steer, -1.0)
            && within(self.brake, 0.0)
            && within(self.handbrake, 0.0)
    }
}

/// One wheel of a [`VehicleState`], after the last step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WheelState {
    /// The entity the wheel touches, if it touches a body that belongs to one.
    pub contact: Option<EntityId>,
    /// True when the wheel touches the ground (any body).
    pub on_ground: bool,
    /// The contact point in world space (meaningful when `on_ground`).
    pub contact_point: Vec3,
    /// Current suspension length, metres.
    pub suspension_length: f32,
    /// Spin speed, rad/s.
    pub angular_velocity: f32,
    /// Steering angle, degrees; positive is right.
    pub steer_deg: f32,
}

/// A vehicle's state after the last step, from `re_physics::Physics::vehicle`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VehicleState {
    /// Speed along the chassis' forward axis, m/s; negative when reversing.
    pub speed: f32,
    /// Engine speed, rpm.
    pub rpm: f32,
    /// Current gear: positive forward, negative reverse, 0 neutral.
    pub gear: i32,
    /// The input last set.
    pub input: VehicleInput,
    /// The wheels, in [`VehicleDesc::wheels`] order.
    pub wheels: Vec<WheelState>,
}

/// The world pose of one vehicle wheel after a step, for drawing it.
///
/// `rot` turns a wheel model built in the chassis' frame (axle along X) into the
/// wheel: at rest, unsteered and unrolled, it equals the chassis' rotation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct WheelPose {
    /// The vehicle's (chassis') entity.
    pub id: EntityId,
    /// The wheel's index in [`VehicleDesc::wheels`].
    pub wheel: u32,
    /// The wheel's centre, world space.
    pub pos: Vec3,
    /// The wheel's rotation, world space.
    pub rot: Quat,
}

/// One change to the physics world.
///
/// Serialized with a `cmd` tag, for example `{"cmd": "remove_body", "id": 7}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum PhysicsCmd {
    /// Create a body for an entity.
    AddBody {
        /// The entity.
        id: EntityId,
        /// The body.
        desc: BodyDesc,
    },
    /// Remove an entity's body.
    RemoveBody {
        /// The entity.
        id: EntityId,
    },
    /// Teleport a body.
    SetTransform {
        /// The entity.
        id: EntityId,
        /// New world position.
        pos: Vec3,
        /// New world rotation.
        rot: Quat,
    },
    /// Move a kinematic body to a target over the next step.
    SetKinematicTarget {
        /// The entity.
        id: EntityId,
        /// Target world position.
        pos: Vec3,
        /// Target world rotation.
        rot: Quat,
    },
    /// Apply an impulse at the centre of mass (N·s).
    ApplyImpulse {
        /// The entity.
        id: EntityId,
        /// The impulse.
        impulse: Vec3,
    },
    /// Set a body's velocities. On a character, sets its linear velocity for the
    /// next step (angular is ignored); an upward speed makes it jump.
    SetVelocity {
        /// The entity.
        id: EntityId,
        /// Linear velocity (m/s).
        linear: Vec3,
        /// Angular velocity (rad/s).
        angular: Vec3,
    },
    /// Replace a body's shape.
    SetShape {
        /// The entity.
        id: EntityId,
        /// The new shape.
        shape: ShapeDesc,
    },
    /// Create a character for an entity.
    AddCharacter {
        /// The entity.
        id: EntityId,
        /// The character.
        desc: CharacterDesc,
        /// Initial world position (the capsule's centre).
        pos: Vec3,
        /// Initial world rotation.
        rot: Quat,
    },
    /// Remove an entity's character.
    RemoveCharacter {
        /// The entity.
        id: EntityId,
    },
    /// Ask a character to move this step.
    MoveCharacter {
        /// The entity.
        id: EntityId,
        /// Desired horizontal velocity (m/s); gravity is applied by physics.
        desired_velocity: Vec3,
        /// Jump if on the ground.
        jump: bool,
    },
    /// Create a constraint, owned by its own entity, between two bodies.
    AddConstraint {
        /// The constraint's entity.
        id: EntityId,
        /// The constraint.
        desc: ConstraintDesc,
    },
    /// Remove a constraint.
    RemoveConstraint {
        /// The constraint's entity.
        id: EntityId,
    },
    /// Remove an entity's vehicle; its chassis body stays. Removing the chassis body
    /// removes its vehicle too.
    RemoveVehicle {
        /// The chassis entity.
        id: EntityId,
    },
    /// Put a vehicle on an entity's dynamic body, its chassis (ADR-0040).
    AddVehicle {
        /// The chassis entity, which must have a dynamic body.
        id: EntityId,
        /// The vehicle.
        desc: VehicleDesc,
    },
    /// Set what the driver asks of a vehicle; it holds until the next input.
    SetVehicleInput {
        /// The chassis entity.
        id: EntityId,
        /// The input.
        input: VehicleInput,
    },
    /// Remove an entity's ragdoll and its parts.
    RemoveRagdoll {
        /// The entity.
        id: EntityId,
    },
    /// Build a ragdoll for an entity (ADR-0040), animated, in `pose` (or the
    /// skeleton's neutral pose). An entity has a ragdoll or a body, not both.
    AddRagdoll {
        /// The entity.
        id: EntityId,
        /// The ragdoll.
        desc: RagdollDesc,
        /// World position of the skeleton's model space.
        pos: Vec3,
        /// World rotation of the skeleton's model space.
        rot: Quat,
        /// The starting pose, one model-space transform per skeleton joint.
        pose: Option<Vec<JointPose>>,
    },
    /// Set how a ragdoll moves. Switching from animated keeps the parts' velocities.
    SetRagdollMode {
        /// The entity.
        id: EntityId,
        /// The mode.
        mode: RagdollMode,
    },
    /// Set the pose an animated or powered ragdoll drives to, from this step until
    /// the next target.
    SetRagdollTarget {
        /// The entity.
        id: EntityId,
        /// World position of the skeleton's model space.
        pos: Vec3,
        /// World rotation of the skeleton's model space.
        rot: Quat,
        /// One model-space transform per skeleton joint.
        pose: Vec<JointPose>,
    },
    /// Apply an impulse (N·s) to the part on a joint, or on its nearest ancestor
    /// with a part.
    ApplyRagdollImpulse {
        /// The entity.
        id: EntityId,
        /// The skeleton joint, by name.
        joint: String,
        /// The impulse.
        impulse: Vec3,
    },
}

impl PhysicsCmd {
    /// The entity the command is about.
    pub fn id(&self) -> EntityId {
        match self {
            PhysicsCmd::AddBody { id, .. }
            | PhysicsCmd::RemoveBody { id }
            | PhysicsCmd::SetTransform { id, .. }
            | PhysicsCmd::SetKinematicTarget { id, .. }
            | PhysicsCmd::ApplyImpulse { id, .. }
            | PhysicsCmd::SetVelocity { id, .. }
            | PhysicsCmd::SetShape { id, .. }
            | PhysicsCmd::AddCharacter { id, .. }
            | PhysicsCmd::RemoveCharacter { id }
            | PhysicsCmd::MoveCharacter { id, .. }
            | PhysicsCmd::AddConstraint { id, .. }
            | PhysicsCmd::RemoveConstraint { id }
            | PhysicsCmd::RemoveVehicle { id }
            | PhysicsCmd::AddVehicle { id, .. }
            | PhysicsCmd::SetVehicleInput { id, .. }
            | PhysicsCmd::RemoveRagdoll { id }
            | PhysicsCmd::AddRagdoll { id, .. }
            | PhysicsCmd::SetRagdollMode { id, .. }
            | PhysicsCmd::SetRagdollTarget { id, .. }
            | PhysicsCmd::ApplyRagdollImpulse { id, .. } => *id,
        }
    }

    /// The rank of the command's kind in the apply order: removals first
    /// (constraints before the bodies they join), then creations (constraints after
    /// their bodies), shape changes, teleports, targets, velocities, impulses and
    /// character moves; then the vehicle commands (ADR-0040), ranked after every
    /// earlier kind so those ranks never change: removals, creations (after the
    /// chassis body) and inputs; then the ragdoll commands (ADR-0040): removals,
    /// creations, modes, targets and impulses.
    pub fn kind_rank(&self) -> u8 {
        match self {
            PhysicsCmd::RemoveConstraint { .. } => 0,
            PhysicsCmd::RemoveBody { .. } => 1,
            PhysicsCmd::RemoveCharacter { .. } => 2,
            PhysicsCmd::AddBody { .. } => 3,
            PhysicsCmd::AddCharacter { .. } => 4,
            PhysicsCmd::AddConstraint { .. } => 5,
            PhysicsCmd::SetShape { .. } => 6,
            PhysicsCmd::SetTransform { .. } => 7,
            PhysicsCmd::SetKinematicTarget { .. } => 8,
            PhysicsCmd::SetVelocity { .. } => 9,
            PhysicsCmd::ApplyImpulse { .. } => 10,
            PhysicsCmd::MoveCharacter { .. } => 11,
            PhysicsCmd::RemoveVehicle { .. } => 12,
            PhysicsCmd::AddVehicle { .. } => 13,
            PhysicsCmd::SetVehicleInput { .. } => 14,
            PhysicsCmd::RemoveRagdoll { .. } => 15,
            PhysicsCmd::AddRagdoll { .. } => 16,
            PhysicsCmd::SetRagdollMode { .. } => 17,
            PhysicsCmd::SetRagdollTarget { .. } => 18,
            PhysicsCmd::ApplyRagdollImpulse { .. } => 19,
        }
    }

    /// Whether the command removes something. Removals apply before every other
    /// kind, so a later kind's removal (a vehicle or a ragdoll) still frees its
    /// entity before a creation on the same entity in the same buffer.
    pub fn is_removal(&self) -> bool {
        matches!(
            self,
            PhysicsCmd::RemoveConstraint { .. }
                | PhysicsCmd::RemoveBody { .. }
                | PhysicsCmd::RemoveCharacter { .. }
                | PhysicsCmd::RemoveVehicle { .. }
                | PhysicsCmd::RemoveRagdoll { .. }
        )
    }

    /// The deterministic sort key `(phase, kind, entity)`: removals (phase 0)
    /// before everything else (phase 1), then by [`kind_rank`](Self::kind_rank).
    pub fn sort_key(&self) -> (u8, u8, EntityId) {
        (u8::from(!self.is_removal()), self.kind_rank(), self.id())
    }

    /// Sorts a command buffer by [`sort_key`](Self::sort_key), keeping the relative
    /// order of commands with equal keys (for example two impulses on one body).
    pub fn sort(commands: &mut [PhysicsCmd]) {
        commands.sort_by_key(PhysicsCmd::sort_key);
    }
}

/// A body's pose after a step. Only active (awake) bodies are reported.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BodyPose {
    /// The entity.
    pub id: EntityId,
    /// World position.
    pub pos: Vec3,
    /// World rotation.
    pub rot: Quat,
}

/// A change in contact between two bodies during a step.
///
/// Serialized with an `event` tag (`begin` or `end`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ContactEvent {
    /// The bodies started touching (or a body entered a sensor).
    Begin {
        /// The first entity (the smaller id).
        a: EntityId,
        /// The second entity.
        b: EntityId,
        /// A contact point in world space.
        point: Vec3,
        /// The contact normal, pointing from `a` to `b`.
        normal: Vec3,
        /// True when one of the two is a sensor.
        sensor: bool,
    },
    /// The bodies stopped touching (or a body left a sensor). One of them may
    /// already have been removed.
    End {
        /// The first entity (the smaller id).
        a: EntityId,
        /// The second entity.
        b: EntityId,
        /// True when one of the two is a sensor.
        sensor: bool,
    },
}

impl ContactEvent {
    /// The two entities, smaller id first.
    pub fn pair(&self) -> (EntityId, EntityId) {
        match *self {
            ContactEvent::Begin { a, b, .. } | ContactEvent::End { a, b, .. } => (a, b),
        }
    }
}

/// The closest hit of a ray.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct RayHit {
    /// The entity hit.
    pub entity: EntityId,
    /// The hit point in world space.
    pub point: Vec3,
    /// The surface normal at the hit point.
    pub normal: Vec3,
    /// Distance from the ray origin.
    pub distance: f32,
}

/// Where a character stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum GroundState {
    /// On walkable ground.
    OnGround,
    /// Touching ground too steep to walk on; it slides.
    OnSteepGround,
    /// Touching something, but not supported by it.
    NotSupported,
    /// Falling or jumping.
    #[default]
    InAir,
}

/// A character's state after a step.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CharacterState {
    /// World position (the capsule's centre).
    pub pos: Vec3,
    /// World rotation.
    pub rot: Quat,
    /// Linear velocity (m/s).
    pub velocity: Vec3,
    /// Where it stands.
    pub ground: GroundState,
    /// The ground normal (meaningful unless in the air).
    pub ground_normal: Vec3,
    /// The entity it stands on, if that is a body.
    pub ground_entity: Option<EntityId>,
}

impl CharacterState {
    /// True when standing on walkable ground.
    pub fn on_ground(&self) -> bool {
        self.ground == GroundState::OnGround
    }
}
