//! [`ProcGraph`]: the validated IR of a graph document (ARCHITECTURE §5).

use std::collections::BTreeMap;

use indexmap::IndexMap;
use re_core::Diagnostic;
use re_types::graph::{EdgeDoc, GraphDoc, KindDoc, NodeDoc, ParamDoc, PortDoc};
use serde_json::Value as Json;

use crate::codes;
use crate::nodes::NodeRegistry;
use crate::params::{self, Binding};
use crate::value::PortType;

/// A node id, unique within its graph.
pub type NodeId = String;
/// A port name.
pub type PortName = String;
/// A graph's id (its document's `name`).
pub type GraphId = String;

/// One node: its kind, the kind version it was written for, and its
/// parameters (a JSON object).
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// The node kind (`terrain.heightfield`).
    pub kind: String,
    /// The kind version the parameters were written for.
    pub version: u32,
    /// The parameters.
    pub params: Json,
}

/// An edge from an output port to an input port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edge {
    /// The source node and its output port.
    pub from: (NodeId, PortName),
    /// The destination node and input port.
    pub to: (NodeId, PortName),
}

/// A declared output of a graph (ADR-0061 §4) and what feeds it.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphOutput {
    /// The declaration.
    pub port: PortDoc,
    /// The node output wired to `["$out", <name>]`, as `(node, port)`; or
    /// `("$in", <input>)` for a kind's input wired straight to it.
    pub from: (NodeId, PortName),
}

/// An edge from a kind graph's declared input, `["$in", <input>]`, to a
/// node input (ADR-0061 §4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputEdge {
    /// The declared input.
    pub input: String,
    /// The node and input port it feeds.
    pub to: (NodeId, PortName),
}

/// A validated graph: every node kind exists, parameters match their
/// schemas, edges connect existing ports of matching types, required inputs
/// are connected and there is no cycle. Graph params are bound: each node
/// param bound to one holds its value (ADR-0061 §2).
#[derive(Clone, Debug, PartialEq)]
pub struct ProcGraph {
    /// The graph's name.
    pub id: GraphId,
    /// The default seed (a procedural source's `Procedural.seed` wins).
    pub seed: u64,
    /// The nodes, in document order.
    pub nodes: IndexMap<NodeId, Node>,
    /// The edges, in document order.
    pub edges: Vec<Edge>,
    /// The document's node positions on the editor's canvas, carried through
    /// [`from_doc`](Self::from_doc) and [`to_doc`](Self::to_doc) unchanged;
    /// evaluation never reads them (ADR-0044).
    pub layout: BTreeMap<NodeId, [f32; 2]>,
    /// The `kind` block of a graph that is a node kind (ADR-0061 §6).
    pub kind: Option<KindDoc>,
    /// The graph's params, in declaration order. Each default is the value
    /// in use: a source's values replace the defaults before the graph is
    /// built ([`params::apply_values`]).
    pub params: Vec<ParamDoc>,
    /// The node params bound to graph params, in document order. `nodes`
    /// holds their values; [`to_doc`](Self::to_doc) writes the references
    /// back.
    pub bindings: Vec<Binding>,
    /// A node kind's declared inputs.
    pub inputs: Vec<PortDoc>,
    /// The edges from declared inputs to node inputs. Nothing feeds them
    /// when the graph is evaluated on its own.
    pub input_edges: Vec<InputEdge>,
    /// The declared outputs, in declaration order, each with what feeds it.
    /// When there are any, they are the graph's output instead of its sinks.
    pub outputs: Vec<GraphOutput>,
}

fn diag(code: re_core::Code, graph: &str, message: String) -> Diagnostic {
    Diagnostic::error(code, format!("graph `{graph}`: {message}"))
}

/// The output port name every node also answers to: an edge from
/// `[node, "out"]` is read as an edge from that node's only output port.
pub const OUT_ALIAS: &str = "out";

