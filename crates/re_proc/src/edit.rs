//! The graph-editing commands (ADR-0044): `graph.add_node`,
//! `graph.remove_node`, `graph.connect`, `graph.disconnect`,
//! `graph.move_nodes` and `graph.set_default`. Each opens the graph file as
//! a document in the World (when it is not open yet) and changes it with one
//! transaction of `set_doc` operations, so the editor's canvas, `real do` and
//! agents share them, and each edit is one undo step. The file itself is
//! written by the project save (`scene.save`).
//!
//! The checks here are the ones an edit can break: node kinds and
//! parameters, ports and their types, and cycles. A required input left
//! unconnected is allowed while editing (generation reports it, `RE4006`).

use std::collections::{BTreeMap, BTreeSet};

use re_command::{Call, EditTarget, Op};
use re_core::{Diagnostic, Location};
use re_scene::World;
use re_types::graph::{EdgeDoc, GraphDoc, NodeDoc};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

use crate::NodeRegistry;
use crate::codes;
use crate::store::{GraphStore, parse_graph};

/// Arguments of `graph.add_node`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddNodeArgs {
    /// The graph file, relative to the project (`graphs/island.graph.json`).
    pub graph: String,
    /// The node kind (`describe` lists them).
    pub kind: String,
    /// The new node's id; a fresh one (`<kind>_<n>`) when omitted.
    #[serde(default)]
    pub id: Option<String>,
    /// The node's parameters (missing ones take the kind's defaults).
    #[serde(default)]
    pub params: Option<Map<String, Json>>,
    /// Where the canvas shows the node; omitted, the canvas lays it out.
    #[serde(default)]
    pub pos: Option<[f32; 2]>,
}

/// Arguments of `graph.remove_node`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveNodeArgs {
    /// The graph file, relative to the project.
    pub graph: String,
    /// The node's id; its edges and its layout entry go with it.
    pub node: String,
}

/// Arguments of `graph.connect` and `graph.disconnect`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectArgs {
    /// The graph file, relative to the project.
    pub graph: String,
    /// The output: `[node, port]` (the port is the output type's name, or `out`).
    pub from: (String, String),
    /// The input: `[node, port]`.
    pub to: (String, String),
}

/// Arguments of `graph.move_nodes`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveNodesArgs {
    /// The graph file, relative to the project.
    pub graph: String,
    /// The new canvas positions: `{"<node>": [x, y]}`.
    pub positions: BTreeMap<String, [f32; 2]>,
}

/// Arguments of `graph.set_default`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetDefaultArgs {
    /// The graph file, relative to the project.
    pub graph: String,
    /// The node's id.
    pub node: String,
    /// The parameter's name.
    pub param: String,
    /// The new value in the graph file, checked against the node kind;
    /// `null` removes it (the kind's default applies).
    pub value: Json,
}

/// One token of a JSON pointer (RFC 6901 escaping).
fn token(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}

fn invalid(path: &str, message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::INVALID_GRAPH, message).at(Location::file(path.to_string()))
}

