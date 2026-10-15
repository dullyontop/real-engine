//! [`ProcGraph`]: the validated IR of a graph document (ARCHITECTURE §5).

use indexmap::IndexMap;
use re_core::Diagnostic;
use re_types::graph::{EdgeDoc, GraphDoc, NodeDoc};
use serde_json::Value as Json;

use crate::codes;
use crate::nodes::NodeRegistry;

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

/// A validated graph: every node kind exists, parameters match their
/// schemas, edges connect existing ports of matching types, required inputs
/// are connected and there is no cycle.
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
}

fn diag(code: re_core::Code, graph: &str, message: String) -> Diagnostic {
    Diagnostic::error(code, format!("graph `{graph}`: {message}"))
}

/// The output port name every node also answers to: an edge from
/// `[node, "out"]` is read as an edge from that node's only output port.
pub const OUT_ALIAS: &str = "out";

impl ProcGraph {
    /// Builds and validates the IR of `doc`. Every problem found is
    /// returned (`RE4000`–`RE4007`, `RE4012`), not only the first. An edge
    /// from port [`OUT_ALIAS`] is stored with the node's real output port.
    pub fn from_doc(doc: &GraphDoc, registry: &NodeRegistry) -> Result<ProcGraph, Vec<Diagnostic>> {
        if let Err(e) = doc.validate() {
            return Err(vec![
                diag(codes::INVALID_GRAPH, &doc.name, e.to_string())
                    .with_hint("see graph.schema.json (`real schema`) for the format"),
            ]);
        }
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
                .map(|e| {
                    // `out` names a node's only output port, whatever its type
                    // (what script builders write for a bare node handle).
                    let mut from = e.from.clone();
                    if from.1 == OUT_ALIAS
                        && let Some(def) = doc.node(&from.0).and_then(|n| registry.get(&n.kind))
                    {
                        from.1 = def.output_port().to_string();
                    }
                    Edge {
                        from,
                        to: e.to.clone(),
                    }
                })
                .collect(),
        };
        let problems = graph.validate(registry);
        if problems.is_empty() {
            Ok(graph)
        } else {
            Err(problems)
        }
    }

    /// The graph as a document.
    pub fn to_doc(&self) -> GraphDoc {
        let mut doc = GraphDoc::new(self.id.clone(), self.seed as i64);
        doc.nodes = self
            .nodes
            .iter()
            .map(|(id, n)| NodeDoc {
                id: id.clone(),
                kind: n.kind.clone(),
                version: n.version,
                params: n.params.as_object().cloned().unwrap_or_default(),
            })
            .collect();
        doc.edges = self
            .edges
            .iter()
            .map(|e| EdgeDoc {
                from: e.from.clone(),
                to: e.to.clone(),
            })
            .collect();
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
            }
            if let Err(e) = def.check(&node.params) {
                out.push(diag(
                    codes::INVALID_PARAMS,
                    g,
                    format!("node `{id}` ({}): {e}", node.kind),
                ));
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
    /// graph's output when it is a fragment or a mesh).
    pub fn is_sink(&self, index: usize) -> bool {
        let id = self.nodes.get_index(index).map(|(k, _)| k.as_str());
        !self.edges.iter().any(|e| Some(e.from.0.as_str()) == id)
    }
}