impl ProcGraph {
    /// Builds and validates the IR of `doc`. Every problem found is
    /// returned (`RE4000`–`RE4007`, `RE4012`, and ADR-0061's `RE4200`–`RE4206`),
    /// not only the first. Graph params are bound with their defaults
    /// ([`params::resolve`]; apply a source's values first with
    /// [`params::apply_values`]). An edge from port [`OUT_ALIAS`] is stored
    /// with the node's real output port. Edges from `$in` and to `$out` are
    /// kept apart, as [`input_edges`](Self::input_edges) and
    /// [`outputs`](Self::outputs).
    pub fn from_doc(doc: &GraphDoc, registry: &NodeRegistry) -> Result<ProcGraph, Vec<Diagnostic>> {
        if let Err(e) = doc.validate() {
            return Err(vec![
                diag(codes::INVALID_GRAPH, &doc.name, e.to_string())
                    .with_hint("see graph.schema.json (`real schema`) for the format"),
            ]);
        }
        let bindings = params::bindings(doc);
        let doc = &params::resolve(doc, registry)?;
        let mut problems = check_ports(doc, registry);
        // `out` names a node's only output port, whatever its type (what
        // script builders write for a bare node handle).
        let resolve = |from: &(String, String)| {
            let mut from = from.clone();
            if from.1 == OUT_ALIAS
                && let Some(def) = doc.node(&from.0).and_then(|n| registry.get(&n.kind))
            {
                from.1 = def.output_port().to_string();
            }
            from
        };
        let graph = ProcGraph {
            id: doc.name.clone(),
            seed: doc.seed as u64,
            nodes: doc
                .nodes
                .iter()
                .map(|n| {
                    (
                        n.id.clone(),
                        Node {
                            kind: n.kind.clone(),
                            version: n.version,
                            params: Json::Object(n.params.clone()),
                        },
                    )
                })
                .collect(),
            edges: doc
                .edges
                .iter()
                .filter(|e| e.from.0 != GraphDoc::INPUTS && e.to.0 != GraphDoc::OUTPUTS)
                .map(|e| Edge {
                    from: resolve(&e.from),
                    to: e.to.clone(),
                })
                .collect(),
            layout: doc.layout.clone(),
            kind: doc.kind.clone(),
            params: doc.params.clone(),
            bindings,
            inputs: doc.inputs.clone(),
            input_edges: doc
                .edges
                .iter()
                .filter(|e| e.from.0 == GraphDoc::INPUTS && e.to.0 != GraphDoc::OUTPUTS)
                .map(|e| InputEdge {
                    input: e.from.1.clone(),
                    to: e.to.clone(),
                })
                .collect(),
            outputs: doc
                .outputs
                .iter()
                .filter_map(|port| {
                    let e = doc
                        .edges
                        .iter()
                        .find(|e| e.to.0 == GraphDoc::OUTPUTS && e.to.1 == port.name)?;
                    Some(GraphOutput {
                        port: port.clone(),
                        from: resolve(&e.from),
                    })
                })
                .collect(),
        };
        problems.extend(graph.validate(registry));
        if problems.is_empty() {
            Ok(graph)
        } else {
            Err(problems)
        }
    }

