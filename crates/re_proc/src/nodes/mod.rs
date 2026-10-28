//! The node registry and the M1 node set (ARCHITECTURE §5).
//!
//! A node kind is a [`NodeDef`]: its input ports, its one output port, a
//! parameter type with a schemars schema, and a Rust function. Parameters
//! are plain JSON in the graph document; every node kind deserializes them
//! into its own struct (unknown keys are refused, omitted keys take their
//! defaults) and checks ranges, so a bad parameter is `RE4005` before
//! anything runs.

mod erode;
mod field;
mod fragment;
mod mesh;
mod points;
mod road;
mod scatter;
mod shape;
mod terrain;

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

use indexmap::IndexMap;
use re_core::{Diagnostic, TaskPool};
use schemars::JsonSchema;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value as Json, json};

use crate::eval::NodeStop;
use crate::graph::ProcGraph;
use crate::value::{Field, MeshValue, Output, Points, PortType, RoadPath, Shapes};
use crate::{SubEvaluation, SubInput};

pub use fragment::{mesh_item, sink_fragment, sink_fragment_in};

/// An input port of a node kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
/// refused, missing keys defaulted) and then range-checked. Serialized back,
/// they are the node's parameters in canonical form, which its cache key
/// hashes ([`NodeDef::canonical_params`]).
pub trait Params: DeserializeOwned + Serialize + JsonSchema + Send + Sync + 'static {
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

impl From<re_geom::GeomError> for NodeError {
    fn from(e: re_geom::GeomError) -> Self {
        NodeError {
            code: e.code(),
            message: e.to_string(),
        }
    }
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
    /// Works cell by cell; its keys are prefixed by the cell. A mesh it
    /// outputs in a window is local to the window's corner (`min x, 0,
    /// min z`), where the entity drawing it is placed. It can read its
    /// fields beyond the window ([`NodeCtx::upstream`]).
    Cell,
    /// Its output does not depend on the window (shapes, road paths): a
    /// streamed evaluation runs it without one, keyed without one, so every
    /// cell and chunk shares one result. Its inputs other than fields come
    /// from global nodes; it does not get its fields over the window, but
    /// samples them where it needs them ([`NodeCtx::sample`]). It outputs
    /// shapes or a road path (ADR-0043 §5).
    Global,
}

impl Streaming {
    /// The class's name, as `describe` and `real graph describe` show it.
    pub fn name(self) -> &'static str {
        match self {
            Streaming::Whole => "whole",
            Streaming::Pointwise => "pointwise",
            Streaming::Region => "region",
            Streaming::Cell => "cell",
            Streaming::Global => "global",
        }
    }
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

impl Domain {
    /// The world position (metres) of global sample index `i` on one axis:
    /// `i as f32 * spacing`, so every window agrees on shared samples.
    pub fn position(&self, i: i64) -> f32 {
        i as f32 * self.spacing
    }

    /// The world X and Z of the window's first sample.
    pub fn min(&self) -> [f32; 2] {
        [self.position(self.origin[0]), self.position(self.origin[1])]
    }

    /// The world X and Z of the window's last sample.
    pub fn max(&self) -> [f32; 2] {
        let last = i64::from(self.n.saturating_sub(1));
        [
            self.position(self.origin[0] + last),
            self.position(self.origin[1] + last),
        ]
    }

