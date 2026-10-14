//! The built-in commands of `re_command` (ARCHITECTURE §6, "MCP tool set"):
//! read tier `describe`, `query`, `entity.get`, `command.describe`; edit tier
//! `entity.spawn`, `entity.set`, `entity.reparent`, `entity.delete`,
//! `component.add`, `component.remove`, `scene.apply`, `undo`, `redo`.

use std::collections::BTreeMap;

use indexmap::IndexMap;
use re_core::{Diagnostic, Location};
use re_reflect::Doc;
use re_scene::hierarchy::{children, parent};
use re_scene::scene::ComponentsDoc;
use re_scene::{Entity, Generated, SaveOptions, StableId};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::codes;
use crate::op::Op;
use crate::registry::{Call, CommandRegistry, EditTarget, NoArgs, Tier};

/// Arguments of `query`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct QueryArgs {
    /// Only entities that have every one of these components.
    #[serde(default)]
    pub with: Vec<String>,
    /// Only entities with exactly this name.
    #[serde(default)]
    pub name: Option<String>,
    /// Only direct children of this entity.
    #[serde(default)]
    pub parent: Option<String>,
    /// `true` for generated entities only, `false` for authored ones only.
    #[serde(default)]
    pub generated: Option<bool>,
    /// At most this many results (default 100).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Skip this many results first (default 0).
    #[serde(default)]
    pub offset: Option<usize>,
}

/// Arguments of `entity.get`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EntityGetArgs {
    /// The entity's id.
    pub id: String,
    /// Only these components (default: all).
    #[serde(default)]
    pub components: Option<Vec<String>>,
}

/// Arguments of `command.describe`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandDescribeArgs {
    /// The command's name.
    pub name: String,
}

/// Arguments of `entity.spawn`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpawnArgs {
    /// The new entity's stable id (non-empty, no `/`); generated when omitted.
    #[serde(default)]
    pub id: Option<String>,
    /// The display name.
    #[serde(default)]
    pub name: String,
    /// The parent's id (a root when omitted).
    #[serde(default)]
    pub parent: Option<String>,
    /// Position among the parent's children (default: last).
    #[serde(default)]
    pub index: Option<usize>,
    /// Components and fields; omitted fields take their defaults.
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, BTreeMap<String, serde_json::Value>>")]
    pub components: ComponentsDoc,
}

/// Arguments of `entity.set`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetArgs {
    /// The entity's id.
    pub id: String,
    /// The component's name.
    pub component: String,
    /// The field's name.
    pub field: String,
    /// The new value (vectors as `[x, y, z]`, entity references as ids).
    #[schemars(with = "serde_json::Value")]
    pub value: Doc,
}

/// Arguments of `entity.reparent`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReparentArgs {
    /// The entity's id.
    pub id: String,
    /// The new parent's id, or `null` for a root.
    pub parent: Option<String>,
    /// Position among the new parent's children (default: last).
    #[serde(default)]
    pub index: Option<usize>,
}

/// Arguments of `entity.delete`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeleteArgs {
    /// The entity's id (its whole subtree is deleted).
    pub id: String,
}

/// Arguments of `component.add`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentAddArgs {
    /// The entity's id.
    pub id: String,
    /// The component's name.
    pub component: String,
    /// Field values; omitted fields take their defaults.
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, serde_json::Value>")]
    pub fields: IndexMap<String, Doc>,
}

/// Arguments of `component.remove`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComponentRemoveArgs {
    /// The entity's id.
    pub id: String,
    /// The component's name.
    pub component: String,
}

/// Arguments of `scene.apply`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneApplyArgs {
    /// A label for the undo history and the log.
    #[serde(default)]
    pub label: Option<String>,
    /// The operations, applied in order as one atomic transaction.
    pub ops: Vec<Op>,
}

fn changed_ids(ops: &[Op]) -> Json {
    match ops.first() {
        Some(Op::Spawn { id: Some(id), .. }) => json!({ "id": id }),
        _ => Json::Null,
    }
}

