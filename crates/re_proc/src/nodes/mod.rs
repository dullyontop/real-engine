//! The node registry and the M1 node set (ARCHITECTURE §5).
//!
//! A node kind is a [`NodeDef`]: its input ports, its one output port, a
//! parameter type with a schemars schema, and a Rust function. Parameters
//! are plain JSON in the graph document; every node kind deserializes them
//! into its own struct (unknown keys are refused, omitted keys take their
//! defaults) and checks ranges, so a bad parameter is `RE4005` before
//! anything runs.

mod field;
mod fragment;
mod mesh;
mod points;
mod scatter;
mod terrain;

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use indexmap::IndexMap;
use re_core::TaskPool;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};

use crate::value::{Field, MeshValue, Output, Points, PortType};

pub use fragment::{mesh_item, sink_fragment};

/// An input port of a node kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortDef {
    /// The port's name.
    pub name: &'static str,
    /// The types it accepts.
    pub accepts: &'static [PortType],
    /// True if the node cannot run without it.
    pub required: bool,
    /// One sentence for `describe`.
    pub doc: &'static str,
}

/// A node's parameters: a JSON object deserialized with serde (unknown keys
/// refused, missing keys defaulted) and then range-checked.
pub trait Params: DeserializeOwned + JsonSchema + Send + Sync + 'static {
    /// Checks values serde cannot (ranges, non-empty strings, …).
    fn validate(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Why a node failed: a message and its code (`RE4008` unless the node
/// says otherwise, for example `RE4014` for a prefab).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeError {
    /// The diagnostic code.
    pub code: re_core::Code,
    /// What went wrong.
    pub message: String,
}

impl From<String> for NodeError {
    fn from(message: String) -> Self {
        NodeError {
            code: crate::codes::NODE_FAILED,
            message,
        }
    }
}

/// How a node kind can be evaluated over part of a world (ADR-0042). WP23's
/// streaming decides what each variant allows; a kind that says nothing is
/// [`Streaming::Whole`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Streaming {
    /// Needs its whole input at once (for example a mask relative to the
    /// field's size): not streamable.
    #[default]
    Whole,
    /// Each output sample depends only on the same sample of its inputs.
    Pointwise,
    /// Works on region tiles with an apron of neighbouring samples.
    Region,
    /// Works cell by cell; its keys are prefixed by the cell.
    Cell,
}

/// The window of a world a node is evaluated over (ADR-0042): global sample
/// indices `origin .. origin + n` on each axis, `spacing` metres apart. When
/// an evaluation has one, it is part of every node's key.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Domain {
    /// The global sample index of the window's first sample, on x and z.
    pub origin: [i64; 2],
    /// Samples per side.
    pub n: u32,
    /// Metres between samples.
    pub spacing: f32,
}

/// One step of a kind's parameter migration, from version `from` to
/// `from + 1` (ADR-0043). Steps run in memory before a node is checked and
/// evaluated, so a kind's function sees one parameter shape whatever version
/// the node was written for; `real migrate-graph` writes the result.
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    /// The version this step upgrades from.
    pub from: u32,
    /// What changed, for the migration report.
    pub note: &'static str,
    /// True if version `from + 1` gives different output than `from` for
    /// some parameters (so migrating a graph can change the world).
    pub changes_output: bool,
    /// Rewrites the parameters into the next version's shape.
    pub params: fn(&mut serde_json::Map<String, Json>) -> Result<(), String>,
}

type CheckFn = Arc<dyn Fn(&Json) -> Result<(), String> + Send + Sync>;
type EvalFn = Arc<dyn Fn(&NodeCtx, &Json) -> Result<Output, NodeError> + Send + Sync>;

