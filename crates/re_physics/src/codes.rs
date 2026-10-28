//! The diagnostic codes `re_physics` owns (range `RE5000`–`RE5999`).
//!
//! Each code has an explain page in `crates/re_physics/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// The physics world could not be created from its [`PhysicsConfig`](crate::PhysicsConfig).
pub const WORLD_CREATE_FAILED: Code = Code::new("RE5001");

/// A collision shape is malformed or Jolt refused it.
pub const SHAPE_INVALID: Code = Code::new("RE5002");

/// A triangle mesh was given to a dynamic body, or a heightfield to a body that can move.
pub const STATIC_ONLY_SHAPE: Code = Code::new("RE5003");

/// A command names an entity that has no body, character or constraint.
pub const UNKNOWN_ENTITY: Code = Code::new("RE5004");

/// A body, character or constraint was added for an entity that already has one.
pub const DUPLICATE_ENTITY: Code = Code::new("RE5005");

/// A position lies outside the world limits (±5 km with single precision).
pub const OUTSIDE_WORLD: Code = Code::new("RE5006");

/// A body's numbers (mass, friction, rotation, velocity, ...) are out of range.
pub const BODY_INVALID: Code = Code::new("RE5007");

/// A constraint is malformed, names a missing body, or Jolt refused it.
pub const CONSTRAINT_INVALID: Code = Code::new("RE5008");

/// A character's description is out of range or Jolt refused it.
pub const CHARACTER_INVALID: Code = Code::new("RE5009");

/// A command does not suit the body's kind (an impulse on a static body, a target on
/// a dynamic one).
pub const WRONG_BODY_KIND: Code = Code::new("RE5010");

/// A step dropped contacts because a fixed-size buffer was full.
pub const STEP_INCOMPLETE: Code = Code::new("RE5011");

/// A saved state could not be restored.
pub const RESTORE_FAILED: Code = Code::new("RE5012");

/// A triangle mesh lost degenerate triangles.
pub const MESH_TRIANGLES_DROPPED: Code = Code::new("RE5013");

/// Removing a body also removed the constraints attached to it.
pub const CONSTRAINT_REMOVED_WITH_BODY: Code = Code::new("RE5014");

/// A layer is not in the world's layer table.
pub const UNKNOWN_LAYER: Code = Code::new("RE5015");

/// A step length or query argument is out of range.
pub const INVALID_ARGUMENT: Code = Code::new("RE5016");

/// An assembly's part list is empty or malformed, or Jolt refused its shape.
pub const ASSEMBLY_INVALID: Code = Code::new("RE5017");

/// A vehicle could not be created or driven: no dynamic chassis body, no wheels or
/// driven wheels, an input or setting out of range.
pub const VEHICLE_INVALID: Code = Code::new("RE5018");

/// A ragdoll could not be created, posed or driven: a part names a missing joint, the
/// parts have no single root, limits or masses are out of range, or a pose does not
/// fit the skeleton.
pub const RAGDOLL_INVALID: Code = Code::new("RE5019");
