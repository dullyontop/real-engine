//! The change journal (ADR-0048 amendment, WP27 "incremental render"): which
//! entities changed since a reader last drained it, so a reader that keeps
//! state per entity (the compat view) visits only those.
//!
//! It records only while the world has a [`ChangeJournal`] resource, from
//! [`World::record_changes`](crate::World::record_changes):
//! * every registry write that changes a component (`re_reflect`'s change
//!   hook: `set_field` with a new value, `insert`, `remove`);
//! * every change of parent, spawn under a parent and detach, from
//!   observers on `ChildOf`;
//! * moves written to `Transform` outside the registry, which their writer
//!   reports with [`note_moved`].
//!
//! A world without the resource records nothing, and its registry has no
//! hook installed.

use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::ChildOf;
use bevy_ecs::lifecycle::{Discard, Insert};
use bevy_ecs::observer::On;
use bevy_ecs::prelude::Resource;
use bevy_ecs::system::ResMut;
use bevy_ecs::world::World as EcsWorld;
use re_reflect::ComponentKey;

/// What changed since the last [`ChangeJournal::drain`].
#[derive(Resource, Debug, Default)]
pub struct ChangeJournal {
    /// Components written through the registry, in order, with repeats.
    pub fields: Vec<(Entity, ComponentKey)>,
    /// Entities whose parent changed, or that were spawned under a parent
    /// or detached (a despawn detaches).
    pub structure: Vec<Entity>,
    /// Entities whose `Transform` was written outside the registry.
    pub moved: Vec<Entity>,
}

impl ChangeJournal {
    /// Takes everything recorded.
    pub fn drain(&mut self) -> ChangeJournal {
        std::mem::take(self)
    }

    /// True when nothing is recorded.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.structure.is_empty() && self.moved.is_empty()
    }
}

/// Marks that the observers are installed in a world.
#[derive(Resource)]
struct JournalObservers;

/// The registry's change hook while the journal records.
pub(crate) fn field_changed(world: &mut EcsWorld, entity: Entity, key: ComponentKey) {
    if let Some(mut j) = world.get_resource_mut::<ChangeJournal>() {
        j.fields.push((entity, key));
    }
}

fn parented(ev: On<Insert<ChildOf>>, journal: Option<ResMut<ChangeJournal>>) {
    if let Some(mut j) = journal {
        j.structure.push(ev.event().entity);
    }
}

fn unparented(ev: On<Discard<ChildOf>>, journal: Option<ResMut<ChangeJournal>>) {
    if let Some(mut j) = journal {
        j.structure.push(ev.event().entity);
    }
}

/// Installs the journal's observers once per world.
pub(crate) fn install(world: &mut EcsWorld) {
    if !world.contains_resource::<JournalObservers>() {
        world.insert_resource(JournalObservers);
        world.add_observer(parented);
        world.add_observer(unparented);
    }
    world.init_resource::<ChangeJournal>();
}

/// Records that `entity`'s `Transform` was written without the registry (a
/// physics write-back). Does nothing when the world keeps no journal.
pub fn note_moved(world: &mut EcsWorld, entity: Entity) {
    if let Some(mut j) = world.get_resource_mut::<ChangeJournal>() {
        j.moved.push(entity);
    }
}
