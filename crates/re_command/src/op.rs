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
//!
//! Open documents (ADR-0044): `set_doc` edits a project file the World holds
//! open (a graph file, for example) at a JSON pointer:
//!
//! ```json
//! {"op": "set_doc", "doc": "graphs/island.graph.json", "pointer": "/layout/h", "value": [0, 120]}
//! ```

use std::collections::{BTreeMap, HashSet};

use indexmap::IndexMap;
use re_reflect::Doc;
use re_scene::hierarchy::{self, check_reparent, sibling_index};
use re_scene::scene::ComponentsDoc;
use re_scene::structure::{Insert, Move, Replace, StructuralKind};
use re_scene::{
    Entity, EntityDoc, Generated, GeneratedDoc, Name, OverrideDoc, Overrides, SaveOptions,
    Snapshot, World, validate_id,
};
use schemars::JsonSchema;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as Json;

use crate::error::OpError;

/// Deserializes a present field (including `null`) as `Some`, so that a
/// missing field and `null` can be told apart.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

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
        /// Sets (`{"parent", "index"?}`) or, with `null`, clears the move of
        /// `target` (ADR-0043 §3).
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "Option<serde_json::Value>")]
        moved: Option<Option<Move>>,
        /// Sets (`{"prefab" | "components", "keep_children"?}`) or, with
        /// `null`, clears the replacement of `target`.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "Option<serde_json::Value>")]
        replaced: Option<Option<Box<Replace>>>,
        /// Sets (`{"parent"?, "index"?, "name"?, "prefab" | "components"}`)
        /// or, with `null`, clears the insert whose key is `target`
        /// (`+<key>`).
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "present"
        )]
        #[schemars(with = "Option<serde_json::Value>")]
        insert: Option<Option<Box<Insert>>>,
    },
    /// Add, replace or remove the JSON value at `pointer` (RFC 6901) in an
    /// open document (ADR-0044). With `value`, as JSON Patch's `add`: an
    /// object member is added or replaced, and an array element is inserted
    /// at its index (`-` appends); the empty pointer replaces the whole
    /// document. Without `value`, the value at `pointer` is removed. The
    /// document must be open (`World::open_doc`). A transaction whose
    /// resulting document fails its kind's validator is rejected.
    ///
    /// Adding at an existing array index inserts before that element, as in
    /// JSON Patch: setting `/layout/h/0` on a `[x, y]` position makes it
    /// three numbers. To move a node, set `/layout/h` to the whole `[x, y]`.
    /// The parent of `pointer` must exist (adding `/layout/h` to a graph
    /// without `layout` fails; add `/layout` as `{"h": [x, y]}` instead).
    SetDoc {
        /// The document's project-relative path (`graphs/island.graph.json`).
        doc: String,
        /// Where in the document, as a JSON pointer (`/nodes/0/params/size`).
        pointer: String,
        /// The value to add or replace (`null` included); omitted to remove.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            deserialize_with = "some_json"
        )]
        value: Option<Json>,
    },
}

/// Deserializes a present value, `null` included, as `Some` (an absent one
/// is `None` through `#[serde(default)]`).
fn some_json<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Json>, D::Error> {
    Json::deserialize(d).map(Some)
}

/// The tokens of an RFC 6901 JSON pointer (`""` is the whole document).
fn pointer_tokens(pointer: &str) -> Result<Vec<String>, String> {
    if pointer.is_empty() {
        return Ok(Vec::new());
    }
    let Some(rest) = pointer.strip_prefix('/') else {
        return Err("a JSON pointer must be empty or start with `/`".into());
    };
    rest.split('/')
        .map(|t| {
            if t.replace("~0", "").replace("~1", "").contains('~') {
                Err("in a JSON pointer, `~` must be followed by `0` or `1`".into())
            } else {
                Ok(t.replace("~1", "/").replace("~0", "~"))
            }
        })
        .collect()
}

/// The JSON pointer of `tokens`.
fn pointer_of(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|t| format!("/{}", t.replace('~', "~0").replace('/', "~1")))
        .collect()
}

