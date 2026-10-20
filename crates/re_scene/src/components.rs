//! Built-in components (ARCHITECTURE §4; fields frozen in docs/M1_PLAN.md, WP0).
//!
//! Everything a scene file, script or agent can name is declared with
//! [`component!`] and registered by [`register_builtins`]. `Name` (the file's
//! `name`), [`StableId`](crate::StableId) (the file's `id`), [`GlobalTransform`],
//! [`Generated`](crate::Generated) and the hierarchy are not registry
//! components.

use bevy_ecs::component::Component;
use bevy_ecs::world::World as EcsWorld;
use glam::{Affine3A, Quat, Vec3};
use re_reflect::{Color, Registry, component, field_enum};

use crate::procedural::{GraphSource, Procedural};

/// Display name. Not unique; written to scene files as the entity's `name`.
#[derive(Component, Clone, Debug, PartialEq, Eq, Default)]
pub struct Name(pub String);

component! {
    /// Position, rotation and scale relative to the parent entity (or to the
    /// world for a root entity).
    pub struct Transform requires(GlobalTransform) {
        /// Offset from the parent's origin, in metres.
        pub translation: Vec3 = Vec3::ZERO,
        /// Orientation relative to the parent. Normalised on write.
        pub rotation: Quat = Quat::IDENTITY,
        /// Per-axis scale relative to the parent.
        pub scale: Vec3 = Vec3::ONE,
    }
}

impl Transform {
    /// A transform with only a translation.
    pub fn from_translation(translation: Vec3) -> Self {
        Self {
            translation,
            ..Default::default()
        }
    }

    /// The transform as an affine matrix (scale, then rotation, then translation).
    pub fn affine(&self) -> Affine3A {
        Affine3A::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }
}

/// World-space transform, derived from [`Transform`] and the hierarchy by
/// transform propagation. Never written to files.
#[derive(Component, Clone, Copy, Debug, PartialEq, Default)]
pub struct GlobalTransform(pub Affine3A);

component! {
    /// The mesh drawn for this entity.
    pub struct MeshRef {
        /// An asset path (`assets/crate.glb`), or `derived:<hash>` for a
        /// procedurally generated mesh.
        pub asset: String = String::new(),
    }
}

component! {
    /// The material the mesh is drawn with.
    pub struct MaterialRef {
        /// A material file path (`materials/stone.material.json`).
        pub asset: String = String::new(),
    }
}

component! {
    /// Whether the entity is drawn.
    pub struct Visible {
        /// False hides the entity (it still simulates).
        pub visible: bool = true,
    }
}

component! {
    /// A perspective camera. The active camera renders the frame.
    pub struct Camera {
        /// Vertical field of view, in degrees.
        pub fov_y_deg: f32 = 60.0,
        /// Near clipping distance, in metres.
        pub near: f32 = 0.1,
        /// Far clipping distance, in metres.
        pub far: f32 = 1000.0,
        /// True for the camera that renders the frame (one at a time).
        pub active: bool = false,
    }
}

field_enum! {
    /// The kind of a [`Light`].
    pub enum LightKind { Directional, Point }
}

component! {
    /// A light source. A directional light shines along the entity's -Z axis.
    pub struct Light {
        /// Directional (sun-like) or point.
        pub kind: LightKind = LightKind::Point,
        /// Linear colour.
        pub color: Color = Color::WHITE,
        /// Intensity: lumens for point lights, lux for directional lights.
        pub intensity: f32 = 800.0,
        /// Distance in metres beyond which a point light has no effect.
        pub range: f32 = 20.0,
        /// Whether the light casts shadows.
        pub shadows: bool = false,
    }
}

field_enum! {
    /// How the physics engine treats a body.
    pub enum BodyKind { Static, Dynamic, Kinematic }
}

component! {
    /// Makes the entity a physics body. Needs a [`Collider`] to collide; a
    /// [`Collider`] without a `RigidBody` is a static body.
    pub struct RigidBody {
        /// Static (never moves), dynamic (simulated) or kinematic (moved by code).
        pub kind: BodyKind = BodyKind::Static,
        /// Mass in kilograms (dynamic bodies only).
        pub mass: f32 = 1.0,
        /// Friction coefficient.
        pub friction: f32 = 0.5,
        /// Restitution (bounciness), 0 to 1.
        pub restitution: f32 = 0.0,
        /// Linear velocity damping per second.
        pub linear_damping: f32 = 0.05,
        /// Angular velocity damping per second.
        pub angular_damping: f32 = 0.05,
        /// Moves the centre of mass from the shape's own, in the body's local
        /// space, in metres (dynamic bodies only). A vehicle chassis lowers it to
        /// stay upright.
        pub center_of_mass_offset: Vec3 = Vec3::ZERO,
    }
}

field_enum! {
    /// The shape of a [`Collider`].
    pub enum ColliderShape { Box, Sphere, Capsule, Convex, Mesh, Heightfield }
}