    /// True if `(x, z)` lies in the window's half-open square
    /// `[min, max)`: a point on a border between two windows belongs to one.
    pub fn owns(&self, x: f32, z: f32) -> bool {
        let (lo, hi) = (self.min(), self.max());
        x >= lo[0] && x < hi[0] && z >= lo[1] && z < hi[1]
    }
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

/// Checks a node's parameters (`Err` is the message of `RE4005`).
pub type CheckFn = Arc<dyn Fn(&Json) -> Result<(), String> + Send + Sync>;
/// Runs a node.
pub type EvalFn = Arc<dyn Fn(&NodeCtx, &Json) -> Result<Output, NodeError> + Send + Sync>;
/// Rewrites parameters written for a version into the current shape, for a
/// kind whose migration is not Rust (a Luau node's `migrate`). `Err` is the
/// failed step's `from` version and its message.
pub type MigrateFn =
    Arc<dyn Fn(u32, &mut serde_json::Map<String, Json>) -> Result<(), (u32, String)> + Send + Sync>;
/// Checks a node's inputs, in natural port order, whenever it runs or its
/// output comes from a cache: warnings for its report, such as `RE4030` for
/// `field.combine` inputs that cover different areas.
pub type InputCheckFn = Arc<dyn Fn(&[InputValue]) -> Vec<Diagnostic> + Send + Sync>;

/// One input of a node, for [`NodeDef::with_input_check`].
#[derive(Clone, Copy, Debug)]
pub struct InputValue<'a> {
    /// The input port.
    pub port: &'a str,
    /// The value on it.
    pub value: &'a Output,
    /// The kind of the node it comes from.
    pub from_kind: &'a str,
}
/// Writes a kind's parameters in canonical form ([`NodeDef::canonical_params`]).
type CanonFn = Arc<dyn Fn(&Json) -> Json + Send + Sync>;

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
    custom_migrate: Option<MigrateFn>,
    /// `None`: canonical parameters come from `params_schema`.
    canon: Option<CanonFn>,
    input_check: Option<InputCheckFn>,
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

/// An input port of a [`CustomKind`].
#[derive(Clone, Debug)]
pub struct CustomPort {
    /// The port's name.
    pub name: String,
    /// The types it accepts.
    pub accepts: Vec<PortType>,
    /// True if the node cannot run without it.
    pub required: bool,
    /// One sentence for `describe`.
    pub doc: String,
}

/// A node kind defined outside this binary, for [`NodeDef::custom`].
#[derive(Clone)]
pub struct CustomKind {
    /// See [`NodeDef::kind`].
    pub kind: String,
    /// See [`NodeDef::version`].
    pub version: u32,
    /// See [`NodeDef::oldest`].
    pub oldest: u32,
    /// See [`NodeDef::streaming`].
    pub streaming: Streaming,
    /// See [`NodeDef::implementation`]: the hash of the code that runs it.
    pub implementation: [u8; 32],
    /// See [`NodeDef::doc`].
    pub doc: String,
    /// See [`NodeDef::inputs`].
    pub inputs: Vec<CustomPort>,
    /// See [`NodeDef::output`].
    pub output: PortType,
    /// See [`NodeDef::uses_seed`].
    pub uses_seed: bool,
    /// See [`NodeDef::params_schema`].
    pub params_schema: Json,
    /// Checks parameters in the current shape.
    pub check: CheckFn,
    /// Runs the node; it sees parameters in the current shape.
    pub eval: EvalFn,
    /// Migrates parameters from an older version, when the kind has one.
    pub migrate: Option<MigrateFn>,
}

impl fmt::Debug for CustomKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CustomKind")
            .field("kind", &self.kind)
            .field("version", &self.version)
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

/// `params` parsed into `P` and written back: every parameter there, at its
/// default when it was left out, and each number as its type writes it (`16`
/// and `16.0` for an `f32` are both `16.0`). Parameters `P` does not parse
/// are returned as they are.
fn canonical<P: Params>(params: &Json) -> Json {
    let empty = Json::Object(Default::default());
    let value = if params.is_null() { &empty } else { params };
    P::deserialize(value)
        .ok()
        .and_then(|p| serde_json::to_value(&p).ok())
        .unwrap_or_else(|| params.clone())
}

/// The canonical form of the parameters of a kind with no Rust parameter
/// type ([`NodeDef::custom`]), from its schema: each top-level property that
/// was left out at its `default`, and numbers written by the property's type
/// (`number` as a float, `integer` as an integer, an array of numbers with
/// each as a float). Other values are kept as they are.
fn canonical_from_schema(schema: &Json, params: &Json) -> Json {
    let mut map = match params {
        Json::Object(m) => m.clone(),
        Json::Null => serde_json::Map::new(),
        _ => return params.clone(),
    };
    if let Some(properties) = schema.get("properties").and_then(Json::as_object) {
        for (name, property) in properties {
            if !map.contains_key(name)
                && let Some(default) = property.get("default")
            {
                map.insert(name.clone(), default.clone());
            }
            if let Some(v) = map.get_mut(name) {
                *v = typed_number(property, v);
            }
        }
    }
    Json::Object(map)
}

