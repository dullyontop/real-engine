//! The diagnostic codes `re_geom` owns (`RE4500`–`RE4599`, inside the
//! `RE4xxx` block it shares with `re_proc`).
//!
//! Each code has an explain page in `crates/re_geom/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// A heightfield has fewer than 2 samples per side, the wrong number of
/// samples, a non-positive spacing, or a non-finite value.
pub const INVALID_HEIGHTFIELD: Code = Code::new("RE4500");

/// A primitive's size or tessellation is out of range.
pub const INVALID_PRIMITIVE: Code = Code::new("RE4501");

/// A merged mesh would have more vertices than a `u32` index can name.
pub const MESH_TOO_LARGE: Code = Code::new("RE4502");
