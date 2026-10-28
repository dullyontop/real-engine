//! The editor's own `re_proc` commands (the ADR-0044 amendment, §7 and §8).
//!
//! `proc.convert_builder` is the node canvas's "Convert to a graph file",
//! which turns a procedural source whose graph a builder script makes into
//! one that reads a graph file. The file is made through an absent document
//! (ADR-0064): the command opens it absent and, in one transaction, gives it
//! the graph the builder made (with the positions the canvas drew it at in
//! `layout`), sets `GraphSource.graph` to it and empties
//! `GraphSource.builder`. Generation reads the open document, so the source
//! generates the same fragment before the file is saved. Undo empties the
//! document again, so the next save removes the file, and gives the source
//! its builder back. The script stays where it is.
//!
//! `proc.revert` puts back what a source generates: a generated entity's
//! fields without their overrides (`id`), or an entity the source keeps a
//! deletion override for (`source` and `key`). It reads the source's
//! fragment, computed and not applied
//! ([`generated_fragment`](re_scene::procedural::generated_fragment)), and
//! is one transaction of `set_field`, `set_override` and `restore`
//! operations, so undo puts the overrides and the values back exactly.
//! Clearing an override alone would leave the overridden value, or the gap,
//! in place: neither a field nor a deletion override is part of the source's
//! fingerprint, so nothing regenerates.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use re_command::{Call, CommandRegistry, EditTarget, Op, Tier, codes as command_codes};
use re_core::{Diagnostic, Location};
use re_reflect::{Doc, Value};
use re_scene::scene::ComponentsDoc;
use re_scene::{
    Entity, EntityDoc, GenNode, Generated, GeneratedDoc, OverrideDoc, Overrides, Procedural,
    SceneFragment, Streamed, World, hierarchy,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use super::{graph_source, store_of, valid_graph_path};
use crate::codes;
use crate::store::GraphStore;

/// Arguments of `proc.convert_builder`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConvertBuilderArgs {
    /// The procedural source: one whose graph its `GraphSource.builder`
    /// script makes.
    pub source: String,
    /// The graph file to make, relative to the project, ending in
    /// `.graph.json` (default `graphs/<source>.graph.json`). It must not
    /// exist, or be open with content.
    #[serde(default)]
    #[schemars(extend("x-real-path" = "write"))]
    pub graph: Option<String>,
    /// Where the node canvas draws nodes, `[x, y]` by node id, written into
    /// the file's `layout` (the editor sends the positions it laid the built
    /// graph out at). Nodes without one are laid out by the canvas.
    #[serde(default)]
    pub layout: BTreeMap<String, [f32; 2]>,
}

/// Arguments of `proc.revert`: `id` (with `component` and `field` to narrow
/// it) for a generated entity's field overrides, or `source` and `key` for an
/// entity the source keeps a deletion override for.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevertArgs {
    /// A generated entity (`<source>/<key>`): its field overrides go, and
    /// the fields take what the source generates.
    #[serde(default)]
    pub id: Option<String>,
    /// With `id`: only the overrides of this component's fields.
    #[serde(default)]
    pub component: Option<String>,
    /// With `id` and `component`: only this field's override.
    #[serde(default)]
    pub field: Option<String>,
    /// A procedural source: with `key`, the deleted entity to bring back.
    #[serde(default)]
    pub source: Option<String>,
    /// With `source`: the key of the generated entity it keeps a deletion
    /// override for (`trees/12`; the entity's id was `<source>/<key>`).
    #[serde(default)]
    pub key: Option<String>,
}

fn not_builder(id: &str, why: &str) -> Diagnostic {
    Diagnostic::error(
        codes::NOT_A_BUILDER_SOURCE,
        format!("`{id}` is not a builder source: {why}"),
    )
    .at(Location::entity(id.to_string()))
}

fn taken(path: &str, why: &str) -> Diagnostic {
    Diagnostic::error(
        codes::GRAPH_FILE_TAKEN,
        format!("cannot make {path}: {why}"),
    )
    .at(Location::file(path.to_string()))
    .with_hint("pass `graph` to make another file")
}

