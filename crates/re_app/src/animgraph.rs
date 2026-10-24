//! The animation graph commands (ADR-0039):
//!
//! * `animgraph.create {path, graph?}` writes a new `*.animgraph.json` file
//!   (an empty graph unless `graph` is given);
//! * `animgraph.edit {path, ops}` opens the file as a document (ADR-0044)
//!   and applies a batch of upserts and removals, each of one parameter,
//!   node, layer, mask, state or transition by id. The batch is validated as
//!   a whole and recorded as one undo step; the project save writes the
//!   file;
//! * `animgraph.validate {path, skin?}` checks a graph (the open document
//!   when there is one) and, with `skin`, binds it to that skin;
//! * `anim.set_param {entity, param, value}` sets a parameter of the graph an
//!   entity's `Animator` plays. It is runtime state, not undoable.

use std::path::Path;
use std::sync::Arc;

use re_anim::codes as anim_codes;
use re_anim::graph::{BoundGraph, Param, validate};
use re_command::{Call, CommandRegistry, Op, Tier};
use re_core::{Diagnostic, Location};
use re_scene::{World, entity_id};
use re_types::anim::{
    AnimGraph, GraphLayer, GraphNode, GraphParam, GraphState, JointMask, Transition,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value as Json, json};

use crate::app::App;
use crate::codes;

/// The suffix of animation graph files.
pub const SUFFIX: &str = ".animgraph.json";

/// Arguments of `animgraph.create`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    /// The new file, relative to the project, ending in `.animgraph.json`.
    pub path: String,
    /// The graph to write (validated); an empty graph when omitted.
    #[serde(default)]
    pub graph: Option<Json>,
}

/// Arguments of `animgraph.edit`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditArgs {
    /// The graph file, relative to the project.
    pub path: String,
    /// The changes, applied in order and checked together.
    pub ops: Vec<EditOp>,
}

/// The kinds of graph element an edit addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Element {
    /// A parameter.
    Param,
    /// A blend tree node.
    Node,
    /// A layer (with its states and transitions).
    Layer,
    /// A joint mask.
    Mask,
    /// A state of `layer`.
    State,
    /// A transition of `layer`.
    Transition,
}

/// One change of an `animgraph.edit` batch.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditOp {
    /// Replaces the element with the same `id` in place, or appends it.
    Upsert {
        /// What it is.
        element: Element,
        /// The layer, for a state or transition.
        #[serde(default)]
        layer: Option<String>,
        /// The element as written in the file, with its `id`.
        value: Json,
    },
    /// Removes the element `id`.
    Remove {
        /// What it is.
        element: Element,
        /// The layer, for a state or transition.
        #[serde(default)]
        layer: Option<String>,
        /// Its id.
        id: String,
    },
}

/// Arguments of `animgraph.validate`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateArgs {
    /// The graph file, relative to the project.
    pub path: String,
    /// A skinned mesh asset (`<model>#<skin>`) to bind the graph to as
    /// well, which loads its clips and resolves its masks.
    #[serde(default)]
    pub skin: Option<String>,
}

/// Arguments of `anim.set_param`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetParamArgs {
    /// The entity's stable id.
    pub entity: String,
    /// The parameter's id.
    pub param: String,
    /// A number for a float parameter, a boolean for a bool; `true` sets a
    /// trigger and `false` clears it.
    pub value: Json,
}

fn bad(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(anim_codes::GRAPH_INVALID, message)
}

/// `path` as a project path ending in `.animgraph.json`.
fn graph_path(path: &str) -> Result<String, Diagnostic> {
    let rel = crate::commands::project_path(path, "the graph file", anim_codes::GRAPH_INVALID)?;
    if !rel.ends_with(SUFFIX) {
        return Err(bad(format!(
            "`{rel}` is not an animation graph file (`*{SUFFIX}`)"
        ))
        .at(Location::file(rel.clone())));
    }
    Ok(rel)
}

/// The validator of open `*.animgraph.json` documents: the file parses and
/// passes [`validate`].
fn validate_doc(value: &Json) -> Result<(), String> {
    let graph: AnimGraph = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let problems = validate(&graph);
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems
            .iter()
            .map(|d| format!("[{}] {}", d.code, d.message))
            .collect::<Vec<_>>()
            .join("; "))
    }
}