/// Opens the graph file `graph` as a document (when it is not open) and
/// returns its normalised path and parsed content.
fn open_graph(world: &mut World, graph: &str) -> Result<(String, GraphDoc), Diagnostic> {
    let path = re_scene::doc_path(graph).map_err(|e| invalid(graph, e.to_string()))?;
    crate::register_doc_validators(world);
    let is_source_graph = world
        .ecs
        .query::<&re_scene::GraphSource>()
        .iter(&world.ecs)
        .any(|g| re_scene::doc_path(&g.graph).ok().as_deref() == Some(path.as_str()));
    if !path.ends_with(".graph.json") && !is_source_graph {
        return Err(invalid(
            &path,
            format!(
                "{path} is not a graph file (`*.graph.json`, or the `graph` of a procedural source)"
            ),
        ));
    }
    if world.doc(&path).is_none() {
        let store = world.ecs.get_resource::<GraphStore>();
        let text = match (
            store.and_then(GraphStore::files),
            store.and_then(GraphStore::root),
        ) {
            (Some(files), _) => files.read_str(&path).map_err(|e| e.to_string()),
            (None, Some(root)) => {
                std::fs::read_to_string(root.join(&path)).map_err(|e| e.to_string())
            }
            (None, None) => Err("the project folder is unknown".to_string()),
        }
        .map_err(|e| {
            Diagnostic::error(codes::NO_GRAPH, format!("{path} cannot be read: {e}"))
                .at(Location::file(path.clone()))
        })?;
        let value: Json = serde_json::from_str(&text)
            .map_err(|e| invalid(&path, format!("{path} is not JSON: {e}")))?;
        world
            .open_doc(&path, value)
            .map_err(|e| invalid(&path, e.to_string()))?;
    }
    let value = &world.doc(&path).expect("opened above").value;
    let doc = parse_graph(value).map_err(|e| {
        invalid(
            &path,
            format!("the open document {path} is not a valid graph: {e}"),
        )
        .with_hint("fix or undo the last edit first")
    })?;
    Ok((path, doc))
}

fn registry(world: &World) -> std::sync::Arc<crate::Evaluator> {
    world
        .ecs
        .get_resource::<GraphStore>()
        .map(|s| s.evaluator().clone())
        .unwrap_or_default()
}

fn node_index(doc: &GraphDoc, path: &str, id: &str) -> Result<usize, Diagnostic> {
    doc.nodes.iter().position(|n| n.id == id).ok_or_else(|| {
        let mut message = format!("{path} has no node `{id}`");
        if let Some(s) = re_reflect::suggest(id, doc.nodes.iter().map(|n| n.id.as_str())) {
            message.push_str(&format!(" (did you mean `{s}`?)"));
        }
        invalid(path, message)
    })
}

/// Checks `params` (in the kind's current shape) against `kind`.
fn check_params(
    registry: &NodeRegistry,
    path: &str,
    id: &str,
    kind: &str,
    version: u32,
    params: &Map<String, Json>,
) -> Result<(), Diagnostic> {
    let def = registry.get(kind).expect("the kind was checked");
    let params = Json::Object(params.clone());
    let current = def.migrate(version, &params).map_err(|(from, e)| {
        Diagnostic::error(
            codes::MIGRATION_FAILED,
            format!("node `{id}` ({kind}): its parameters could not be migrated from version {from}: {e}"),
        )
        .at(Location::file(path.to_string()))
    })?;
    def.check(&current).map_err(|e| {
        Diagnostic::error(codes::INVALID_PARAMS, format!("node `{id}` ({kind}): {e}"))
            .at(Location::file(path.to_string()))
            .with_hint("`describe` lists each node kind's parameters")
    })
}

/// True when the edges of `doc` lead from node `from` to node `to` (so an
/// edge `to -> from` would close a cycle).
fn reaches(doc: &GraphDoc, from: &str, to: &str) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![from];
    while let Some(n) = stack.pop() {
        if n == to {
            return true;
        }
        if !seen.insert(n) {
            continue;
        }
        stack.extend(
            doc.edges
                .iter()
                .filter(|e| e.from.0 == n)
                .map(|e| e.to.0.as_str()),
        );
    }
    false
}

/// The ops that remove the edges at `indices` (any order), highest first so
/// each index is still valid when its op runs.
fn remove_edges(path: &str, mut indices: Vec<usize>) -> Vec<Op> {
    indices.sort_unstable();
    indices.dedup();
    indices
        .into_iter()
        .rev()
        .map(|i| Op::SetDoc {
            doc: path.to_string(),
            pointer: format!("/edges/{i}"),
            value: None,
        })
        .collect()
}

fn edge_json(e: &EdgeDoc) -> Json {
    json!({ "from": [e.from.0, e.from.1], "to": [e.to.0, e.to.1] })
}

