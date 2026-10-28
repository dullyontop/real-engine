//! `re_proc`: the procedural core of Real Engine (ARCHITECTURE §5, ADR-0015).
//!
//! * [`ProcGraph`]: the validated IR of a `*.graph.json` document
//!   ([`re_types::graph::GraphDoc`]), with errors `RE4000`–`RE4007`;
//! * [`NodeRegistry`]: the 28 built-in node kinds ([`NodeRegistry::m1`]:
//!   M1's 16, the two terrain kinds and ADR-0043 §5's polygon and road kinds), each
//!   with typed ports and a schemars parameter schema
//!   ([`NodeRegistry::graph_json_schema`] gives `graph.schema.json`);
//! * [`Evaluator`]: topological, parallel evaluation on the
//!   [`TaskPool`](re_core::TaskPool), with per-node blake3 keys, per-node
//!   seeds (`Pcg32::from_seed(seed).fork(hash64(node_id))`), a memory cache
//!   and a postcard disk cache in `.real/cache/proc/` kept to a size limit
//!   (least recently used entries go first), reported in an [`EvalReport`];
//! * [`GraphStore`]: the world resource holding each procedural source's
//!   graph, the shared evaluator, the derived assets (meshes and
//!   heightfields named `derived:<key>`) and the last reports; a source whose
//!   graph file is open in the World is generated from the open document
//!   ([`register_doc_validators`] checks open graph documents, ADR-0044);
//! * [`register_generator`]: the `graph` generator for `re_scene`'s
//!   procedural sources (StableKeys `<node id>/<local key>`);
//! * [`register_commands`]: `graph.set_param`, `graph.regenerate`,
//!   `graph.generate` and `graph.report`;
//! * [`stream`]: world streaming (ADR-0042), cells generated and applied
//!   around focus points at deterministic ticks; shapes and road paths are
//!   [global](Streaming::Global) (one result for every cell and chunk), and
//!   road and shape meshes are cut cell by cell.
//!
//! # Ports and the graph's output
//!
//! Values are [`Field`]s (square grids of heights in metres, or weights),
//! meshes, [`Points`] and fragments (entities to create). Each node has one
//! output port named after its type (`field`, `mesh`, `points`,
//! `fragment`; an edge may also name it `out`), and named inputs; `mesh.merge` and `fragment.merge` take
//! inputs on any port names. The graph's output is the fragment of every
//! node that no edge leaves (a mesh there becomes one entity), in document
//! order.
//!
//! # Determinism
//!
//! The same graph, parameters and seed give the same fragment, bit for bit,
//! on any number of threads: nodes are pure functions of their key, noise
//! and geometry follow `re_noise`'s portable rules, and results are
//! gathered in document order.
//!
//! `re_proc` reaches no GPU, physics or scripting crate.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod bake;
mod cache;
pub mod canon;
pub mod codes;
mod commands;
mod edit;
mod eval;
mod generator;
mod glb;
mod graph;
pub mod migrate;
pub mod nodes;
mod store;
pub mod stream;
mod structure;
pub mod value;

pub use canon::version_stamp;
pub use commands::{
    DEFAULT_GENERATE_ENTITIES, DEFAULT_GENERATE_MS, GenerateArgs, MigrationReportArgs,
    RegenerateArgs, ReportArgs, SetParamArgs, register_commands,
};
pub use edit::{AddNodeArgs, ConnectArgs, MoveNodesArgs, RemoveNodeArgs, SetDefaultArgs};
pub use eval::{
    CacheState, DEFAULT_DISK_CACHE_LIMIT, DEFAULT_MEMORY_BUDGET, EvalOptions, EvalReport,
    Evaluation, Evaluator, NodeReport, OutputInfo, fragment_hash_hex, to_scene_fragment,
};
pub use generator::{
    GENERATOR, GENERATOR_VERSION, ParamOverrides, apply_overrides, overrides_text, parse_overrides,
    register_generator,
};
pub use graph::{Edge, GraphId, Node, NodeId, OUT_ALIAS, PortName, ProcGraph};
pub use nodes::{
    CheckFn, CustomKind, CustomPort, Domain, EvalFn, MAX_LOG_LINE, MAX_LOG_LINES, MigrateFn,
    Migration, NodeCtx, NodeDef, NodeError, NodeRegistry, NodeSpec, Params, PortDef, Streaming,
};
pub use store::{
    Budget, CurrentGraph, GraphStore, MEMORY_DISK_CACHE_LIMIT, register_doc_validators,
    validate_graph_json,
};
pub use value::{
    DerivedAsset, Field, FragItem, Fragment, MeshValue, Output, Point, Points, PortType, PrefabUse,
    RoadPath, Shapes,
};

use re_core::DiagInfo;

