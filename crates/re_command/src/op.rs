//! Edit operations: the closed set of scene changes (ADR-0003). Every
//! operation is plain JSON addressed by stable ids and names, is validated
//! before it changes anything, and yields its exact inverse.
//!
//! ```json
//! {"op": "set_field", "id": "crate", "component": "RigidBody", "field": "mass", "value": 12.5}
//! ```
//!
//! Generated entities (ADR-0017): `set_field` on one also records an override
//! on its source; `despawn` of one records a deletion override; `reparent`,
//! `rename`, `add_component` and `remove_component` on one are refused.

use std::collections::{BTreeMap, HashSet};

use indexmap::IndexMap;
use re_reflect::Doc;
use re_scene::hierarchy::{self, check_reparent, sibling_index};
use re_scene::scene::ComponentsDoc;
use re_scene::{
    Entity, EntityDoc, Generated, GeneratedDoc, Name, Overrides, SaveOptions, Snapshot, World,
    validate_id,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::OpError;

/// One edit operation.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Op {
    /// Create an entity. Without `id`, a fresh id is generated (and recorded
    /// in the log, so redo recreates the same id).
    Spawn {
        /// The new entity's stable id (non-empty, no `/`); generated when omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        /// The display name.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        name: String,
        /// The parent's id (a root when omitted).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
        /// Position among the parent's children (default: last).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        /// Components and fields; omitted fields take their defaults.
        #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
        #[schemars(with = "BTreeMap<String, BTreeMap<String, serde_json::Value>>")]
        components: ComponentsDoc,
    },
    /// Remove an entity and all its descendants. References to them from
    /// other entities are cleared (and restored by undo).
    Despawn {
        /// The entity's id.
        id: String,
    },
    /// Set one field of one component.
    SetField {
        /// The entity's id.
        id: String,
        /// The component's name.
        component: String,
        /// The field's name.
        field: String,
        /// The new value (vectors as `[x, y, z]`, entity references as ids).
        #[schemars(with = "serde_json::Value")]
        value: Doc,
    },
    /// Add a component; omitted fields take their defaults.
    AddComponent {
        /// The entity's id.
        id: String,
        /// The component's name.
        component: String,
        /// Field values.
        #[serde(default, skip_serializing_if = "IndexMap::is_empty")]
        #[schemars(with = "BTreeMap<String, serde_json::Value>")]
        fields: IndexMap<String, Doc>,
    },
    /// Remove a component.
    RemoveComponent {
        /// The entity's id.
        id: String,
        /// The component's name.
        component: String,
    },
    /// Move an entity under a new parent (`null` makes it a root).
    Reparent {
        /// The entity's id.
        id: String,
        /// The new parent's id, or `null`.
        parent: Option<String>,
        /// Position among the new parent's children (default: last).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
    /// Change an entity's display name.
    Rename {
        /// The entity's id.
        id: String,
        /// The new name.
        name: String,
    },
    /// Recreate a despawned subtree exactly (the inverse of `despawn`). The
    /// entities must form one subtree: every entity after the first has an
    /// earlier one as its parent.
    Restore {
        /// The subtree, root first, with every field.
        #[schemars(with = "Vec<serde_json::Value>")]
        entities: Vec<EntityDoc>,
        /// The root's position among its parent's children.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
        /// The generated entities among them, each by its source's id and its
        /// key (the entity's id is `<source>/<key>`; the source must carry
        /// `Procedural`).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[schemars(with = "Vec<GeneratedDocSchema>")]
        generated: Vec<GeneratedDoc>,
        /// Ids of the procedural sources among them whose generated output
        /// was up to date (restored without forcing a regeneration).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        fresh: Vec<String>,
    },
    /// Set or clear one override a procedural source keeps for one of its
    /// generated entities, without touching the entity (the change takes
    /// effect at the next regeneration). With `component` and `field`, sets
    /// the field override to `value` (or clears it when `value` is omitted);
    /// with `deleted`, adds or removes the deletion override.
    SetOverride {
        /// The procedural source's id.
        source: String,
        /// The generated entity's key.
        target: String,
        /// The component of a field override.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        component: Option<String>,
        /// The field of a field override.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        field: Option<String>,
        /// The field override's value; omitted to clear it.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "re_scene::procedural::some_doc"
        )]
        #[schemars(with = "Option<serde_json::Value>")]
        value: Option<Doc>,
        /// Adds (`true`) or removes (`false`) the deletion override.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deleted: Option<bool>,
        /// Where an override that is added goes in the source's override list
        /// (default: last). Undo uses it to restore the list exactly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<usize>,
    },
}