/// Registers the open-document validator of animation graphs (idempotent).
pub fn register_doc_validator(world: &mut World) {
    world.register_doc_validator(SUFFIX, validate_doc);
}

fn parse(path: &str, value: &Json) -> Result<AnimGraph, Diagnostic> {
    serde_json::from_value(value.clone())
        .map_err(|e| bad(format!("{path} is not an animation graph: {e}")).at(Location::file(path)))
}

/// The graph at `path`: the open document if there is one, else the file.
fn read(app: &App, path: &str) -> Result<AnimGraph, Diagnostic> {
    if let Some(doc) = app.world().doc(path) {
        return parse(path, &doc.value);
    }
    let text = app.files().read_str(path).map_err(|e| {
        Diagnostic::error(
            codes::ASSET_UNAVAILABLE,
            format!("cannot read the graph {path}: {e}"),
        )
        .at(Location::file(path))
    })?;
    let value: Json = serde_json::from_str(&text)
        .map_err(|e| bad(format!("{path} is not JSON: {e}")).at(Location::file(path)))?;
    parse(path, &value)
}

fn problems_result(problems: &[Diagnostic]) -> Result<(), Diagnostic> {
    let mut problems = problems.iter().cloned();
    match problems.next() {
        None => Ok(()),
        Some(first) => Err(problems.fold(first, Diagnostic::with_related)),
    }
}

fn create(app: &mut App, a: CreateArgs, call: &mut Call) -> Result<Json, Diagnostic> {
    let path = graph_path(&a.path)?;
    let graph = match a.graph {
        Some(v) => parse(&path, &v)?,
        None => AnimGraph::new(),
    };
    problems_result(&validate(&graph))?;
    let Some(dir) = app.files().project_dir().map(Path::to_path_buf) else {
        return Err(Diagnostic::error(
            codes::PACKAGED_WRITE,
            format!("cannot create {path}: this run has no project directory"),
        ));
    };
    let file = dir.join(&path);
    if file.exists() || app.world().doc(&path).is_some() {
        return Err(bad(format!("{path} already exists")).at(Location::file(path.clone())));
    }
    let mut text = serde_json::to_string_pretty(&graph).expect("a graph serializes");
    text.push('\n');
    if !call.dry_run() {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| write_failed(&path, e))?;
        }
        std::fs::write(&file, text).map_err(|e| write_failed(&path, e))?;
    }
    Ok(json!({ "path": path }))
}

fn write_failed(path: &str, e: std::io::Error) -> Diagnostic {
    Diagnostic::error(
        codes::SCENE_SAVE_FAILED,
        format!("cannot write {path}: {e}"),
    )
    .at(Location::file(path))
}

/// Replaces the element of `list` whose id is `id` in place, or appends it.
fn upsert<T>(list: &mut Vec<T>, id_of: impl Fn(&T) -> &str, item: T) {
    let id = id_of(&item).to_string();
    match list.iter().position(|x| id_of(x) == id) {
        Some(i) => list[i] = item,
        None => list.push(item),
    }
}

fn remove<T>(
    list: &mut Vec<T>,
    id_of: impl Fn(&T) -> &str,
    id: &str,
    what: &str,
) -> Result<(), Diagnostic> {
    match list.iter().position(|x| id_of(x) == id) {
        Some(i) => {
            list.remove(i);
            Ok(())
        }
        None => Err(bad(format!("there is no {what} `{id}` to remove"))),
    }
}

fn item<T: serde::de::DeserializeOwned>(value: Json, what: &str) -> Result<T, Diagnostic> {
    serde_json::from_value(value).map_err(|e| bad(format!("the {what} is not valid: {e}")))
}

fn layer_mut<'a>(
    graph: &'a mut AnimGraph,
    layer: &Option<String>,
    what: &str,
) -> Result<&'a mut GraphLayer, Diagnostic> {
    let Some(id) = layer else {
        return Err(bad(format!("a {what} edit needs `layer`")));
    };
    graph
        .layers
        .iter_mut()
        .find(|l| l.id == *id)
        .ok_or_else(|| bad(format!("there is no layer `{id}`")))
}