fn entity_json<C: EditTarget>(
    target: &mut C,
    e: Entity,
    only: Option<&[String]>,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let Some(doc) = world.entity_doc(e, SaveOptions::default()) else {
        return Ok(Json::Null);
    };
    if let Some(only) = only {
        for name in only {
            world.registry.key(name).map_err(|e| e.to_diagnostic())?;
        }
    }
    let components: IndexMap<String, IndexMap<String, Doc>> = doc
        .components
        .into_iter()
        .filter(|(name, _)| only.is_none_or(|o| o.iter().any(|n| n == name)))
        .collect();
    let generated = world
        .ecs
        .get::<Generated>(e)
        .map(|g| json!({ "source": world.id_of(g.source), "key": g.key }));
    let kids: Vec<&str> = children(&world.ecs, e)
        .iter()
        .filter_map(|c| world.id_of(*c))
        .collect();
    Ok(json!({
        "id": doc.id,
        "name": doc.name,
        "parent": doc.parent,
        "children": kids,
        "generated": generated,
        "components": components,
        "overrides": doc.overrides,
    }))
}

/// Registers the built-in commands.
pub fn register_builtin_commands<C: EditTarget + 'static>(registry: &mut CommandRegistry<C>) {
    registry.register_typed::<NoArgs, _>(
        "describe",
        "Describe what exists: every component (fields, types, defaults, docs), every generator and every command.",
        Tier::Read,
        true,
        |target, _args, call| {
            let mut d = target.world().describe();
            if let Some(obj) = d.as_object_mut() {
                obj.insert("commands".into(), serde_json::to_value(call.specs()).unwrap_or_default());
            }
            Ok(d)
        },
    );

    registry.register_typed::<QueryArgs, _>(
        "query",
        "List entities by components, name, parent or generated state, sorted by id.",
        Tier::Read,
        true,
        |target, args, _call| {
            let world = target.world();
            let mut keys = Vec::new();
            for name in &args.with {
                keys.push(world.registry.key(name).map_err(|e| e.to_diagnostic())?);
            }
            let parent_e = match &args.parent {
                Some(p) => Some(world.entity(p).map_err(|e| e.to_diagnostic())?),
                None => None,
            };
            let mut rows: Vec<(String, Entity)> = world
                .ecs
                .query::<(Entity, &StableId)>()
                .iter(&world.ecs)
                .map(|(e, id)| (id.0.clone(), e))
                .collect();
            rows.sort();
            let matched: Vec<Json> = rows
                .into_iter()
                .filter(|(_, e)| keys.iter().all(|k| world.registry.has(&world.ecs, *e, *k)))
                .filter(|(_, e)| args.name.as_ref().is_none_or(|n| world.name_of(*e) == n))
                .filter(|(_, e)| parent_e.is_none_or(|p| parent(&world.ecs, *e) == Some(p)))
                .filter(|(_, e)| {
                    args.generated
                        .is_none_or(|g| world.ecs.get::<Generated>(*e).is_some() == g)
                })
                .map(|(id, e)| {
                    json!({
                        "id": id,
                        "name": world.name_of(e),
                        "parent": parent(&world.ecs, e).and_then(|p| world.id_of(p)),
                    })
                })
                .collect();
            let total = matched.len();
            let offset = args.offset.unwrap_or(0);
            let limit = args.limit.unwrap_or(100);
            let page: Vec<Json> = matched.into_iter().skip(offset).take(limit).collect();
            Ok(json!({ "total": total, "entities": page }))
        },
    );

    registry.register_typed::<EntityGetArgs, _>(
        "entity.get",
        "Read one entity: name, parent, children, generated source and key, components and overrides.",
        Tier::Read,
        true,
        |target, args, _call| {
            let e = target.world().entity(&args.id).map_err(|e| e.to_diagnostic())?;
            entity_json(target, e, args.components.as_deref())
        },
    );

    registry.register_typed::<CommandDescribeArgs, _>(
        "command.describe",
        "Describe one command: summary, tier, input schema and annotations.",
        Tier::Read,
        true,
        |_target, args, call| match call.specs().iter().find(|s| s.name == args.name) {
            Some(spec) => Ok(serde_json::to_value(spec).unwrap_or_default()),
            None => {
                let suggestion =
                    re_reflect::suggest(&args.name, call.specs().iter().map(|s| s.name.as_str()));
                Err(Diagnostic::error(
                    codes::UNKNOWN_COMMAND,
                    match suggestion {
                        Some(s) => format!("unknown command {:?} (did you mean {s:?}?)", args.name),
                        None => format!("unknown command {:?}", args.name),
                    },
                )
                .at(Location::command(args.name.clone())))
            }
        },
    );

    registry.register_typed::<SpawnArgs, _>(
        "entity.spawn",
        "Create an entity with components; returns its id (generated when not given).",
        Tier::Edit,
        false,
        |target, args, call| {
            let op = Op::Spawn {
                id: args.id,
                name: args.name,
                parent: args.parent,
                index: args.index,
                components: args.components,
            };
            let applied = call.transact(target, "entity.spawn", vec![op])?;
            Ok(changed_ids(&applied))
        },
    );

    registry.register_typed::<SetArgs, _>(
        "entity.set",
        "Set one field of one component. On a generated entity this also records an override on its source.",
        Tier::Edit,
        true,
        |target, args, call| {
            let op = Op::SetField {
                id: args.id,
                component: args.component,
                field: args.field,
                value: args.value,
            };
            call.transact(target, "entity.set", vec![op])?;
            Ok(Json::Null)
        },
    );

    registry.register_typed::<ReparentArgs, _>(
        "entity.reparent",
        "Move an entity under a new parent (null makes it a root), optionally at a child index.",
        Tier::Edit,
        true,
        |target, args, call| {
            let op = Op::Reparent {
                id: args.id,
                parent: args.parent,
                index: args.index,
            };
            call.transact(target, "entity.reparent", vec![op])?;
            Ok(Json::Null)
        },
    );

    registry.register_typed::<DeleteArgs, _>(
        "entity.delete",
        "Delete an entity and its subtree (undoable). Deleting a generated entity records a deletion override.",
        Tier::Edit,
        false,
        |target, args, call| {
            call.transact(target, "entity.delete", vec![Op::Despawn { id: args.id }])?;
            Ok(Json::Null)
        },
    );

    registry.register_typed::<ComponentAddArgs, _>(
        "component.add",
        "Add a component to an entity; omitted fields take their defaults.",
        Tier::Edit,
        false,
        |target, args, call| {
            let op = Op::AddComponent {
                id: args.id,
                component: args.component,
                fields: args.fields,
            };
            call.transact(target, "component.add", vec![op])?;
            Ok(Json::Null)
        },
    );

    registry.register_typed::<ComponentRemoveArgs, _>(
        "component.remove",
        "Remove a component from an entity.",
        Tier::Edit,
        false,
        |target, args, call| {
            let op = Op::RemoveComponent {
                id: args.id,
                component: args.component,
            };
            call.transact(target, "component.remove", vec![op])?;
            Ok(Json::Null)
        },
    );

    registry.register_typed::<SceneApplyArgs, _>(
        "scene.apply",
        "Apply a list of operations as one atomic transaction: if any fails, nothing changes.",
        Tier::Edit,
        false,
        |target, args, call| {
            let label = args.label.unwrap_or_else(|| "scene.apply".into());
            let applied = call.transact(target, label, args.ops)?;
            let spawned: Vec<&str> = applied
                .iter()
                .filter_map(|op| match op {
                    Op::Spawn { id: Some(id), .. } => Some(id.as_str()),
                    _ => None,
                })
                .collect();
            Ok(json!({ "spawned": spawned }))
        },
    );

    registry.register_typed::<NoArgs, _>(
        "undo",
        "Undo the last transaction.",
        Tier::Edit,
        false,
        |target, _args, call| undo_redo(target, call, true),
    );

    registry.register_typed::<NoArgs, _>(
        "redo",
        "Redo the last undone transaction.",
        Tier::Edit,
        false,
        |target, _args, call| undo_redo(target, call, false),
    );
}

fn undo_redo<C: EditTarget>(
    target: &mut C,
    call: &mut Call,
    undo: bool,
) -> Result<Json, Diagnostic> {
    let history = target.history();
    let (pending, label) = if undo {
        (
            history.peek_undo().map(<[Op]>::to_vec),
            history.undo_label().map(str::to_string),
        )
    } else {
        (
            history.peek_redo().map(<[Op]>::to_vec),
            history.redo_label().map(str::to_string),
        )
    };
    let Some(ops) = pending else {
        return Ok(json!({ "done": false, "label": Json::Null }));
    };
    if call.dry_run() {
        call.record_changes(ops);
        return Ok(json!({ "done": false, "label": label }));
    }
    let mut history = std::mem::take(target.history());
    let result = if undo {
        history.undo(target.world())
    } else {
        history.redo(target.world())
    };
    *target.history() = history;
    match result {
        Ok(done) => {
            call.record_changes(ops);
            Ok(json!({ "done": done, "label": label }))
        }
        Err(e) => Err(Diagnostic::error(
            codes::UNDO_FAILED,
            format!("{} failed: {e}", if undo { "undo" } else { "redo" }),
        )
        .at(Location::command(call.name().to_string()))
        .with_related(e.to_diagnostic())),
    }
}
