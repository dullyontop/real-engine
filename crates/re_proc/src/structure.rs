//! The structural override commands (ADR-0043 §3): `proc.move`,
//! `proc.replace`, `proc.insert` and `proc.clear_structure`. Each records
//! one `set_override` per structural override it changes and regenerates the
//! source, so the change shows at once; undo puts the override list back and
//! the next regeneration (the override list is part of the source's
//! fingerprint) gives back the output it had.

use re_command::{Call, EditTarget, Op};
use re_core::{Diagnostic, Location};
use re_scene::scene::ComponentsDoc;
use re_scene::structure::{
    INSERTED_PREFIX, Insert, Move, Replace, Replacement, SceneFiles, StructuralKind,
    check_insert_key,
};
use re_scene::{Entity, Generated, OverrideDoc, Overrides, Procedural, Streamed, World};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::codes;
use crate::store::GraphStore;

/// Arguments of `proc.move`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveArgs {
    /// The generated entity's id (`<source>/<key>`).
    pub id: String,
    /// The new parent: another entity of the same source's output, or the
    /// source itself (the default).
    #[serde(default)]
    pub parent: Option<String>,
    /// The position among the new parent's generated children (default:
    /// last).
    #[serde(default)]
    pub index: Option<usize>,
}

/// Arguments of `proc.replace`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceArgs {
    /// The generated entity's id (`<source>/<key>`).
    pub id: String,
    /// A prefab with one root entity: the entity becomes its root, and the
    /// prefab's other entities its descendants.
    #[serde(default)]
    pub prefab: Option<String>,
    /// Components as in a scene file: `{"<component>": {"<field>": value}}`.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Map<String, Json>>")]
    pub components: Option<ComponentsDoc>,
    /// Keep the entity's generated descendants (by default they go).
    #[serde(default)]
    pub keep_children: bool,
}

/// Arguments of `proc.insert`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InsertArgs {
    /// The procedural source's id.
    pub source: String,
    /// The new entity's key, without the `+` its id gets
    /// (`<source>/+<key>`).
    pub key: String,
    /// The parent: an entity of the source's output, or the source itself
    /// (the default).
    #[serde(default)]
    pub parent: Option<String>,
    /// The position among the parent's generated children (default: last).
    #[serde(default)]
    pub index: Option<usize>,
    /// The display name (default: the prefab root's name, or the key).
    #[serde(default)]
    pub name: Option<String>,
    /// A prefab with one root entity.
    #[serde(default)]
    pub prefab: Option<String>,
    /// Components as in a scene file.
    #[serde(default)]
    #[schemars(with = "Option<serde_json::Map<String, Json>>")]
    pub components: Option<ComponentsDoc>,
}

/// Arguments of `proc.clear_structure`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClearStructureArgs {
    /// The procedural source's id.
    pub source: String,
    /// Only the structural overrides of this key (`trees/12`, or `+lamp1`
    /// for an insert); default: all of the source's.
    #[serde(default)]
    pub key: Option<String>,
}

fn refused(command: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::STRUCTURE_REFUSED, message).at(Location::command(command))
}

/// The procedural source `id`, which must be generating (not streamed).
fn source_entity(world: &World, command: &str, id: &str) -> Result<Entity, Diagnostic> {
    let e = world.entity(id).map_err(|e| e.to_diagnostic())?;
    if world.ecs.get::<Procedural>(e).is_none() {
        return Err(refused(
            command,
            format!("`{id}` is not a procedural source"),
        ));
    }
    if world.ecs.get::<Streamed>(e).is_some() {
        return Err(refused(
            command,
            format!("`{id}` is streamed; structural overrides apply to sources that are not"),
        ));
    }
    Ok(e)
}

/// The source id and key of the generated entity `id`.
fn generated(world: &World, command: &str, id: &str) -> Result<(String, String), Diagnostic> {
    let e = world.entity(id).map_err(|e| e.to_diagnostic())?;
    let g = world.ecs.get::<Generated>(e).cloned().ok_or_else(|| {
        refused(
            command,
            format!("`{id}` is not a generated entity; edit authored entities directly"),
        )
    })?;
    let source = world.id_of(g.source).unwrap_or_default().to_string();
    source_entity(world, command, &source)?;
    Ok((source, g.key))
}

