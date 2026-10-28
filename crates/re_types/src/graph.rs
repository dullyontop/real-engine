//! The `*.graph.json` file: a ProcGraph as data.
//!
//! `re_proc` turns a [`GraphDoc`] into its `ProcGraph` IR (ARCHITECTURE §5). On disk:
//!
//! ```json
//! { "format": "real-engine/graph", "version": 1, "name": "island", "seed": 1234,
//!   "nodes": [{ "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 256 } }],
//!   "edges": [{ "from": ["h", "field"], "to": ["t", "a"] }],
//!   "layout": { "h": [0.0, 120.0] } }
//! ```
//!
//! Node ids are unique, `nodes` keep file order and unknown keys are errors.
//! `layout` is optional and omitted when empty (ADR-0044).
//!
//! **Version 2** (ADR-0061) adds four optional fields between `seed` and
//! `nodes`: `kind` (the file is a node kind), `params` (typed graph params),
//! and `inputs` and `outputs` (declared ports). A node param whose whole value
//! is `{"$param": "<name>"}` is bound to a graph param, and edges may start at
//! `["$in", "<input>"]` and end at `["$out", "<output>"]`. A document is
//! written at the lowest version that holds it: version 1 until it uses one
//! of these ([`GraphDoc::raise_version`]). Version 1 documents mean what they
//! always did. Here only the structure is checked; `re_proc` checks the
//! declarations, the bindings and the wiring.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// A procedural graph document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GraphDoc {
    /// Always [`GraphDoc::FORMAT`].
    pub format: String,
    /// The file format version, 1 or [`GraphDoc::VERSION`].
    pub version: u32,
    /// The graph's name.
    pub name: String,
    /// The default seed. A procedural source's `Procedural.seed` wins over it.
    pub seed: i64,
    /// Makes the file a node kind (version 2; ADR-0061 §6). Omitted when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<KindDoc>,
    /// The graph's params, in the order an inspector shows them (version 2;
    /// ADR-0061 §2). Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamDoc>,
    /// A node kind's inputs, fed to edges from `["$in", name]` (version 2;
    /// ADR-0061 §4). Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<PortDoc>,
    /// The graph's outputs, each fed by one edge to `["$out", name]` (version
    /// 2; ADR-0061 §4). Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<PortDoc>,
    /// The nodes, in file order.
    pub nodes: Vec<NodeDoc>,
    /// The edges between node ports.
    pub edges: Vec<EdgeDoc>,
    /// Where the editor's node canvas draws each node, `[x, y]` by node id
    /// (ADR-0044). Only nodes that were moved have an entry. Layout is not
    /// part of a node: generation, node cache keys, the document hash and the
    /// version stamp all ignore it. Omitted when empty.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub layout: BTreeMap<String, [f32; 2]>,
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
    pub params: Map<String, Value>,
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

/// What makes a graph file a node kind (ADR-0061 §6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct KindDoc {
    /// The kind's name: `project.` then lowercase letters, digits, `_` and `.`.
    pub name: String,
    /// The kind's current version, from 1.
    pub version: u32,
    /// The oldest version an instance may record. 1 when omitted.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub oldest: u32,
    /// What the kind makes, for `describe` and the editor.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    /// One step per version, from `oldest` up to `version`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub migrations: Vec<KindMigrationDoc>,
}

/// One version step of a kind graph's params: `rename`, then `remove`, then
/// `set` (ADR-0061 §6).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct KindMigrationDoc {
    /// The version this step upgrades from.
    pub from: u32,
    /// What changed, for the migration report.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// True if the new version gives different output for some params.
    #[serde(default, skip_serializing_if = "is_false")]
    pub changes_output: bool,
    /// Params renamed, old name to new name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub rename: BTreeMap<String, String>,
    /// Params removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    /// Params set to a value (added, or replaced).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub set: Map<String, Value>,
}

