//! `path.spline`, `road.mesh`, `road.footprint` and `terrain.conform`
//! (ADR-0043 §5).

use std::sync::Arc;

use re_geom::polygon::MAX_COORD;
use re_geom::road::{self, ControlPoint, Network, RoadStyle};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::shape::dist2_to_segment;
use super::{
    NodeCtx, NodeDef, NodeError, NodeRegistry, NodeSpec, Params, PortDef, Streaming, within,
};
use crate::value::{MeshValue, Output, PortType, RoadPath};

/// The largest width, radius or distance a road node takes, in metres.
const MAX_DISTANCE: f64 = 10_000.0;

fn distance(name: &str, v: f64, min: f64) -> Result<(), String> {
    if v.is_finite() && v >= min && v <= MAX_DISTANCE {
        Ok(())
    } else {
        Err(format!(
            "`{name}` must be between {min} and {MAX_DISTANCE}, got {v}"
        ))
    }
}

/// A control point of `path.spline`: `[x, y, z]`, or an object that can
/// also set the width and bank there.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum PointDoc {
    /// A position `[x, y, z]` in metres.
    At([f64; 3]),
    /// A position with its own width or bank.
    With {
        /// Position `[x, y, z]` in metres.
        position: [f64; 3],
        /// The road's width here (default: the node's `width`).
        #[serde(default)]
        width: Option<f64>,
        /// The bank here in degrees (default: the node's `bank`).
        #[serde(default)]
        bank: Option<f64>,
    },
}

/// Parameters of `path.spline`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct SplineParams {
    /// The control points (2 or more; 3 or more when `closed`). With a
    /// `field` connected, each point's `y` is added to the field's height
    /// under it.
    pub points: Vec<PointDoc>,
    /// The road's width in metres, where a point does not set one.
    pub width: f64,
    /// The bank in degrees, where a point does not set one; positive raises
    /// the left edge.
    pub bank: f64,
    /// Metres between samples along the centreline.
    pub spacing: f64,
    /// Joins the last point back to the first.
    pub closed: bool,
}

impl Default for SplineParams {
    fn default() -> Self {
        SplineParams {
            points: Vec::new(),
            width: 6.0,
            bank: 0.0,
            spacing: 1.0,
            closed: false,
        }
    }
}

impl SplineParams {
    fn control_points(&self) -> Vec<ControlPoint> {
        self.points
            .iter()
            .map(|p| match p {
                PointDoc::At(position) => ControlPoint {
                    position: *position,
                    width: self.width,
                    bank: self.bank,
                },
                PointDoc::With {
                    position,
                    width,
                    bank,
                } => ControlPoint {
                    position: *position,
                    width: width.unwrap_or(self.width),
                    bank: bank.unwrap_or(self.bank),
                },
            })
            .collect()
    }
}

impl Params for SplineParams {
    fn validate(&self) -> Result<(), String> {
        distance("width", self.width, 0.01)?;
        distance("spacing", self.spacing, 0.05)?;
        if !self.bank.is_finite() || self.bank.abs() > 60.0 {
            return Err(format!(
                "`bank` must be between -60 and 60, got {}",
                self.bank
            ));
        }
        let min = if self.closed { 3 } else { 2 };
        if self.points.len() < min {
            return Err(format!(
                "`points` needs at least {min} control points, got {}",
                self.points.len()
            ));
        }
        for (i, c) in self.control_points().iter().enumerate() {
            distance(&format!("points[{i}].width"), c.width, 0.01)?;
            if !c.bank.is_finite() || c.bank.abs() > 60.0 {
                return Err(format!(
                    "`points[{i}].bank` must be between -60 and 60, got {}",
                    c.bank
                ));
            }
        }
        Ok(())
    }
}