/// Applies one edit to `graph`.
pub fn apply(graph: &mut AnimGraph, op: EditOp) -> Result<(), Diagnostic> {
    match op {
        EditOp::Upsert {
            element,
            layer,
            value,
        } => match element {
            Element::Param => upsert(
                &mut graph.params,
                |p: &GraphParam| &p.id,
                item(value, "param")?,
            ),
            Element::Node => upsert(
                &mut graph.nodes,
                |n: &GraphNode| &n.id,
                item(value, "node")?,
            ),
            Element::Layer => upsert(
                &mut graph.layers,
                |l: &GraphLayer| &l.id,
                item(value, "layer")?,
            ),
            Element::Mask => upsert(
                &mut graph.masks,
                |m: &JointMask| &m.id,
                item(value, "mask")?,
            ),
            Element::State => {
                let s: GraphState = item(value, "state")?;
                upsert(&mut layer_mut(graph, &layer, "state")?.states, |s| &s.id, s);
            }
            Element::Transition => {
                let t: Transition = item(value, "transition")?;
                upsert(
                    &mut layer_mut(graph, &layer, "transition")?.transitions,
                    |t| &t.id,
                    t,
                );
            }
        },
        EditOp::Remove { element, layer, id } => match element {
            Element::Param => remove(&mut graph.params, |p| &p.id, &id, "param")?,
            Element::Node => remove(&mut graph.nodes, |n| &n.id, &id, "node")?,
            Element::Layer => remove(&mut graph.layers, |l| &l.id, &id, "layer")?,
            Element::Mask => remove(&mut graph.masks, |m| &m.id, &id, "mask")?,
            Element::State => remove(
                &mut layer_mut(graph, &layer, "state")?.states,
                |s| &s.id,
                &id,
                "state",
            )?,
            Element::Transition => remove(
                &mut layer_mut(graph, &layer, "transition")?.transitions,
                |t| &t.id,
                &id,
                "transition",
            )?,
        },
    }
    Ok(())
}

fn edit(app: &mut App, a: EditArgs, call: &mut Call) -> Result<Json, Diagnostic> {
    let path = graph_path(&a.path)?;
    let at = |d: Diagnostic| {
        if d.location.is_some() {
            d
        } else {
            d.at(Location::file(path.clone()))
        }
    };
    let mut graph = read(app, &path)?;
    let count = a.ops.len();
    for (i, op) in a.ops.into_iter().enumerate() {
        apply(&mut graph, op).map_err(|d| {
            let message = format!("ops[{i}]: {}", d.message);
            at(Diagnostic { message, ..d })
        })?;
    }
    problems_result(&validate(&graph)).map_err(at)?;
    let value = serde_json::to_value(&graph).expect("a graph serializes");

    let world = app.world_mut();
    register_doc_validator(world);
    let opened = world.doc(&path).is_none();
    if opened {
        let text = app.files().read_str(&path).map_err(|e| {
            Diagnostic::error(
                codes::ASSET_UNAVAILABLE,
                format!("cannot read the graph {path}: {e}"),
            )
            .at(Location::file(path.clone()))
        })?;
        let saved: Json = serde_json::from_str(&text).map_err(|e| {
            bad(format!("{path} is not JSON: {e}")).at(Location::file(path.clone()))
        })?;
        app.world_mut()
            .open_doc(&path, saved)
            .map_err(|e| bad(e.to_string()).at(Location::file(path.clone())))?;
    }
    let unchanged = app.world().doc(&path).is_some_and(|d| d.value == value);
    let result = if unchanged {
        Ok(())
    } else {
        call.transact(
            app,
            format!(
                "animgraph.edit {path} ({count} op{})",
                if count == 1 { "" } else { "s" }
            ),
            vec![Op::SetDoc {
                doc: path.clone(),
                pointer: String::new(),
                value: Some(value),
            }],
        )
        .map(|_| ())
    };
    if opened && (unchanged || result.is_err() || call.dry_run()) {
        app.world_mut().close_doc(&path);
    }
    result?;
    Ok(json!({ "path": path, "changed": !unchanged }))
}