    /// The graph as a document: its bound node params as references again,
    /// its declarations, and its edges from `$in` and to `$out` after the
    /// others. It is at version 2 when it uses something version 2 holds.
    pub fn to_doc(&self) -> GraphDoc {
        let mut doc = GraphDoc::new(self.id.clone(), self.seed as i64);
        doc.kind = self.kind.clone();
        doc.params = self.params.clone();
        doc.inputs = self.inputs.clone();
        doc.outputs = self.outputs.iter().map(|o| o.port.clone()).collect();
        doc.nodes = self
            .nodes
            .iter()
            .map(|(id, n)| {
                let mut params = n.params.as_object().cloned().unwrap_or_default();
                for b in self.bindings.iter().filter(|b| &b.node == id) {
                    params.insert(b.param.clone(), params::reference(&b.graph_param));
                }
                NodeDoc {
                    id: id.clone(),
                    kind: n.kind.clone(),
                    version: n.version,
                    params,
                }
            })
            .collect();
        doc.edges = self
            .edges
            .iter()
            .map(|e| EdgeDoc {
                from: e.from.clone(),
                to: e.to.clone(),
            })
            .chain(self.input_edges.iter().map(|e| EdgeDoc {
                from: (GraphDoc::INPUTS.to_string(), e.input.clone()),
                to: e.to.clone(),
            }))
            .chain(self.outputs.iter().map(|o| EdgeDoc {
                from: o.from.clone(),
                to: (GraphDoc::OUTPUTS.to_string(), o.port.name.clone()),
            }))
            .collect();
        doc.layout = self.layout.clone();
        doc.raise_version();
        doc
    }

    /// Every problem with this graph against `registry`.
    pub fn validate(&self, registry: &NodeRegistry) -> Vec<Diagnostic> {
        let g = self.id.as_str();
        let mut out = Vec::new();
        for (id, node) in &self.nodes {
            let Some(def) = registry.get(&node.kind) else {
                let mut message = format!("node `{id}`: unknown node kind `{}`", node.kind);
                if let Some(s) = re_reflect::suggest(&node.kind, registry.kinds()) {
                    message.push_str(&format!(" (did you mean `{s}`?)"));
                }
                out.push(
                    diag(codes::UNKNOWN_NODE_KIND, g, message)
                        .with_hint("`describe` lists the node kinds"),
                );
                continue;
            };
            if node.version == 0 || node.version > def.version {
                out.push(diag(
                    codes::UNSUPPORTED_NODE_VERSION,
                    g,
                    format!(
                        "node `{id}` ({}): version {} is not supported (this engine has version {})",
                        node.kind, node.version, def.version
                    ),
                ));
                continue;
            }
            if !def.supports(node.version) {
                out.push(
                    diag(
                        codes::RETIRED_NODE_VERSION,
                        g,
                        format!(
                            "node `{id}` ({}): version {} is retired; this engine evaluates versions {} to {}",
                            node.kind, node.version, def.oldest, def.version
                        ),
                    )
                    .with_hint("run `real migrate-graph` with an engine release that still evaluates it"),
                );
                continue;
            }
            match def.migrate(node.version, &node.params) {
                Ok(params) => {
                    if let Err(e) = def.check(&params) {
                        out.push(diag(
                            codes::INVALID_PARAMS,
                            g,
                            format!("node `{id}` ({}): {e}", node.kind),
                        ));
                    }
                }
                Err((from, e)) => out.push(diag(
                    codes::MIGRATION_FAILED,
                    g,
                    format!(
                        "node `{id}` ({}): its parameters could not be migrated from version {from} to {}: {e}",
                        node.kind,
                        from + 1
                    ),
                )),
            }
        }
        // Edges: ports and types.
        let mut into: IndexMap<(&str, &str), usize> = IndexMap::new();
        for (i, e) in self.edges.iter().enumerate() {
            let (Some(from), Some(to)) = (self.nodes.get(&e.from.0), self.nodes.get(&e.to.0))
            else {
                out.push(diag(
                    codes::INVALID_GRAPH,
                    g,
                    format!("edge {i} names a node that does not exist"),
                ));
                continue;
            };
            let (Some(fdef), Some(tdef)) = (registry.get(&from.kind), registry.get(&to.kind))
            else {
                continue;
            };
            if e.from.1 != fdef.output_port() {
                out.push(diag(
                    codes::UNKNOWN_PORT,
                    g,
                    format!(
                        "edge {i}: node `{}` ({}) has no output `{}` (its output is `{}`)",
                        e.from.0,
                        from.kind,
                        e.from.1,
                        fdef.output_port()
                    ),
                ));
                continue;
            }
            let Some(port) = tdef.input(&e.to.1) else {
                let names: Vec<&str> = tdef.inputs.iter().map(|p| p.name).collect();
                out.push(diag(
                    codes::UNKNOWN_PORT,
                    g,
                    format!(
                        "edge {i}: node `{}` ({}) has no input `{}` (inputs: {})",
                        e.to.0,
                        to.kind,
                        e.to.1,
                        if names.is_empty() {
                            "none".into()
                        } else {
                            names.join(", ")
                        }
                    ),
                ));
                continue;
            };
            if !port.accepts.contains(&fdef.output) {
                let accepts: Vec<String> = port.accepts.iter().map(ToString::to_string).collect();
                out.push(diag(
                    codes::PORT_TYPE_MISMATCH,
                    g,
                    format!(
                        "edge {i}: `{}` outputs {}, but input `{}` of `{}` ({}) takes {}",
                        e.from.0,
                        fdef.output,
                        e.to.1,
                        e.to.0,
                        to.kind,
                        accepts.join(" or ")
                    ),
                ));
            }
            *into.entry((e.to.0.as_str(), e.to.1.as_str())).or_default() += 1;
        }
        // A node kind's declared inputs connect node inputs too (their ports
        // and types are checked with the declarations).
        for e in &self.input_edges {
            if self.nodes.contains_key(&e.to.0) {
                *into.entry((e.to.0.as_str(), e.to.1.as_str())).or_default() += 1;
            }
        }
        for ((node, port), count) in &into {
            let kind = &self.nodes[*node].kind;
            let variadic = registry.get(kind).is_some_and(|d| d.variadic.is_some());
            if *count > 1 && !variadic {
                out.push(diag(
                    codes::INPUT_CONNECTED_TWICE,
                    g,
                    format!(
                        "node `{node}` ({kind}): input `{port}` has {count} edges, it takes one"
                    ),
                ));
            }
        }
        for (id, node) in &self.nodes {
            let Some(def) = registry.get(&node.kind) else {
                continue;
            };
            for port in def.inputs.iter().filter(|p| p.required) {
                if !into.contains_key(&(id.as_str(), port.name)) {
                    out.push(diag(
                        codes::MISSING_INPUT,
                        g,
                        format!(
                            "node `{id}` ({}): input `{}` is not connected",
                            node.kind, port.name
                        ),
                    ));
                }
            }
        }
        if let Err(cycle) = self.levels() {
            out.push(diag(
                codes::CYCLE,
                g,
                format!("the edges form a cycle through nodes {}", cycle.join(", ")),
            ));
        }
        out
    }