/// The schema of one `restore.generated` entry ([`GeneratedDoc`]).
#[derive(JsonSchema)]
#[allow(dead_code)]
struct GeneratedDocSchema {
    /// The procedural source's id.
    source: String,
    /// The key within the source's output.
    key: String,
}

const STRUCTURAL_HINT: &str = "change its source's generator parameters instead";

fn reject_generated(
    world: &World,
    entity: Entity,
    id: &str,
    hint: &'static str,
) -> Result<(), OpError> {
    if world.ecs.get::<Generated>(entity).is_some() {
        return Err(OpError::GeneratedEntity {
            id: id.to_string(),
            hint,
        });
    }
    Ok(())
}

/// Checks the shape of a `restore` that the world does not check itself: the
/// entities form one subtree (each after the first has an earlier one as its
/// parent), and no authored entity is restored under a generated one.
fn check_restore(
    world: &World,
    entities: &[EntityDoc],
    generated: &[GeneratedDoc],
) -> Result<(), OpError> {
    let generated_ids: HashSet<String> = generated.iter().map(GeneratedDoc::id).collect();
    let mut earlier: HashSet<&str> = HashSet::new();
    for (i, d) in entities.iter().enumerate() {
        let parent = d.parent.as_deref();
        if i > 0 && !parent.is_some_and(|p| earlier.contains(p)) {
            return Err(OpError::Invalid(format!(
                "restore: entities[{i}] (id {:?}) must have an earlier entity of the list as its parent",
                d.id
            )));
        }
        if !generated_ids.contains(&d.id)
            && let Some(p) = parent
        {
            let parent_generated = if earlier.contains(p) {
                generated_ids.contains(p)
            } else {
                world
                    .ids()
                    .get(p)
                    .is_some_and(|pe| world.ecs.get::<Generated>(pe).is_some())
            };
            if parent_generated {
                return Err(OpError::GeneratedEntity {
                    id: p.to_string(),
                    hint: "generated entities cannot have authored children",
                });
            }
        }
        earlier.insert(d.id.as_str());
    }
    Ok(())
}

fn source_overrides(world: &World, source: Entity) -> Overrides {
    world
        .ecs
        .get::<Overrides>(source)
        .cloned()
        .unwrap_or_default()
}

fn store_overrides(world: &mut World, source: Entity, ov: Overrides) {
    if ov.0.is_empty() {
        world.ecs.entity_mut(source).remove::<Overrides>();
    } else {
        world.ecs.entity_mut(source).insert(ov);
    }
}