fn bad_argument(command: &str, message: String) -> Diagnostic {
    Diagnostic::error(command_codes::INVALID_ARGUMENTS, message).at(Location::command(command))
}

/// The graph file a source converts to by default:
/// `graphs/<source id>.graph.json`, lower case, with `_` for what a file
/// name does not hold.
fn default_path(id: &str) -> String {
    let stem: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("graphs/{stem}.graph.json")
}

/// True when the project has a file at `path`, as `store` reads files.
fn file_exists(store: &GraphStore, path: &str) -> bool {
    match (store.files(), store.root()) {
        (Some(files), _) => files.is_file(path),
        (None, Some(root)) => root.join(path).is_file(),
        (None, None) => Path::new(path).is_file(),
    }
}

/// A `GraphSource` field of `id`, or `""`.
fn source_field(world: &World, id: &str, field: &str) -> String {
    match world.get(id, "GraphSource", field) {
        Ok(Value::Str(s)) => s,
        _ => String::new(),
    }
}

fn convert_builder<C: EditTarget>(
    target: &mut C,
    a: ConvertBuilderArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let src = graph_source(world, &a.source)?;
    let graph = source_field(world, &a.source, "graph");
    if !graph.trim().is_empty() {
        return Err(not_builder(
            &a.source,
            &format!("it reads the graph file {graph}"),
        ));
    }
    let builder = source_field(world, &a.source, "builder");
    if builder.trim().is_empty() {
        return Err(not_builder(&a.source, "it has no builder script"));
    }
    let path = match &a.graph {
        Some(g) => re_scene::doc_path(g)
            .ok()
            .filter(|p| valid_graph_path(p))
            .ok_or_else(|| {
                Diagnostic::error(
                    command_codes::DOCUMENT_PATH,
                    format!(
                        "`{g}` is not a graph file path: a project path of plain names \
                         ending in `.graph.json`"
                    ),
                )
                .at(Location::command("proc.convert_builder"))
            })?,
        None => {
            let path = default_path(&a.source);
            // The default file is named by no argument, so the policy has
            // not seen it yet.
            call.check_write(&path)?;
            path
        }
    };
    let mut doc = src.doc;
    for (node, p) in &a.layout {
        if doc.node(node).is_none() {
            return Err(bad_argument(
                "proc.convert_builder",
                format!("`layout` places node `{node}`, which the built graph does not have"),
            ));
        }
        if !p.iter().all(|x| x.is_finite()) {
            return Err(bad_argument(
                "proc.convert_builder",
                format!("`layout` places node `{node}` at a position that is not a finite pair"),
            ));
        }
    }
    doc.layout.extend(a.layout);
    let content = serde_json::to_value(&doc).expect("a GraphDoc always serializes");

    // The file is free: the World holds no content for it, and the project
    // has no such file (or it is open absent, so the next save removes it).
    let opened = match world.doc(&path) {
        Some(d) if d.is_absent() => false,
        Some(_) => return Err(taken(&path, "it is open with content")),
        None => {
            let store = store_of(world).expect("graph_source checked the store");
            if file_exists(&store, &path) {
                return Err(taken(&path, "the file exists"));
            }
            crate::register_doc_validators(world);
            world
                .open_doc(&path, Json::Null)
                .map_err(|e| bad_argument("proc.convert_builder", e.to_string()))?;
            true
        }
    };
    let nodes = doc.nodes.len();
    let result = call.transact(
        target,
        format!("proc.convert_builder {} to {path}", a.source),
        vec![
            Op::SetDoc {
                doc: path.clone(),
                pointer: String::new(),
                value: Some(content),
            },
            Op::SetField {
                id: a.source.clone(),
                component: "GraphSource".into(),
                field: "graph".into(),
                value: Doc::Str(path.clone()),
            },
            Op::SetField {
                id: a.source.clone(),
                component: "GraphSource".into(),
                field: "builder".into(),
                value: Doc::Str(String::new()),
            },
        ],
    );
    if opened && (result.is_err() || call.dry_run()) {
        target.world().close_doc(&path);
    }
    result?;
    let mut data = json!({
        "source": a.source,
        "graph": path,
        "builder": builder,
        "nodes": nodes,
    });
    if call.dry_run() {
        data["dry_run"] = Json::Bool(true);
    }
    Ok(data)
}

