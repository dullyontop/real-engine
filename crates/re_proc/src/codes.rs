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