/// `v` written as the number type `schema` gives it, when it is a number (or
/// an array of them). Negative zero is kept: it is not `0` everywhere.
fn typed_number(schema: &Json, v: &Json) -> Json {
    match (schema.get("type").and_then(Json::as_str), v) {
        (Some("number"), Json::Number(n)) => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| v.clone(), Json::Number),
        (Some("integer"), Json::Number(n)) => match n.as_f64() {
            Some(f)
                if f.fract() == 0.0
                    && f.abs() <= 9.007_199_254_740_992e15
                    && !(f == 0.0 && f.is_sign_negative()) =>
            {
                Json::from(f as i64)
            }
            _ => v.clone(),
        },
        (Some("array"), Json::Array(items)) => match schema.get("items") {
            Some(item) => Json::Array(items.iter().map(|x| typed_number(item, x)).collect()),
            None => v.clone(),
        },
        _ => v.clone(),
    }
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
            custom_migrate: None,
            canon: Some(Arc::new(canonical::<P>)),
            input_check: None,
        }
    }

    /// A node kind defined outside this binary (a Luau node, ADR-0043 §4):
    /// its names, ports and documentation are owned strings, its parameters
    /// are checked by `check` against `params_schema`, and its migration
    /// (when it has versions to migrate from) is `migrate`.
    ///
    /// The strings and ports are kept for the life of the process
    /// (`NodeDef`'s fields are `'static`), interned by content, so declaring
    /// the same kinds again (a hot reload) keeps no more memory.
    ///
    /// Its cache keys take the parameters in the canonical form its schema
    /// gives ([`NodeDef::canonical_params`]), so `eval` must treat a
    /// parameter that is left out as its schema's `default`, and a number by
    /// its value (an `integer` written `4.0` is `4`), as Luau nodes do.
    pub fn custom(def: CustomKind) -> NodeDef {
        use std::collections::{HashMap, HashSet};
        use std::sync::{LazyLock, Mutex, PoisonError};
        static STRINGS: LazyLock<Mutex<HashSet<&'static str>>> = LazyLock::new(Mutex::default);
        static PORTS: LazyLock<Mutex<HashMap<Vec<PortDef>, &'static [PortDef]>>> =
            LazyLock::new(Mutex::default);
        static TYPES: LazyLock<Mutex<HashMap<Vec<PortType>, &'static [PortType]>>> =
            LazyLock::new(Mutex::default);
        fn keep(s: String) -> &'static str {
            let mut set = STRINGS.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(k) = set.get(s.as_str()) {
                return k;
            }
            let k: &'static str = Box::leak(s.into_boxed_str());
            set.insert(k);
            k
        }
        fn types(t: Vec<PortType>) -> &'static [PortType] {
            let mut map = TYPES.lock().unwrap_or_else(PoisonError::into_inner);
            map.entry(t.clone())
                .or_insert_with(|| Box::leak(t.into_boxed_slice()))
        }
        let inputs: Vec<PortDef> = def
            .inputs
            .into_iter()
            .map(|p| PortDef {
                name: keep(p.name),
                accepts: types(p.accepts),
                required: p.required,
                doc: keep(p.doc),
            })
            .collect();
        let inputs: &'static [PortDef] = {
            let mut map = PORTS.lock().unwrap_or_else(PoisonError::into_inner);
            map.entry(inputs.clone())
                .or_insert_with(|| Box::leak(inputs.into_boxed_slice()))
        };
        NodeDef {
            kind: keep(def.kind),
            version: def.version,
            oldest: def.oldest,
            migrations: &[],
            streaming: def.streaming,
            implementation: def.implementation,
            doc: keep(def.doc),
            inputs,
            variadic: None,
            output: def.output,
            uses_seed: def.uses_seed,
            file_params: &[],
            params_schema: def.params_schema,
            check: def.check,
            eval: def.eval,
            custom_migrate: def.migrate,
            canon: None,
            input_check: None,
        }
    }

    /// Sets [`NodeDef::implementation`].
    #[must_use]
    pub fn with_implementation(mut self, hash: [u8; 32]) -> Self {
        self.implementation = hash;
        self
    }

    /// Checks the node's inputs with `check` whenever it runs or its output
    /// comes from a cache ([`InputCheckFn`]).
    #[must_use]
    pub fn with_input_check(
        mut self,
        check: impl Fn(&[InputValue]) -> Vec<Diagnostic> + Send + Sync + 'static,
    ) -> Self {
        self.input_check = Some(Arc::new(check));
        self
    }

    /// True if the kind checks its inputs ([`NodeDef::with_input_check`]).
    pub fn checks_inputs(&self) -> bool {
        self.input_check.is_some()
    }

    /// The warnings [`NodeDef::with_input_check`]'s check gives for `inputs`.
    pub fn check_inputs(&self, inputs: &[InputValue]) -> Vec<Diagnostic> {
        self.input_check
            .as_ref()
            .map_or_else(Vec::new, |check| check(inputs))
    }

    /// `params` (in the current shape) in canonical form: for a Rust kind,
    /// parsed into its parameter type and written back, so every parameter
    /// is there (at its default when it was left out) and each number is
    /// written as its type writes it; for a [custom](NodeDef::custom) kind,
    /// filled in and written from its schema. Parameter objects with the
    /// same canonical form give the same output, so a node's cache key
    /// hashes this form: `16`, `16.0` and a left-out default of 16 are one
    /// key.
    pub fn canonical_params(&self, params: &Json) -> Json {
        match &self.canon {
            Some(canon) => canon(params),
            None => canonical_from_schema(&self.params_schema, params),
        }
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
        let custom = self
            .custom_migrate
            .as_ref()
            .filter(|_| version < self.version);
        if steps.is_empty() && custom.is_none() {
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
        if let Some(migrate) = custom {
            migrate(version, &mut map)?;
        }
        Ok(std::borrow::Cow::Owned(Json::Object(map)))
    }

    /// True if the kind's migration is not Rust ([`NodeDef::custom`]): its
    /// steps are not listed, and any of them may change the output.
    pub fn migrates_custom(&self) -> bool {
        self.custom_migrate.is_some()
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

    /// The registry with the 28 built-in node kinds: the 16 of M1 (hence the
    /// name), the two streaming added (`terrain.erode` and `terrain.chunks`,
    /// ADR-0042) and the 10 polygon and road kinds of ADR-0043 §5.
    pub fn m1() -> Self {
        let mut r = Self::new();
        terrain::register(&mut r);
        erode::register(&mut r);
        field::register(&mut r);
        scatter::register(&mut r);
        points::register(&mut r);
        mesh::register(&mut r);
        fragment::register(&mut r);
        shape::register(&mut r);
        road::register(&mut r);
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

/// Checks how the nodes of a streamed graph fit together (ADR-0042,
/// ADR-0043 §5), so every cell and chunk sees the same shapes and roads: a
/// [`Streaming::Global`] node outputs shapes or a road path, takes fields
/// only from pointwise or region nodes (it samples them) and its other
/// inputs only from global nodes, everything upstream of it is global,
/// pointwise or region, and shapes and road paths come only from global
/// nodes. `Err` says which node breaks this (`RE4301`).
pub fn check_streaming(
    doc: &re_types::graph::GraphDoc,
    registry: &NodeRegistry,
) -> Result<(), String> {
    let defs: HashMap<&str, (&str, &NodeDef)> = doc
        .nodes
        .iter()
        .filter_map(|n| {
            registry
                .get(&n.kind)
                .map(|d| (n.id.as_str(), (n.kind.as_str(), d)))
        })
        .collect();
    for node in &doc.nodes {
        if let Some((kind, d)) = defs.get(node.id.as_str())
            && d.streaming == Streaming::Global
            && !matches!(d.output, PortType::Shapes | PortType::Path)
        {
            return Err(format!(
                "node `{}` ({kind}) is global but its output is `{}`; a global node outputs `shapes` or a `path`",
                node.id,
                d.output.port_name()
            ));
        }
    }
    for e in &doc.edges {
        let (Some((from_kind, from)), Some((to_kind, to))) =
            (defs.get(e.from.0.as_str()), defs.get(e.to.0.as_str()))
        else {
            continue;
        };
        if matches!(from.output, PortType::Shapes | PortType::Path)
            && from.streaming != Streaming::Global
        {
            return Err(format!(
                "node `{}` ({from_kind}) outputs `{}` but streams as `{}`, not `global`, so cells could see different ones",
                e.from.0,
                from.output.port_name(),
                from.streaming.name()
            ));
        }
        let sampled = from.output == PortType::Field
            && matches!(from.streaming, Streaming::Pointwise | Streaming::Region);
        if to.streaming == Streaming::Global && !sampled && from.streaming != Streaming::Global {
            return Err(format!(
                "node `{}` ({to_kind}) is global, but its input `{}` comes from node `{}` ({from_kind}), which streams as `{}`; a global node takes fields from pointwise or region nodes and its other inputs from global nodes",
                e.to.0,
                e.to.1,
                e.from.0,
                from.streaming.name()
            ));
        }
    }
    // What a global node sees must not depend on the window either.
    let mut inputs: HashMap<&str, Vec<&str>> = HashMap::new();
    for e in &doc.edges {
        inputs
            .entry(e.to.0.as_str())
            .or_default()
            .push(e.from.0.as_str());
    }
    for node in &doc.nodes {
        let Some((kind, d)) = defs.get(node.id.as_str()) else {
            continue;
        };
        if d.streaming != Streaming::Global {
            continue;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut todo: Vec<&str> = inputs.get(node.id.as_str()).cloned().unwrap_or_default();
        while let Some(up) = todo.pop() {
            if !seen.insert(up) {
                continue;
            }
            if let Some((up_kind, u)) = defs.get(up)
                && !matches!(
                    u.streaming,
                    Streaming::Global | Streaming::Pointwise | Streaming::Region
                )
            {
                return Err(format!(
                    "node `{}` ({kind}) is global, but node `{up}` ({up_kind}) upstream of it streams as `{}`; everything upstream of a global node streams as global, pointwise or region",
                    node.id,
                    u.streaming.name()
                ));
            }
            todo.extend(inputs.get(up).into_iter().flatten().copied());
        }
    }
    Ok(())
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
    /// The window being evaluated, for streamed evaluation (ADR-0042);
    /// `None` for a [`Streaming::Global`] node, which runs without one.
    pub domain: Option<Domain>,
    /// Rows and other splittable work run on the calling thread
    /// ([`EvalOptions::serial`](crate::EvalOptions::serial)).
    pub serial: bool,
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
    /// Lines the node logged ([`NodeCtx::log`]), for its report.
    pub logs: std::sync::Mutex<Vec<String>>,
    /// For a [`Streaming::Region`] or [`Streaming::Cell`] node in a streamed
    /// evaluation: the field on an input port evaluated over another window
    /// (a region node's tiles, a cell node's window grown by an apron),
    /// through the same evaluator and caches.
    pub upstream: Option<&'a Upstream<'a>>,
    /// For a [`Streaming::Global`] node in a streamed evaluation: the field
    /// on an input port sampled at points `[x, z]` (metres), on the triangles
    /// of the streamed source's sample grid, the same for every cell and
    /// chunk (`Ok(None)` when the port is not connected). Its fields are not
    /// in [`NodeCtx::inputs`] then.
    pub sample: Option<&'a Sampler<'a>>,
    /// The evaluator's store of values made from inputs and shared between
    /// evaluations ([`NodeCtx::shared`]); `None` makes them every time.
    pub share: Option<&'a Share<'a>>,
    /// Evaluates a graph inside this node's evaluation
    /// ([`NodeCtx::evaluate`]).
    pub nested: Option<&'a Nested<'a>>,
    /// The evaluation's deadline and cancel flag ([`NodeCtx::check`]).
    pub(crate) stop: Option<&'a NodeStop<'a>>,
}

/// Evaluates the field on input `port` over a window (see
/// [`NodeCtx::upstream`]).
pub type Upstream<'a> = dyn Fn(&str, Domain) -> Result<Arc<Field>, String> + Sync + 'a;

/// Samples the field on input `port` at points `[x, z]` (see
/// [`NodeCtx::sample`]).
pub type Sampler<'a> = dyn Fn(&str, &[[f64; 2]]) -> Result<Option<Vec<f64>>, String> + Sync + 'a;

/// Evaluates a graph with a seed and the values on its `["$in", name]`
/// edges (see [`NodeCtx::evaluate`]).
pub type Nested<'a> = dyn Fn(&ProcGraph, u64, &[SubInput]) -> SubEvaluation + Sync + 'a;

/// A value made for [`NodeCtx::shared`], and about how many bytes it holds.
pub type SharedValue = (Arc<dyn Any + Send + Sync>, u64);

/// Gives the value shared under a key, made by the function the first time
/// (see [`NodeCtx::shared`]).
pub type Share<'a> = dyn Fn(
        [u8; 32],
        &mut dyn FnMut() -> Result<SharedValue, NodeError>,
    ) -> Result<Arc<dyn Any + Send + Sync>, NodeError>
    + Sync
    + 'a;

/// At most this many lines of a node's log reach its report.
pub const MAX_LOG_LINES: usize = 100;

/// Longer log lines are cut to this many bytes.
pub const MAX_LOG_LINE: usize = 1000;

impl NodeCtx<'_> {
    /// Adds a line to the node's log (in its report). Past
    /// [`MAX_LOG_LINES`] lines one last line says how many were dropped.
    pub fn log(&self, text: &str) {
        let mut logs = self.logs.lock().unwrap_or_else(|e| e.into_inner());
        match logs.len() {
            n if n < MAX_LOG_LINES => {
                let mut line = text.to_string();
                if line.len() > MAX_LOG_LINE {
                    let mut end = MAX_LOG_LINE;
                    while !line.is_char_boundary(end) {
                        end -= 1;
                    }
                    line.truncate(end);
                    line.push('…');
                }
                logs.push(line);
            }
            n if n == MAX_LOG_LINES => logs.push("(more lines dropped)".into()),
            _ => {}
        }
    }

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

    /// The shapes on `port`, if connected.
    pub fn shapes(&self, port: &str) -> Option<&Arc<Shapes>> {
        match self.input(port) {
            Some(Output::Shapes(s)) => Some(s),
            _ => None,
        }
    }

    /// The road path on `port`, if connected.
    pub fn path(&self, port: &str) -> Option<&Arc<RoadPath>> {
        match self.input(port) {
            Some(Output::Path(p)) => Some(p),
            _ => None,
        }
    }

    /// The required shapes on `port`.
    pub fn need_shapes(&self, port: &str) -> Result<&Arc<Shapes>, String> {
        self.shapes(port)
            .ok_or_else(|| format!("input `{port}` has no shapes"))
    }

    /// Every road path on the variadic ports, in natural port-name order.
    pub fn paths(&self) -> Vec<&Arc<RoadPath>> {
        let mut paths: Vec<(&str, &Arc<RoadPath>)> = self
            .inputs
            .iter()
            .filter_map(|(port, o)| match o {
                Output::Path(p) => Some((port.as_str(), p)),
                _ => None,
            })
            .collect();
        paths.sort_by(|a, b| natural_cmp(a.0, b.0));
        paths.into_iter().map(|(_, p)| p).collect()
    }

    /// The value `make` makes, made once per `key` and kept in the
    /// evaluator's memory cache with the outputs (counted against its budget
    /// as the bytes `make` gives, least recently used out), so the cells and
    /// chunks of a streamed source share one road network or triangulation.
    /// A node asking while another makes it waits for that one, so `make`
    /// must not evaluate graphs or wait on the pool. `key` hashes everything
    /// the value depends on, with a tag naming what it is. A failure is
    /// given to the nodes waiting, and not kept.
    pub fn shared<T: Any + Send + Sync>(
        &self,
        key: [u8; 32],
        make: impl FnOnce() -> Result<(T, u64), NodeError>,
    ) -> Result<Arc<T>, NodeError> {
        let Some(share) = self.share else {
            return make().map(|(v, _)| Arc::new(v));
        };
        let mut make = Some(make);
        let mut once = || -> Result<SharedValue, NodeError> {
            let make = make
                .take()
                .ok_or_else(|| "a shared value was made twice".to_string())?;
            let (v, bytes) = make()?;
            Ok((Arc::new(v) as Arc<dyn Any + Send + Sync>, bytes))
        };
        share(key, &mut once)?.downcast::<T>().map_err(|_| {
            "a value of another type is shared under the same key"
                .to_string()
                .into()
        })
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
        let h = re_noise::mix64(self.window_seed() ^ re_noise::mix64(id ^ salt.rotate_left(29)));
        (h >> 40) as f32 * (1.0 / 16_777_216.0)
    }

    /// The seed for drawing per-window random numbers: [`NodeCtx::seed`]
    /// without a domain; with one, the seed forked by the window's origin, so
    /// a cell-local node draws different numbers in each cell (ADR-0042).
    /// Pointwise nodes use `seed` itself, so windows agree on shared samples.
    pub fn window_seed(&self) -> u64 {
        match self.domain {
            None => self.seed,
            Some(d) => re_noise::mix64(
                self.seed
                    ^ re_noise::mix64(d.origin[0] as u64)
                    ^ re_noise::mix64(d.origin[1] as u64).rotate_left(17),
            ),
        }
    }

    /// Fills `out` (rows of `row_len` values) in parallel on the pool:
    /// `row(z, values)` fills row `z`. Each row is computed on its own, so
    /// the result does not depend on the thread count. Once the evaluation
    /// stops ([`NodeCtx::stopped`]) the rows not started are left as they
    /// are, and the node's output is dropped.
    pub fn par_rows(
        &self,
        out: &mut [f32],
        row_len: usize,
        row: impl Fn(usize, &mut [f32]) + Sync,
    ) {
        let row = &row;
        let row_len = row_len.max(1);
        let rows_per_job = (4096 / row_len).max(1);
        let stop = self.stop;
        let job = move |j: usize, chunk: &mut [f32]| {
            if stop.is_some_and(NodeStop::check) {
                return;
            }
            for (i, values) in chunk.chunks_mut(row_len).enumerate() {
                row(j * rows_per_job + i, values);
            }
        };
        if self.serial {
            for (j, chunk) in out.chunks_mut(row_len * rows_per_job).enumerate() {
                job(j, chunk);
            }
            return;
        }
        let job = &job;
        self.pool.scope(|s| {
            for (j, chunk) in out.chunks_mut(row_len * rows_per_job).enumerate() {
                s.spawn(move |_| job(j, chunk));
            }
        });
    }

    /// True once the evaluation is past its deadline or cancelled. A long
    /// node checks between pieces of work (a few milliseconds apart) and
    /// returns early: whatever it returns after seeing this is dropped,
    /// never cached, and the node is reported as skipped.
    pub fn stopped(&self) -> bool {
        self.stop.is_some_and(NodeStop::check)
    }

    /// `Err` once the evaluation is past its deadline or cancelled
    /// ([`NodeCtx::stopped`]), for `?` between pieces of work.
    pub fn check(&self) -> Result<(), String> {
        if self.stopped() {
            Err("the evaluation stopped".into())
        } else {
            Ok(())
        }
    }

    /// Evaluates `graph` (a graph kind bound to its parameters) inside this
    /// node's evaluation, with `seed` and `inputs` on its `["$in", name]`
    /// edges: through the same memory and disk caches and budget, never
    /// evicting what this evaluation uses, without a window, and stopping
    /// when this evaluation stops. Its nodes run on the pool while this one
    /// waits, so it is safe on a worker. `graph` is not validated here: a
    /// graph kind is validated when it is declared.
    pub fn evaluate(&self, graph: &ProcGraph, seed: u64, inputs: &[SubInput]) -> SubEvaluation {
        match self.nested {
            Some(nested) => nested(graph, seed, inputs),
            None => {
                // Only the evaluator makes a context, and it always can.
                let report = crate::EvalReport {
                    graph: graph.id.clone(),
                    seed,
                    diagnostics: vec![Diagnostic::error(
                        crate::codes::INVALID_GRAPH,
                        format!(
                            "node `{}` cannot evaluate graph `{}` here",
                            self.node_id, graph.id
                        ),
                    )],
                    ..Default::default()
                };
                let n = graph.nodes.len();
                SubEvaluation {
                    report,
                    ids: graph.nodes.keys().cloned().collect(),
                    sinks: vec![false; n],
                    outputs: vec![None; n],
                    keys: vec![None; n],
                    cache_keys: vec![None; n],
                    stopped: false,
                }
            }
        }
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

/// Checks a tessellation count: `min` to
/// [`MAX_TESSELLATION`](re_geom::primitives::MAX_TESSELLATION).
pub(crate) fn tessellation(name: &str, v: u32, min: u32) -> Result<(), String> {
    if (min..=re_geom::primitives::MAX_TESSELLATION).contains(&v) {
        Ok(())
    } else {
        Err(format!(
            "`{name}` must be {min} to {}, got {v}",
            re_geom::primitives::MAX_TESSELLATION
        ))
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
    fn canonical_parameters_of_a_rust_kind() {
        let r = NodeRegistry::m1();
        let h = r.get("terrain.heightfield").unwrap();
        let c = |p: Json| h.canonical_params(&p);
        assert_eq!(
            c(json!({ "cell": 2 })),
            c(json!({ "cell": 2.0, "size": 128 }))
        );
        assert_eq!(c(json!({ "cell": 2 }))["cell"], json!(2.0));
        assert_eq!(c(Json::Null), c(json!({})));
        assert_ne!(c(json!({ "cell": 2 })), c(json!({ "cell": 3 })));
        // Parameters the type refuses are kept as they are.
        assert_eq!(c(json!({ "sise": 1 })), json!({ "sise": 1 }));
    }

    #[test]
    fn canonical_parameters_from_a_schema() {
        let schema = json!({ "properties": {
            "r": { "type": "number", "default": 16 },
            "n": { "type": "integer" },
            "v": { "type": "array", "items": { "type": "number" } },
            "s": { "type": "string", "default": "x" },
        }});
        let c = |p: Json| canonical_from_schema(&schema, &p);
        assert_eq!(
            c(json!({ "n": 4.0, "v": [1, 2.5] })),
            json!({ "r": 16.0, "n": 4, "v": [1.0, 2.5], "s": "x" })
        );
        assert_eq!(c(Json::Null), json!({ "r": 16.0, "s": "x" }));
        // An integer written with a fraction, or as negative zero, keeps its
        // own key.
        assert_eq!(c(json!({ "n": 4.5 }))["n"], json!(4.5));
        let zero = &c(json!({ "n": -0.0 }))["n"];
        assert!(zero.is_f64() && zero.as_f64().unwrap().is_sign_negative());
        // Unknown properties are kept; a value that is not an object too.
        assert_eq!(c(json!({ "q": 1 }))["q"], json!(1));
        assert_eq!(c(json!(3)), json!(3));
    }

    #[test]
    fn the_registry_has_twenty_eight_kinds() {
        let r = NodeRegistry::m1();
        assert_eq!(r.len(), 28);
        let schema = r.graph_json_schema();
        assert!(schema.pointer("/$defs/NodeDoc/allOf/27").is_some());
    }
}
