//! `shape.polygon`, `shape.boolean`, `shape.offset`, `shape.mesh`,
//! `field.from_shapes` and `points.mask` (ADR-0043 §5).
//!
//! Shapes are polygons with holes in the source's XZ plane, in metres. Every
//! node that outputs shapes outputs them simplified (no overlaps, snapped to
//! the 1 mm grid of `re_geom::polygon`).

use std::sync::Arc;

use re_geom::polygon::{self, Shape};
use schemars::JsonSchema;
use serde::Deserialize;

use super::{
    NodeCtx, NodeDef, NodeError, NodeRegistry, NodeSpec, Params, PortDef, Streaming, within,
};
use crate::value::{Field, MeshValue, Output, Points, PortType, Shapes};

/// The largest distance (offsets, falloffs) a shape node takes, in metres.
const MAX_DISTANCE: f64 = 100_000.0;

/// Parameters of `shape.polygon`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct PolygonParams {
    /// The outer contour, `[x, z]` in metres (3 points or more, either
    /// direction).
    pub points: Vec<[f64; 2]>,
    /// Holes inside it, each 3 points or more.
    pub holes: Vec<Vec<[f64; 2]>>,
}

impl PolygonParams {
    fn shapes(&self) -> Shapes {
        Shapes {
            shapes: vec![Shape {
                outer: self.points.clone(),
                holes: self.holes.clone(),
            }],
        }
    }
}

impl Params for PolygonParams {
    fn validate(&self) -> Result<(), String> {
        self.shapes().validate().map_err(|e| e.to_string())
    }
}

fn polygon_node(_: &NodeCtx, p: &PolygonParams) -> Result<Output, NodeError> {
    Ok(Output::Shapes(Arc::new(p.shapes().simplified()?)))
}

/// A boolean operation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BoolOp {
    /// Area in `a` or `b`.
    #[default]
    Union,
    /// Area in both.
    Intersect,
    /// Area in `a` and not in `b`.
    Difference,
    /// Area in exactly one of them.
    Xor,
}

/// Parameters of `shape.boolean`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct BooleanParams {
    /// `union`, `intersect`, `difference` (`a` minus `b`) or `xor`.
    pub op: BoolOp,
}

impl Params for BooleanParams {}

fn boolean(ctx: &NodeCtx, p: &BooleanParams) -> Result<Output, NodeError> {
    let a = ctx.need_shapes("a")?;
    let b = ctx.shapes("b").cloned().unwrap_or_default();
    let op = match p.op {
        BoolOp::Union => polygon::BoolOp::Union,
        BoolOp::Intersect => polygon::BoolOp::Intersect,
        BoolOp::Difference => polygon::BoolOp::Difference,
        BoolOp::Xor => polygon::BoolOp::Xor,
    };
    Ok(Output::Shapes(Arc::new(a.boolean(&b, op)?)))
}

/// How an offset turns corners.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Join {
    /// An arc.
    #[default]
    Round,
    /// A sharp corner (bevelled when very sharp).
    Miter,
    /// The corner cut off.
    Bevel,
}

/// Parameters of `shape.offset`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct OffsetParams {
    /// Metres to grow the shapes by (negative shrinks them).
    pub distance: f64,
    /// `round`, `miter` or `bevel`.
    pub join: Join,
}

impl Params for OffsetParams {
    fn validate(&self) -> Result<(), String> {
        if self.distance.is_finite() && self.distance.abs() <= MAX_DISTANCE {
            Ok(())
        } else {
            Err(format!(
                "`distance` must be between -{MAX_DISTANCE} and {MAX_DISTANCE}, got {}",
                self.distance
            ))
        }
    }
}

fn offset(ctx: &NodeCtx, p: &OffsetParams) -> Result<Output, NodeError> {
    let s = ctx.need_shapes("shapes")?;
    let join = match p.join {
        Join::Round => polygon::Join::Round,
        Join::Miter => polygon::Join::Miter,
        Join::Bevel => polygon::Join::Bevel,
    };
    Ok(Output::Shapes(Arc::new(s.offset(p.distance, join)?)))
}

/// Parameters of `shape.mesh`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ShapeMeshParams {
    /// The height of the flat mesh, in metres.
    pub height: f32,
    /// A material file (`materials/plaza.material.json`), or empty.
    pub material: String,
}

impl Params for ShapeMeshParams {
    fn validate(&self) -> Result<(), String> {
        within("height", self.height, -1.0e6, 1.0e6)
    }
}

