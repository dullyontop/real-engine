//! What crosses the physics boundary (ADR-0006, ADR-0008).
//!
//! `re_app` turns World changes into a buffer of [`PhysicsCmd`]s, sorted by
//! [`PhysicsCmd::sort_key`] so that the order is deterministic, and hands it to
//! `re_physics`. After the step it pulls [`BodyPose`]s (active bodies only, position
//! and rotation, no velocities) and [`ContactEvent`]s back.

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::ids::EntityId;

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
    },
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
            } => {
                *n >= 2
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
            | PhysicsCmd::RemoveConstraint { id } => *id,
        }
    }

    /// The rank of the command's kind in the apply order: removals first
    /// (constraints before the bodies they join), then creations (constraints after
    /// their bodies), shape changes, teleports, targets, velocities, impulses and
    /// character moves.
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
        }
    }

    /// The deterministic sort key `(kind, entity)`.
    pub fn sort_key(&self) -> (u8, EntityId) {
        (self.kind_rank(), self.id())
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
