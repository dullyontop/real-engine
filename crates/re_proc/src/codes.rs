//! The diagnostic codes `re_proc` owns (`RE4000`–`RE4499`, inside the
//! `RE4xxx` block it shares with `re_geom`, which uses `RE4500`–`RE4599`).
//!
//! Each code has an explain page in `crates/re_proc/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// A graph document does not parse, or breaks a structural rule (format,
/// version, unique node ids, edges naming existing nodes).
pub const INVALID_GRAPH: Code = Code::new("RE4000");

/// A node's `kind` is not a registered node kind.
pub const UNKNOWN_NODE_KIND: Code = Code::new("RE4001");

/// An edge names a port the node does not have.
pub const UNKNOWN_PORT: Code = Code::new("RE4002");

/// An edge connects an output to an input that does not accept its type.
pub const PORT_TYPE_MISMATCH: Code = Code::new("RE4003");

/// The edges form a cycle.
pub const CYCLE: Code = Code::new("RE4004");

/// A node's parameters do not match its kind's parameter schema.
pub const INVALID_PARAMS: Code = Code::new("RE4005");

/// A required input port is not connected.
pub const MISSING_INPUT: Code = Code::new("RE4006");

/// An input port that takes one edge has several.
pub const INPUT_CONNECTED_TWICE: Code = Code::new("RE4007");

/// A node failed while evaluating.
pub const NODE_FAILED: Code = Code::new("RE4008");

/// No graph document is stored for a procedural source.
pub const NO_GRAPH: Code = Code::new("RE4009");

/// An override's target is no longer generated; the override is kept (warning).
pub const ORPHANED_OVERRIDE: Code = Code::new("RE4010");

/// A parameter override names a node or parameter the graph does not have.
pub const UNKNOWN_OVERRIDE: Code = Code::new("RE4011");

/// A node's `version` is newer than this engine's node kind.
pub const UNSUPPORTED_NODE_VERSION: Code = Code::new("RE4012");

/// Two entities of a graph's output have the same key.
pub const DUPLICATE_KEY: Code = Code::new("RE4013");

/// A prefab file could not be read, parsed or instantiated.
pub const PREFAB_FAILED: Code = Code::new("RE4014");

/// The disk cache could not be read or written (warning; evaluation goes on).
pub const CACHE_IO: Code = Code::new("RE4015");

/// An evaluation went over its time or entity budget.
pub const BUDGET_EXCEEDED: Code = Code::new("RE4016");

/// A command's `source` is not a procedural source of the `graph` generator.
pub const NOT_A_GRAPH_SOURCE: Code = Code::new("RE4017");

/// A node's version is older than the oldest version its kind still evaluates.
pub const RETIRED_NODE_VERSION: Code = Code::new("RE4018");

/// A node is written for an older version of its kind than the current one
/// (warning; it still evaluates at its own version).
pub const OUTDATED_NODE_VERSION: Code = Code::new("RE4019");

/// A node's parameters could not be migrated to its kind's current version.
pub const MIGRATION_FAILED: Code = Code::new("RE4020");
/// `proc.bake` could not bake a source (nothing to bake, an unknown key, a
/// key whose generated parent is not baked, or a baked asset that cannot be
/// written).
pub const BAKE_FAILED: Code = Code::new("RE4021");

/// A source's open graph differs from its graph file, which a migration
/// reads and rewrites.
pub const OPEN_GRAPH_DIFFERS: Code = Code::new("RE4022");

/// A Luau node script under `scripts/generators/` does not declare a valid
/// node kind: it fails to compile or run, does not return `proc.node{...}`,
/// or its kind is malformed or taken (ADR-0043 §4).
pub const LUAU_DECLARATION: Code = Code::new("RE4100");

/// A Luau node ran past its safepoint budget (`[proc] luau_budget`) or its
/// memory limit (`[proc] luau_memory_mb`). The same on every machine.
pub const LUAU_BUDGET: Code = Code::new("RE4101");

/// A Luau node ran past the wall-clock backstop (60 s), for example inside
/// one long builtin. Machine-dependent, unlike `RE4101`.
pub const LUAU_BACKSTOP: Code = Code::new("RE4102");

/// A Luau node returned an output that does not fit its declared type: a
/// non-finite number, a wrong length, a bad key or component name.
pub const LUAU_OUTPUT: Code = Code::new("RE4103");

/// `proc.create_source` cannot make a source from the kind: it is unknown,
/// does not output a fragment, needs inputs, or the graph file exists.
pub const CREATE_SOURCE: Code = Code::new("RE4104");

/// The `[stream]` section of `real.toml` has an unknown key or a bad value.
pub const STREAM_CONFIG: Code = Code::new("RE4300");

/// A streamed source's graph uses a node that cannot be evaluated per cell.
pub const NOT_STREAMABLE: Code = Code::new("RE4301");

/// A streamed procedural source does not have an identity transform.
pub const STREAMED_SOURCE_MOVED: Code = Code::new("RE4302");

/// Entities outside a streamed subtree reference into it while its cell is
/// unloaded (warning; the references read as empty until it loads).
pub const REFERENCE_INTO_UNLOADED: Code = Code::new("RE4303");

/// An entity was named that lies in an unloaded cell.
pub const IN_UNLOADED_CELL: Code = Code::new("RE4304");

/// A cell's content was not ready at its apply tick and was applied late
/// (warning, in play).
pub const LATE_CELL: Code = Code::new("RE4305");

/// The streaming memory budget cannot hold every cell within `radius`; the
/// load radius shrinks (warning).
pub const STREAM_MEMORY: Code = Code::new("RE4306");

/// Generating a cell failed; the cell stays empty until its inputs change.
pub const CELL_FAILED: Code = Code::new("RE4307");

/// The floating origin cannot keep every focus within physics range.
pub const ORIGIN_RANGE: Code = Code::new("RE4308");

/// Content lies beyond the precision limit of world-space positions (warning).
pub const PRECISION_LIMIT: Code = Code::new("RE4309");

/// A streaming job's evaluation was cancelled before it finished (its cell
/// was no longer wanted, or its inputs changed). Never shown as a problem:
/// the result is dropped.
pub const CANCELLED: Code = Code::new("RE4310");
