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
//! hook installed. A journal nobody drains keeps at most
//! [`ChangeJournal::CAP`] entries: past that it empties itself and says it
//! overflowed, so its reader looks at everything once.

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
    /// More than [`ChangeJournal::CAP`] entries came in since the last
    /// drain (no reader drained for a long time): the lists were cleared
    /// and stay empty, and the reader must look at everything.
    pub overflowed: bool,
}

impl ChangeJournal {
    /// The most entries kept between two drains, in all three lists.
    pub const CAP: usize = 65_536;

    /// Takes everything recorded.
    pub fn drain(&mut self) -> ChangeJournal {
        std::mem::take(self)
    }

    /// True when nothing is recorded.
    pub fn is_empty(&self) -> bool {
        !self.overflowed
            && self.fields.is_empty()
            && self.structure.is_empty()
            && self.moved.is_empty()
    }

    /// True when one more entry may be kept; past [`ChangeJournal::CAP`] the
    /// journal clears its lists and marks itself overflowed.
    fn admit(&mut self) -> bool {
        if self.overflowed {
            return false;
        }
        if self.fields.len() + self.structure.len() + self.moved.len() >= Self::CAP {
            *self = ChangeJournal {
                overflowed: true,
                ..ChangeJournal::default()
            };
            return false;
        }
        true
    }
}

/// Marks that the observers are installed in a world.
#[derive(Resource)]
struct JournalObservers;

/// The registry's change hook while the journal records.
pub(crate) fn field_changed(world: &mut EcsWorld, entity: Entity, key: ComponentKey) {
    if let Some(mut j) = world.get_resource_mut::<ChangeJournal>()
        && j.admit()
    {
        j.fields.push((entity, key));
    }
}

fn parented(ev: On<Insert<ChildOf>>, journal: Option<ResMut<ChangeJournal>>) {
    if let Some(mut j) = journal
        && j.admit()
    {
        j.structure.push(ev.event().entity);
    }
}

fn unparented(ev: On<Discard<ChildOf>>, journal: Option<ResMut<ChangeJournal>>) {
    if let Some(mut j) = journal
        && j.admit()
    {
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
    if let Some(mut j) = world.get_resource_mut::<ChangeJournal>()
        && j.admit()
    {
        j.moved.push(entity);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_journal_nobody_drains_stops_at_its_cap() {
        let mut w = EcsWorld::new();
        install(&mut w);
        let e = w.spawn_empty().id();
        for _ in 0..ChangeJournal::CAP {
            note_moved(&mut w, e);
        }
        let j = w.resource::<ChangeJournal>();
        assert_eq!(j.moved.len(), ChangeJournal::CAP);
        assert!(!j.overflowed);

        note_moved(&mut w, e);
        note_moved(&mut w, e);
        let j = w.resource::<ChangeJournal>();
        assert!(j.overflowed && j.moved.is_empty());
        assert!(!j.is_empty());

        let drained = w.resource_mut::<ChangeJournal>().drain();
        assert!(drained.overflowed);
        note_moved(&mut w, e);
        let j = w.resource::<ChangeJournal>();
        assert!(!j.overflowed);
        assert_eq!(j.moved, [e]);
    }
}
