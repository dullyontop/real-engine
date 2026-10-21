//! `[stream]` in `real.toml` (ADR-0042).

use re_core::Diagnostic;
use serde::{Deserialize, Serialize};

use crate::codes;

/// The `[stream]` section of `real.toml`: the cell grid, the load radii and
/// the per-tick budgets of world streaming (ADR-0042). Every field has a
/// default; an unknown key is refused when the file is parsed and a bad value
/// by [`StreamConfig::validate`], both `RE4300`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct StreamConfig {
    /// Metres per cell side.
    pub cell_size: f32,
    /// Cells within this distance of a focus load.
    pub radius: f32,
    /// And out to this distance, within ±60° of a focus's forward.
    pub radius_ahead: f32,
    /// Hysteresis: a loaded cell unloads only when every focus is farther
    /// than `radius_ahead + unload_margin` from it, so turning never unloads.
    pub unload_margin: f32,
    /// Resident streamed data, in MiB (accounted from data sizes).
    pub memory_mb: u32,
    /// Generation jobs in flight; 0 is the pool's threads − 1, at least 1.
    pub workers: u32,
    /// A cell requested at tick `t` is applied at tick `t + delay_ticks`.
    pub delay_ticks: u32,
    /// Streamed entities spawned or despawned per tick, at most (at least
    /// one cell is always applied).
    pub apply_entities: u32,
    /// Terrain collision chunks added or removed per tick, at most.
    pub apply_chunks: u32,
    /// The terrain level-of-detail horizon, in metres.
    pub terrain_view: f32,
    /// Terrain collision chunks within this distance of a focus.
    pub physics_radius: f32,
    /// The floating origin snaps to this grid, in metres.
    pub origin_grid: f32,
    /// And moves when a focus is this far from it on X or Z, in metres.
    pub origin_shift: f32,
}

impl Default for StreamConfig {
    fn default() -> Self {
        StreamConfig {
            cell_size: 256.0,
            radius: 768.0,
            radius_ahead: 1280.0,
            unload_margin: 128.0,
            memory_mb: 1024,
            workers: 0,
            delay_ticks: 30,
            apply_entities: 400,
            apply_chunks: 8,
            terrain_view: 6000.0,
            physics_radius: 256.0,
            origin_grid: 1024.0,
            origin_shift: 2048.0,
        }
    }
}

impl StreamConfig {
    /// Checks every value (`RE4300`, naming the first bad key).
    pub fn validate(&self) -> Result<(), Diagnostic> {
        let bad = |key: &str, why: String| {
            Err(
                Diagnostic::error(codes::STREAM_CONFIG, format!("[stream] {key}: {why}"))
                    .with_hint("see `real explain RE4300` for each key and its range"),
            )
        };
        let metres = [
            ("cell_size", self.cell_size, 1.0, 65_536.0),
            ("radius", self.radius, 0.0, 1.0e6),
            ("radius_ahead", self.radius_ahead, 0.0, 1.0e6),
            ("unload_margin", self.unload_margin, 0.0, 1.0e6),
            ("terrain_view", self.terrain_view, 0.0, 1.0e6),
            ("physics_radius", self.physics_radius, 0.0, 1.0e6),
            ("origin_grid", self.origin_grid, 1.0, 1.0e6),
            ("origin_shift", self.origin_shift, 1.0, 1.0e6),
        ];
        for (key, v, lo, hi) in metres {
            if !(v.is_finite() && v >= lo && v <= hi) {
                return bad(key, format!("{v} is not in {lo}..={hi} metres"));
            }
        }
        if self.radius_ahead < self.radius {
            return bad(
                "radius_ahead",
                format!(
                    "{} is less than radius ({})",
                    self.radius_ahead, self.radius
                ),
            );
        }
        if self.origin_shift < self.origin_grid {
            return bad(
                "origin_shift",
                format!(
                    "{} is less than origin_grid ({}), so the origin would move every tick",
                    self.origin_shift, self.origin_grid
                ),
            );
        }
        let counts = [
            ("memory_mb", self.memory_mb, 1),
            ("apply_entities", self.apply_entities, 1),
            ("apply_chunks", self.apply_chunks, 1),
        ];
        for (key, v, lo) in counts {
            if v < lo {
                return bad(key, format!("must be at least {lo}"));
            }
        }
        if self.delay_ticks > 3600 {
            return bad(
                "delay_ticks",
                format!("{} is more than 3600", self.delay_ticks),
            );
        }
        if self.workers > 256 {
            return bad("workers", format!("{} is more than 256", self.workers));
        }
        Ok(())
    }

    /// The generation jobs in flight for a pool of `threads` workers.
    pub fn jobs(&self, threads: usize) -> usize {
        match self.workers {
            0 => threads.saturating_sub(1).max(1),
            n => n as usize,
        }
    }

    /// The memory budget in bytes.
    pub fn memory_bytes(&self) -> u64 {
        u64::from(self.memory_mb) << 20
    }
}