/// A node kind.
#[derive(Clone)]
pub struct NodeDef {
    /// The kind's name (`terrain.heightfield`).
    pub kind: &'static str,
    /// The kind's current version. A node evaluates at the version it was
    /// written for (its recorded version, which is part of its key).
    pub version: u32,
    /// The oldest version this engine still evaluates.
    pub oldest: u32,
    /// The parameter migrations, one per version step that changed the
    /// parameters' shape (steps not listed change nothing).
    pub migrations: &'static [Migration],
    /// How the kind can be streamed (ADR-0042).
    pub streaming: Streaming,
    /// A hash of the kind's implementation when it is not part of this
    /// binary (a Luau node's scripts); all zeros for Rust kinds, whose
    /// versions and goldens guard them.
    pub implementation: [u8; 32],
    /// What the node does, for `describe`.
    pub doc: &'static str,
    /// The named input ports.
    pub inputs: &'static [PortDef],
    /// For `mesh.merge` and `fragment.merge`: inputs on any port name, of
    /// these types, in natural port-name order (`in2` before `in10`).
    pub variadic: Option<&'static [PortType]>,
    /// The output type; the output port is named [`PortType::port_name`].
    pub output: PortType,
    /// True if the node uses its seed (only then is the seed in its key, so
    /// a new graph seed does not re-run the others).
    pub uses_seed: bool,
    /// Parameters naming project files whose contents are part of the key
    /// (a string, or an object of strings).
    pub file_params: &'static [&'static str],
    /// The JSON Schema of the parameters.
    pub params_schema: Json,
    check: CheckFn,
    eval: EvalFn,
}

impl fmt::Debug for NodeDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NodeDef")
            .field("kind", &self.kind)
            .field("version", &self.version)
            .field("output", &self.output)
            .finish_non_exhaustive()
    }
}

/// The parts of a [`NodeDef`] other than its parameter type and function.
#[derive(Clone, Copy, Debug)]
pub struct NodeSpec {
    /// See [`NodeDef::kind`].
    pub kind: &'static str,
    /// See [`NodeDef::version`].
    pub version: u32,
    /// See [`NodeDef::oldest`].
    pub oldest: u32,
    /// See [`NodeDef::migrations`].
    pub migrations: &'static [Migration],
    /// See [`NodeDef::streaming`].
    pub streaming: Streaming,
    /// See [`NodeDef::doc`].
    pub doc: &'static str,
    /// See [`NodeDef::inputs`].
    pub inputs: &'static [PortDef],
    /// See [`NodeDef::variadic`].
    pub variadic: Option<&'static [PortType]>,
    /// See [`NodeDef::output`].
    pub output: PortType,
    /// See [`NodeDef::uses_seed`].
    pub uses_seed: bool,
    /// See [`NodeDef::file_params`].
    pub file_params: &'static [&'static str],
}

fn parse<P: Params>(params: &Json) -> Result<P, String> {
    let value = if params.is_null() {
        Json::Object(Default::default())
    } else {
        params.clone()
    };
    let p: P = serde_json::from_value(value).map_err(|e| e.to_string())?;
    p.validate()?;
    Ok(p)
}

impl NodeDef {
    /// A node kind whose parameters are `P`.
    pub fn new<P: Params, E: Into<NodeError>>(
        spec: NodeSpec,
        eval: impl Fn(&NodeCtx, &P) -> Result<Output, E> + Send + Sync + 'static,
    ) -> NodeDef {
        let schema = serde_json::to_value(schemars::schema_for!(P)).unwrap_or(Json::Null);
        NodeDef {
            kind: spec.kind,
            version: spec.version,
            oldest: spec.oldest,
            migrations: spec.migrations,
            streaming: spec.streaming,
            implementation: [0; 32],
            doc: spec.doc,
            inputs: spec.inputs,
            variadic: spec.variadic,
            output: spec.output,
            uses_seed: spec.uses_seed,
            file_params: spec.file_params,
            params_schema: schema,
            check: Arc::new(|params| parse::<P>(params).map(|_| ())),
            eval: Arc::new(move |ctx, params| {
                let p = parse::<P>(params).map_err(|m| NodeError {
                    code: crate::codes::INVALID_PARAMS,
                    message: m,
                })?;
                eval(ctx, &p).map_err(Into::into)
            }),
        }
    }

    /// Sets [`NodeDef::implementation`].
    #[must_use]
    pub fn with_implementation(mut self, hash: [u8; 32]) -> Self {
        self.implementation = hash;
        self
    }

    /// Checks parameters against the kind (`Err` is the message of `RE4005`).
    /// They must already be in the current shape ([`NodeDef::migrate`]).
    pub fn check(&self, params: &Json) -> Result<(), String> {
        (self.check)(params)
    }

    /// True if this engine evaluates nodes written for `version`.
    pub fn supports(&self, version: u32) -> bool {
        version >= self.oldest.max(1) && version <= self.version
    }

