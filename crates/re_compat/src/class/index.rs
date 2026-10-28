//! [`ClassIndex`]: the instances of each class, kept exact by
//! [`ClassMarker`]'s hooks, so finding every instance of a class visits
//! those instances only instead of every entity in the world.
//!
//! The hooks keep a world's index once it has one. A world gets its index
//! the first time one is asked for ([`ClassIndex::of`]), from one walk over
//! its instances, because a world can hold instances the hooks never
//! indexed: a fork, or a scene loaded before the first question.

use std::collections::{BTreeSet, HashMap};

use re_scene::ecs as bevy_ecs;
use re_scene::{Entity, World};

use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::world::DeferredWorld;

use super::{ClassMarker, ClassRegistry};

/// The live instances of each class, by exact class name, each set in
/// `Entity` order.
#[derive(bevy_ecs::resource::Resource, Default, Debug)]
pub struct ClassIndex {
    map: HashMap<String, BTreeSet<Entity>>,
}

impl ClassIndex {
    /// The index of `world`, made from a walk over its instances if it has
    /// none yet.
    pub fn of(world: &mut World) -> &ClassIndex {
        if !world.ecs.contains_resource::<ClassIndex>() {
            let mut index = ClassIndex::default();
            let mut q = world.ecs.query::<(Entity, &ClassMarker)>();
            for (e, m) in q.iter(&world.ecs) {
                index.add(&m.class, e);
            }
            world.ecs.insert_resource(index);
        }
        world.ecs.resource::<ClassIndex>()
    }

    /// The instances whose class is exactly `class` (not a subclass), in
    /// `Entity` order.
    pub fn exactly(&self, class: &str) -> impl Iterator<Item = Entity> + '_ {
        self.map.get(class).into_iter().flatten().copied()
    }

    fn add(&mut self, class: &str, entity: Entity) {
        match self.map.get_mut(class) {
            Some(set) => {
                set.insert(entity);
            }
            None => {
                self.map.insert(class.to_string(), BTreeSet::from([entity]));
            }
        }
    }

    fn remove(&mut self, class: &str, entity: Entity) {
        if let Some(set) = self.map.get_mut(class) {
            set.remove(&entity);
            if set.is_empty() {
                self.map.remove(class);
            }
        }
    }
}

impl ClassRegistry {
    /// Every instance of `class` or of a class that inherits from it, in
    /// `Entity` order (the order `sort` gives), from the world's
    /// [`ClassIndex`]. Empty for an unknown class.
    pub fn instances(&self, world: &mut World, class: &str) -> Vec<Entity> {
        let Some(id) = self.id(class) else {
            return Vec::new();
        };
        let index = ClassIndex::of(world);
        let mut out = Vec::new();
        let mut sets = 0;
        for &c in self.family(id) {
            if let Some(set) = index.map.get(&self.def(c).name) {
                out.extend(set.iter().copied());
                sets += 1;
            }
        }
        // Each set is in order already; only a merge of several needs a sort.
        if sets > 1 {
            out.sort_unstable();
        }
        out
    }

    /// Every instance whose class is exactly `class` (not a subclass), in
    /// `Entity` order.
    pub fn instances_exactly(&self, world: &mut World, class: &str) -> Vec<Entity> {
        ClassIndex::of(world).exactly(class).collect()
    }
}

pub(super) fn marker_inserted(mut world: DeferredWorld, ctx: HookContext) {
    if let Some(class) = indexed_class(&world, ctx)
        && let Some(mut index) = world.get_resource_mut::<ClassIndex>()
    {
        index.add(&class, ctx.entity);
    }
}

pub(super) fn marker_discarded(mut world: DeferredWorld, ctx: HookContext) {
    if let Some(class) = indexed_class(&world, ctx)
        && let Some(mut index) = world.get_resource_mut::<ClassIndex>()
    {
        index.remove(&class, ctx.entity);
    }
}

/// The class in the hooked entity's marker, if the world keeps an index.
fn indexed_class(world: &DeferredWorld, ctx: HookContext) -> Option<String> {
    if !world.contains_resource::<ClassIndex>() {
        return None;
    }
    world
        .get::<ClassMarker>(ctx.entity)
        .map(|m| m.class.clone())
}