fn nothing_to_revert(at: &str, message: String) -> Diagnostic {
    Diagnostic::error(codes::NOTHING_TO_REVERT, message).at(Location::entity(at.to_string()))
}

fn not_generated_now(at: &str, message: String) -> Diagnostic {
    Diagnostic::error(codes::KEY_NOT_GENERATED, message).at(Location::entity(at.to_string()))
}

/// What `source` generates, for reverting `id` (key `key`): its fragment,
/// computed and not applied. An entity of a stream cell is refused: the
/// cell's output is generated per cell, not in the source's fragment.
fn fragment_of(
    world: &World,
    s: Entity,
    source: &str,
    key: &str,
    id: &str,
) -> Result<SceneFragment, Diagnostic> {
    if world.ecs.get::<Streamed>(s).is_some()
        && let Some(cell) = re_scene::stream::owning_cell(key)
    {
        return Err(not_generated_now(
            id,
            format!(
                "`{id}` is part of stream cell `{cell}`, which `{source}` generates per cell; \
                 proc.revert reads what the source generates outside its cells"
            ),
        )
        .with_hint(
            "set the field with `entity.set`, or remove the override with `scene.apply` and a \
             `set_override` operation",
        ));
    }
    re_scene::procedural::generated_fragment(world, s).map_err(|e| e.to_diagnostic())
}

/// The value `node` gives field `field` of `component`, as a regeneration
/// coerces it, or the field's default when the node does not set it.
/// `None` when the component or the field is unknown.
fn generated_value(
    world: &World,
    source: &str,
    node: &GenNode,
    component: &str,
    field: &str,
) -> Result<Option<Doc>, Diagnostic> {
    let Some(key) = world.registry.try_key(component) else {
        return Ok(None);
    };
    let schema = world.registry.schema(key);
    let Ok(index) = schema.field_index(field) else {
        return Ok(None);
    };
    let f = &schema.fields[index];
    let set = node
        .components
        .iter()
        .find(|(c, _)| c == component)
        .and_then(|(_, fields)| fields.iter().find(|(name, _)| name == field));
    let value = match set {
        Some((_, v)) => f.ty.coerce(v.clone()).map_err(|error| {
            Diagnostic::error(
                re_scene::codes::GENERATOR_FAILED,
                format!(
                    "{source}: node {:?}: {component}.{field}: {error}",
                    node.key
                ),
            )
            .at(Location::entity(source.to_string()))
        })?,
        None => f.default.clone(),
    };
    Ok(Some(Doc::from_value(&value, &f.ty, &|e| {
        world.id_of(e).map(str::to_string)
    })))
}

/// The `set_override` that removes the field override of
/// `component.field` on `target`.
fn clear_field_override(source: &str, target: &str, component: &str, field: &str) -> Op {
    Op::SetOverride {
        source: source.to_string(),
        target: target.to_string(),
        component: Some(component.to_string()),
        field: Some(field.to_string()),
        value: None,
        deleted: None,
        index: None,
        moved: None,
        replaced: None,
        insert: None,
    }
}