    /// `params`, written for `version`, in the current shape: each listed
    /// migration step from `version` up runs in order. Borrowed when no step
    /// applies. `Err` is the failed step's `from` version and its message
    /// (`RE4020`).
    pub fn migrate<'p>(
        &self,
        version: u32,
        params: &'p Json,
    ) -> Result<std::borrow::Cow<'p, Json>, (u32, String)> {
        let steps = self.steps_from(version);
        if steps.is_empty() {
            return Ok(std::borrow::Cow::Borrowed(params));
        }
        let mut map = match params {
            Json::Object(m) => m.clone(),
            Json::Null => serde_json::Map::new(),
            _ => return Err((version, "parameters must be an object".into())),
        };
        for step in steps {
            (step.params)(&mut map).map_err(|e| (step.from, e))?;
        }
        Ok(std::borrow::Cow::Owned(Json::Object(map)))
    }

    /// The migration steps from `version` to the current one, in order.
    pub fn steps_from(&self, version: u32) -> Vec<&Migration> {
        let mut steps: Vec<&Migration> = self
            .migrations
            .iter()
            .filter(|m| m.from >= version && m.from < self.version)
            .collect();
        steps.sort_by_key(|m| m.from);
        steps
    }

    /// Runs the node.
    pub fn eval(&self, ctx: &NodeCtx, params: &Json) -> Result<Output, NodeError> {
        (self.eval)(ctx, params)
    }

    /// The output port's name.
    pub fn output_port(&self) -> &'static str {
        self.output.port_name()
    }

    /// The input named `port`: a named port, or a variadic one.
    pub fn input(&self, port: &str) -> Option<PortDef> {
        if let Some(p) = self.inputs.iter().find(|p| p.name == port) {
            return Some(*p);
        }
        self.variadic.map(|accepts| PortDef {
            name: "*",
            accepts,
            required: false,
            doc: "",
        })
    }

    /// The names of the parameters (the schema's properties).
    pub fn param_names(&self) -> Vec<String> {
        self.params_schema
            .get("properties")
            .and_then(Json::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// The kind's description, for `describe` and `real graph`.
    pub fn describe(&self) -> Json {
        let inputs: Vec<Json> = self
            .inputs
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "accepts": p.accepts,
                    "required": p.required,
                    "doc": p.doc,
                })
            })
            .collect();
        json!({
            "kind": self.kind,
            "version": self.version,
            "oldest": self.oldest,
            "streaming": self.streaming,
            "doc": self.doc,
            "inputs": inputs,
            "variadic": self.variadic,
            "output": { "name": self.output_port(), "type": self.output },
            "uses_seed": self.uses_seed,
            "params": self.params_schema,
        })
    }
}

/// The node kinds, by name.
#[derive(Clone, Debug, Default)]
pub struct NodeRegistry {
    defs: IndexMap<String, Arc<NodeDef>>,
}