fn shape_mesh(ctx: &NodeCtx, p: &ShapeMeshParams) -> Result<Output, NodeError> {
    let s = ctx.need_shapes("shapes")?;
    let mesh = match ctx.domain {
        // Streamed: the shapes cut to the cell, local to its corner.
        Some(d) => {
            let (min, max) = (d.min().map(f64::from), d.max().map(f64::from));
            polygon::triangulate_at(&s.clip(min, max)?, p.height, min)?
        }
        None => polygon::triangulate(s, p.height)?,
    };
    Ok(Output::Mesh(Arc::new(MeshValue {
        mesh,
        material: p.material.clone(),
        asset: ctx.key,
    })))
}

/// Parameters of `field.from_shapes`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct FromShapesParams {
    /// Samples per side when no `field` is connected (2 to 4097).
    pub size: u32,
    /// Metres between samples when no `field` is connected.
    pub cell: f32,
    /// Outside the shapes, the mask falls from 1 to 0 over this many metres
    /// (0 for a hard edge).
    pub falloff: f32,
    /// Gives 0 inside and 1 outside instead.
    pub invert: bool,
}

impl Default for FromShapesParams {
    fn default() -> Self {
        FromShapesParams {
            size: 129,
            cell: 1.0,
            falloff: 0.0,
            invert: false,
        }
    }
}

impl Params for FromShapesParams {
    fn validate(&self) -> Result<(), String> {
        if !(2..=4097).contains(&self.size) {
            return Err(format!("`size` must be 2 to 4097, got {}", self.size));
        }
        super::positive("cell", self.cell)?;
        within("falloff", self.falloff, 0.0, MAX_DISTANCE as f32)
    }
}

/// An edge of a contour in X and Z.
type Edge = ([f64; 2], [f64; 2]);

fn edges_of(shapes: &Shapes) -> Vec<Edge> {
    let mut edges = Vec::with_capacity(shapes.point_count());
    for c in shapes.contours() {
        for i in 0..c.len() {
            edges.push((c[i], c[(i + 1) % c.len()]));
        }
    }
    edges
}

fn from_shapes(ctx: &NodeCtx, p: &FromShapesParams) -> Result<Output, NodeError> {
    let shapes = ctx.need_shapes("shapes")?;
    let grid = match (ctx.field("field"), ctx.domain) {
        (Some(f), _) => f.like(vec![0.0; f.0.samples.len()])?,
        // Streamed: the window's samples.
        (None, Some(d)) => Field::at(&d, vec![0.0; (d.n as usize).pow(2)])?,
        (None, None) => {
            let n = p.size;
            Field::new(n, p.cell, vec![0.0; (n * n) as usize])?
        }
    };
    let (n, spacing, origin) = (grid.0.n as usize, f64::from(grid.0.spacing), grid.0.offset);
    let edges = edges_of(shapes);
    let falloff = f64::from(p.falloff);
    let mut values = vec![0.0f32; n * n];
    ctx.par_rows(&mut values, n, |iz, row| {
        let z = f64::from(origin.z) + iz as f64 * spacing;
        // Even-odd crossings of this row, sorted by x.
        let mut xs: Vec<f64> = edges
            .iter()
            .filter(|(a, b)| (a[1] > z) != (b[1] > z))
            .map(|(a, b)| a[0] + (z - a[1]) * (b[0] - a[0]) / (b[1] - a[1]))
            .collect();
        xs.sort_by(f64::total_cmp);
        let near: Vec<&Edge> = if falloff > 0.0 {
            edges
                .iter()
                .filter(|(a, b)| a[1].min(b[1]) - falloff <= z && a[1].max(b[1]) + falloff >= z)
                .collect()
        } else {
            Vec::new()
        };
        let mut k = 0;
        for (ix, v) in row.iter_mut().enumerate() {
            let x = f64::from(origin.x) + ix as f64 * spacing;
            while k < xs.len() && xs[k] <= x {
                k += 1;
            }
            let inside = k % 2 == 1;
            let mask = if inside {
                1.0
            } else if falloff > 0.0 {
                let mut best = falloff * falloff;
                for (a, b) in &near {
                    best = best.min(dist2_to_segment([x, z], *a, *b));
                }
                let t = 1.0 - best.sqrt() / falloff;
                // Smoothstep, so the mask meets the shape's edge flat.
                t * t * (3.0 - 2.0 * t)
            } else {
                0.0
            };
            *v = if p.invert { 1.0 - mask } else { mask } as f32;
        }
    });
    Ok(Output::Field(Arc::new(grid.like(values)?)))
}