/// An array index token: `0`, or digits without a leading zero.
fn array_index(token: &str) -> Option<usize> {
    let digits = !token.is_empty() && token.bytes().all(|b| b.is_ascii_digit());
    if !digits || (token.len() > 1 && token.starts_with('0')) {
        return None;
    }
    token.parse().ok()
}

/// Sets (`Some`) or removes (`None`) the value at `tokens` in `root`, as
/// JSON Patch's `add` and `remove`. Returns the inverse `(pointer, value)`;
/// on error `root` is unchanged.
fn edit_json(
    root: &mut Json,
    tokens: &[String],
    value: Option<Json>,
) -> Result<(String, Option<Json>), String> {
    let Some((last, parents)) = tokens.split_last() else {
        return match value {
            Some(v) => Ok((String::new(), Some(std::mem::replace(root, v)))),
            None => Err("the whole document cannot be removed".into()),
        };
    };
    let mut parent = &mut *root;
    for (i, t) in parents.iter().enumerate() {
        let missing = || format!("no value at {}", pointer_of(&tokens[..=i]));
        parent = match parent {
            Json::Object(m) => m.get_mut(t).ok_or_else(missing)?,
            Json::Array(a) => array_index(t)
                .and_then(|i| a.get_mut(i))
                .ok_or_else(missing)?,
            _ => return Err(missing()),
        };
    }
    let here = pointer_of(tokens);
    match (parent, value) {
        (Json::Object(m), Some(v)) => {
            // An existing member is replaced in place (key order kept).
            let old = m.insert(last.clone(), v);
            Ok((here, old))
        }
        (Json::Object(m), None) => {
            let position = m
                .keys()
                .position(|k| k == last)
                .ok_or_else(|| format!("no value at {here}"))?;
            if position + 1 == m.len() {
                // The last member: adding it back puts it where it was.
                let old = m.shift_remove(last);
                Ok((here, old))
            } else {
                // A member before others: only the whole object restores
                // the member order exactly.
                let before = Json::Object(m.clone());
                m.shift_remove(last);
                Ok((pointer_of(parents), Some(before)))
            }
        }
        (Json::Array(a), Some(v)) => {
            let index = match last.as_str() {
                "-" => Some(a.len()),
                t => array_index(t).filter(|i| *i <= a.len()),
            }
            .ok_or_else(|| format!("{here} is not a position where an element can be inserted"))?;
            a.insert(index, v);
            let mut at = parents.to_vec();
            at.push(index.to_string());
            Ok((pointer_of(&at), None))
        }
        (Json::Array(a), None) => {
            let index = array_index(last)
                .filter(|i| *i < a.len())
                .ok_or_else(|| format!("no value at {here}"))?;
            let old = a.remove(index);
            Ok((here, Some(old)))
        }
        _ => Err(format!(
            "{} is neither an object nor an array",
            pointer_of(parents)
        )),
    }
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
/// What to do instead of re-parenting a generated entity (ADR-0043 §3).
const MOVE_HINT: &str = "move it within its source's output with `proc.move`";
/// What to do instead of putting an entity under a generated one.
const INSERT_HINT: &str = "generated entities cannot have authored children; add one to the output with `proc.insert`, or move a generated entity there with `proc.move`";

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
                reject_generated(world, pe, p, INSERT_HINT)?;
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
                    moved: None,
                    replaced: None,
                    insert: None,
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
                    moved: None,
                    replaced: None,
                    insert: None,
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
                    moved: None,
                    replaced: None,
                    insert: None,
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
            moved,
            replaced,
            insert,
        } => {
            let s = world.entity(source)?;
            let mut ov = source_overrides(world, s);
            let structural = [moved.is_some(), replaced.is_some(), insert.is_some()]
                .into_iter()
                .filter(|b| *b)
                .count();
            let inverse = match (component, field, deleted) {
                _ if structural > 0 => {
                    if structural > 1
                        || component.is_some()
                        || field.is_some()
                        || deleted.is_some()
                        || value.is_some()
                    {
                        return Err(OpError::Invalid(
                            "set_override takes one structural kind (`moved`, `replaced` or `insert`) and no field or deletion"
                                .into(),
                        ));
                    }
                    let (kind, new) = if let Some(m) = moved {
                        (
                            StructuralKind::Moved,
                            m.clone().map(|to| OverrideDoc::Moved {
                                target: target.clone(),
                                to,
                            }),
                        )
                    } else if let Some(r) = replaced {
                        (
                            StructuralKind::Replaced,
                            r.clone().map(|replace| OverrideDoc::Replaced {
                                target: target.clone(),
                                replace: *replace,
                            }),
                        )
                    } else {
                        let key = target.strip_prefix('+').ok_or_else(|| {
                            OpError::Invalid(format!(
                                "an inserted entity's key starts with `+` (got {target:?})"
                            ))
                        })?;
                        re_scene::structure::check_insert_key(key).map_err(OpError::Invalid)?;
                        (
                            StructuralKind::Inserted,
                            insert
                                .clone()
                                .flatten()
                                .map(|insert| OverrideDoc::Inserted {
                                    target: target.clone(),
                                    insert: *insert,
                                }),
                        )
                    };
                    let pos = ov.position_structural(target, kind);
                    let cleared = new.is_none();
                    let prev = ov.set_structural_at(target, kind, new, *index);
                    let mut inverse = Op::SetOverride {
                        source: source.clone(),
                        target: target.clone(),
                        component: None,
                        field: None,
                        value: None,
                        deleted: None,
                        // A cleared override is re-added where it was.
                        index: if cleared { pos } else { None },
                        moved: None,
                        replaced: None,
                        insert: None,
                    };
                    if let Op::SetOverride {
                        moved,
                        replaced,
                        insert,
                        ..
                    } = &mut inverse
                    {
                        match (kind, prev) {
                            (StructuralKind::Moved, Some(OverrideDoc::Moved { to, .. })) => {
                                *moved = Some(Some(to))
                            }
                            (StructuralKind::Moved, _) => *moved = Some(None),
                            (
                                StructuralKind::Replaced,
                                Some(OverrideDoc::Replaced { replace, .. }),
                            ) => *replaced = Some(Some(Box::new(replace))),
                            (StructuralKind::Replaced, _) => *replaced = Some(None),
                            (
                                StructuralKind::Inserted,
                                Some(OverrideDoc::Inserted { insert: i, .. }),
                            ) => *insert = Some(Some(Box::new(i))),
                            (StructuralKind::Inserted, _) => *insert = Some(None),
                        }
                    }
                    inverse
                }
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
                        moved: None,
                        replaced: None,
                        insert: None,
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
                        moved: None,
                        replaced: None,
                        insert: None,
                    }
                }
                _ => {
                    return Err(OpError::Invalid(
                        "set_override needs either `component` and `field` (with an optional `value`), `deleted`, or one of `moved`, `replaced` and `insert`"
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
            reject_generated(world, e, id, MOVE_HINT)?;
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
                    reject_generated(world, p, pid, INSERT_HINT)?;
                    let len = hierarchy::children(&world.ecs, p).len();
                    hierarchy::insert_child(&mut world.ecs, p, index.unwrap_or(len), e);
                }
                None => hierarchy::remove_parent(&mut world.ecs, e),
            }
            Ok((op.clone(), vec![inverse]))
        }
        Op::SetDoc {
            doc,
            pointer,
            value,
        } => {
            let invalid = |e: String| OpError::Invalid(format!("set_doc {doc} {pointer:?}: {e}"));
            let tokens = pointer_tokens(pointer).map_err(invalid)?;
            let (at, old) = world
                .edit_doc(doc, |root| edit_json(root, &tokens, value.clone()))
                .ok_or_else(|| {
                    OpError::Invalid(format!(
                        "set_doc: the document {doc:?} is not open (open it with World::open_doc)"
                    ))
                })?
                .map_err(invalid)?;
            Ok((
                op.clone(),
                vec![Op::SetDoc {
                    doc: doc.clone(),
                    pointer: at,
                    value: old,
                }],
            ))
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
