//! The sync glue between the World and the subsystems (ADR-0006): each module
//! turns World state into a subsystem's plain-data input (`re_types`) and, for
//! physics, applies the results back. Sync uses typed bevy queries through
//! `re_scene::World::ecs`, never by-name access.

pub mod anim;
pub mod audio;
pub mod physics;
pub mod render;
