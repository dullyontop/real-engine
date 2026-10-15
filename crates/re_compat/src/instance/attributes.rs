//! Attributes and tags of instances (the `Attributes` and `Tags` components,
//! each replaced whole on every change).

use re_reflect::Value;
use re_scene::{Attributes, Entity, Tags, World};

use crate::codes;
use crate::error::CompatError;

/// Checks an attribute name: 1 to 100 letters, digits and underscores, not
/// starting with `RBX`.
pub fn check_name(name: &str) -> Result<(), CompatError> {
    let bad = |why: &str| CompatError::Runtime {
        code: codes::INVALID_ATTRIBUTE,
        message: format!("attribute name {name:?} {why}"),
    };
    if name.is_empty() || name.len() > 100 {
        return Err(bad("must be 1 to 100 characters long"));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(bad("may hold only letters, digits and underscores"));
    }
    if name.starts_with("RBX") {
        return Err(bad("may not start with RBX (reserved)"));
    }
    Ok(())
}

/// One attribute's value.
pub fn get(w: &World, e: Entity, name: &str) -> Option<Value> {
    w.ecs
        .get::<Attributes>(e)
        .and_then(|a| a.get(name).cloned())
}

/// Every attribute.
pub fn all(w: &World, e: Entity) -> Vec<(String, Value)> {
    w.ecs
        .get::<Attributes>(e)
        .map(|a| {
            a.values
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Sets (or, with `None`, removes) one attribute. An unchanged value writes
/// nothing, so no change event is recorded.
pub fn set(w: &mut World, e: Entity, name: &str, value: Option<Value>) {
    let mut map = w
        .ecs
        .get::<Attributes>(e)
        .map(|a| a.values.clone())
        .unwrap_or_default();
    match value {
        None => {
            if map.remove(name).is_none() {
                return;
            }
        }
        Some(v) => {
            if map.get(name) == Some(&v) {
                return;
            }
            map.insert(name.to_string(), v);
        }
    }
    w.ecs.entity_mut(e).insert(Attributes { values: map });
}

/// The tags of `e`, in the order they were added.
pub fn tags(w: &World, e: Entity) -> Vec<String> {
    w.ecs
        .get::<Tags>(e)
        .map(|t| t.tags.clone())
        .unwrap_or_default()
}

/// True if `e` has `tag`.
pub fn has_tag(w: &World, e: Entity, tag: &str) -> bool {
    w.ecs.get::<Tags>(e).is_some_and(|t| t.has(tag))
}

/// Adds `tag` to `e` (nothing happens when it has it).
pub fn add_tag(w: &mut World, e: Entity, tag: &str) {
    let mut tags = tags(w, e);
    if tags.iter().any(|t| t == tag) {
        return;
    }
    tags.push(tag.to_string());
    w.ecs.entity_mut(e).insert(Tags { tags });
}

/// Removes `tag` from `e` (nothing happens when it lacks it).
pub fn remove_tag(w: &mut World, e: Entity, tag: &str) {
    let mut tags = tags(w, e);
    let before = tags.len();
    tags.retain(|t| t != tag);
    if tags.len() != before {
        w.ecs.entity_mut(e).insert(Tags { tags });
    }
}
