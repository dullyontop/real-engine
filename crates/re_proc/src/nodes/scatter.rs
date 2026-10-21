//! `scatter.poisson` and `scatter.grid_jitter`.
//!
//! Both scatter over the input field's square (or over `extent` metres with
//! no field), place each point on the field's surface, and only then drop
//! points outside the height and slope limits. So changing a limit removes
//! points but never moves the others, and point ids stay the same.

use std::f64::consts::TAU;
use std::sync::Arc;

use glam::Vec3;
use re_geom::math::sin_cos;
use re_noise::Pcg32;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{
    NodeCtx, NodeDef, NodeRegistry, NodeSpec, Params, PortDef, check_range, positive, within,
};
use crate::value::{Field, Output, Point, Points, PortType};

/// Height and slope limits shared by the scatter nodes and `points.filter`
/// (each of their parameter structs has these four fields).
#[derive(Clone, Copy, Debug, Default)]
pub struct Limits {
    /// Drop points below this height (metres).
    pub height_min: Option<f32>,
    /// Drop points above this height (metres).
    pub height_max: Option<f32>,
    /// Drop points flatter than this slope (rise over run: 0 is flat, 1 is 45°).
    pub slope_min: Option<f32>,
    /// Drop points steeper than this slope (rise over run).
    pub slope_max: Option<f32>,
}

/// Implements `limits()` for a parameter struct with the four limit fields.
macro_rules! limits_of {
    ($t:ty) => {
        impl $t {
            pub(crate) fn limits(&self) -> Limits {
                Limits {
                    height_min: self.height_min,
                    height_max: self.height_max,
                    slope_min: self.slope_min,
                    slope_max: self.slope_max,
                }
            }
        }
    };
}
pub(crate) use limits_of;

limits_of!(PoissonParams);
limits_of!(GridParams);

impl Limits {
    pub(crate) fn validate(&self) -> Result<(), String> {
        check_range("height", self.height_min, self.height_max)?;
        check_range("slope", self.slope_min, self.slope_max)
    }

    pub(crate) fn keeps(&self, p: &Point) -> bool {
        let h = p.position.y;
        let s = p.slope();
        self.height_min.is_none_or(|m| h >= m)
            && self.height_max.is_none_or(|m| h <= m)
            && self.slope_min.is_none_or(|m| s >= m)
            && self.slope_max.is_none_or(|m| s <= m)
    }
}

/// Places a point at `(x, z)` on the field (or at height 0 without one).
fn on_surface(field: Option<&Field>, id: u32, x: f32, z: f32) -> Option<Point> {
    let (y, normal) = match field {
        Some(f) => (f.0.height_at(x, z)?, f.0.normal_at(x, z)?),
        None => (0.0, Vec3::Y),
    };
    Some(Point {
        id,
        position: Vec3::new(x, y, z),
        normal,
        tag: String::new(),
    })
}

fn half_extent(field: Option<&Field>, extent: f32) -> f32 {
    field.map_or(extent * 0.5, Field::half_extent)
}

/// Parameters of `scatter.poisson`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct PoissonParams {
    /// The smallest distance between two points, in metres.
    pub radius: f32,
    /// Candidates tried around each point before it is retired (Bridson's `k`).
    pub attempts: u32,
    /// Stop after this many points.
    pub max_points: u32,
    /// The side of the square to fill when no field is connected, in metres.
    pub extent: f32,
    /// Drop points below this height (metres).
    pub height_min: Option<f32>,
    /// Drop points above this height (metres).
    pub height_max: Option<f32>,
    /// Drop points flatter than this slope (rise over run: 0 is flat, 1 is 45°).
    pub slope_min: Option<f32>,
    /// Drop points steeper than this slope (rise over run).
    pub slope_max: Option<f32>,
}

impl Default for PoissonParams {
    fn default() -> Self {
        PoissonParams {
            radius: 4.0,
            attempts: 30,
            max_points: 100_000,
            extent: 64.0,
            height_min: None,
            height_max: None,
            slope_min: None,
            slope_max: None,
        }
    }
}

