//! [`Attributes`]: named values that scripts, the editor and agents attach to
//! any entity, and the [`AttributeChanged`] events (ADR-0032 amendment).
//!
//! The component holds one `map` field, so attributes are saved, undone,
//! reflected and diffed like any other field. It is an immutable bevy
//! component: every change replaces it, and its hooks compare the old and new
//! maps to record one [`AttributeChanged`] per added, changed or removed key.
//! Events are recorded only while the world has an [`AttributeFeed`]
//! ([`World::record_attribute_changes`](crate::World::record_attribute_changes)),
//! so a world nobody listens to keeps nothing.
//!
//! Which keys and value kinds are allowed is the caller's rule (the
//! Roblox-style layer checks names and types in `SetAttribute`); the
//! component accepts any map the reflection rules accept.

use std::collections::{BTreeMap, HashMap};

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::lifecycle::HookContext;
use bevy_ecs::resource::Resource;
use bevy_ecs::world::DeferredWorld;
use re_reflect::{ComponentOrigin, ComponentSchema, FieldSchema, FieldType, Reflect, Value};

/// Named values on an entity: string keys, dynamically typed values.
#[derive(Component, Clone, Debug, PartialEq, Default)]
#[component(
    immutable,
    on_insert = attributes_inserted,
    on_discard = attributes_discarded,
    on_remove = attributes_removed
)]
pub struct Attributes {
    /// The attributes by name.
    pub values: BTreeMap<String, Value>,
}

impl Attributes {
    /// The value of one attribute.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }
}

impl Reflect for Attributes {
    const NAME: &'static str = "Attributes";

    fn schema() -> ComponentSchema {
        ComponentSchema {
            name: Self::NAME.to_string(),
            doc: "Named values attached to the entity by scripts, the editor or agents."
                .to_string(),
            fields: vec![FieldSchema {
                name: "values".to_string(),
                ty: FieldType::Map,
                doc: "The attributes by name. Values are bools, numbers, strings, vectors, \
                      colours, enum names, records, or lists of those."
                    .to_string(),
                default: Value::Map(BTreeMap::new()),
            }],
            origin: ComponentOrigin::Native,
        }
    }

    fn get_field(&self, index: usize) -> Value {
        assert_eq!(index, 0, "Attributes: field index {index} out of range");
        Value::Map(self.values.clone())
    }

    fn set_field(&mut self, index: usize, value: &Value) {
        assert_eq!(index, 0, "Attributes: field index {index} out of range");
        let Value::Map(map) = value else {
            panic!("value was coerced to the field type");
        };
        self.values = map.clone();
    }
}

/// One attribute of one entity was added, changed or removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributeChanged {
    /// The entity.
    pub entity: Entity,
    /// The attribute's name.
    pub key: String,
}

/// The attribute changes recorded since the last
/// [`World::drain_attribute_changes`](crate::World::drain_attribute_changes),
/// in the order they happened. Present only while someone listens.
#[derive(Resource, Default, Debug)]
pub struct AttributeFeed {
    events: Vec<AttributeChanged>,
    /// The map an entity had before its `Attributes` was replaced or removed,
    /// kept from the `on_discard` hook to the hook that follows it.
    before: HashMap<Entity, BTreeMap<String, Value>>,
}

impl AttributeFeed {
    /// Takes the recorded events.
    pub fn drain(&mut self) -> Vec<AttributeChanged> {
        std::mem::take(&mut self.events)
    }
}

fn attributes_discarded(mut world: DeferredWorld, ctx: HookContext) {
    let Some(old) = world
        .get::<Attributes>(ctx.entity)
        .map(|a| a.values.clone())
    else {
        return;
    };
    if let Some(mut feed) = world.get_resource_mut::<AttributeFeed>() {
        feed.before.insert(ctx.entity, old);
    }
}

fn attributes_inserted(mut world: DeferredWorld, ctx: HookContext) {
    if !world.contains_resource::<AttributeFeed>() {
        return;
    }
    let new = world
        .get::<Attributes>(ctx.entity)
        .map(|a| a.values.clone())
        .unwrap_or_default();
    let mut feed = world.resource_mut::<AttributeFeed>();
    let old = feed.before.remove(&ctx.entity).unwrap_or_default();
    if let Some(re_reflect::ValueChange::Keys(keys)) = Value::Map(old).diff(&Value::Map(new)) {
        for key in keys {
            feed.events.push(AttributeChanged {
                entity: ctx.entity,
                key,
            });
        }
    }
}

fn attributes_removed(mut world: DeferredWorld, ctx: HookContext) {
    if let Some(mut feed) = world.get_resource_mut::<AttributeFeed>() {
        let old = feed.before.remove(&ctx.entity).unwrap_or_default();
        for key in old.into_keys() {
            feed.events.push(AttributeChanged {
                entity: ctx.entity,
                key,
            });
        }
    }
}