fn spline(ctx: &NodeCtx, p: &SplineParams) -> Result<Output, NodeError> {
    let mut points = p.control_points();
    // Drape the control points; the spline then runs smoothly between them
    // rather than following every bump.
    if let Some(sample) = ctx.sample {
        // Streamed: the field on the source's grid, the same for every cell.
        // A point out of range stays as it is, and `road::spline` says why.
        let usable = |c: &ControlPoint| {
            c.position
                .iter()
                .all(|v| v.is_finite() && v.abs() <= MAX_COORD)
        };
        let at: Vec<[f64; 2]> = points
            .iter()
            .filter(|c| usable(c))
            .map(|c| [c.position[0], c.position[2]])
            .collect();
        if let Some(heights) = sample("field", &at)? {
            for (c, h) in points.iter_mut().filter(|c| usable(c)).zip(heights) {
                c.position[1] += h;
            }
        }
    } else if let Some(field) = ctx.field("field") {
        for c in &mut points {
            let [x, _, z] = c.position;
            if let Some(h) = field.0.height_at(x as f32, z as f32) {
                c.position[1] += f64::from(h);
            }
        }
    }
    let path = road::spline(&points, p.spacing, p.closed)?;
    Ok(Output::Path(Arc::new(path)))
}

/// Parameters of `road.mesh`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct RoadMeshParams {
    /// Roads are cut back this far (metres) around a junction, and the
    /// junction surface fills the disc (0 for no junctions).
    pub junction_radius: f64,
    /// A road end this close (metres) to another road makes a junction.
    pub snap: f64,
    /// Over this distance (metres) before a junction, the bank fades to 0
    /// and the road eases to the junction's height.
    pub bank_fade: f64,
    /// A material file (`materials/asphalt.material.json`), or empty.
    pub material: String,
}

impl Default for RoadMeshParams {
    fn default() -> Self {
        RoadMeshParams {
            junction_radius: 8.0,
            snap: 2.0,
            bank_fade: 10.0,
            material: String::new(),
        }
    }
}

impl Params for RoadMeshParams {
    fn validate(&self) -> Result<(), String> {
        distance("junction_radius", self.junction_radius, 0.0)?;
        distance("snap", self.snap, 0.0)?;
        distance("bank_fade", self.bank_fade, 0.0)
    }
}

fn paths(ctx: &NodeCtx) -> Result<Vec<road::Path>, String> {
    let paths: Vec<road::Path> = ctx.paths().into_iter().map(|p| (**p).clone()).collect();
    if paths.is_empty() {
        return Err("no path is connected".into());
    }
    Ok(paths)
}

fn road_mesh(ctx: &NodeCtx, p: &RoadMeshParams) -> Result<Output, NodeError> {
    let style = RoadStyle {
        junction_radius: p.junction_radius,
        snap: p.snap,
        bank_fade: p.bank_fade,
    };
    let (mesh, junctions) = match ctx.domain {
        // Streamed: the cell's part of the network every cell shares, local
        // to the cell's corner.
        Some(d) => {
            let network = shared_network(ctx, &ctx.paths(), &style)?;
            let (min, max) = (d.min().map(f64::from), d.max().map(f64::from));
            (network.mesh_in(min, max)?, network.junctions().len())
        }
        None => {
            let road = road::road_mesh(&paths(ctx)?, &style)?;
            (road.mesh, road.junctions.len())
        }
    };
    if junctions > 0 {
        ctx.log(&format!("{junctions} junctions"));
    }
    Ok(Output::Mesh(Arc::new(MeshValue {
        mesh,
        material: p.material.clone(),
        asset: ctx.key,
    })))
}

/// The network of `paths`, joined once for all the cells that ask and kept
/// in the evaluator's memory cache ([`NodeCtx::shared`]).
fn shared_network(
    ctx: &NodeCtx,
    paths: &[&Arc<RoadPath>],
    style: &RoadStyle,
) -> Result<Arc<Network>, NodeError> {
    if paths.is_empty() {
        return Err("no path is connected".to_string().into());
    }
    let mut h = blake3::Hasher::new();
    h.update(b"re_proc road.mesh network v1");
    for v in [style.junction_radius, style.snap, style.bank_fade] {
        h.update(&v.to_le_bytes());
    }
    for p in paths {
        h.update(&[u8::from(p.closed)]);
        h.update(&(p.samples.len() as u64).to_le_bytes());
        for s in &p.samples {
            let [x, y, z] = s.position;
            for v in [x, y, z, s.width, s.bank, s.distance] {
                h.update(&v.to_le_bytes());
            }
        }
    }
    ctx.shared(*h.finalize().as_bytes(), || {
        let owned: Vec<road::Path> = paths.iter().map(|p| (***p).clone()).collect();
        let network = road::network(&owned, style)?;
        let bytes = network.bytes();
        Ok((network, bytes))
    })
}

