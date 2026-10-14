//! The `*.graph.json` file: a ProcGraph as data.
//!
//! `re_proc` turns a [`GraphDoc`] into its `ProcGraph` IR (ARCHITECTURE §5). On disk:
//!
//! ```json
//! { "format": "real-engine/graph", "version": 1, "name": "island", "seed": 1234,
//!   "nodes": [{ "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 256 } }],
//!   "edges": [{ "from": ["h", "field"], "to": ["t", "a"] }] }
//! ```
//!
//! Node ids are unique, `nodes` keep file order and unknown keys are errors.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A procedural graph document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GraphDoc {
    /// Always [`GraphDoc::FORMAT`].
    pub format: String,
    /// The file format version, [`GraphDoc::VERSION`].
    pub version: u32,
    /// The graph's name.
    pub name: String,
    /// The default seed. A procedural source's `Procedural.seed` wins over it.
    pub seed: i64,
    /// The nodes, in file order.
    pub nodes: Vec<NodeDoc>,
    /// The edges between node ports.
    pub edges: Vec<EdgeDoc>,
}

/// One node of a [`GraphDoc`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeDoc {
    /// Unique within the graph.
    pub id: String,
    /// The node type, for example `terrain.heightfield`.
    pub kind: String,
    /// The node type's version (for migrating parameters).
    pub version: u32,
    /// The node's parameters, in file order; checked by the node type.
    pub params: serde_json::Map<String, serde_json::Value>,
}

/// An edge from an output port to an input port.
///
/// Each end is `(node id, port)`, written as a two-element array.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EdgeDoc {
    /// The source node and output port.
    pub from: (String, String),
    /// The destination node and input port.
    pub to: (String, String),
}

impl GraphDoc {
    /// The value of the `format` key.
    pub const FORMAT: &'static str = "real-engine/graph";
    /// The current file format version.
    pub const VERSION: u32 = 1;

    /// An empty graph with the current format and version.
    pub fn new(name: impl Into<String>, seed: i64) -> GraphDoc {
        GraphDoc {
            format: Self::FORMAT.to_string(),
            version: Self::VERSION,
            name: name.into(),
            seed,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    /// Parses a document from JSON text and [`validate`](Self::validate)s it.
    pub fn from_json(text: &str) -> Result<GraphDoc, GraphDocError> {
        let doc: GraphDoc =
            serde_json::from_str(text).map_err(|e| GraphDocError::Parse(e.to_string()))?;
        doc.validate()?;
        Ok(doc)
    }

    /// Writes the document as pretty JSON ending with a newline.
    pub fn to_json(&self) -> String {
        let mut text = serde_json::to_string_pretty(self).expect("a GraphDoc always serializes");
        text.push('\n');
        text
    }

    /// Checks the structural rules: the format string, a supported version, unique
    /// node ids, and edges that name existing nodes.
    pub fn validate(&self) -> Result<(), GraphDocError> {
        if self.format != Self::FORMAT {
            return Err(GraphDocError::WrongFormat(self.format.clone()));
        }
        if self.version == 0 || self.version > Self::VERSION {
            return Err(GraphDocError::UnsupportedVersion(self.version));
        }
        for (i, node) in self.nodes.iter().enumerate() {
            if self.nodes[..i].iter().any(|other| other.id == node.id) {
                return Err(GraphDocError::DuplicateNode(node.id.clone()));
            }
        }
        for (i, edge) in self.edges.iter().enumerate() {
            for end in [&edge.from.0, &edge.to.0] {
                if !self.nodes.iter().any(|node| &node.id == end) {
                    return Err(GraphDocError::UnknownNode {
                        edge: i,
                        node: end.clone(),
                    });
                }
            }
        }
        Ok(())
    }

    /// The node with this id.
    pub fn node(&self, id: &str) -> Option<&NodeDoc> {
        self.nodes.iter().find(|node| node.id == id)
    }
}

/// Why a [`GraphDoc`] is not valid. `re_proc` turns these into `RE4xxx` diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GraphDocError {
    /// The JSON text does not match the format (includes unknown keys).
    Parse(String),
    /// `format` is not `real-engine/graph`.
    WrongFormat(String),
    /// `version` is newer than this engine, or zero.
    UnsupportedVersion(u32),
    /// Two nodes share an id.
    DuplicateNode(String),
    /// An edge names a node that does not exist.
    UnknownNode {
        /// The edge's index in `edges`.
        edge: usize,
        /// The missing node id.
        node: String,
    },
}

impl fmt::Display for GraphDocError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphDocError::Parse(e) => write!(f, "invalid graph file: {e}"),
            GraphDocError::WrongFormat(format) => {
                write!(f, "`format` is `{format}`, expected `{}`", GraphDoc::FORMAT)
            }
            GraphDocError::UnsupportedVersion(v) => write!(
                f,
                "graph file version {v} is not supported (this engine reads 1 to {})",
                GraphDoc::VERSION
            ),
            GraphDocError::DuplicateNode(id) => write!(f, "node id `{id}` is used twice"),
            GraphDocError::UnknownNode { edge, node } => {
                write!(f, "edge {edge} names node `{node}`, which does not exist")
            }
        }
    }
}

impl std::error::Error for GraphDocError {}