fn validate_cmd(app: &mut App, a: ValidateArgs) -> Result<Json, Diagnostic> {
    let path = graph_path(&a.path)?;
    let graph = read(app, &path)?;
    let mut problems = validate(&graph);
    let mut warnings = Vec::new();
    if problems.is_empty()
        && let Some(skin) = &a.skin
    {
        let media = app.media_mut();
        match media.skin(skin) {
            Err(d) => problems.push(d),
            Ok(loaded) => {
                let bound = BoundGraph::bind(&graph, &loaded.skin, &mut |clip| {
                    let clip = media.clip(clip)?;
                    Ok(Arc::new(re_anim::Sampler::new((*clip).clone())?))
                });
                match bound {
                    Ok((_, w)) => warnings = w,
                    Err(d) => problems.push(d),
                }
            }
        }
    }
    let list = |ds: &[Diagnostic]| {
        ds.iter()
            .map(|d| json!({ "code": d.code.to_string(), "message": d.message }))
            .collect::<Vec<_>>()
    };
    Ok(json!({
        "path": path,
        "valid": problems.is_empty(),
        "problems": list(&problems),
        "warnings": list(&warnings),
    }))
}

fn set_param(app: &mut App, a: SetParamArgs) -> Result<Json, Diagnostic> {
    let entity = app.world().entity(&a.entity).map_err(|e| {
        Diagnostic::error(codes::HOST_NO_SUCH_THING, e.to_string()).at(Location::entity(&a.entity))
    })?;
    let id = entity_id(entity);
    let player = app.skin_sync_mut().player_mut(id).ok_or_else(|| {
        Diagnostic::error(
            codes::HOST_NO_SUCH_THING,
            format!(
                "`{}` plays no animation graph (it needs a SkinnedMesh and an Animator whose graph loads; graphs start in the first fixed step)",
                a.entity
            ),
        )
        .at(Location::entity(&a.entity))
    })?;
    let current = player.get(&a.param)?;
    let value = match (current, &a.value) {
        (Param::Float(_), Json::Number(n)) => Param::Float(n.as_f64().unwrap_or(f64::NAN) as f32),
        (Param::Bool(_), Json::Bool(b)) => Param::Bool(*b),
        (Param::Trigger(_), Json::Bool(b)) => Param::Trigger(*b),
        (_, Json::Number(n)) => Param::Float(n.as_f64().unwrap_or(f64::NAN) as f32),
        (_, Json::Bool(b)) => Param::Bool(*b),
        (_, other) => {
            return Err(Diagnostic::error(
                anim_codes::GRAPH_PARAM_UNKNOWN,
                format!(
                    "animation graph parameter `{}`: the value must be a number or a boolean, not {other}",
                    a.param
                ),
            ));
        }
    };
    player.set(&a.param, value)?;
    Ok(json!({ "entity": a.entity, "param": a.param, "value": a.value }))
}

/// Registers the animation graph commands.
pub fn register(registry: &mut CommandRegistry<App>) {
    registry.register_typed::<CreateArgs, _>(
        "animgraph.create",
        "Create an animation graph file (`*.animgraph.json`, ADR-0039): an empty graph, or the `graph` given, validated. Fails if the file exists.",
        Tier::Edit,
        false,
        create,
    );
    registry.register_typed::<EditArgs, _>(
        "animgraph.edit",
        "Edit an animation graph file: a batch of `{op: \"upsert\", element, layer?, value}` and `{op: \"remove\", element, layer?, id}` changes (element: param, node, layer, mask, state or transition; layer for states and transitions), validated together and applied as one undo step. The project save writes the file.",
        Tier::Edit,
        false,
        edit,
    );
    registry.register_typed::<ValidateArgs, _>(
        "animgraph.validate",
        "Check an animation graph (the open document if there is one): ids, references, parameter types, ranges and blend-tree cycles; with `skin`, also bind it to that skinned mesh, loading its clips and resolving its masks.",
        Tier::Read,
        true,
        |app, args, _call| validate_cmd(app, args),
    );
    registry.register_typed::<SetParamArgs, _>(
        "anim.set_param",
        "Set a parameter of the animation graph an entity's Animator plays (runtime state, not undoable): a number for a float, a boolean for a bool, true to set a trigger.",
        Tier::Edit,
        false,
        |app, args, _call| set_param(app, args),
    );
}