impl NodeRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// The registry with the 16 M1 node kinds.
    pub fn m1() -> Self {
        let mut r = Self::new();
        terrain::register(&mut r);
        field::register(&mut r);
        scatter::register(&mut r);
        points::register(&mut r);
        mesh::register(&mut r);
        fragment::register(&mut r);
        r
    }

    /// Registers (or replaces) a node kind.
    pub fn register(&mut self, def: NodeDef) {
        self.defs.insert(def.kind.to_string(), Arc::new(def));
    }

    /// The kind named `kind`.
    pub fn get(&self, kind: &str) -> Option<&NodeDef> {
        self.defs.get(kind).map(|d| &**d)
    }

    /// Every kind, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &NodeDef> {
        self.defs.values().map(|d| &**d)
    }

    /// The kind names, in registration order.
    pub fn kinds(&self) -> impl Iterator<Item = &str> {
        self.defs.keys().map(String::as_str)
    }

    /// The number of kinds.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// True if no kind is registered.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }

    /// Every kind's description.
    pub fn describe(&self) -> Json {
        Json::Array(self.iter().map(NodeDef::describe).collect())
    }

    /// The JSON Schema (2020-12) of `*.graph.json` files with these node
    /// kinds: a [`GraphDoc`](re_types::graph::GraphDoc) whose nodes are
    /// checked per kind (`kind` selects the parameter schema). `real schema`
    /// writes it as `graph.schema.json`.
    pub fn graph_json_schema(&self) -> Json {
        let mut doc = serde_json::to_value(schemars::schema_for!(re_types::graph::GraphDoc))
            .unwrap_or(Json::Null);
        // Each kind's `$defs` move to the root (`$ref`s resolve against the
        // document root), renamed `<kind>.<name>` so kinds cannot collide.
        let mut root_defs = serde_json::Map::new();
        let variants: Vec<Json> = self
            .iter()
            .map(|d| {
                let mut params = d.params_schema.clone();
                let mut defs = Json::Null;
                if let Some(obj) = params.as_object_mut() {
                    obj.remove("$schema");
                    obj.remove("title");
                    defs = obj.remove("$defs").unwrap_or(Json::Null);
                }
                let prefix = format!("{}.", d.kind);
                rename_refs(&mut params, &prefix);
                if let Json::Object(defs) = defs {
                    for (name, mut def) in defs {
                        rename_refs(&mut def, &prefix);
                        root_defs.insert(format!("{prefix}{name}"), def);
                    }
                }
                json!({
                    "if": { "properties": { "kind": { "const": d.kind } }, "required": ["kind"] },
                    "then": { "properties": { "params": params } },
                })
            })
            .collect();
        if let Some(defs) = doc.get_mut("$defs").and_then(Json::as_object_mut) {
            defs.extend(root_defs);
        } else if let Some(obj) = doc.as_object_mut() {
            obj.insert("$defs".into(), Json::Object(root_defs));
        }
        let kinds: Vec<&str> = self.kinds().collect();
        if let Some(node) = doc
            .pointer_mut("/$defs/NodeDoc")
            .and_then(Json::as_object_mut)
        {
            node.insert("allOf".into(), Json::Array(variants));
            if let Some(kind) = node
                .get_mut("properties")
                .and_then(|p| p.get_mut("kind"))
                .and_then(Json::as_object_mut)
            {
                kind.insert("enum".into(), json!(kinds));
            }
        }
        doc
    }
}

/// Rewrites every `"$ref": "#/$defs/<name>"` in `v` to `#/$defs/<prefix><name>`.
fn rename_refs(v: &mut Json, prefix: &str) {
    match v {
        Json::Object(m) => {
            for (k, x) in m.iter_mut() {
                if k == "$ref"
                    && let Json::String(r) = x
                    && let Some(name) = r.strip_prefix("#/$defs/")
                {
                    *r = format!("#/$defs/{prefix}{name}");
                } else {
                    rename_refs(x, prefix);
                }
            }
        }
        Json::Array(a) => a.iter_mut().for_each(|x| rename_refs(x, prefix)),
        _ => {}
    }
}

/// What a node sees while it runs.
pub struct NodeCtx<'a> {
    /// The node's id.
    pub node_id: &'a str,
    /// The node's key (the hash of everything that decides its output).
    pub key: [u8; 32],
    /// The version the node was written for. A kind keeps the output of
    /// every version it supports, branching on this when versions differ.
    pub version: u32,
    /// The window being evaluated, for streamed evaluation (ADR-0042).
    pub domain: Option<Domain>,
    /// The node's seed: `Pcg32::from_seed(graph seed).fork(hash64(node id))`'s
    /// first `u64`.
    pub seed: u64,
    /// The inputs, by port, in edge order.
    pub inputs: Vec<(String, Output)>,
    /// The contents of the files named by the kind's `file_params`, by path
    /// (`None` when the file could not be read, with the error).
    pub files: HashMap<String, Result<Arc<Vec<u8>>, String>>,
    /// The worker pool, for nodes that split their own work.
    pub pool: &'a TaskPool,
}

