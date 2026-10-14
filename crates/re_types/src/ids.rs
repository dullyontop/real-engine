//! Entity identity across the contract boundary.

use std::fmt;

use serde::{Deserialize, Serialize};

/// An entity, as subsystems see it.
///
/// It carries the bits of the ECS entity handle (`bevy_ecs::entity::Entity::to_bits()`).
/// Only `re_scene` converts between the two; every other crate treats the value as
/// an opaque key. It is not the entity's stable string id from the scene file,
/// and it is not stable across runs.
///
/// Serialized as a bare number.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntityId(pub u64);

impl EntityId {
    /// Wraps raw entity bits.
    pub const fn from_bits(bits: u64) -> EntityId {
        EntityId(bits)
    }

    /// The raw entity bits.
    pub const fn to_bits(self) -> u64 {
        self.0
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}
