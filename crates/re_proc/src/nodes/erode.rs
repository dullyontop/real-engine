//! `terrain.erode`: hydraulic erosion of a height field (ADR-0042).
//!
//! Not streamed, the whole field erodes at once. Streamed, the field erodes
//! on region tiles of [`TILE_CELLS`] cells at the node's own `spacing`, each
//! with an [`APRON`] read through [`NodeCtx::upstream`](super::NodeCtx), and
//! the tiles are blended with weights that depend only on global position:
//! the eroded terrain is one function of position, which every window (cell,
//! level-of-detail chunk, collision chunk) samples bilinearly at its exact
//! positions.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

use re_geom::terrain::{APRON, ErosionParams, TILE_CELLS, erode, tile_blend, tile_window};
use re_noise::Pcg32;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Domain, NodeCtx, NodeDef, NodeRegistry, NodeSpec, Params, PortDef, positive, within};
use crate::value::{Field, Output, PortType};

/// Eroded tiles kept in memory (each about 400 KiB): more than the 6 km
/// terrain horizon covers (about 130 tiles at 4 m).
const TILE_CACHE: usize = 256;

/// Parameters of `terrain.erode`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ErodeParams {
    /// Metres between the samples erosion runs on, when streamed (the
    /// field's own spacing otherwise).
    pub spacing: f32,
    /// Droplets per cell of the erosion grid.
    pub droplets: f32,
    /// Steps a droplet lives at most.
    pub steps: u32,
    /// Share of the free capacity eroded per step (0 to 1).
    pub erosion: f32,
    /// Share of the excess sediment dropped per step (0 to 1).
    pub deposition: f32,
    /// Sediment a droplet can carry, per unit of slope, speed and water.
    pub capacity: f32,
}

impl Default for ErodeParams {
    fn default() -> Self {
        let d = ErosionParams::default();
        ErodeParams {
            spacing: 4.0,
            droplets: 0.5,
            steps: d.steps,
            erosion: d.erosion,
            deposition: d.deposition,
            capacity: d.capacity,
        }
    }
}

impl Params for ErodeParams {
    fn validate(&self) -> Result<(), String> {
        positive("spacing", self.spacing)?;
        within("droplets", self.droplets, 0.0, 16.0)?;
        within("erosion", self.erosion, 0.0, 1.0)?;
        within("deposition", self.deposition, 0.0, 1.0)?;
        within("capacity", self.capacity, 0.0, 1000.0)?;
        if !(1..=1024).contains(&self.steps) {
            return Err(format!("`steps` must be 1 to 1024, got {}", self.steps));
        }
        Ok(())
    }
}

impl ErodeParams {
    fn erosion(&self) -> ErosionParams {
        ErosionParams {
            steps: self.steps,
            erosion: self.erosion,
            deposition: self.deposition,
            capacity: self.capacity,
            ..ErosionParams::default()
        }
    }
}

/// Erodes `heights` (`n × n`, metres, `spacing` apart) with `cells²·droplets`
/// droplets from `rng`.
fn erode_grid(
    heights: &[f32],
    n: usize,
    spacing: f32,
    cells: usize,
    p: &ErodeParams,
    mut rng: Pcg32,
) -> Vec<f32> {
    let mut h: Vec<f32> = heights.iter().map(|v| v / spacing).collect();
    let count = (p.droplets * (cells * cells) as f32) as usize;
    let last = (n - 2) as f32;
    let starts: Vec<(f32, f32)> = (0..count)
        .map(|_| (rng.next_f32() * last, rng.next_f32() * last))
        .collect();
    erode(&mut h, n, &p.erosion(), starts);
    h.iter().map(|v| v * spacing).collect()
}

type TileKey = [u8; 32];

#[derive(Default)]
struct TileCache {
    tiles: HashMap<TileKey, Arc<Vec<f32>>>,
    order: VecDeque<TileKey>,
}

fn tiles() -> &'static Mutex<TileCache> {
    static CACHE: OnceLock<Mutex<TileCache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Eroded tile `(tx, tz)`: its window's heights, eroded.