/// `proc.revert {id, component?, field?}`: the generated entity's field
/// overrides (every one, one component's or one field's) go, and each field
/// takes what the source generates. A `set_field` first (on a generated
/// entity it records the generated value as the override), then a
/// `set_override` that removes the override, so undo puts both back where
/// they were. A field of a component the entity does not have only loses
/// its override.
fn revert_fields<C: EditTarget>(
    target: &mut C,
    id: &str,
    component: Option<&str>,
    field: Option<&str>,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let e = world.entity(id).map_err(|e| e.to_diagnostic())?;
    let Some(g) = world.ecs.get::<Generated>(e).cloned() else {
        return Err(nothing_to_revert(
            id,
            format!("`{id}` is not a generated entity: it has no overrides to revert"),
        ));
    };
    let source = world.id_of(g.source).unwrap_or_default().to_string();
    let overrides = world
        .ecs
        .get::<Overrides>(g.source)
        .cloned()
        .unwrap_or_default();
    let fields: Vec<(String, String)> = overrides
        .0
        .iter()
        .filter_map(|o| match o {
            OverrideDoc::Field {
                target,
                component: c,
                field: f,
                ..
            } if *target == g.key
                && component.is_none_or(|x| x == c)
                && field.is_none_or(|x| x == f) =>
            {
                Some((c.clone(), f.clone()))
            }
            _ => None,
        })
        .collect();
    if fields.is_empty() {
        let what = match (component, field) {
            (Some(c), Some(f)) => format!("`{c}.{f}`"),
            (Some(c), None) => format!("a field of `{c}`"),
            _ => "any field".to_string(),
        };
        return Err(nothing_to_revert(
            id,
            format!("`{source}` keeps no override of {what} of `{id}`"),
        )
        .with_hint(format!(
            "`entity.get {{\"id\": \"{source}\"}}` lists the overrides `{source}` keeps"
        )));
    }
    let fragment = fragment_of(world, g.source, &source, &g.key, id)?;
    let Some(node) = fragment.iter().find(|n| n.key == g.key) else {
        return Err(
            not_generated_now(id, format!("`{source}` no longer generates `{}`", g.key))
                .with_hint("regenerate the source: the entity goes, and its overrides are orphans"),
        );
    };
    let mut ops = Vec::with_capacity(2 * fields.len());
    let mut reverted = Vec::with_capacity(fields.len());
    for (c, f) in &fields {
        let has = world
            .registry
            .try_key(c)
            .is_some_and(|k| world.registry.has(&world.ecs, e, k));
        let value = if has {
            generated_value(world, &source, node, c, f)?
        } else {
            None
        };
        match value {
            Some(value) => {
                reverted.push(json!({ "component": c, "field": f, "value": &value }));
                ops.push(Op::SetField {
                    id: id.to_string(),
                    component: c.clone(),
                    field: f.clone(),
                    value,
                });
            }
            _ => reverted.push(json!({ "component": c, "field": f })),
        }
        ops.push(clear_field_override(&source, &g.key, c, f));
    }
    call.transact(target, format!("proc.revert {id}"), ops)?;
    let mut data = json!({
        "id": id,
        "source": source,
        "key": g.key,
        "reverted": reverted,
    });
    if call.dry_run() {
        data["dry_run"] = Json::Bool(true);
    }
    Ok(data)
}

/// Why a regeneration skips `key` once its own deletion override is gone:
/// it is suppressed, or under an entity that is deleted or suppressed.
fn skipped_because(
    world: &World,
    fragment: &[GenNode],
    overrides: &Overrides,
    source: &str,
    key: &str,
) -> Option<String> {
    let suppressed = |k: &str| world.suppressions().contains(&format!("{source}/{k}"));
    if suppressed(key) {
        return Some(format!(
            "`{source}/{key}` is suppressed for this play session"
        ));
    }
    let mut at = key;
    while let Some(parent) = fragment
        .iter()
        .find(|n| n.key == at)
        .and_then(|n| n.parent_key.as_deref())
    {
        if overrides.is_deleted(parent) {
            return Some(format!(
                "`{source}` does not generate `{key}` while `{parent}` is deleted: restore `{parent}` first"
            ));
        }
        if suppressed(parent) {
            return Some(format!(
                "`{source}` does not generate `{key}` while `{source}/{parent}` is suppressed for this play session"
            ));
        }
        at = parent;
    }
    None
}