/// The op that sets the layout entries `positions` (the `layout` object is
/// created when the document has none).
fn layout_ops(path: &str, doc: &GraphDoc, positions: &BTreeMap<String, [f32; 2]>) -> Vec<Op> {
    if positions.is_empty() {
        return Vec::new();
    }
    if doc.layout.is_empty() {
        let layout: Map<String, Json> = positions
            .iter()
            .map(|(id, p)| (id.clone(), json!(p)))
            .collect();
        return vec![Op::SetDoc {
            doc: path.to_string(),
            pointer: "/layout".into(),
            value: Some(Json::Object(layout)),
        }];
    }
    positions
        .iter()
        .map(|(id, p)| Op::SetDoc {
            doc: path.to_string(),
            pointer: format!("/layout/{}", token(id)),
            value: Some(json!(p)),
        })
        .collect()
}

fn check_position(path: &str, id: &str, p: [f32; 2]) -> Result<(), Diagnostic> {
    if p.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(invalid(
            path,
            format!("the position of `{id}` must be finite, got {p:?}"),
        ))
    }
}

pub(crate) fn add_node<C: EditTarget>(
    target: &mut C,
    a: AddNodeArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let (path, doc) = open_graph(world, &a.graph)?;
    let evaluator = registry(world);
    let registry = evaluator.registry();
    let Some(def) = registry.get(&a.kind) else {
        let mut message = format!("unknown node kind `{}`", a.kind);
        if let Some(s) = re_reflect::suggest(&a.kind, registry.kinds()) {
            message.push_str(&format!(" (did you mean `{s}`?)"));
        }
        return Err(Diagnostic::error(codes::UNKNOWN_NODE_KIND, message)
            .at(Location::file(path))
            .with_hint("`describe` lists the node kinds"));
    };
    let id = match a.id {
        Some(id) if id.is_empty() => return Err(invalid(&path, "a node id must not be empty")),
        Some(id) if doc.node(&id).is_some() => {
            return Err(invalid(&path, format!("{path} already has a node `{id}`")));
        }
        Some(id) => id,
        None => {
            let short = a.kind.rsplit('.').next().unwrap_or(&a.kind).to_string();
            (1..)
                .map(|n| format!("{short}_{n}"))
                .find(|id| doc.node(id).is_none())
                .expect("some id is free")
        }
    };
    let params = a.params.unwrap_or_default();
    check_params(registry, &path, &id, &a.kind, def.version, &params)?;
    let node = NodeDoc {
        id: id.clone(),
        kind: a.kind.clone(),
        version: def.version,
        params,
    };
    let mut ops = vec![Op::SetDoc {
        doc: path.clone(),
        pointer: "/nodes/-".into(),
        value: Some(serde_json::to_value(&node).expect("a node serializes")),
    }];
    if let Some(p) = a.pos {
        check_position(&path, &id, p)?;
        ops.extend(layout_ops(&path, &doc, &BTreeMap::from([(id.clone(), p)])));
    }
    call.transact(target, format!("graph.add_node {id} ({})", a.kind), ops)?;
    Ok(json!({ "graph": path, "node": id, "kind": a.kind, "version": def.version }))
}

pub(crate) fn remove_node<C: EditTarget>(
    target: &mut C,
    a: RemoveNodeArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let (path, doc) = open_graph(target.world(), &a.graph)?;
    let index = node_index(&doc, &path, &a.node)?;
    let edges: Vec<usize> = doc
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| e.from.0 == a.node || e.to.0 == a.node)
        .map(|(i, _)| i)
        .collect();
    let removed: Vec<Json> = edges.iter().map(|&i| edge_json(&doc.edges[i])).collect();
    let mut ops = remove_edges(&path, edges);
    ops.push(Op::SetDoc {
        doc: path.clone(),
        pointer: format!("/nodes/{index}"),
        value: None,
    });
    if doc.layout.contains_key(&a.node) {
        ops.push(Op::SetDoc {
            doc: path.clone(),
            pointer: format!("/layout/{}", token(&a.node)),
            value: None,
        });
    }
    call.transact(target, format!("graph.remove_node {}", a.node), ops)?;
    Ok(json!({ "graph": path, "node": a.node, "edges_removed": removed }))
}

