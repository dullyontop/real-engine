//! `re_physics`: Jolt Physics behind a command-buffer API keyed by
//! [`EntityId`](re_types::EntityId) (ADR-0008).
//!
//! The rest of the engine never sees a Jolt type. It sends
//! [`PhysicsCmd`](re_types::physics::PhysicsCmd)s, steps, and reads back
//! [`BodyPose`](re_types::physics::BodyPose)s,
//! [`ContactEvent`](re_types::physics::ContactEvent)s and
//! [`CharacterState`](re_types::physics::CharacterState)s:
//!
//! ```
//! use re_core::TaskPool;
//! use re_physics::{Physics, PhysicsConfig};
//! use re_types::EntityId;
//! use re_types::glam::Vec3;
//! use re_types::physics::{BodyDesc, BodyKind, PhysicsCmd, ShapeDesc};
//!
//! let mut physics = Physics::new(PhysicsConfig::default(), TaskPool::global()).unwrap();
//! let ball = EntityId(1);
//! let mut cmds = vec![PhysicsCmd::AddBody {
//!     id: ball,
//!     desc: BodyDesc {
//!         kind: BodyKind::Dynamic,
//!         shape: ShapeDesc::Sphere { radius: 0.5 },
//!         pos: Vec3::new(0.0, 3.0, 0.0),
//!         ..BodyDesc::default()
//!     },
//! }];
//! physics.apply(&mut cmds);
//! physics.step(1.0 / 60.0).unwrap();
//! let mut poses = Vec::new();
//! physics.poses_into(&mut poses);
//! assert!(poses[0].pos.y < 3.0);
//! ```
//!
//! What the wrapper adds over Jolt (the gotchas of ADR-0008):
//!
//! * commands are sorted by `(kind, entity)` before they apply, so results do not
//!   depend on the order scripts issued them;
//! * Jolt's jobs run on the engine's one [`TaskPool`](re_core::TaskPool);
//! * contacts are reported per body pair, with removed bodies' contacts ended at once
//!   and Jolt's late reports about them dropped;
//! * a kinematic body that stops receiving targets stops, instead of drifting on;
//! * the step a body falls asleep in reports its final pose;
//! * meshes and heightfields on moving bodies are refused with `RE5003`, positions
//!   beyond ±5 km with `RE5006`, never with a panic.
//!
//! Limits that stay: rest heights include up to 0.02 m of Jolt's penetration slop
//! (oxijolt 1.1.0 does not expose `PhysicsSettings`); a body's position is its shape
//! origin, not its centre of mass; oxijolt never calls `JPH_Shutdown`, which is
//! harmless at process exit. Diagnostic codes are `RE5000`–`RE5999`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod codes;
mod config;
mod convert;
mod jobs;
pub mod parts;
mod query;
mod shapes;
mod world;

pub use config::{LayerTable, PhysicsConfig};
pub use parts::{
    AssemblyDesc, AssemblyInfo, GroupTable, HingeDrive, JointDesc, JointKind, PartDesc, PartEvent,
    PartFilter, PartGeom, PartHit, PartMaterial, PartShape, PartView, Pose,
};
pub use query::QueryFilter;
pub use world::{BodyMotion, MotionState, Physics, PhysicsSnapshot, WORLD_LIMIT};

use re_core::DiagInfo;

/// Every diagnostic code `re_physics` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::WORLD_CREATE_FAILED,
        title: "The physics world could not be created",
        page: include_str!("../explain/RE5001.md"),
    },
    DiagInfo {
        code: codes::SHAPE_INVALID,
        title: "Invalid collision shape",
        page: include_str!("../explain/RE5002.md"),
    },
    DiagInfo {
        code: codes::STATIC_ONLY_SHAPE,
        title: "Mesh or heightfield collider on a moving body",
        page: include_str!("../explain/RE5003.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_ENTITY,
        title: "Physics command for an entity without a body",
        page: include_str!("../explain/RE5004.md"),
    },
    DiagInfo {
        code: codes::DUPLICATE_ENTITY,
        title: "Entity already has a body",
        page: include_str!("../explain/RE5005.md"),
    },
    DiagInfo {
        code: codes::OUTSIDE_WORLD,
        title: "Position outside the world limits",
        page: include_str!("../explain/RE5006.md"),
    },
    DiagInfo {
        code: codes::BODY_INVALID,
        title: "Invalid body settings",
        page: include_str!("../explain/RE5007.md"),
    },
    DiagInfo {
        code: codes::CONSTRAINT_INVALID,
        title: "Invalid constraint",
        page: include_str!("../explain/RE5008.md"),
    },
    DiagInfo {
        code: codes::CHARACTER_INVALID,
        title: "Invalid character",
        page: include_str!("../explain/RE5009.md"),
    },
    DiagInfo {
        code: codes::WRONG_BODY_KIND,
        title: "Command does not suit the body kind",
        page: include_str!("../explain/RE5010.md"),
    },
    DiagInfo {
        code: codes::STEP_INCOMPLETE,
        title: "Physics step dropped contacts",
        page: include_str!("../explain/RE5011.md"),
    },
    DiagInfo {
        code: codes::RESTORE_FAILED,
        title: "Physics state could not be restored",
        page: include_str!("../explain/RE5012.md"),
    },
    DiagInfo {
        code: codes::MESH_TRIANGLES_DROPPED,
        title: "Mesh triangles dropped",
        page: include_str!("../explain/RE5013.md"),
    },
    DiagInfo {
        code: codes::CONSTRAINT_REMOVED_WITH_BODY,
        title: "Constraint removed with its body",
        page: include_str!("../explain/RE5014.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_LAYER,
        title: "Unknown collision layer",
        page: include_str!("../explain/RE5015.md"),
    },
    DiagInfo {
        code: codes::INVALID_ARGUMENT,
        title: "Invalid step length or query",
        page: include_str!("../explain/RE5016.md"),
    },
    DiagInfo {
        code: codes::ASSEMBLY_INVALID,
        title: "Invalid assembly of parts",
        page: include_str!("../explain/RE5017.md"),
    },
];

#[cfg(test)]
mod tests {
    #[test]
    fn catalogue_is_valid() {
        let problems = re_core::validate_catalog(super::DIAGNOSTICS, 5000..=5999);
        assert!(problems.is_empty(), "{problems:?}");
    }

    /// The app may step physics on a worker thread (ADR-0008).
    #[test]
    fn physics_is_send() {
        fn send<T: Send>() {}
        send::<super::Physics>();
        send::<super::PhysicsSnapshot>();
    }
}