/// `proc.revert {source, key}`: the deletion override of `key` goes, and the
/// entity comes back as the source generates it, through a `restore`: its
/// fragment node and descendants (without the ones a regeneration skips),
/// with their field overrides, among its generated siblings in output order.
/// The `restore` comes first, so undo puts the deletion override back where
/// it was before the entity goes.
fn revert_deletion<C: EditTarget>(
    target: &mut C,
    source: &str,
    key: &str,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let s = world.entity(source).map_err(|e| e.to_diagnostic())?;
    if world.ecs.get::<Procedural>(s).is_none() {
        return Err(nothing_to_revert(
            source,
            format!("`{source}` is not a procedural source: it keeps no overrides"),
        ));
    }
    let overrides = world.ecs.get::<Overrides>(s).cloned().unwrap_or_default();
    let id = format!("{source}/{key}");
    if !overrides.is_deleted(key) {
        return Err(nothing_to_revert(
            source,
            format!("`{source}` keeps no deletion override of `{key}`"),
        )
        .with_hint(format!(
            "`entity.get {{\"id\": \"{source}\"}}` lists the overrides `{source}` keeps"
        )));
    }
    let fragment = fragment_of(world, s, source, key, &id)?;
    let Some(at) = fragment.iter().position(|n| n.key == key) else {
        return Err(
            not_generated_now(source, format!("`{source}` no longer generates `{key}`")).with_hint(
                "the deletion override is an orphan: remove it with `scene.apply` and a \
             `set_override` operation",
            ),
        );
    };
    if let Some(why) = skipped_because(world, &fragment, &overrides, source, key) {
        return Err(not_generated_now(source, why));
    }
    let root = &fragment[at];
    let parent = match &root.parent_key {
        None => s,
        Some(p) => world.ids().get(&format!("{source}/{p}")).ok_or_else(|| {
            not_generated_now(
                source,
                format!("`{source}/{p}`, the parent of `{key}`, is not in the scene"),
            )
            .with_hint("regenerate the source first")
        })?,
    };

    // The subtree, as a regeneration makes it: what it skips (deleted,
    // suppressed, or under a skipped entity) stays out.
    let mut tree: HashSet<&str> = HashSet::from([key]);
    let mut nodes = vec![root];
    for n in &fragment[at + 1..] {
        let Some(p) = n.parent_key.as_deref() else {
            continue;
        };
        let skip = overrides.is_deleted(&n.key)
            || world
                .suppressions()
                .contains(&format!("{source}/{}", n.key));
        if tree.contains(p) && !skip {
            tree.insert(&n.key);
            nodes.push(n);
        }
    }
    let mut entities = Vec::with_capacity(nodes.len());
    for n in &nodes {
        let mut components = ComponentsDoc::new();
        for (c, fields) in &n.components {
            let mut docs = indexmap::IndexMap::new();
            for (f, _) in fields {
                if let Some(value) = generated_value(world, source, n, c, f)? {
                    docs.insert(f.clone(), value);
                }
            }
            components.insert(c.clone(), docs);
        }
        entities.push(EntityDoc {
            id: format!("{source}/{}", n.key),
            name: n.name.clone(),
            note: String::new(),
            parent: Some(match &n.parent_key {
                Some(p) if n.key != key => format!("{source}/{p}"),
                _ => world.id_of(parent).unwrap_or_default().to_string(),
            }),
            components,
            overrides: Vec::new(),
        });
    }
    // Their field overrides, as a regeneration applies them: one it cannot
    // apply is left out with the warning a regeneration gives.
    let ids = world.ids();
    for o in &overrides.0 {
        let OverrideDoc::Field {
            target,
            component,
            field,
            value,
        } = o
        else {
            continue;
        };
        let Some(i) = nodes.iter().position(|n| n.key == *target) else {
            continue;
        };
        let applies = world.registry.key(component).ok().and_then(|k| {
            let schema = world.registry.schema(k);
            let f = schema.field_index(field).ok()?;
            value.to_value(&schema.fields[f].ty, &|id| ids.get(id)).ok()
        });
        match (applies, entities[i].components.get_mut(component)) {
            (Some(_), Some(fields)) => {
                fields.insert(field.clone(), value.clone());
            }
            _ => call.push_diagnostic(
                Diagnostic::warning(
                    re_scene::codes::OVERRIDE_NOT_APPLIED,
                    format!("{source}: override of {component}.{field} on {target:?} not applied"),
                )
                .at(Location::entity(source.to_string())),
            ),
        }
    }
    let generated: Vec<GeneratedDoc> = nodes
        .iter()
        .map(|n| GeneratedDoc {
            source: source.to_string(),
            key: n.key.clone(),
        })
        .collect();
    let restored: Vec<String> = entities.iter().map(|d| d.id.clone()).collect();

    // Among the parent's children, after the source's entities that come
    // before it in the output, or else before those that come after it.
    let order = |e: &Entity| {
        world
            .ecs
            .get::<Generated>(*e)
            .filter(|g| g.source == s)
            .and_then(|g| fragment.iter().position(|n| n.key == g.key))
    };
    let siblings = hierarchy::children(&world.ecs, parent);
    let index = match siblings
        .iter()
        .rposition(|c| order(c).is_some_and(|i| i < at))
    {
        Some(i) => i + 1,
        None => siblings
            .iter()
            .position(|c| order(c).is_some())
            .unwrap_or_else(|| {
                siblings
                    .iter()
                    .filter(|c| world.ecs.get::<Generated>(**c).is_none())
                    .count()
            }),
    };

    call.transact(
        target,
        format!("proc.revert {id}"),
        vec![
            Op::Restore {
                entities,
                index: Some(index),
                generated,
                fresh: Vec::new(),
            },
            Op::SetOverride {
                source: source.to_string(),
                target: key.to_string(),
                component: None,
                field: None,
                value: None,
                deleted: Some(false),
                index: None,
                moved: None,
                replaced: None,
                insert: None,
            },
        ],
    )?;
    let mut data = json!({
        "source": source,
        "key": key,
        "id": id,
        "restored": restored,
    });
    if call.dry_run() {
        data["dry_run"] = Json::Bool(true);
    }
    Ok(data)
}