impl NodeCtx<'_> {
    fn input(&self, port: &str) -> Option<&Output> {
        self.inputs.iter().find(|(p, _)| p == port).map(|(_, o)| o)
    }

    /// The field on `port`, if connected.
    pub fn field(&self, port: &str) -> Option<&Arc<Field>> {
        match self.input(port) {
            Some(Output::Field(f)) => Some(f),
            _ => None,
        }
    }

    /// The mesh on `port`, if connected.
    pub fn mesh(&self, port: &str) -> Option<&Arc<MeshValue>> {
        match self.input(port) {
            Some(Output::Mesh(m)) => Some(m),
            _ => None,
        }
    }

    /// The points on `port`, if connected.
    pub fn points(&self, port: &str) -> Option<&Arc<Points>> {
        match self.input(port) {
            Some(Output::Points(p)) => Some(p),
            _ => None,
        }
    }

    /// The required field on `port` (validation guarantees it).
    pub fn need_field(&self, port: &str) -> Result<&Arc<Field>, String> {
        self.field(port)
            .ok_or_else(|| format!("input `{port}` has no field"))
    }

    /// The required mesh on `port`.
    pub fn need_mesh(&self, port: &str) -> Result<&Arc<MeshValue>, String> {
        self.mesh(port)
            .ok_or_else(|| format!("input `{port}` has no mesh"))
    }

    /// The required points on `port`.
    pub fn need_points(&self, port: &str) -> Result<&Arc<Points>, String> {
        self.points(port)
            .ok_or_else(|| format!("input `{port}` has no points"))
    }

    /// A deterministic number in `[0, 1)` for `id` and `salt`, from the node's seed.
    pub fn random01(&self, id: u64, salt: u64) -> f32 {
        let h = re_noise::mix64(self.seed ^ re_noise::mix64(id ^ salt.rotate_left(29)));
        (h >> 40) as f32 * (1.0 / 16_777_216.0)
    }

    /// Fills `out` (rows of `row_len` values) in parallel on the pool:
    /// `row(z, values)` fills row `z`. Each row is computed on its own, so
    /// the result does not depend on the thread count.
    pub fn par_rows(
        &self,
        out: &mut [f32],
        row_len: usize,
        row: impl Fn(usize, &mut [f32]) + Sync,
    ) {
        let row = &row;
        let rows_per_job = (4096 / row_len.max(1)).max(1);
        self.pool.scope(|s| {
            for (j, chunk) in out.chunks_mut(row_len * rows_per_job).enumerate() {
                s.spawn(move |_| {
                    for (i, values) in chunk.chunks_mut(row_len).enumerate() {
                        row(j * rows_per_job + i, values);
                    }
                });
            }
        });
    }
}

/// Validates an optional `[min, max]` pair of limits.
pub(crate) fn check_range(what: &str, min: Option<f32>, max: Option<f32>) -> Result<(), String> {
    for v in [min, max].into_iter().flatten() {
        if !v.is_finite() {
            return Err(format!("{what} limits must be finite, got {v}"));
        }
    }
    if let (Some(a), Some(b)) = (min, max)
        && a > b
    {
        return Err(format!("{what}_min ({a}) is above {what}_max ({b})"));
    }
    Ok(())
}

/// Checks `lo <= v <= hi` (and finite).
pub(crate) fn within(name: &str, v: f32, lo: f32, hi: f32) -> Result<(), String> {
    if v.is_finite() && v >= lo && v <= hi {
        Ok(())
    } else {
        Err(format!("`{name}` must be between {lo} and {hi}, got {v}"))
    }
}

/// Checks `v > 0` (and finite).
pub(crate) fn positive(name: &str, v: f32) -> Result<(), String> {
    if v.is_finite() && v > 0.0 {
        Ok(())
    } else {
        Err(format!("`{name}` must be finite and positive, got {v}"))
    }
}

/// Natural order of port names: digit runs compare as numbers (`in2` < `in10`).
pub(crate) fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn parts(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                out.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        out
    }
    let (pa, pb) = (parts(a), parts(b));
    for (x, y) in pa.iter().zip(&pb) {
        let ord = match (x, y) {
            ((true, x), (true, y)) => {
                let (tx, ty) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                tx.len()
                    .cmp(&ty.len())
                    .then(tx.cmp(ty))
                    .then(x.len().cmp(&y.len()))
            }
            ((_, x), (_, y)) => x.cmp(y),
        };
        if ord.is_ne() {
            return ord;
        }
    }
    pa.len().cmp(&pb.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut v = vec!["in10", "in2", "b", "a", "in1"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a", "b", "in1", "in2", "in10"]);
    }

    #[test]
    fn m1_has_sixteen_kinds() {
        let r = NodeRegistry::m1();
        assert_eq!(r.len(), 16);
        let schema = r.graph_json_schema();
        assert!(schema.pointer("/$defs/NodeDoc/allOf/15").is_some());
    }
}
