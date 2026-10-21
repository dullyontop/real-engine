//! World streaming (ADR-0042): a world larger than memory, loaded cell by
//! cell around its focus points.
//!
//! * [`StreamConfig`]: `[stream]` in `real.toml` (`RE4300`).
//! * [`Streamer`]: the manager, a world resource. [`step`] runs it once per
//!   fixed tick: the focuses ([`re_scene::StreamFocus`], else the active
//!   camera) give the desired cells; a cell requested at tick `t` is
//!   generated and applied at tick `t + delay_ticks`, in key order, within
//!   `apply_entities` per tick; loaded cells farther than `radius_ahead +
//!   unload_margin` from every focus unload; memory is accounted from data
//!   sizes. Everything depends only on the world and the tick, so a replay
//!   loads the same cells at the same ticks on any machine.
//! * [`StreamEvents`]: the cells loaded and unloaded at the last tick, as
//!   [`re_scene::StreamCell`]s, for scripts' `stream.on_loaded` and
//!   `stream.on_unloaded`.
//!
//! A streamed procedural source (a `graph` source with
//! [`re_scene::Streamed`] and an identity transform, else `RE4302`) has its
//! graph evaluated once per cell, over the cell's window
//! ([`crate::Domain`]): `cell_size / spacing + 1` samples per side at global
//! sample indices, where the spacing is the `cell` of the graph's
//! `terrain.heightfield` nodes. Every node must stream (`RE4301`); how each
//! kind does is its [`crate::Streaming`]. The cell's keys are the graph's
//! keys under `c/<x>_<z>/`, applied with [`re_scene::World::apply_cell`], so
//! overrides survive unloading. Authored `Streamed` roots are stashed with
//! their cell and come back exactly.

mod config;
mod manager;
mod source;
mod terrain;

pub use config::StreamConfig;
pub use manager::{
    ENTITY_BYTES, Focus, Lateness, SHAPE_BYTES, StepReport, StreamEvents, Streamer, cell_at,
    focuses, step,
};
pub use terrain::{TerrainView, ViewChunk};

#[cfg(test)]
mod tests;