component! {
    /// A collision shape, in the entity's local space.
    ///
    /// `re_app` maps it to a `re_types::physics::BodyDesc`: without a
    /// [`RigidBody`] the body is static, and `enabled = false` removes the body.
    /// `Mesh` and `Heightfield` shapes are static only (`re_physics` refuses them
    /// on dynamic and kinematic bodies with `RE5003`).
    pub struct Collider {
        /// The shape kind.
        pub shape: ColliderShape = ColliderShape::Box,
        /// Half extents of a box, in metres.
        pub half_extents: Vec3 = Vec3::splat(0.5),
        /// Radius of a sphere or capsule, in metres.
        pub radius: f32 = 0.5,
        /// Half height of a capsule's cylindrical part, in metres.
        pub half_height: f32 = 0.5,
        /// The mesh or heightfield asset for Convex, Mesh and Heightfield shapes. A
        /// heightfield asset carries its sample count per side (`n`), `spacing` and
        /// `offset` as well as the samples.
        pub source: String = String::new(),
        /// Object layer, 0 to 65535 (`re_types::physics::Layer`); the physics layer
        /// table decides which layers collide.
        pub layer: i32 = 0,
        /// Collision group bit mask, 0 to `u32::MAX` (`re_types::physics::GroupMask`:
        /// bit `g` is group `g`, so the default 1 is group 0). Queries and characters
        /// only see bodies whose groups intersect theirs.
        pub groups: i64 = 1,
        /// A sensor reports contacts but does not collide.
        pub sensor: bool = false,
        /// False removes the body from the simulation.
        pub enabled: bool = true,
    }
}

component! {
    /// A kinematic character (Jolt CharacterVirtual).
    pub struct CharacterController {
        /// Total height, in metres.
        pub height: f32 = 1.8,
        /// Capsule radius, in metres.
        pub radius: f32 = 0.4,
        /// Steepest walkable slope, in degrees.
        pub max_slope_deg: f32 = 45.0,
        /// Highest step climbed without jumping, in metres.
        pub step_height: f32 = 0.3,
        /// Mass in kilograms, used when pushing bodies.
        pub mass: f32 = 70.0,
        /// True adds an inner rigid body, so the character takes part in contact events.
        pub inner_body: bool = true,
        /// Object layer, 0 to 65535, as in [`Collider`].
        pub layer: i32 = 0,
        /// Collision group bit mask, 0 to `u32::MAX`, as in [`Collider`]. The
        /// character only collides with bodies whose groups intersect these.
        pub groups: i64 = 1,
    }
}

field_enum! {
    /// The script context a [`ScriptRef`] runs in. `Game` and `Tools` are the
    /// native contexts. `Server`, `Client` and `Module` are the compatibility
    /// layer's (ADR-0012): a `Script` runs in `Server`, a `LocalScript` in
    /// `Client`, and a `Module` script runs only when required. The native
    /// runtime starts `Game` and `Tools` scripts only.
    pub enum ScriptContext { Game, Tools, Server, Client, Module }
}

component! {
    /// A Luau script attached to the entity.
    pub struct ScriptRef {
        /// Script path relative to the project (`scripts/game/player.luau`).
        pub path: String = String::new(),
        /// The context (VM) the script runs in.
        pub context: ScriptContext = ScriptContext::Game,
    }
}

component! {
    /// A sound emitter.
    pub struct AudioSource {
        /// A project asset path such as `assets/sfx/pickup.wav`, as in `AudioCmd`.
        pub clip: String = String::new(),
        /// Linear volume, 0 to 1.
        pub volume: f32 = 1.0,
        /// Restart the clip when it ends.
        pub looping: bool = false,
        /// Attenuate and pan by the entity's position.
        pub spatial: bool = true,
        /// Start playing when the entity appears.
        pub autoplay: bool = false,
    }
}

component! {
    /// Plays a node animation clip from a glTF asset.
    pub struct AnimPlayer {
        /// `<model asset path>#<clip name>`, e.g. `assets/models/door.glb#open`; the
        /// clip name is `ClipData.name` (the glTF animation name, or
        /// `animation_<index>`). Playback time is runtime state in `re_app`, not a
        /// field.
        pub clip: String = String::new(),
        /// Playback speed multiplier.
        pub speed: f32 = 1.0,
        /// Restart the clip when it ends.
        pub looping: bool = true,
        /// Whether the clip advances.
        pub playing: bool = true,
    }
}

field_enum! {
    /// Where a [`HudText`] is anchored on screen.
    pub enum HudAnchor { TopLeft, Top, TopRight, Center, BottomLeft, Bottom, BottomRight }
}

