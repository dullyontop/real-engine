//! The `proc` library: the ProcGraph builder (ADR-0015) and `proc.regenerate`.
//!
//! A builder script records a graph and returns it; it runs nothing:
//!
//! ```lua
//! local g = proc.graph("island", { seed = 1234 })
//! local h = g:node("terrain.heightfield", { size = 256, octaves = 5 })
//! local m = g:node("terrain.island_mask", { falloff = 0.6 })
//! local t = g:node("field.combine", { op = "mul" }, { a = h, b = m })
//! g:node("terrain.mesh", { cell = 1.0 }, { field = t })
//! return g
//! ```
//!
//! Inputs map an input port to a node (its output port [`DEFAULT_OUTPUT`]) or to
//! `node:out("port")`. A node is recorded at version 1 of its kind unless the
//! kind asks for another, as in `"terrain.heightfield@2"` (ADR-0043). Node ids are `n1`, `n2`, … in creation order unless an id
//! is given. The result is a [`GraphDoc`]; `re_script` does not depend on
//! `re_proc`, which validates node kinds and ports.

use std::cell::RefCell;
use std::rc::Rc;

use mlua::{AnyUserData, Lua, Result, Table, UserData, UserDataFields, UserDataMethods, Value};
use re_types::graph::{EdgeDoc, GraphDoc, NodeDoc};

use super::{Lib, json};
use crate::api::libs::host_err;
use crate::codes;
use crate::host::with_host;

/// The output port a bare node handle stands for.
pub const DEFAULT_OUTPUT: &str = "out";

/// The declarations of `Graph`, `GraphNode` and `GraphPort`.
pub(crate) const GRAPH_DECLS: &str = "-- A ProcGraph being recorded by a builder script.
declare extern type Graph with
    read name: string
    read seed: number
    -- Adds a node. `inputs` maps input ports to nodes or ports; `id` defaults to n1, n2, ...
    -- The node records version 1 of its kind; write 'kind@2' to ask for version 2.
    function node(self, kind: string, params: { [string]: any }?, inputs: { [string]: GraphNode | GraphPort }?, id: string?): GraphNode
    -- Connects an output to a node's input port.
    function connect(self, from: GraphNode | GraphPort, to: GraphNode, port: string): ()
end

-- A node of a Graph; as an input it stands for its \"out\" port.
declare extern type GraphNode with
    read id: string
    read kind: string
    -- A named output port of this node.
    function out(self, port: string): GraphPort
end

-- An output port of a GraphNode.
declare extern type GraphPort with
    read node: string
    read port: string
end";

/// A graph being recorded (`Graph` in Luau).
#[derive(Clone, Debug)]
pub struct GraphBuilder(pub Rc<RefCell<GraphDoc>>);

/// A node handle (`GraphNode`).
#[derive(Clone, Debug)]
struct NodeRef {
    graph: Rc<RefCell<GraphDoc>>,
    id: String,
    kind: String,
}

/// An output port (`GraphPort`).
#[derive(Clone, Debug)]
struct PortRef {
    graph: Rc<RefCell<GraphDoc>>,
    node: String,
    port: String,
}

fn bad(msg: impl std::fmt::Display) -> mlua::Error {
    super::error(codes::GRAPH_BUILDER, msg)
}

/// Reads a node or port handle that belongs to `graph`.
fn source(graph: &Rc<RefCell<GraphDoc>>, v: &Value, what: &str) -> Result<(String, String)> {
    if let Value::UserData(ud) = v {
        if let Ok(n) = ud.borrow::<NodeRef>() {
            if !Rc::ptr_eq(&n.graph, graph) {
                return Err(bad(format!(
                    "{what}: node {:?} belongs to another graph",
                    n.id
                )));
            }
            return Ok((n.id.clone(), DEFAULT_OUTPUT.to_string()));
        }
        if let Ok(p) = ud.borrow::<PortRef>() {
            if !Rc::ptr_eq(&p.graph, graph) {
                return Err(bad(format!(
                    "{what}: port of {:?} belongs to another graph",
                    p.node
                )));
            }
            return Ok((p.node.clone(), p.port.clone()));
        }
    }
    Err(bad(format!(
        "{what}: expected a GraphNode or GraphPort, got {}",
        v.type_name()
    )))
}

/// Splits `terrain.heightfield@2` into the kind and its version (1 when
/// there is no `@`).
fn split_version(kind: &str) -> mlua::Result<(String, u32)> {
    let Some((name, version)) = kind.rsplit_once('@') else {
        return Ok((kind.to_string(), 1));
    };
    match version.parse::<u32>() {
        Ok(v) if v >= 1 && !name.is_empty() => Ok((name.to_string(), v)),
        _ => Err(bad(format!(
            "node {kind:?}: a version is written `<kind>@<n>` with n a whole number from 1, as in \"terrain.heightfield@2\""
        ))),
    }
}