/// Applies one operation; returns the operation as applied (with a generated
/// id filled in) and its inverse (possibly several operations, in the order
/// they must be applied).
pub fn apply(world: &mut World, op: &Op) -> Result<(Op, Vec<Op>), OpError> {
    match op {
        Op::Spawn {
            id,
            name,
            parent: parent_id,
            index,
            components,
        } => {
            let id = match id {
                Some(id) if world.ids().get(id).is_some() => {
                    return Err(re_scene::SceneError::DuplicateId(id.clone()).into());
                }
                Some(id) => {
                    validate_id(id)?;
                    id.clone()
                }
                None => world.new_id(),
            };
            if let Some(p) = parent_id {
                let pe = world.entity(p)?;
                reject_generated(
                    world,
                    pe,
                    p,
                    "generated entities cannot have authored children",
                )?;
            }
            let doc = EntityDoc {
                id: id.clone(),
                name: name.clone(),
                parent: parent_id.clone(),
                components: components.clone(),
                overrides: vec![],
            };
            world.spawn_docs(std::slice::from_ref(&doc), *index)?;
            let applied = Op::Spawn {
                id: Some(id.clone()),
                name: name.clone(),
                parent: parent_id.clone(),
                index: *index,
                components: components.clone(),
            };
            Ok((applied, vec![Op::Despawn { id }]))
        }
        Op::Despawn { id } => {
            let e = world.entity(id)?;
            let generated = world.ecs.get::<Generated>(e).cloned();
            let index = sibling_index(&world.ecs, e);
            let snapshot = world.snapshot_subtree(e);
            // References from outside the subtree would dangle: clear them
            // now (observable, undoable) and restore them after `Restore`.
            // Each is cleared through `set_field`, so a generated referrer
            // records (and its inverse removes) an override symmetrically.
            let subtree: HashSet<Entity> = hierarchy::subtree(&world.ecs, e).into_iter().collect();
            let mut inverse = vec![Op::Restore {
                entities: snapshot.entities,
                index,
                generated: snapshot.generated,
                fresh: snapshot.fresh,
            }];
            let mut clears = Vec::new();
            for (referrer, key, fi) in world.registry.references_to(&world.ecs, &subtree) {
                if subtree.contains(&referrer) {
                    continue;
                }
                let Some(referrer_id) = world.id_of(referrer).map(str::to_string) else {
                    continue;
                };
                let schema = world.registry.schema(key);
                clears.push(Op::SetField {
                    id: referrer_id,
                    component: schema.name.clone(),
                    field: schema.fields[fi].name.clone(),
                    value: Doc::Null,
                });
            }
            let mut cleared: Vec<Vec<Op>> = Vec::new();
            for clear in &clears {
                match apply(world, clear) {
                    Ok((_, inv)) => cleared.push(inv),
                    Err(e) => {
                        // Leave the world as it was.
                        for o in cleared.iter().rev().flatten() {
                            let _ = apply(world, o);
                        }
                        return Err(e);
                    }
                }
            }
            inverse.extend(cleared.into_iter().flatten());
            // Despawning a generated entity records a deletion override.
            if let Some(g) = generated {
                let source_id = world.id_of(g.source).unwrap_or_default().to_string();
                let mut ov = source_overrides(world, g.source);
                let prev = ov.set_deleted(&g.key, true);
                store_overrides(world, g.source, ov);
                inverse.push(Op::SetOverride {
                    source: source_id,
                    target: g.key,
                    component: None,
                    field: None,
                    value: None,
                    deleted: Some(prev),
                    index: None,
                });
            }
            hierarchy::despawn_recursive(&mut world.ecs, e);
            Ok((op.clone(), inverse))
        }
        Op::Restore {
            entities,
            index,
            generated,
            fresh,
        } => {
            let root = entities
                .first()
                .ok_or_else(|| OpError::Invalid("restore needs at least one entity".into()))?;
            check_restore(world, entities, generated)?;
            // Despawning a generated root records a deletion override; the
            // inverse puts the previous deletion state back.
            let root_generated = generated.iter().find(|g| g.id() == root.id);
            let prev_deleted = root_generated.map(|g| {
                world
                    .ids()
                    .get(&g.source)
                    .and_then(|s| world.ecs.get::<Overrides>(s))
                    .is_some_and(|o| o.is_deleted(&g.key))
            });
            world.restore(
                &Snapshot {
                    entities: entities.clone(),
                    generated: generated.clone(),
                    fresh: fresh.clone(),
                },
                *index,
            )?;
            let mut inverse = vec![Op::Despawn {
                id: root.id.clone(),
            }];
            if let (Some(g), Some(prev)) = (root_generated, prev_deleted) {
                inverse.push(Op::SetOverride {
                    source: g.source.clone(),
                    target: g.key.clone(),
                    component: None,
                    field: None,
                    value: None,
                    deleted: Some(prev),
                    index: None,
                });
            }
            Ok((op.clone(), inverse))
        }
        Op::SetField {
            id,
            component,
            field,
            value,
        } => {
            let e = world.entity(id)?;
            let key = world.registry.key(component)?;
            let idx = world.registry.schema(key).field_index(field)?;
            let old = world.get_doc(id, component, field)?;
            let new = world
                .values_from_doc(key, &IndexMap::from([(field.clone(), value.clone())]))?
                .swap_remove(idx);
            world.registry.set_field(&mut world.ecs, e, key, idx, new)?;
            let mut inverse = vec![Op::SetField {
                id: id.clone(),
                component: component.clone(),
                field: field.clone(),
                value: old,
            }];
            // An edit to a generated entity becomes an override on its source.
            if let Some(g) = world.ecs.get::<Generated>(e).cloned() {
                let source_id = world.id_of(g.source).unwrap_or_default().to_string();
                let canonical = world.get_doc(id, component, field)?;
                let mut ov = source_overrides(world, g.source);
                let prev = ov.set(&g.key, component, field, Some(canonical));
                store_overrides(world, g.source, ov);
                inverse.push(Op::SetOverride {
                    source: source_id,
                    target: g.key,
                    component: Some(component.clone()),
                    field: Some(field.clone()),
                    value: prev,
                    deleted: None,
                    index: None,
                });
            }
            Ok((op.clone(), inverse))
        }
        Op::SetOverride {
            source,
            target,
            component,
            field,
            value,
            deleted,
            index,
        } => {
            let s = world.entity(source)?;
            let mut ov = source_overrides(world, s);
            let inverse = match (component, field, deleted) {
                (Some(c), Some(f), None) => {
                    let pos = ov.position(target, c, f);
                    let prev = ov.set_at(target, c, f, value.clone(), *index);
                    Op::SetOverride {
                        source: source.clone(),
                        target: target.clone(),
                        component: Some(c.clone()),
                        field: Some(f.clone()),
                        // A cleared override is re-added where it was.
                        index: if value.is_none() { pos } else { None },
                        value: prev,
                        deleted: None,
                    }
                }
                (None, None, Some(d)) if value.is_none() => {
                    let pos = ov.position_deleted(target);
                    let prev = ov.set_deleted_at(target, *d, *index);
                    Op::SetOverride {
                        source: source.clone(),
                        target: target.clone(),
                        component: None,
                        field: None,
                        value: None,
                        deleted: Some(prev),
                        index: if *d { None } else { pos },
                    }
                }
                _ => {
                    return Err(OpError::Invalid(
                        "set_override needs either `component` and `field` (with an optional `value`), or `deleted`"
                            .into(),
                    ));
                }
            };
            store_overrides(world, s, ov);
            Ok((op.clone(), vec![inverse]))
        }
        Op::AddComponent {
            id,
            component,
            fields,
        } => {
            let e = world.entity(id)?;
            reject_generated(world, e, id, STRUCTURAL_HINT)?;
            let key = world.registry.key(component)?;
            if world.registry.has(&world.ecs, e, key) {
                return Err(OpError::ComponentAlreadyPresent {
                    component: component.clone(),
                });
            }
            let values = world.values_from_doc(key, fields)?;
            world.registry.insert(&mut world.ecs, e, key, values)?;
            Ok((
                op.clone(),
                vec![Op::RemoveComponent {
                    id: id.clone(),
                    component: component.clone(),
                }],
            ))
        }
        Op::RemoveComponent { id, component } => {
            let e = world.entity(id)?;
            reject_generated(world, e, id, STRUCTURAL_HINT)?;
            let key = world.registry.key(component)?;
            if !world.registry.has(&world.ecs, e, key) {
                return Err(re_reflect::ReflectError::MissingComponent {
                    component: component.clone(),
                }
                .into());
            }
            let fields = world
                .entity_doc(e, SaveOptions::default())
                .and_then(|d| d.components.get(component).cloned())
                .unwrap_or_default();
            world.registry.remove(&mut world.ecs, e, key);
            Ok((
                op.clone(),
                vec![Op::AddComponent {
                    id: id.clone(),
                    component: component.clone(),
                    fields,
                }],
            ))
        }
        Op::Reparent {
            id,
            parent: new_parent,
            index,
        } => {
            let e = world.entity(id)?;
            reject_generated(world, e, id, STRUCTURAL_HINT)?;
            let old_parent = hierarchy::parent(&world.ecs, e);
            let old_index = sibling_index(&world.ecs, e);
            let inverse = Op::Reparent {
                id: id.clone(),
                parent: old_parent.and_then(|p| world.id_of(p)).map(str::to_string),
                index: old_index,
            };
            match new_parent {
                Some(pid) => {
                    let p = world.entity(pid)?;
                    check_reparent(&world.ecs, e, p)?;
                    reject_generated(
                        world,
                        p,
                        pid,
                        "generated entities cannot have authored children",
                    )?;
                    let len = hierarchy::children(&world.ecs, p).len();
                    hierarchy::insert_child(&mut world.ecs, p, index.unwrap_or(len), e);
                }
                None => hierarchy::remove_parent(&mut world.ecs, e),
            }
            Ok((op.clone(), vec![inverse]))
        }
        Op::Rename { id, name } => {
            let e = world.entity(id)?;
            reject_generated(world, e, id, "generated names come from the generator")?;
            let old = world.name_of(e).to_string();
            world.ecs.entity_mut(e).insert(Name(name.clone()));
            Ok((
                op.clone(),
                vec![Op::Rename {
                    id: id.clone(),
                    name: old,
                }],
            ))
        }
    }
}
