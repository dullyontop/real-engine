//! [`Tags`]: string labels on an entity, the [`TagIndex`] from tag to
//! entities, and the [`TagChanged`] events (ADR-0032 amendment).
//!
//! `Tags` is an immutable bevy component with one `list` field. Every change
//! replaces it, so its hooks see every old and new tag list: they keep the
//! [`TagIndex`] resource exact across spawn, despawn, field writes, undo,
//! scene load and fork, the way [`StableId`](crate::StableId)'s hooks keep the
//! `IdIndex`. While the world has a [`TagFeed`]
//! ([`World::record_tag_changes`](crate::World::record_tag_changes)), the hooks
//! also record one [`TagChanged`] per tag added or removed; removing the
//! component or despawning the entity removes all of its tags.
//!
//! A tag list holds each tag once: a write with repeats keeps the first.

use std::collections::{BTreeSet, HashMap};

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::DeferredWorld;
use re_reflect::{ComponentOrigin, ComponentSchema, FieldSchema, FieldType, Reflect, Value};

/// String labels on an entity, each at most once, in the order they were added.
#[derive(Component, Clone, Debug, PartialEq, Default)]
#[component(
    immutable,
    on_insert = tags_inserted,
    on_discard = tags_discarded,
    on_remove = tags_removed
)]
pub struct Tags {
    /// The tags.
    pub tags: Vec<String>,
}

impl Tags {
    /// True if the entity has `tag`.
    pub fn has(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t == tag)
    }
}

impl Reflect for Tags {
    const NAME: &'static str = "Tags";

    fn schema() -> ComponentSchema {
        ComponentSchema {
            name: Self::NAME.to_string(),
            doc: "String labels on the entity, found through the tag index.".to_string(),
            fields: vec![FieldSchema {
                name: "tags".to_string(),
                ty: FieldType::List(Box::new(FieldType::String)),
                doc: "The tags, each at most once, in the order they were added.".to_string(),
                default: Value::List(Vec::new()),
            }],
            origin: ComponentOrigin::Native,
        }
    }

    fn get_field(&self, index: usize) -> Value {
        assert_eq!(index, 0, "Tags: field index {index} out of range");
        Value::List(self.tags.iter().cloned().map(Value::Str).collect())
    }

    fn set_field(&mut self, index: usize, value: &Value) {
        assert_eq!(index, 0, "Tags: field index {index} out of range");
        let Value::List(items) = value else {
            panic!("value was coerced to the field type");
        };
        self.tags.clear();
        for item in items {
            if let Value::Str(t) = item
                && !self.tags.contains(t)
            {
                self.tags.push(t.clone());
            }
        }
    }
}

/// The entities carrying each tag, kept exact by [`Tags`]' hooks.
#[derive(Resource, Default, Debug)]
pub struct TagIndex {
    map: HashMap<String, BTreeSet<Entity>>,
}

impl TagIndex {
    /// The entities with `tag`, ordered by runtime handle (sort by stable id
    /// where an order must survive a reload).
    pub fn tagged(&self, tag: &str) -> impl Iterator<Item = Entity> + '_ {
        self.map.get(tag).into_iter().flatten().copied()
    }

    /// The number of entities with `tag`.
    pub fn count(&self, tag: &str) -> usize {
        self.map.get(tag).map_or(0, BTreeSet::len)
    }

    /// True if `entity` has `tag`.
    pub fn has(&self, tag: &str, entity: Entity) -> bool {
        self.map.get(tag).is_some_and(|s| s.contains(&entity))
    }

    /// Every tag that at least one entity carries, sorted.
    pub fn tags(&self) -> Vec<&str> {
        let mut tags: Vec<&str> = self.map.keys().map(String::as_str).collect();
        tags.sort();
        tags
    }

    fn add(&mut self, tag: &str, entity: Entity) {
        self.map.entry(tag.to_string()).or_default().insert(entity);
    }

    fn remove(&mut self, tag: &str, entity: Entity) {
        if let Some(set) = self.map.get_mut(tag) {
            set.remove(&entity);
            if set.is_empty() {
                self.map.remove(tag);
            }
        }
    }
}

/// One tag was added to or removed from one entity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagChanged {
    /// The entity.
    pub entity: Entity,
    /// The tag.
    pub tag: String,
    /// True if the tag was added, false if it was removed.
    pub added: bool,
}

/// The tag changes recorded since the last
/// [`World::drain_tag_changes`](crate::World::drain_tag_changes), in the
/// order they happened. Present only while someone listens.
#[derive(Resource, Default, Debug)]
pub struct TagFeed {
    events: Vec<TagChanged>,
    /// The tags an entity had before its `Tags` was replaced or removed, kept
    /// from the `on_discard` hook to the hook that follows it.
    before: HashMap<Entity, Vec<String>>,
}

impl TagFeed {
    /// Takes the recorded events.
    pub fn drain(&mut self) -> Vec<TagChanged> {
        std::mem::take(&mut self.events)
    }
}

fn tags_discarded(mut world: DeferredWorld, ctx: HookContext) {
    let Some(old) = world.get::<Tags>(ctx.entity).map(|t| t.tags.clone()) else {
        return;
    };
    if let Some(mut index) = world.get_resource_mut::<TagIndex>() {
        for tag in &old {
            index.remove(tag, ctx.entity);
        }
    }
    if let Some(mut feed) = world.get_resource_mut::<TagFeed>() {
        feed.before.insert(ctx.entity, old);
    }
}

fn tags_inserted(mut world: DeferredWorld, ctx: HookContext) {
    let Some(new) = world.get::<Tags>(ctx.entity).map(|t| t.tags.clone()) else {
        return;
    };
    if let Some(mut index) = world.get_resource_mut::<TagIndex>() {
        for tag in &new {
            index.add(tag, ctx.entity);
        }
    }
    if let Some(mut feed) = world.get_resource_mut::<TagFeed>() {
        let old = feed.before.remove(&ctx.entity).unwrap_or_default();
        for tag in old.iter().filter(|t| !new.contains(t)) {
            feed.events.push(TagChanged {
                entity: ctx.entity,
                tag: tag.clone(),
                added: false,
            });
        }
        for tag in new.iter().filter(|t| !old.contains(t)) {
            feed.events.push(TagChanged {
                entity: ctx.entity,
                tag: tag.clone(),
                added: true,
            });
        }
    }
}

fn tags_removed(mut world: DeferredWorld, ctx: HookContext) {
    if let Some(mut feed) = world.get_resource_mut::<TagFeed>() {
        let old = feed.before.remove(&ctx.entity).unwrap_or_default();
        for tag in old {
            feed.events.push(TagChanged {
                entity: ctx.entity,
                tag,
                added: false,
            });
        }
    }
}