pub(crate) fn connect<C: EditTarget>(
    target: &mut C,
    a: ConnectArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let (path, doc) = open_graph(world, &a.graph)?;
    let evaluator = registry(world);
    let registry = evaluator.registry();
    let from = &doc.nodes[node_index(&doc, &path, &a.from.0)?];
    let to = &doc.nodes[node_index(&doc, &path, &a.to.0)?];
    let unknown_kind = |n: &NodeDoc| {
        Diagnostic::error(
            codes::UNKNOWN_NODE_KIND,
            format!("node `{}`: unknown node kind `{}`", n.id, n.kind),
        )
        .at(Location::file(path.clone()))
    };
    let fdef = registry.get(&from.kind).ok_or_else(|| unknown_kind(from))?;
    let tdef = registry.get(&to.kind).ok_or_else(|| unknown_kind(to))?;
    if a.from.1 != fdef.output_port() && a.from.1 != "out" {
        return Err(Diagnostic::error(
            codes::UNKNOWN_PORT,
            format!(
                "node `{}` ({}) has no output `{}` (its output is `{}`)",
                from.id,
                from.kind,
                a.from.1,
                fdef.output_port()
            ),
        )
        .at(Location::file(path)));
    }
    let Some(port) = tdef.input(&a.to.1) else {
        let names: Vec<&str> = tdef.inputs.iter().map(|p| p.name).collect();
        return Err(Diagnostic::error(
            codes::UNKNOWN_PORT,
            format!(
                "node `{}` ({}) has no input `{}` (inputs: {})",
                to.id,
                to.kind,
                a.to.1,
                if names.is_empty() {
                    "none".into()
                } else {
                    names.join(", ")
                }
            ),
        )
        .at(Location::file(path)));
    };
    if !port.accepts.contains(&fdef.output) {
        let accepts: Vec<String> = port.accepts.iter().map(ToString::to_string).collect();
        return Err(Diagnostic::error(
            codes::PORT_TYPE_MISMATCH,
            format!(
                "`{}` outputs {}, but input `{}` of `{}` ({}) takes {}",
                from.id,
                fdef.output,
                a.to.1,
                to.id,
                to.kind,
                accepts.join(" or ")
            ),
        )
        .at(Location::file(path)));
    }
    let edge = EdgeDoc {
        from: (from.id.clone(), fdef.output_port().to_string()),
        to: (to.id.clone(), a.to.1.clone()),
    };
    if from.id == to.id || reaches(&doc, &to.id, &from.id) {
        return Err(Diagnostic::error(
            codes::CYCLE,
            format!("connecting `{}` to `{}` would form a cycle", from.id, to.id),
        )
        .at(Location::file(path)));
    }
    let same = |e: &EdgeDoc| e.from.0 == edge.from.0 && e.to == edge.to;
    if doc.edges.iter().any(same) {
        return Ok(
            json!({ "graph": path, "edge": edge_json(&edge), "replaced": [], "unchanged": true }),
        );
    }
    // An input takes one edge (a variadic one takes many): an edge already
    // into it is replaced in the same transaction.
    let replaced: Vec<usize> = if tdef.variadic.is_some() {
        Vec::new()
    } else {
        doc.edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.to == edge.to)
            .map(|(i, _)| i)
            .collect()
    };
    let replaced_json: Vec<Json> = replaced.iter().map(|&i| edge_json(&doc.edges[i])).collect();
    let mut ops = remove_edges(&path, replaced);
    ops.push(Op::SetDoc {
        doc: path.clone(),
        pointer: "/edges/-".into(),
        value: Some(edge_json(&edge)),
    });
    call.transact(
        target,
        format!(
            "graph.connect {}.{} -> {}.{}",
            edge.from.0, edge.from.1, edge.to.0, edge.to.1
        ),
        ops,
    )?;
    Ok(json!({ "graph": path, "edge": edge_json(&edge), "replaced": replaced_json }))
}