impl Params for PoissonParams {
    fn validate(&self) -> Result<(), String> {
        positive("radius", self.radius)?;
        positive("extent", self.extent)?;
        if !(1..=100).contains(&self.attempts) {
            return Err(format!(
                "`attempts` must be 1 to 100, got {}",
                self.attempts
            ));
        }
        if !(1..=1_000_000).contains(&self.max_points) {
            return Err(format!(
                "`max_points` must be 1 to 1000000, got {}",
                self.max_points
            ));
        }
        self.limits().validate()
    }
}

/// Bridson's Poisson-disc sampling over `[-half, half]²`, single-threaded
/// and driven by one PCG32 stream, so the result depends only on the seed
/// and the parameters.
fn poisson_disc(
    rng: &mut Pcg32,
    half: f32,
    radius: f32,
    attempts: u32,
    max: u32,
) -> Vec<(f32, f32)> {
    let cell = radius / std::f32::consts::SQRT_2;
    let side = 2.0 * half;
    let cols = ((side / cell) as usize + 1).max(1);
    let mut grid: Vec<u32> = vec![u32::MAX; cols * cols];
    let cell_of = |x: f32, z: f32| {
        let cx = (((x + half) / cell) as usize).min(cols - 1);
        let cz = (((z + half) / cell) as usize).min(cols - 1);
        (cx, cz)
    };
    let mut points: Vec<(f32, f32)> = Vec::new();
    let mut active: Vec<u32> = Vec::new();
    let first = (rng.range_f32(-half, half), rng.range_f32(-half, half));
    let (cx, cz) = cell_of(first.0, first.1);
    grid[cz * cols + cx] = 0;
    points.push(first);
    active.push(0);
    let r2 = radius * radius;
    while !active.is_empty() && (points.len() as u32) < max {
        let slot = rng.range_u32(0, active.len() as u32) as usize;
        let (px, pz) = points[active[slot] as usize];
        let mut placed = false;
        for _ in 0..attempts {
            let (s, c) = sin_cos(TAU * f64::from(rng.next_f32()));
            // Uniform by area in the annulus [r, 2r].
            let d = (r2 + 3.0 * r2 * rng.next_f32()).sqrt();
            let (x, z) = (px + c as f32 * d, pz + s as f32 * d);
            if !(x >= -half && x <= half && z >= -half && z <= half) {
                continue;
            }
            let (cx, cz) = cell_of(x, z);
            let mut free = true;
            'scan: for gz in cz.saturating_sub(2)..(cz + 3).min(cols) {
                for gx in cx.saturating_sub(2)..(cx + 3).min(cols) {
                    let other = grid[gz * cols + gx];
                    if other != u32::MAX {
                        let (ox, oz) = points[other as usize];
                        let (dx, dz) = (ox - x, oz - z);
                        if dx * dx + dz * dz < r2 {
                            free = false;
                            break 'scan;
                        }
                    }
                }
            }
            if free {
                let id = points.len() as u32;
                grid[cz * cols + cx] = id;
                points.push((x, z));
                active.push(id);
                placed = true;
                break;
            }
        }
        if !placed {
            active.swap_remove(slot);
        }
    }
    points
}

fn poisson(ctx: &NodeCtx, p: &PoissonParams) -> Result<Output, String> {
    let field = ctx.field("field").map(|f| &**f);
    let half = half_extent(field, p.extent);
    let cells = (2.0 * half / p.radius) as f64;
    if cells * cells > 4.0e6 {
        return Err(format!(
            "`radius` {} is too small for a {} m square (at most about 4 million points)",
            p.radius,
            2.0 * half
        ));
    }
    let mut rng = Pcg32::from_seed(ctx.seed);
    let xz = poisson_disc(&mut rng, half, p.radius, p.attempts, p.max_points);
    let points = xz
        .into_iter()
        .enumerate()
        .filter_map(|(i, (x, z))| on_surface(field, i as u32, x, z))
        .filter(|pt| p.limits().keeps(pt))
        .collect();
    Ok(Output::Points(Arc::new(Points { points })))
}