component! {
    /// A line of text drawn on the heads-up display.
    pub struct HudText {
        /// The text.
        pub text: String = String::new(),
        /// Screen anchor.
        pub anchor: HudAnchor = HudAnchor::TopLeft,
        /// Font size in logical pixels.
        pub size: f32 = 18.0,
    }
}

field_enum! {
    /// The vehicle model a [`Vehicle`] uses.
    pub enum VehicleKind { Wheeled, Motorcycle }
}

field_enum! {
    /// How a [`Vehicle`]'s wheels find the ground (ADR-0040).
    pub enum WheelTester { Ray, CastSphere, CastCylinder }
}

component! {
    /// Makes a dynamic [`RigidBody`] a wheeled vehicle chassis (ADR-0040). Its
    /// wheels are the child entities with a [`VehicleWheel`], in child order;
    /// each wheel's [`Transform`] when the vehicle is created is its attachment
    /// point, and `re_app` writes the simulated wheel pose back to it. Scripts
    /// drive it with `physics.drive`.
    pub struct Vehicle {
        /// Wheeled (any number of wheels) or motorcycle (two wheels that lean).
        pub kind: VehicleKind = VehicleKind::Wheeled,
        /// Engine torque at full throttle, in newton metres.
        pub max_torque: f32 = 500.0,
        /// Idle engine speed, in revolutions per minute.
        pub min_rpm: f32 = 1000.0,
        /// Highest engine speed, in revolutions per minute.
        pub max_rpm: f32 = 6000.0,
        /// Forward gear ratios, first gear first.
        pub gears: Vec<f32> = vec![2.66, 1.78, 1.3, 1.0, 0.74],
        /// Reverse gear ratios (negative).
        pub reverse_gears: Vec<f32> = vec![-2.9],
        /// Engine speed that shifts up, in revolutions per minute.
        pub shift_up_rpm: f32 = 4000.0,
        /// Engine speed that shifts down, in revolutions per minute.
        pub shift_down_rpm: f32 = 2000.0,
        /// Ratio between gearbox and driven wheels.
        pub differential_ratio: f32 = 3.42,
        /// Anti-roll bar stiffness on each two-wheel axle; 0 adds none.
        pub anti_roll_stiffness: f32 = 1000.0,
        /// Steepest pitch or roll the chassis may reach, in degrees; 180 is free.
        pub max_pitch_roll_deg: f32 = 180.0,
        /// How the wheels find the ground.
        pub tester: WheelTester = WheelTester::Ray,
        /// Sphere radius of the `CastSphere` tester, in metres.
        pub tester_radius: f32 = 0.3,
    }
}

component! {
    /// One wheel of the [`Vehicle`] on the entity's parent.
    pub struct VehicleWheel {
        /// Radius, in metres.
        pub radius: f32 = 0.3,
        /// Width, in metres.
        pub width: f32 = 0.1,
        /// Shortest suspension length, in metres.
        pub suspension_min: f32 = 0.3,
        /// Longest suspension length, in metres.
        pub suspension_max: f32 = 0.5,
        /// Suspension spring frequency, in hertz.
        pub suspension_frequency: f32 = 1.5,
        /// Suspension damping ratio.
        pub suspension_damping: f32 = 0.5,
        /// Steering angle at full steer, in degrees; 0 does not steer.
        pub max_steer_deg: f32 = 0.0,
        /// Brake torque at full brake, in newton metres.
        pub max_brake_torque: f32 = 1500.0,
        /// Hand brake torque, in newton metres; 0 has no hand brake.
        pub max_hand_brake_torque: f32 = 0.0,
        /// True if the engine drives this wheel.
        pub driven: bool = false,
    }
}

/// Registers every built-in component, in a fixed order: `Transform`,
/// `MeshRef`, `MaterialRef`, `Visible`, `Camera`, `Light`, `RigidBody`,
/// `Collider`, `CharacterController`, `ScriptRef`, `AudioSource`, `AnimPlayer`,
/// `HudText`, `Procedural`, `GraphSource`, `Attributes`, `Tags`, `Vehicle`,
/// `VehicleWheel`.
pub fn register_builtins(registry: &mut Registry, world: &mut EcsWorld) {
    registry.register::<Transform>(world);
    registry.register::<MeshRef>(world);
    registry.register::<MaterialRef>(world);
    registry.register::<Visible>(world);
    registry.register::<Camera>(world);
    registry.register::<Light>(world);
    registry.register::<RigidBody>(world);
    registry.register::<Collider>(world);
    registry.register::<CharacterController>(world);
    registry.register::<ScriptRef>(world);
    registry.register::<AudioSource>(world);
    registry.register::<AnimPlayer>(world);
    registry.register::<HudText>(world);
    registry.register::<Procedural>(world);
    registry.register::<GraphSource>(world);
    registry.register::<crate::Attributes>(world);
    registry.register::<crate::Tags>(world);
    registry.register::<Vehicle>(world);
    registry.register::<VehicleWheel>(world);
}