/// The key of `parent` in `source`'s output: `None` for the source itself.
fn parent_key(
    world: &World,
    command: &str,
    source: &str,
    parent: Option<&str>,
) -> Result<Option<String>, Diagnostic> {
    match parent {
        None => Ok(None),
        Some(p) if p == source => Ok(None),
        Some(p) => match generated(world, command, p) {
            Ok((s, key)) if s == source => Ok(Some(key)),
            _ => Err(refused(
                command,
                format!("the parent `{p}` is not `{source}` or an entity of its output"),
            )),
        },
    }
}

/// Checks that `with` turns into entities (components known, prefab
/// readable with one root), so a broken override is refused rather than
/// recorded.
fn check_replacement(
    world: &World,
    command: &str,
    with: &Replacement,
    key: &str,
) -> Result<(), Diagnostic> {
    let files = world
        .ecs
        .get_resource::<SceneFiles>()
        .map(|f| f.0.clone())
        .or_else(|| {
            world
                .ecs
                .get_resource::<GraphStore>()
                .and_then(|s| s.files().cloned())
        });
    let ids = world.ids();
    re_scene::structure::resolve(with, key, &world.registry, files.as_ref(), &|id| {
        ids.get(id)
    })
    .map(|_| ())
    .map_err(|e| refused(command, format!("{key}: {e}")).with_related(e.to_diagnostic()))
}

fn replacement(
    command: &str,
    prefab: Option<String>,
    components: Option<ComponentsDoc>,
) -> Result<Replacement, Diagnostic> {
    match (prefab, components) {
        (Some(p), None) if !p.is_empty() => Ok(Replacement::Prefab(p)),
        (None, Some(c)) => Ok(Replacement::Components(c)),
        _ => Err(refused(
            command,
            "give either `prefab` or `components`".to_string(),
        )),
    }
}

/// A `set_override` of one structural kind: `Some` sets, `None` clears.
fn structural_op(
    source: &str,
    target: &str,
    kind: StructuralKind,
    set: Option<&OverrideDoc>,
) -> Op {
    let mut moved = None;
    let mut replaced = None;
    let mut insert = None;
    match (kind, set) {
        (StructuralKind::Moved, Some(OverrideDoc::Moved { to, .. })) => {
            moved = Some(Some(to.clone()))
        }
        (StructuralKind::Moved, _) => moved = Some(None),
        (StructuralKind::Replaced, Some(OverrideDoc::Replaced { replace, .. })) => {
            replaced = Some(Some(Box::new(replace.clone())))
        }
        (StructuralKind::Replaced, _) => replaced = Some(None),
        (StructuralKind::Inserted, Some(OverrideDoc::Inserted { insert: i, .. })) => {
            insert = Some(Some(Box::new(i.clone())))
        }
        (StructuralKind::Inserted, _) => insert = Some(None),
    }
    Op::SetOverride {
        source: source.to_string(),
        target: target.to_string(),
        component: None,
        field: None,
        value: None,
        deleted: None,
        index: None,
        moved,
        replaced,
        insert,
    }
}

/// Records `ops` and regenerates `source`; the result names what changed.
fn apply<C: EditTarget>(
    target: &mut C,
    call: &mut Call,
    label: String,
    source: &str,
    ops: Vec<Op>,
    result: Json,
) -> Result<Json, Diagnostic> {
    call.transact(target, label, ops)?;
    let mut result = result;
    if call.dry_run() {
        result["dry_run"] = json!(true);
        return Ok(result);
    }
    let world = target.world();
    let entity = world.entity(source).map_err(|e| e.to_diagnostic())?;
    let mut report = world.regenerate_changed();
    if !report.sources.iter().any(|s| s == source) {
        report.merge(world.regenerate(entity));
    }
    if let Some(store) = world.ecs.get_resource::<GraphStore>() {
        for name in &report.released_assets {
            store.release(name);
        }
    }
    let (warnings, error) = crate::commands::regen_diagnostics(world, source, &report);
    for w in warnings {
        call.push_diagnostic(w);
    }
    if let Some(e) = error {
        return Err(e);
    }
    result["regen"] = crate::commands::regen_json(&report);
    Ok(result)
}