fn revert<C: EditTarget>(
    target: &mut C,
    a: RevertArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let bad = |m: &str| bad_argument("proc.revert", m.to_string());
    match a {
        RevertArgs {
            id: Some(id),
            component,
            field,
            source: None,
            key: None,
        } => {
            if field.is_some() && component.is_none() {
                return Err(bad("`field` needs `component`"));
            }
            revert_fields(target, &id, component.as_deref(), field.as_deref(), call)
        }
        RevertArgs {
            id: None,
            component: None,
            field: None,
            source: Some(source),
            key: Some(key),
        } => revert_deletion(target, &source, &key, call),
        RevertArgs {
            id: None,
            source: Some(_),
            key: Some(_),
            ..
        } => Err(bad(
            "`component` and `field` go with `id`: `source` and `key` revert a deletion",
        )),
        _ => Err(bad(
            "give `id` (with `component` and `field` to narrow it) to revert a generated \
             entity's field overrides, or `source` and `key` to bring a deleted one back",
        )),
    }
}

/// Registers `proc.convert_builder` and `proc.revert`.
pub(super) fn register<C: EditTarget + 'static>(registry: &mut CommandRegistry<C>) {
    registry.register_typed::<ConvertBuilderArgs, _>(
        "proc.convert_builder",
        "Turn a procedural source whose graph a builder script makes into one that reads a graph file: makes the file (default graphs/<source>.graph.json) with the built graph and `layout`, and points GraphSource.graph at it. One undo step; the project save writes the file, and after an undo removes it. The script is kept.",
        Tier::Edit,
        false,
        convert_builder::<C>,
    );
    registry.register_typed::<RevertArgs, _>(
        "proc.revert",
        "Put back what a procedural source generates: with `id` (and `component`, `field` to narrow it), a generated entity's field overrides go and the fields take the generated values; with `source` and `key`, the deletion override goes and the entity comes back as the source generates it. One undo step that restores the overrides and values exactly.",
        Tier::Edit,
        false,
        revert::<C>,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_file_is_named_after_the_source() {
        assert_eq!(default_path("island"), "graphs/island.graph.json");
        assert_eq!(default_path("Town/Hill 2"), "graphs/town_hill_2.graph.json");
        assert!(valid_graph_path(&default_path("Town/Hill 2")));
    }
}