/// A graph param's declaration (ADR-0061 §2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ParamDoc {
    /// Letters, digits and `_`, starting with a letter; at most 64 bytes.
    pub name: String,
    /// The value's type.
    #[serde(rename = "type")]
    pub ty: ParamType,
    /// The value when a source or an instance gives none. Required outside
    /// kind graphs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// The smallest value (`number` and `integer`), inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// The largest value (`number` and `integer`), inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// The allowed values of an `enum`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
    /// The endings a `file` accepts, such as `.prefab.json`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extensions: Vec<String>,
    /// What the param does, for the inspector's tooltip.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
}

/// The type of a graph param (ADR-0061 §2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum ParamType {
    /// A finite number.
    Number,
    /// A whole number, at most 2^53 in size.
    Integer,
    /// `true` or `false`.
    Bool,
    /// A string.
    #[serde(rename = "string")]
    Str,
    /// One of `values`.
    Enum,
    /// `[x, y]`.
    Vec2,
    /// `[x, y, z]`.
    Vec3,
    /// `[r, g, b]` or `[r, g, b, a]`, linear.
    Color,
    /// A project path, or `""` for none.
    File,
    /// `[[x, y], …]`: 2 to 256 points with `x` strictly increasing.
    Curve,
}

/// A declared input or output of a graph (ADR-0061 §4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PortDoc {
    /// Letters, digits and `_`; unique on its side.
    pub name: String,
    /// A port type name, such as `field` or `fragment`; `re_proc` checks it.
    #[serde(rename = "type")]
    pub ty: String,
    /// An input that every instance must connect. Inputs only.
    #[serde(default, skip_serializing_if = "is_false")]
    pub required: bool,
    /// What the port carries, for `describe` and the editor.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
}

fn one() -> u32 {
    1
}

fn is_one(v: &u32) -> bool {
    *v == 1
}

fn is_false(v: &bool) -> bool {
    !*v
}

impl GraphDoc {
    /// The value of the `format` key.
    pub const FORMAT: &'static str = "real-engine/graph";
    /// The newest file format version this engine reads and writes.
    pub const VERSION: u32 = 2;
    /// The node name of a kind's inputs in edges: `["$in", "<input>"]` (version 2).
    pub const INPUTS: &'static str = "$in";
    /// The node name of a graph's outputs in edges: `["$out", "<output>"]` (version 2).
    pub const OUTPUTS: &'static str = "$out";
    /// The key of a node param's reference to a graph param: `{"$param": "<name>"}`.
    pub const PARAM_REF: &'static str = "$param";