/// The squared distance from `p` to segment `a`–`b`.
pub(super) fn dist2_to_segment(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let (dx, dz) = (b[0] - a[0], b[1] - a[1]);
    let len2 = dx * dx + dz * dz;
    let t = if len2 > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dz) / len2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (x, z) = (a[0] + dx * t - p[0], a[1] + dz * t - p[1]);
    x * x + z * z
}

/// Which points `points.mask` keeps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Keep {
    /// Points inside the shapes.
    #[default]
    Inside,
    /// Points outside the shapes.
    Outside,
}

/// Parameters of `points.mask`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct MaskParams {
    /// `inside` or `outside`.
    pub keep: Keep,
}

impl Params for MaskParams {}

fn mask(ctx: &NodeCtx, p: &MaskParams) -> Result<Output, String> {
    let input = ctx.need_points("points")?;
    let shapes = ctx.need_shapes("shapes")?;
    let want = p.keep == Keep::Inside;
    let points = input
        .points
        .iter()
        .filter(|pt| shapes.contains([f64::from(pt.position.x), f64::from(pt.position.z)]) == want)
        .cloned()
        .collect();
    Ok(Output::Points(Arc::new(Points { points })))
}

const SHAPES: &[PortType] = &[PortType::Shapes];

pub(super) fn register(r: &mut NodeRegistry) {
    let spec = |kind, streaming, doc, inputs, output| NodeSpec {
        kind,
        version: 1,
        oldest: 1,
        migrations: &[],
        streaming,
        doc,
        inputs,
        variadic: None,
        output,
        uses_seed: false,
        file_params: &[],
    };
    r.register(NodeDef::new(
        spec(
            "shape.polygon",
            Streaming::Global,
            "A polygon with holes from its points, `[x, z]` in metres.",
            &[],
            PortType::Shapes,
        ),
        polygon_node,
    ));
    r.register(NodeDef::new(
        spec(
            "shape.boolean",
            Streaming::Global,
            "Union, intersection, difference (a minus b) or xor of two sets of shapes, on a 1 mm grid.",
            &[
                PortDef {
                    name: "a",
                    accepts: SHAPES,
                    required: true,
                    doc: "The first shapes.",
                },
                PortDef {
                    name: "b",
                    accepts: SHAPES,
                    required: false,
                    doc: "The second shapes (none when not connected).",
                },
            ],
            PortType::Shapes,
        ),
        boolean,
    ));
    r.register(NodeDef::new(
        spec(
            "shape.offset",
            Streaming::Global,
            "Grows (or, with a negative distance, shrinks) shapes by a distance in metres.",
            &[PortDef {
                name: "shapes",
                accepts: SHAPES,
                required: true,
                doc: "The shapes to offset.",
            }],
            PortType::Shapes,
        ),
        offset,
    ));
    r.register(NodeDef::new(
        spec(
            "shape.mesh",
            Streaming::Cell,
            "A flat mesh of the shapes at a height, facing up, with UVs in metres. Streamed: per cell, the shapes cut to the cell, local to its corner.",
            &[PortDef {
                name: "shapes",
                accepts: SHAPES,
                required: true,
                doc: "The shapes to triangulate.",
            }],
            PortType::Mesh,
        ),
        shape_mesh,
    ));
    r.register(NodeDef::new(
        spec(
            "field.from_shapes",
            Streaming::Pointwise,
            "A mask field: 1 inside the shapes, falling to 0 over `falloff` metres outside. On the connected field's grid, or a centred grid of `size` samples. Streamed: pointwise, over the window.",
            &[
                PortDef {
                    name: "shapes",
                    accepts: SHAPES,
                    required: true,
                    doc: "The shapes to rasterise.",
                },
                PortDef {
                    name: "field",
                    accepts: &[PortType::Field],
                    required: false,
                    doc: "A field whose grid (size, spacing, position) the mask uses.",
                },
            ],
            PortType::Field,
        ),
        from_shapes,
    ));
    r.register(NodeDef::new(
        spec(
            "points.mask",
            Streaming::Cell,
            "Keeps the points inside (or outside) the shapes, in X and Z.",
            &[
                PortDef {
                    name: "points",
                    accepts: &[PortType::Points],
                    required: true,
                    doc: "The points to filter.",
                },
                PortDef {
                    name: "shapes",
                    accepts: SHAPES,
                    required: true,
                    doc: "The shapes to test against.",
                },
            ],
            PortType::Points,
        ),
        mask,
    ));
}