fn tile(ctx: &NodeCtx, p: &ErodeParams, tx: i64, tz: i64) -> Result<Arc<Vec<f32>>, String> {
    let upstream = ctx
        .upstream
        .ok_or("a streamed `terrain.erode` needs its input's tiles")?;
    let (origin, n) = tile_window(tx, tz);
    let input = upstream(
        "field",
        Domain {
            origin,
            n: n as u32,
            spacing: p.spacing,
        },
    )?;
    let mut h = blake3::Hasher::new();
    h.update(b"re_proc terrain.erode tile");
    h.update(&ctx.seed.to_le_bytes());
    h.update(&tx.to_le_bytes());
    h.update(&tz.to_le_bytes());
    h.update(&p.spacing.to_le_bytes());
    h.update(&p.droplets.to_le_bytes());
    h.update(&p.steps.to_le_bytes());
    h.update(&p.erosion.to_le_bytes());
    h.update(&p.deposition.to_le_bytes());
    h.update(&p.capacity.to_le_bytes());
    for v in &input.0.samples {
        h.update(&v.to_le_bytes());
    }
    let key = *h.finalize().as_bytes();
    if let Some(t) = tiles()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .tiles
        .get(&key)
    {
        return Ok(t.clone());
    }
    let rng = Pcg32::from_seed(ctx.seed).fork2(tx as i32, tz as i32);
    let eroded = Arc::new(erode_grid(
        &input.0.samples,
        n,
        p.spacing,
        (TILE_CELLS + 2 * APRON) as usize,
        p,
        rng,
    ));
    let mut c = tiles().lock().unwrap_or_else(|e| e.into_inner());
    if c.tiles.insert(key, eroded.clone()).is_none() {
        c.order.push_back(key);
    }
    while c.order.len() > TILE_CACHE {
        if let Some(old) = c.order.pop_front() {
            c.tiles.remove(&old);
        }
    }
    Ok(eroded)
}

/// The eroded height at global erosion-grid sample `(gx, gz)`: the blend of
/// the tiles covering it.
fn eroded_at(
    ctx: &NodeCtx,
    p: &ErodeParams,
    loaded: &mut HashMap<(i64, i64), Arc<Vec<f32>>>,
    gx: i64,
    gz: i64,
) -> Result<f32, String> {
    let mut v = 0.0f32;
    for (tx, tz, w) in tile_blend(gx, gz) {
        let t = match loaded.get(&(tx, tz)) {
            Some(t) => t.clone(),
            None => {
                let t = tile(ctx, p, tx, tz)?;
                loaded.insert((tx, tz), t.clone());
                t
            }
        };
        let (origin, n) = tile_window(tx, tz);
        let (lx, lz) = ((gx - origin[0]) as usize, (gz - origin[1]) as usize);
        v += t[lz * n + lx] * w;
    }
    Ok(v)
}

fn terrain_erode(ctx: &NodeCtx, p: &ErodeParams) -> Result<Output, String> {
    let Some(d) = ctx.domain else {
        // Not streamed: the whole field at once.
        let f = ctx.need_field("field")?;
        let n = f.n() as usize;
        if n < 3 {
            // Too small to carry water: nothing to erode.
            return Ok(Output::Field(f.clone()));
        }
        let eroded = erode_grid(
            &f.0.samples,
            n,
            f.0.spacing,
            n - 1,
            p,
            Pcg32::from_seed(ctx.seed),
        );
        return Ok(Output::Field(Arc::new(f.like(eroded)?)));
    };
    let n = d.n as usize;
    let mut loaded = HashMap::new();
    // Eroded grid samples along each axis, by global index.
    let mut grid: HashMap<(i64, i64), f32> = HashMap::new();
    let mut values = Vec::with_capacity(n * n);
    for j in 0..n as i64 {
        let ez = d.position(d.origin[1] + j) / p.spacing;
        let gz = ez.floor();
        let fz = ez - gz;
        for i in 0..n as i64 {
            let ex = d.position(d.origin[0] + i) / p.spacing;
            let gx = ex.floor();
            let fx = ex - gx;
            let (gx, gz) = (gx as i64, gz as i64);
            let mut at = |x: i64, z: i64| -> Result<f32, String> {
                if let Some(v) = grid.get(&(x, z)) {
                    return Ok(*v);
                }
                let v = eroded_at(ctx, p, &mut loaded, x, z)?;
                grid.insert((x, z), v);
                Ok(v)
            };
            let h00 = at(gx, gz)?;
            let h10 = at(gx + 1, gz)?;
            let h01 = at(gx, gz + 1)?;
            let h11 = at(gx + 1, gz + 1)?;
            values.push(
                h00 * (1.0 - fx) * (1.0 - fz)
                    + h10 * fx * (1.0 - fz)
                    + h01 * (1.0 - fx) * fz
                    + h11 * fx * fz,
            );
        }
    }
    Ok(Output::Field(Arc::new(Field::at(&d, values)?)))
}

const FIELD: &[PortType] = &[PortType::Field];

pub(super) fn register(r: &mut NodeRegistry) {
    r.register(NodeDef::new(
        NodeSpec {
            kind: "terrain.erode",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Region,
            doc: "Hydraulic (droplet) erosion of a height field: water carves valleys and deposits sediment. Streamed, it erodes 256-cell region tiles at `spacing` with an apron and blends them, so the eroded terrain has no seams and every window agrees on shared samples.",
            inputs: &[PortDef {
                name: "field",
                accepts: FIELD,
                required: true,
                doc: "The heights, in metres.",
            }],
            variadic: None,
            output: PortType::Field,
            uses_seed: true,
            file_params: &[],
        },
        terrain_erode,
    ));
}