    /// An empty graph with the current format, at version 1, the lowest
    /// version that holds it; [`GraphDoc::raise_version`] moves it to version 2
    /// when it uses a version 2 field.
    pub fn new(name: impl Into<String>, seed: i64) -> GraphDoc {
        GraphDoc {
            format: Self::FORMAT.to_string(),
            version: 1,
            name: name.into(),
            seed,
            kind: None,
            params: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            layout: BTreeMap::new(),
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

    /// The name of the graph param `value` refers to, when `value` is a
    /// reference: an object whose only key is `$param`, with a string.
    pub fn param_ref(value: &Value) -> Option<&str> {
        match value {
            Value::Object(m) if m.len() == 1 => m.get(Self::PARAM_REF)?.as_str(),
            _ => None,
        }
    }

    /// True for an id a version 2 document allows: letters, digits, `_` and
    /// `-`, the rule the `graph.*` commands apply. `/` is kept for the ids of
    /// expanded graph kinds and `$` for `$in`, `$out` and `$graph`.
    pub fn valid_node_id(id: &str) -> bool {
        !id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    /// What in the document needs version 2, if anything: a `kind` block,
    /// params, inputs, outputs, an edge from `$in` or to `$out`, or a node
    /// param bound to a graph param.
    pub fn version_2_use(&self) -> Option<String> {
        if self.kind.is_some() {
            return Some("`kind`".into());
        }
        for (field, empty) in [
            ("params", self.params.is_empty()),
            ("inputs", self.inputs.is_empty()),
            ("outputs", self.outputs.is_empty()),
        ] {
            if !empty {
                return Some(format!("`{field}`"));
            }
        }
        for (i, edge) in self.edges.iter().enumerate() {
            if edge.from.0 == Self::INPUTS || edge.to.0 == Self::OUTPUTS {
                return Some(format!(
                    "edge {i}, from `{}` or to `{}`,",
                    Self::INPUTS,
                    Self::OUTPUTS
                ));
            }
        }
        for node in &self.nodes {
            if let Some(param) = node
                .params
                .iter()
                .find_map(|(k, v)| Self::param_ref(v).map(|_| k))
            {
                return Some(format!(
                    "node `{}`'s param `{param}`, bound to a graph param,",
                    node.id
                ));
            }
        }
        None
    }

    /// Moves a version 1 document that uses a version 2 field to version 2.
    /// Nothing lowers a version.
    pub fn raise_version(&mut self) {
        if self.version < 2 && self.version_2_use().is_some() {
            self.version = 2;
        }
    }

    /// Checks the structural rules: the format string, a supported version,
    /// node ids (any id but one starting with `$` in version 1, letters,
    /// digits, `_` and `-` in version 2) that are unique, no version 2 field in
    /// a version 1 document, and edges that name existing nodes, `$in` only
    /// as a start and `$out` only as an end.
    pub fn validate(&self) -> Result<(), GraphDocError> {
        if self.format != Self::FORMAT {
            return Err(GraphDocError::WrongFormat(self.format.clone()));
        }
        if self.version == 0 || self.version > Self::VERSION {
            return Err(GraphDocError::UnsupportedVersion(self.version));
        }
        let v2 = self.version >= 2;
        for (i, node) in self.nodes.iter().enumerate() {
            let allowed = if v2 {
                Self::valid_node_id(&node.id)
            } else {
                !node.id.starts_with('$')
            };
            if !allowed {
                return Err(GraphDocError::InvalidNodeId(node.id.clone()));
            }
            if self.nodes[..i].iter().any(|other| other.id == node.id) {
                return Err(GraphDocError::DuplicateNode(node.id.clone()));
            }
        }
        if !v2 && let Some(what) = self.version_2_use() {
            return Err(GraphDocError::NeedsVersion2(what));
        }
        for (i, edge) in self.edges.iter().enumerate() {
            for (end, port_node, other) in [
                (&edge.from.0, Self::INPUTS, Self::OUTPUTS),
                (&edge.to.0, Self::OUTPUTS, Self::INPUTS),
            ] {
                if v2 && end == port_node {
                    continue;
                }
                if v2 && end == other {
                    return Err(GraphDocError::MisplacedPortNode {
                        edge: i,
                        node: end.clone(),
                    });
                }
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
    /// A node id the document's version does not allow.
    InvalidNodeId(String),
    /// Two nodes share an id.
    DuplicateNode(String),
    /// A version 1 document uses a version 2 field (what it is).
    NeedsVersion2(String),
    /// An edge names a node that does not exist.
    UnknownNode {
        /// The edge's index in `edges`.
        edge: usize,
        /// The missing node id.
        node: String,
    },
    /// An edge ends at `$in` or starts at `$out`.
    MisplacedPortNode {
        /// The edge's index in `edges`.
        edge: usize,
        /// `$in` or `$out`.
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
            GraphDocError::InvalidNodeId(id) => write!(
                f,
                "node id `{id}` is not allowed: ids use letters, digits, `_` and `-` (any id but one starting with `$` in a version 1 file)"
            ),
            GraphDocError::DuplicateNode(id) => write!(f, "node id `{id}` is used twice"),
            GraphDocError::NeedsVersion2(what) => write!(
                f,
                "{what} needs graph file version 2, and the file says version 1: write `\"version\": 2`"
            ),
            GraphDocError::UnknownNode { edge, node } => {
                write!(f, "edge {edge} names node `{node}`, which does not exist")
            }
            GraphDocError::MisplacedPortNode { edge, node } => write!(
                f,
                "edge {edge}: `{node}` is wrong here: edges start at `{}` and end at `{}`",
                GraphDoc::INPUTS,
                GraphDoc::OUTPUTS
            ),
        }
    }
}

impl std::error::Error for GraphDocError {}