    /// A warning (`RE4019`) for each node written for an older version of
    /// its kind than this engine's current one. The node still evaluates at
    /// its own version; `real migrate-graph` upgrades it.
    pub fn outdated(&self, registry: &NodeRegistry) -> Vec<Diagnostic> {
        outdated_nodes(
            &self.id,
            self.nodes
                .iter()
                .map(|(id, n)| (id.as_str(), n.kind.as_str(), n.version)),
            registry,
        )
    }

    /// The graph's version stamp ([`crate::version_stamp`]).
    pub fn version_stamp(&self, registry: &NodeRegistry) -> blake3::Hash {
        crate::canon::stamp(
            self.nodes
                .iter()
                .map(|(id, n)| (id.as_str(), n.kind.as_str(), n.version)),
            registry,
        )
    }

    /// The nodes grouped by depth: level 0 has no inputs, and each node is
    /// one level after its deepest input. Within a level, nodes keep document
    /// order. `Err` lists the nodes on or after a cycle.
    pub fn levels(&self) -> Result<Vec<Vec<usize>>, Vec<String>> {
        let n = self.nodes.len();
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n];
        for e in &self.edges {
            if let (Some(f), Some(t)) = (
                self.nodes.get_index_of(&e.from.0),
                self.nodes.get_index_of(&e.to.0),
            ) {
                preds[t].push(f);
            }
        }
        let mut level: Vec<Option<usize>> = vec![None; n];
        let mut done = 0;
        loop {
            let mut progressed = false;
            for i in 0..n {
                if level[i].is_some() {
                    continue;
                }
                if preds[i].iter().all(|p| level[*p].is_some()) {
                    level[i] = Some(
                        preds[i]
                            .iter()
                            .map(|p| level[*p].unwrap_or(0) + 1)
                            .max()
                            .unwrap_or(0),
                    );
                    done += 1;
                    progressed = true;
                }
            }
            if done == n {
                break;
            }
            if !progressed {
                return Err((0..n)
                    .filter(|i| level[*i].is_none())
                    .map(|i| {
                        self.nodes
                            .get_index(i)
                            .map(|(k, _)| k.clone())
                            .unwrap_or_default()
                    })
                    .collect());
            }
        }
        let depth = level.iter().flatten().max().map_or(0, |m| m + 1);
        let mut out = vec![Vec::new(); depth];
        for (i, l) in level.into_iter().enumerate() {
            if let Some(l) = l {
                out[l].push(i);
            }
        }
        Ok(out)
    }

    /// The input edges of node `index`, as `(port, source node index)`, in
    /// document order.
    pub fn inputs_of(&self, index: usize) -> Vec<(&str, usize)> {
        let id = self.nodes.get_index(index).map(|(k, _)| k.as_str());
        self.edges
            .iter()
            .filter(|e| Some(e.to.0.as_str()) == id)
            .filter_map(|e| Some((e.to.1.as_str(), self.nodes.get_index_of(&e.from.0)?)))
            .collect()
    }

    /// True if no edge leaves node `index` (its output is part of the
    /// graph's output when it is a fragment or a mesh, and the graph
    /// declares no outputs).
    pub fn is_sink(&self, index: usize) -> bool {
        let id = self.nodes.get_index(index).map(|(k, _)| k.as_str());
        !self.edges.iter().any(|e| Some(e.from.0.as_str()) == id)
    }

    /// The nodes whose outputs make the graph's output, in order (ADR-0061
    /// §4): when the graph declares outputs, the nodes that feed its
    /// `fragment` and `mesh` outputs, in declaration order and each once;
    /// otherwise its sinks, in document order. Other nodes still run, and
    /// add nothing.
    pub fn output_nodes(&self) -> Vec<usize> {
        if self.outputs.is_empty() {
            let mut sink = vec![true; self.nodes.len()];
            for e in &self.edges {
                if let Some(i) = self.nodes.get_index_of(&e.from.0) {
                    sink[i] = false;
                }
            }
            return (0..self.nodes.len()).filter(|&i| sink[i]).collect();
        }
        let mut out = Vec::new();
        for o in &self.outputs {
            let entities = matches!(
                PortType::from_name(&o.port.ty),
                Some(PortType::Fragment | PortType::Mesh)
            );
            if let Some(i) = self.nodes.get_index_of(&o.from.0)
                && entities
                && !out.contains(&i)
            {
                out.push(i);
            }
        }
        out
    }
}

