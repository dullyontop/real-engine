//! `re_geom`: geometry for Real Engine, over [`re_types::mesh::MeshData`].
//!
//! * [`primitives`]: box ([`cuboid`]), [`uv_sphere`], [`cylinder`],
//!   [`capsule`] and [`plane`];
//! * [`Heightfield`]: an `n × n` grid of heights in Jolt's layout
//!   (`samples[z * n + x]`), with [`Heightfield::to_mesh`],
//!   [`Heightfield::height_at`] and [`Heightfield::to_shape`];
//! * [`ops`]: [`compute_normals`], [`transform`] and [`merge`];
//! * [`terrain`]: the level-of-detail chunk quadtree, stitched chunk meshes
//!   and hydraulic erosion on region tiles, for streamed worlds (ADR-0042).
//!
//! Everything here is plain computation with no GPU, physics or scripting
//! dependency. Results are **bit-identical on every platform**: angles come
//! from a polynomial [`math::sin_cos`] instead of libm, and vertices are
//! processed in a fixed order with scalar arithmetic and no fused
//! multiply-add, following `re_noise`'s rules. `re_proc` relies on this for
//! its cache keys and golden hashes.
//!
//! Errors are [`GeomError`]s, which convert to [`re_core::Diagnostic`]s with
//! codes `RE4500`–`RE4599`; each has an explain page listed in
//! [`DIAGNOSTICS`].

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod codes;
mod error;
mod heightfield;
pub mod math;
pub mod ops;
pub mod primitives;
pub mod terrain;

pub use error::GeomError;
pub use heightfield::Heightfield;
pub use ops::{compute_normals, merge, transform};
pub use primitives::{capsule, cuboid, cylinder, plane, uv_sphere};

use re_core::DiagInfo;

/// Every diagnostic code `re_geom` uses, with its explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::INVALID_HEIGHTFIELD,
        title: "Invalid heightfield",
        page: include_str!("../explain/RE4500.md"),
    },
    DiagInfo {
        code: codes::INVALID_PRIMITIVE,
        title: "Invalid primitive",
        page: include_str!("../explain/RE4501.md"),
    },
    DiagInfo {
        code: codes::MESH_TOO_LARGE,
        title: "Mesh too large",
        page: include_str!("../explain/RE4502.md"),
    },
];

/// The code range this crate uses: `RE4500`–`RE4599`, the part of the
/// `RE4xxx` block (ARCHITECTURE §7) that `re_proc` leaves to it.
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 4500..=4599;