pub(crate) fn move_entity<C: EditTarget>(
    target: &mut C,
    a: MoveArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    const CMD: &str = "proc.move";
    let world = target.world();
    let (source, key) = generated(world, CMD, &a.id)?;
    let parent = parent_key(world, CMD, &source, a.parent.as_deref())?;
    let moved = world.entity(&a.id).map_err(|e| e.to_diagnostic())?;
    let mut up = a.parent.as_deref().and_then(|p| world.entity(p).ok());
    let mut cycle = false;
    while let Some(e) = up {
        if e == moved {
            cycle = true;
            break;
        }
        up = re_scene::hierarchy::parent(&world.ecs, e);
    }
    if cycle {
        return Err(refused(
            CMD,
            format!("`{}` cannot move under itself or its own descendant", a.id),
        ));
    }
    let o = OverrideDoc::Moved {
        target: key.clone(),
        to: Move {
            parent: parent.clone(),
            index: a.index,
        },
    };
    let op = structural_op(&source, &key, StructuralKind::Moved, Some(&o));
    apply(
        target,
        call,
        format!("proc.move {}", a.id),
        &source,
        vec![op],
        json!({ "source": source, "key": key, "parent": parent, "index": a.index }),
    )
}

pub(crate) fn replace<C: EditTarget>(
    target: &mut C,
    a: ReplaceArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    const CMD: &str = "proc.replace";
    let world = target.world();
    let (source, key) = generated(world, CMD, &a.id)?;
    let with = replacement(CMD, a.prefab, a.components)?;
    check_replacement(world, CMD, &with, &key)?;
    let o = OverrideDoc::Replaced {
        target: key.clone(),
        replace: Replace {
            with,
            keep_children: a.keep_children,
        },
    };
    let op = structural_op(&source, &key, StructuralKind::Replaced, Some(&o));
    apply(
        target,
        call,
        format!("proc.replace {}", a.id),
        &source,
        vec![op],
        json!({ "source": source, "key": key, "keep_children": a.keep_children }),
    )
}

pub(crate) fn insert<C: EditTarget>(
    target: &mut C,
    a: InsertArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    const CMD: &str = "proc.insert";
    let world = target.world();
    let src = source_entity(world, CMD, &a.source)?;
    check_insert_key(&a.key).map_err(|e| refused(CMD, e))?;
    let key = format!("{INSERTED_PREFIX}{}", a.key);
    let exists = world.ecs.get::<Overrides>(src).is_some_and(|o| {
        o.position_structural(&key, StructuralKind::Inserted)
            .is_some()
    });
    if exists {
        return Err(refused(
            CMD,
            format!(
                "`{}` already has an insert `{}`; clear it with proc.clear_structure first",
                a.source, a.key
            ),
        ));
    }
    let parent = parent_key(world, CMD, &a.source, a.parent.as_deref())?;
    let with = replacement(CMD, a.prefab, a.components)?;
    check_replacement(world, CMD, &with, &key)?;
    let o = OverrideDoc::Inserted {
        target: key.clone(),
        insert: Insert {
            parent: parent.clone(),
            index: a.index,
            name: a.name,
            with,
        },
    };
    let op = structural_op(&a.source, &key, StructuralKind::Inserted, Some(&o));
    apply(
        target,
        call,
        format!("proc.insert {}/{key}", a.source),
        &a.source,
        vec![op],
        json!({ "source": a.source, "key": key, "id": format!("{}/{key}", a.source), "parent": parent }),
    )
}

pub(crate) fn clear_structure<C: EditTarget>(
    target: &mut C,
    a: ClearStructureArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    const CMD: &str = "proc.clear_structure";
    let world = target.world();
    let src = source_entity(world, CMD, &a.source)?;
    let overrides = world.ecs.get::<Overrides>(src).cloned().unwrap_or_default();
    // Last first, so each clear's undo puts the override back where it was.
    let mut cleared = Vec::new();
    let mut ops = Vec::new();
    for o in overrides.0.iter().rev() {
        let Some(kind) = o.structural_kind() else {
            continue;
        };
        if a.key.as_deref().is_some_and(|k| k != o.target()) {
            continue;
        }
        cleared.push(json!({ "key": o.target(), "kind": kind.to_string() }));
        ops.push(structural_op(&a.source, o.target(), kind, None));
    }
    if ops.is_empty() {
        return Err(refused(
            CMD,
            match &a.key {
                Some(k) => format!("`{}` has no structural override on `{k}`", a.source),
                None => format!("`{}` has no structural overrides", a.source),
            },
        ));
    }
    cleared.reverse();
    apply(
        target,
        call,
        format!("proc.clear_structure {}", a.source),
        &a.source,
        ops,
        json!({ "source": a.source, "cleared": cleared }),
    )
}
