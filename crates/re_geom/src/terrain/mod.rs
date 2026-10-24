//! Terrain for streamed worlds (ADR-0042): the chunked level-of-detail
//! quadtree, stitched chunk meshes, and hydraulic erosion on region tiles.
//!
//! Everything here is pure computation and follows the crate's determinism
//! rules (no libm transcendental functions, no fused multiply-add, fixed
//! evaluation order).
//!
//! # Positions
//!
//! Level `L` samples the terrain every `base_spacing × 2^L` metres. Sample
//! `i` of level `L` sits at `i as f32 × spacing(L)`; doubling a float is
//! exact, so level `L + 1`'s sample `i` and level `L`'s sample `2i` are the
//! same float, and a height function evaluated at both gives the same bits.
//! That is what lets [`chunk_indices`] stitch neighbouring levels exactly.

mod erode;
mod lod;
mod mesh;

pub use erode::{APRON, ErosionParams, TILE_CELLS, erode, tile_blend, tile_window};
pub use lod::{ChunkKey, Edges, TerrainGrid, select};
pub use mesh::{chunk_indices, chunk_mesh};

/// Cells per chunk side at every level; a chunk has `CHUNK_CELLS + 1`
/// samples per side.
pub const CHUNK_CELLS: u32 = 64;

/// Samples per chunk side.
pub const CHUNK_SAMPLES: u32 = CHUNK_CELLS + 1;