pub(crate) fn disconnect<C: EditTarget>(
    target: &mut C,
    a: ConnectArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let (path, doc) = open_graph(target.world(), &a.graph)?;
    let matches = |e: &EdgeDoc| {
        e.from.0 == a.from.0
            && (e.from.1 == a.from.1 || a.from.1 == "out" || e.from.1 == "out")
            && e.to == a.to
    };
    let edges: Vec<usize> = doc
        .edges
        .iter()
        .enumerate()
        .filter(|(_, e)| matches(e))
        .map(|(i, _)| i)
        .collect();
    if edges.is_empty() {
        return Err(invalid(
            &path,
            format!(
                "{path} has no edge from `{}.{}` to `{}.{}`",
                a.from.0, a.from.1, a.to.0, a.to.1
            ),
        ));
    }
    call.transact(
        target,
        format!(
            "graph.disconnect {}.{} -> {}.{}",
            a.from.0, a.from.1, a.to.0, a.to.1
        ),
        remove_edges(&path, edges),
    )?;
    Ok(json!({ "graph": path, "from": a.from, "to": a.to }))
}

pub(crate) fn move_nodes<C: EditTarget>(
    target: &mut C,
    a: MoveNodesArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let (path, doc) = open_graph(target.world(), &a.graph)?;
    for (id, p) in &a.positions {
        node_index(&doc, &path, id)?;
        check_position(&path, id, *p)?;
    }
    let ops = layout_ops(&path, &doc, &a.positions);
    if !ops.is_empty() {
        let label = match a.positions.len() {
            1 => format!(
                "graph.move_nodes {}",
                a.positions.keys().next().expect("one")
            ),
            n => format!("graph.move_nodes ({n} nodes)"),
        };
        call.transact(target, label, ops)?;
    }
    Ok(json!({ "graph": path, "positions": a.positions }))
}

pub(crate) fn set_default<C: EditTarget>(
    target: &mut C,
    a: SetDefaultArgs,
    call: &mut Call,
) -> Result<Json, Diagnostic> {
    let world = target.world();
    let (path, doc) = open_graph(world, &a.graph)?;
    let evaluator = registry(world);
    let registry = evaluator.registry();
    let index = node_index(&doc, &path, &a.node)?;
    let node = &doc.nodes[index];
    if registry.get(&node.kind).is_none() {
        return Err(Diagnostic::error(
            codes::UNKNOWN_NODE_KIND,
            format!("node `{}`: unknown node kind `{}`", node.id, node.kind),
        )
        .at(Location::file(path)));
    }
    let mut params = node.params.clone();
    if a.value.is_null() {
        params.remove(&a.param);
    } else {
        params.insert(a.param.clone(), a.value.clone());
    }
    check_params(registry, &path, &node.id, &node.kind, node.version, &params)?;
    let pointer = format!("/nodes/{index}/params/{}", token(&a.param));
    let op = if a.value.is_null() {
        if !node.params.contains_key(&a.param) {
            return Ok(
                json!({ "graph": path, "node": a.node, "param": a.param, "value": a.value }),
            );
        }
        Op::SetDoc {
            doc: path.clone(),
            pointer,
            value: None,
        }
    } else if node.params.is_empty() {
        // `params` is omitted from the file when empty.
        Op::SetDoc {
            doc: path.clone(),
            pointer: format!("/nodes/{index}/params"),
            value: Some(Json::Object(params)),
        }
    } else {
        Op::SetDoc {
            doc: path.clone(),
            pointer,
            value: Some(a.value.clone()),
        }
    };
    call.transact(
        target,
        format!("graph.set_default {}.{}", a.node, a.param),
        vec![op],
    )?;
    Ok(json!({ "graph": path, "node": a.node, "param": a.param, "value": a.value }))
}