/// Parameters of `scatter.grid_jitter`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct GridParams {
    /// Distance between grid cells, in metres.
    pub spacing: f32,
    /// How far each point may move from its cell's centre, as a fraction of
    /// the spacing (0 is a regular grid, 1 fills the cell).
    pub jitter: f32,
    /// The side of the square to fill when no field is connected, in metres.
    pub extent: f32,
    /// Drop points below this height (metres).
    pub height_min: Option<f32>,
    /// Drop points above this height (metres).
    pub height_max: Option<f32>,
    /// Drop points flatter than this slope (rise over run: 0 is flat, 1 is 45°).
    pub slope_min: Option<f32>,
    /// Drop points steeper than this slope (rise over run).
    pub slope_max: Option<f32>,
}

impl Default for GridParams {
    fn default() -> Self {
        GridParams {
            spacing: 4.0,
            jitter: 0.5,
            extent: 64.0,
            height_min: None,
            height_max: None,
            slope_min: None,
            slope_max: None,
        }
    }
}

impl Params for GridParams {
    fn validate(&self) -> Result<(), String> {
        positive("spacing", self.spacing)?;
        positive("extent", self.extent)?;
        within("jitter", self.jitter, 0.0, 1.0)?;
        self.limits().validate()
    }
}

fn grid_jitter(ctx: &NodeCtx, p: &GridParams) -> Result<Output, String> {
    let field = ctx.field("field").map(|f| &**f);
    let half = half_extent(field, p.extent);
    let cols = (2.0 * half / p.spacing) as u32;
    if cols > 4096 {
        return Err(format!(
            "`spacing` {} is too small for a {} m square (at most 4096 cells per side)",
            p.spacing,
            2.0 * half
        ));
    }
    // Centre the grid in the square.
    let start = -(cols as f32) * p.spacing * 0.5 + p.spacing * 0.5;
    let base = Pcg32::from_seed(ctx.seed);
    let mut points = Vec::new();
    for iz in 0..cols {
        for ix in 0..cols {
            let mut rng = base.fork2(ix as i32, iz as i32);
            let jx = (rng.next_f32() - 0.5) * p.jitter * p.spacing;
            let jz = (rng.next_f32() - 0.5) * p.jitter * p.spacing;
            let x = start + ix as f32 * p.spacing + jx;
            let z = start + iz as f32 * p.spacing + jz;
            // The id only depends on the cell, so it survives size changes.
            let id = iz * 65_536 + ix;
            if let Some(pt) = on_surface(field, id, x, z)
                && p.limits().keeps(&pt)
            {
                points.push(pt);
            }
        }
    }
    Ok(Output::Points(Arc::new(Points { points })))
}

const FIELD: &[PortType] = &[PortType::Field];
const ON_FIELD: &[PortDef] = &[PortDef {
    name: "field",
    accepts: FIELD,
    required: false,
    doc: "The surface to scatter on (heights in metres); without it, a flat square of `extent` metres.",
}];

pub(super) fn register(r: &mut NodeRegistry) {
    r.register(NodeDef::new(
        NodeSpec {
            kind: "scatter.poisson",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Whole,
            doc: "Points at least `radius` apart (Poisson disc), on a field's surface, filtered by height and slope.",
            inputs: ON_FIELD,
            variadic: None,
            output: PortType::Points,
            uses_seed: true,
            file_params: &[],
        },
        poisson,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "scatter.grid_jitter",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Whole,
            doc: "One point per grid cell, moved randomly by up to `jitter` of the spacing, on a field's surface, filtered by height and slope.",
            inputs: ON_FIELD,
            variadic: None,
            output: PortType::Points,
            uses_seed: true,
            file_params: &[],
        },
        grid_jitter,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisson_respects_radius() {
        let mut rng = Pcg32::from_seed(7);
        let pts = poisson_disc(&mut rng, 20.0, 3.0, 30, 10_000);
        assert!(pts.len() > 50, "{}", pts.len());
        for (i, a) in pts.iter().enumerate() {
            assert!(a.0.abs() <= 20.0 && a.1.abs() <= 20.0);
            for b in &pts[i + 1..] {
                let d2 = (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2);
                assert!(d2 >= 9.0 - 1e-3);
            }
        }
    }
}