/// Parameters of `road.footprint`.
#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct FootprintParams {
    /// Metres to grow the footprint by on each side (for a verge, or to
    /// keep scattered props off the road).
    pub margin: f64,
}

impl Params for FootprintParams {
    fn validate(&self) -> Result<(), String> {
        distance("margin", self.margin, 0.0)
    }
}

fn footprint(ctx: &NodeCtx, p: &FootprintParams) -> Result<Output, NodeError> {
    let shapes = road::footprint(&paths(ctx)?, p.margin)?;
    Ok(Output::Shapes(Arc::new(shapes)))
}

/// Parameters of `terrain.conform`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ConformParams {
    /// Flat ground kept on each side beyond the road's edge, in metres.
    pub shoulder: f32,
    /// Past the shoulder, the ground blends back to the field over this many
    /// metres.
    pub blend: f32,
    /// The flattened ground sits this far below the road's centreline, so
    /// the road surface is drawn above it.
    pub depth: f32,
}

impl Default for ConformParams {
    fn default() -> Self {
        ConformParams {
            shoulder: 0.5,
            blend: 6.0,
            depth: 0.05,
        }
    }
}

impl Params for ConformParams {
    fn validate(&self) -> Result<(), String> {
        within("shoulder", self.shoulder, 0.0, MAX_DISTANCE as f32)?;
        within("blend", self.blend, 0.0, MAX_DISTANCE as f32)?;
        within("depth", self.depth, -100.0, 100.0)
    }
}

/// A road segment for `terrain.conform`: ends in X and Z, heights, half
/// widths.
struct Seg {
    a: [f64; 2],
    b: [f64; 2],
    y: [f64; 2],
    half: [f64; 2],
}