impl UserData for GraphBuilder {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("name", |_, g| Ok(g.0.borrow().name.clone()));
        fields.add_field_method_get("seed", |_, g| Ok(g.0.borrow().seed as f64));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method(
            "node",
            |lua, g, (kind, params, inputs, id): (String, Option<Table>, Option<Table>, Option<String>)| {
                // `kind@N` asks for version N; a bare kind is version 1, so a
                // builder means the same thing on every engine (ADR-0043).
                let (kind, version) = split_version(&kind)?;
                let params = match params {
                    Some(t) => match json::to_json(lua, &Value::Table(t))? {
                        serde_json::Value::Object(m) => m,
                        serde_json::Value::Array(a) if a.is_empty() => Default::default(),
                        _ => return Err(bad(format!("node {kind:?}: params must be a table of named values"))),
                    },
                    None => Default::default(),
                };
                let mut edges = Vec::new();
                if let Some(inputs) = inputs {
                    for pair in inputs.pairs::<String, Value>() {
                        let (port, v) = pair?;
                        let from = source(&g.0, &v, &format!("node {kind:?} input {port:?}"))?;
                        edges.push((from, port));
                    }
                }
                edges.sort_by(|a, b| a.1.cmp(&b.1));
                let mut doc = g.0.borrow_mut();
                let id = id.unwrap_or_else(|| format!("n{}", doc.nodes.len() + 1));
                if doc.node(&id).is_some() {
                    return Err(bad(format!("a node with id {id:?} already exists in graph {:?}", doc.name)));
                }
                doc.nodes.push(NodeDoc {
                    id: id.clone(),
                    kind: kind.clone(),
                    version,
                    params,
                });
                for (from, port) in edges {
                    doc.edges.push(EdgeDoc { from, to: (id.clone(), port) });
                }
                Ok(NodeRef { graph: g.0.clone(), id, kind })
            },
        );
        methods.add_method(
            "connect",
            |_, g, (from, to, port): (Value, AnyUserData, String)| {
                let from = source(&g.0, &from, "connect")?;
                let to = to
                    .borrow::<NodeRef>()
                    .map_err(|_| bad("connect: the target must be a GraphNode"))?
                    .clone();
                if !Rc::ptr_eq(&to.graph, &g.0) {
                    return Err(bad("connect: the target node belongs to another graph"));
                }
                g.0.borrow_mut().edges.push(EdgeDoc {
                    from,
                    to: (to.id, port),
                });
                Ok(())
            },
        );
        super::strict_userdata(methods, "Graph");
    }
}

impl UserData for NodeRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("id", |_, n| Ok(n.id.clone()));
        fields.add_field_method_get("kind", |_, n| Ok(n.kind.clone()));
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("out", |_, n, port: String| {
            Ok(PortRef {
                graph: n.graph.clone(),
                node: n.id.clone(),
                port,
            })
        });
        super::strict_userdata(methods, "GraphNode");
    }
}

impl UserData for PortRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("node", |_, p| Ok(p.node.clone()));
        fields.add_field_method_get("port", |_, p| Ok(p.port.clone()));
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        super::strict_userdata(methods, "GraphPort");
    }
}

/// The graph a builder chunk returned, validated.
pub(crate) fn graph_from_value(v: &Value) -> std::result::Result<GraphDoc, String> {
    let Value::UserData(ud) = v else {
        return Err(format!(
            "a builder script must return the graph made with proc.graph(...), but it returned a {}",
            v.type_name()
        ));
    };
    let g = ud.borrow::<GraphBuilder>().map_err(|_| {
        "a builder script must return the graph made with proc.graph(...)".to_string()
    })?;
    let doc = g.0.borrow().clone();
    doc.validate()
        .map_err(|e| format!("the graph is not valid: {e:?}"))?;
    Ok(doc)
}

pub(crate) fn lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "graph",
        "(name: string, options: { seed: number? }?) -> Graph",
        "Starts recording a ProcGraph; a builder script returns it.",
        |_, (name, options): (String, Option<Table>)| {
            let seed = match options {
                Some(t) => {
                    let seed: Option<f64> = t.get("seed")?;
                    match seed {
                        Some(s) if s.fract() != 0.0 || !s.is_finite() => {
                            return Err(bad(format!(
                                "proc.graph: seed must be an integer, got {s}"
                            )));
                        }
                        Some(s) => s as i64,
                        None => 0,
                    }
                }
                None => 0,
            };
            Ok(GraphBuilder(Rc::new(RefCell::new(GraphDoc::new(
                name, seed,
            )))))
        },
    )?;
    lib.func(
        lua,
        "regenerate",
        "(source_id: string, seed: number) -> ()",
        "Regenerates the procedural source with this stable id using `seed` (applied at the end of the frame).",
        |lua, (source, seed): (String, f64)| {
            if seed.fract() != 0.0 || !seed.is_finite() {
                return Err(super::error(
                    codes::BAD_ARGUMENT,
                    format!("proc.regenerate: seed must be an integer, got {seed}"),
                ));
            }
            with_host(lua, |h| h.services().regenerate(&source, seed as i64).map_err(host_err))
        },
    )
}