/// Gives each declared output of `doc` written without a type (what a
/// builder's `g:output` records, ADR-0061 §10) the type of what feeds it: a
/// node's output, or a declared input. An output nothing feeds keeps its
/// empty type, which [`ProcGraph::from_doc`] reports (`RE4205`, `RE4206`).
pub fn fill_output_types(doc: &mut GraphDoc, registry: &NodeRegistry) {
    let types: Vec<(usize, String)> = doc
        .outputs
        .iter()
        .enumerate()
        .filter(|(_, o)| o.ty.is_empty())
        .filter_map(|(i, o)| {
            let e = doc
                .edges
                .iter()
                .find(|e| e.to.0 == GraphDoc::OUTPUTS && e.to.1 == o.name)?;
            let ty = if e.from.0 == GraphDoc::INPUTS {
                doc.inputs.iter().find(|p| p.name == e.from.1)?.ty.clone()
            } else {
                let def = registry.get(&doc.node(&e.from.0)?.kind)?;
                def.output.port_name().to_string()
            };
            Some((i, ty))
        })
        .collect();
    for (i, ty) in types {
        doc.outputs[i].ty = ty;
    }
}

/// True for a declared port's name: letters, digits and `_`.
fn valid_port_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The problems with `doc`'s declared inputs and outputs (`RE4205`) and
/// with the edges from `$in` and to `$out` (`RE4206`, and `RE4002` and
/// `RE4003` for the node ports they name), ADR-0061 §4.
fn check_ports(doc: &GraphDoc, registry: &NodeRegistry) -> Vec<Diagnostic> {
    let g = doc.name.as_str();
    let mut out = Vec::new();
    let port_type = |p: &PortDoc| PortType::from_name(&p.ty);
    for (side, ports) in [("input", &doc.inputs), ("output", &doc.outputs)] {
        for (i, p) in ports.iter().enumerate() {
            let mut bad = |message: String| out.push(diag(codes::INVALID_GRAPH_PORT, g, message));
            if !valid_port_name(&p.name) {
                bad(format!(
                    "{side} name `{}` is not allowed: letters, digits and `_`",
                    p.name
                ));
            }
            if ports[..i].iter().any(|q| q.name == p.name) {
                bad(format!("{side} `{}` is declared twice", p.name));
            }
            match port_type(p) {
                None if p.ty.is_empty() => bad(format!("{side} `{}` has no type", p.name)),
                None => {
                    let mut message = format!(
                        "{side} `{}` has type `{}`, which is not a port type",
                        p.name, p.ty
                    );
                    if let Some(s) =
                        re_reflect::suggest(&p.ty, PortType::ALL.iter().map(|t| t.port_name()))
                    {
                        message.push_str(&format!(" (did you mean `{s}`?)"));
                    }
                    bad(message);
                }
                Some(PortType::Fragment) if side == "input" => bad(format!(
                    "input `{}` is a `fragment`; a node kind's inputs cannot be fragments",
                    p.name
                )),
                Some(_) => {}
            }
            if side == "output" && p.required {
                bad(format!(
                    "output `{}` is `required`, which only inputs are",
                    p.name
                ));
            }
        }
    }
    if !doc.inputs.is_empty() && doc.kind.is_none() {
        out.push(
            diag(
                codes::INVALID_GRAPH_PORT,
                g,
                "the graph declares inputs, which only a node kind (a graph with a `kind` block) has".into(),
            )
            .with_hint("remove `inputs`, or make the graph a node kind"),
        );
    }
    let mut wiring = |message: String| out.push(diag(codes::GRAPH_PORT_WIRING, g, message));
    for (i, e) in doc.edges.iter().enumerate() {
        if e.from.0 == GraphDoc::INPUTS && !doc.inputs.iter().any(|p| p.name == e.from.1) {
            wiring(format!(
                "edge {i} comes from input `{}`, which the graph does not declare",
                e.from.1
            ));
        }
        if e.to.0 == GraphDoc::OUTPUTS && !doc.outputs.iter().any(|p| p.name == e.to.1) {
            wiring(format!(
                "edge {i} goes to output `{}`, which the graph does not declare",
                e.to.1
            ));
        }
    }
    // What an edge's start carries: a declared input's type, or a node
    // output's (`Err` when the node has no such output).
    let carried = |from: &(String, String)| -> Option<Result<PortType, String>> {
        if from.0 == GraphDoc::INPUTS {
            let p = doc.inputs.iter().find(|p| p.name == from.1)?;
            return port_type(p).map(Ok);
        }
        let def = doc.node(&from.0).and_then(|n| registry.get(&n.kind))?;
        Some(if from.1 == OUT_ALIAS || from.1 == def.output_port() {
            Ok(def.output)
        } else {
            Err(format!(
                "node `{}` has no output `{}` (its output is `{}`)",
                from.0,
                from.1,
                def.output_port()
            ))
        })
    };
    for p in &doc.outputs {
        let Some(want) = port_type(p) else {
            continue;
        };
        let feeds: Vec<(usize, &EdgeDoc)> = doc
            .edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.to.0 == GraphDoc::OUTPUTS && e.to.1 == p.name)
            .collect();
        match feeds.as_slice() {
            [] => out.push(
                diag(
                    codes::GRAPH_PORT_WIRING,
                    g,
                    format!(
                        "output `{}` ({}) has no edge into [\"$out\", \"{}\"]",
                        p.name, p.ty, p.name
                    ),
                )
                .with_hint("connect one node output of the declared type to it"),
            ),
            [(i, e)] => match carried(&e.from) {
                Some(Ok(got)) if got != want => out.push(diag(
                    codes::GRAPH_PORT_WIRING,
                    g,
                    format!(
                        "output `{}` is declared `{}`, but edge {i} brings a `{}` from `{}`",
                        p.name,
                        p.ty,
                        got.port_name(),
                        e.from.0
                    ),
                )),
                Some(Err(message)) => {
                    out.push(diag(codes::UNKNOWN_PORT, g, format!("edge {i}: {message}")))
                }
                _ => {}
            },
            many => out.push(diag(
                codes::GRAPH_PORT_WIRING,
                g,
                format!(
                    "output `{}` has {} edges into it (edges {}); it takes one",
                    p.name,
                    many.len(),
                    many.iter()
                        .map(|(i, _)| i.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )),
        }
    }
    // Edges from inputs to node inputs: the port exists and takes the type.
    for (i, e) in doc.edges.iter().enumerate() {
        if e.from.0 != GraphDoc::INPUTS || e.to.0 == GraphDoc::OUTPUTS {
            continue;
        }
        let (Some(Ok(ty)), Some(node)) = (carried(&e.from), doc.node(&e.to.0)) else {
            continue;
        };
        let Some(def) = registry.get(&node.kind) else {
            continue;
        };
        match def.input(&e.to.1) {
            None => out.push(diag(
                codes::UNKNOWN_PORT,
                g,
                format!(
                    "edge {i}: node `{}` ({}) has no input `{}`",
                    e.to.0, node.kind, e.to.1
                ),
            )),
            Some(port) if !port.accepts.contains(&ty) => out.push(diag(
                codes::PORT_TYPE_MISMATCH,
                g,
                format!(
                    "edge {i}: input `{}` brings a `{}`, but input `{}` of `{}` ({}) takes {}",
                    e.from.1,
                    ty.port_name(),
                    e.to.1,
                    e.to.0,
                    node.kind,
                    port.accepts
                        .iter()
                        .map(|t| t.port_name())
                        .collect::<Vec<_>>()
                        .join(" or ")
                ),
            )),
            Some(_) => {}
        }
    }
    out
}

/// `RE4019` warnings for nodes `(id, kind, version)` older than their kind.
pub(crate) fn outdated_nodes<'a>(
    graph: &str,
    nodes: impl Iterator<Item = (&'a str, &'a str, u32)>,
    registry: &NodeRegistry,
) -> Vec<Diagnostic> {
    nodes
        .filter_map(|(id, kind, version)| {
            let def = registry.get(kind)?;
            (version >= 1 && version < def.version).then(|| {
                Diagnostic::warning(
                    codes::OUTDATED_NODE_VERSION,
                    format!(
                        "graph `{graph}`: node `{id}` ({kind}) is written for version {version}; this engine's current version is {}",
                        def.version
                    ),
                )
                .with_hint(format!(
                    "it still evaluates as version {version}; run `real migrate-graph` to upgrade it and see what changes"
                ))
            })
        })
        .collect()
}