fn conform(ctx: &NodeCtx, p: &ConformParams) -> Result<Output, NodeError> {
    let field = ctx.need_field("field")?;
    let segs: Vec<Seg> = ctx
        .paths()
        .iter()
        .flat_map(|path| {
            path.segments().map(|(a, b)| Seg {
                a: [a.position[0], a.position[2]],
                b: [b.position[0], b.position[2]],
                y: [a.position[1], b.position[1]],
                half: [a.width * 0.5, b.width * 0.5],
            })
        })
        .collect();
    let (shoulder, blend) = (f64::from(p.shoulder), f64::from(p.blend));
    let widest = segs
        .iter()
        .map(|s| s.half[0].max(s.half[1]))
        .fold(0.0, f64::max);
    let reach = widest + shoulder + blend;
    // Segments by grid cell of side `reach`, so each sample only looks at
    // the segments in its own and the neighbouring cells.
    let hf = &field.0;
    let (n, spacing) = (hf.n as usize, f64::from(hf.spacing));
    let (ox, oz) = (f64::from(hf.offset.x), f64::from(hf.offset.z));
    let cell = reach.max(spacing);
    let cells = ((n as f64 - 1.0) * spacing / cell).floor() as usize + 1;
    let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); cells * cells];
    let index = |v: f64, o: f64| ((v - o) / cell).floor();
    for (i, s) in segs.iter().enumerate() {
        let (x0, x1) = (s.a[0].min(s.b[0]) - reach, s.a[0].max(s.b[0]) + reach);
        let (z0, z1) = (s.a[1].min(s.b[1]) - reach, s.a[1].max(s.b[1]) + reach);
        let last = (cells - 1) as f64;
        let (cx0, cx1) = (index(x0, ox).max(0.0), index(x1, ox).min(last));
        let (cz0, cz1) = (index(z0, oz).max(0.0), index(z1, oz).min(last));
        if cx0 > cx1 || cz0 > cz1 {
            continue;
        }
        for cz in cz0 as usize..=cz1 as usize {
            for cx in cx0 as usize..=cx1 as usize {
                buckets[cz * cells + cx].push(i);
            }
        }
    }
    let base = f64::from(hf.offset.y);
    let mut values = hf.samples.clone();
    ctx.par_rows(&mut values, n, |iz, row| {
        let z = oz + iz as f64 * spacing;
        let cz = (index(z, oz) as usize).min(cells - 1);
        for (ix, v) in row.iter_mut().enumerate() {
            let x = ox + ix as f64 * spacing;
            let cx = (index(x, ox) as usize).min(cells - 1);
            // The segment whose edge is nearest: (distance past the edge,
            // road height there).
            let mut best: Option<(f64, f64)> = None;
            for &i in &buckets[cz * cells + cx] {
                let s = &segs[i];
                let d2 = dist2_to_segment([x, z], s.a, s.b);
                let (dx, dz) = (s.b[0] - s.a[0], s.b[1] - s.a[1]);
                let len2 = dx * dx + dz * dz;
                let t = if len2 > 0.0 {
                    (((x - s.a[0]) * dx + (z - s.a[1]) * dz) / len2).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let half = s.half[0] + (s.half[1] - s.half[0]) * t;
                let past = d2.sqrt() - half;
                if past <= shoulder + blend && best.is_none_or(|(b, _)| past < b) {
                    best = Some((past, s.y[0] + (s.y[1] - s.y[0]) * t));
                }
            }
            let Some((past, y)) = best else { continue };
            let target = y - f64::from(p.depth) - base;
            let w = if past <= shoulder || blend <= 0.0 {
                0.0
            } else {
                let t = ((past - shoulder) / blend).clamp(0.0, 1.0);
                t * t * (3.0 - 2.0 * t)
            };
            *v = (target + (f64::from(*v) - target) * w) as f32;
        }
    });
    Ok(Output::Field(Arc::new(field.like(values)?)))
}

const PATHS: &[PortType] = &[PortType::Path];

pub(super) fn register(r: &mut NodeRegistry) {
    let spec = |kind, streaming, doc, inputs, variadic, output| NodeSpec {
        kind,
        version: 1,
        oldest: 1,
        migrations: &[],
        streaming,
        doc,
        inputs,
        variadic,
        output,
        uses_seed: false,
        file_params: &[],
    };
    r.register(NodeDef::new(
        spec(
            "path.spline",
            Streaming::Global,
            "A road centreline: a centripetal Catmull-Rom spline through control points, sampled every `spacing` metres, with width and bank per point. Streamed: one path for every cell, draped on the field sampled at the source's spacing.",
            &[PortDef {
                name: "field",
                accepts: &[PortType::Field],
                required: false,
                doc: "Terrain to drape the control points on (their y is added to its height).",
            }],
            None,
            PortType::Path,
        ),
        spline,
    ));
    r.register(NodeDef::new(
        spec(
            "road.mesh",
            Streaming::Cell,
            "The road surface of the connected paths (on any port names): ribbons cut back around junctions, and a flat surface for each junction. Streamed: per cell, the triangles whose centre lies in the cell, local to its corner, from one network every cell shares.",
            &[],
            Some(PATHS),
            PortType::Mesh,
        ),
        road_mesh,
    ));
    r.register(NodeDef::new(
        spec(
            "road.footprint",
            Streaming::Global,
            "The area the connected paths (on any port names) cover, grown by a margin, as shapes.",
            &[],
            Some(PATHS),
            PortType::Shapes,
        ),
        footprint,
    ));
    r.register(NodeDef::new(
        spec(
            "terrain.conform",
            Streaming::Pointwise,
            "Flattens a field under the connected paths (on any port names other than `field`) to the road's height, blending back to the field beside them. Streamed: pointwise, each sample against the whole of every path.",
            &[PortDef {
                name: "field",
                accepts: &[PortType::Field],
                required: true,
                doc: "The terrain to flatten.",
            }],
            Some(PATHS),
            PortType::Field,
        ),
        conform,
    ));
}