/// Every diagnostic code `re_proc` uses, with its explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::INVALID_GRAPH,
        title: "Invalid graph document",
        page: include_str!("../explain/RE4000.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_NODE_KIND,
        title: "Unknown node kind",
        page: include_str!("../explain/RE4001.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_PORT,
        title: "Unknown port",
        page: include_str!("../explain/RE4002.md"),
    },
    DiagInfo {
        code: codes::PORT_TYPE_MISMATCH,
        title: "Port type mismatch",
        page: include_str!("../explain/RE4003.md"),
    },
    DiagInfo {
        code: codes::CYCLE,
        title: "Cycle in a graph",
        page: include_str!("../explain/RE4004.md"),
    },
    DiagInfo {
        code: codes::INVALID_PARAMS,
        title: "Invalid node parameters",
        page: include_str!("../explain/RE4005.md"),
    },
    DiagInfo {
        code: codes::MISSING_INPUT,
        title: "Required input not connected",
        page: include_str!("../explain/RE4006.md"),
    },
    DiagInfo {
        code: codes::INPUT_CONNECTED_TWICE,
        title: "Input connected twice",
        page: include_str!("../explain/RE4007.md"),
    },
    DiagInfo {
        code: codes::NODE_FAILED,
        title: "Node failed",
        page: include_str!("../explain/RE4008.md"),
    },
    DiagInfo {
        code: codes::NO_GRAPH,
        title: "No graph for a procedural source",
        page: include_str!("../explain/RE4009.md"),
    },
    DiagInfo {
        code: codes::ORPHANED_OVERRIDE,
        title: "Orphaned override",
        page: include_str!("../explain/RE4010.md"),
    },
    DiagInfo {
        code: codes::UNKNOWN_OVERRIDE,
        title: "Override names an unknown node or parameter",
        page: include_str!("../explain/RE4011.md"),
    },
    DiagInfo {
        code: codes::UNSUPPORTED_NODE_VERSION,
        title: "Unsupported node version",
        page: include_str!("../explain/RE4012.md"),
    },
    DiagInfo {
        code: codes::DUPLICATE_KEY,
        title: "Duplicate key in a graph's output",
        page: include_str!("../explain/RE4013.md"),
    },
    DiagInfo {
        code: codes::PREFAB_FAILED,
        title: "Prefab failed",
        page: include_str!("../explain/RE4014.md"),
    },
    DiagInfo {
        code: codes::CACHE_IO,
        title: "Procedural cache I/O failed",
        page: include_str!("../explain/RE4015.md"),
    },
    DiagInfo {
        code: codes::BUDGET_EXCEEDED,
        title: "Budget exceeded",
        page: include_str!("../explain/RE4016.md"),
    },
    DiagInfo {
        code: codes::NOT_A_GRAPH_SOURCE,
        title: "Not a graph source",
        page: include_str!("../explain/RE4017.md"),
    },
    DiagInfo {
        code: codes::RETIRED_NODE_VERSION,
        title: "Retired node version",
        page: include_str!("../explain/RE4018.md"),
    },
    DiagInfo {
        code: codes::OUTDATED_NODE_VERSION,
        title: "Older node version in use",
        page: include_str!("../explain/RE4019.md"),
    },
    DiagInfo {
        code: codes::MIGRATION_FAILED,
        title: "Node migration failed",
        page: include_str!("../explain/RE4020.md"),
    },
    DiagInfo {
        code: codes::BAKE_FAILED,
        title: "Bake failed",
        page: include_str!("../explain/RE4021.md"),
    },
    DiagInfo {
        code: codes::STRUCTURE_REFUSED,
        title: "Structural override refused",
        page: include_str!("../explain/RE4023.md"),
    },
    DiagInfo {
        code: codes::OPEN_GRAPH_DIFFERS,
        title: "Open graph differs from its file",
        page: include_str!("../explain/RE4022.md"),
    },
    DiagInfo {
        code: codes::LUAU_DECLARATION,
        title: "Invalid Luau node declaration",
        page: include_str!("../explain/RE4100.md"),
    },
    DiagInfo {
        code: codes::LUAU_BUDGET,
        title: "Luau node over its budget",
        page: include_str!("../explain/RE4101.md"),
    },
    DiagInfo {
        code: codes::LUAU_BACKSTOP,
        title: "Luau node stopped by the wall-clock backstop",
        page: include_str!("../explain/RE4102.md"),
    },
    DiagInfo {
        code: codes::LUAU_OUTPUT,
        title: "Invalid Luau node output",
        page: include_str!("../explain/RE4103.md"),
    },
    DiagInfo {
        code: codes::CREATE_SOURCE,
        title: "Cannot create a source from a node kind",
        page: include_str!("../explain/RE4104.md"),
    },
    DiagInfo {
        code: codes::STREAM_CONFIG,
        title: "Invalid [stream] settings",
        page: include_str!("../explain/RE4300.md"),
    },
    DiagInfo {
        code: codes::NOT_STREAMABLE,
        title: "Node does not stream",
        page: include_str!("../explain/RE4301.md"),
    },
    DiagInfo {
        code: codes::STREAMED_SOURCE_MOVED,
        title: "Streamed source is not at the origin",
        page: include_str!("../explain/RE4302.md"),
    },
    DiagInfo {
        code: codes::REFERENCE_INTO_UNLOADED,
        title: "Reference into an unloaded cell",
        page: include_str!("../explain/RE4303.md"),
    },
    DiagInfo {
        code: codes::IN_UNLOADED_CELL,
        title: "Entity in an unloaded cell",
        page: include_str!("../explain/RE4304.md"),
    },
    DiagInfo {
        code: codes::LATE_CELL,
        title: "Cell applied late",
        page: include_str!("../explain/RE4305.md"),
    },
    DiagInfo {
        code: codes::STREAM_MEMORY,
        title: "Streaming memory budget too small",
        page: include_str!("../explain/RE4306.md"),
    },
    DiagInfo {
        code: codes::CELL_FAILED,
        title: "Cell generation failed",
        page: include_str!("../explain/RE4307.md"),
    },
    DiagInfo {
        code: codes::ORIGIN_RANGE,
        title: "Floating origin cannot cover every focus",
        page: include_str!("../explain/RE4308.md"),
    },
    DiagInfo {
        code: codes::PRECISION_LIMIT,
        title: "Content beyond the precision limit",
        page: include_str!("../explain/RE4309.md"),
    },
    DiagInfo {
        code: codes::CANCELLED,
        title: "Streaming job cancelled",
        page: include_str!("../explain/RE4310.md"),
    },
];

/// The code range this crate uses: `RE4000`–`RE4499`, the part of the
/// `RE4xxx` block (ARCHITECTURE §7) that `re_geom` leaves to it.
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 4000..=4499;
