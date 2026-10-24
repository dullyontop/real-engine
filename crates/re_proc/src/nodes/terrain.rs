//! `terrain.heightfield`, `terrain.island_mask`, `terrain.mesh`,
//! `terrain.chunks` and `terrain.water_plane`.

use std::sync::Arc;

use glam::Vec3;
use re_geom::Heightfield;
use re_noise::{Basis, Fractal, fbm2, ridged2, warped_fbm2};
use re_reflect::Value;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{Migration, NodeDef, NodeRegistry, NodeSpec, Params, PortDef, positive, within};
use crate::value::{
    DerivedAsset, FragItem, Fragment, MeshValue, Output, PortType, collider_name, derived_name,
};

/// The largest field size (samples per side).
pub const MAX_FIELD_SIZE: u32 = 2049;

/// The noise of a [`HeightfieldParams`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NoiseKind {
    /// Fractal Brownian motion: rolling hills.
    #[default]
    Fbm,
    /// Ridged multifractal: sharp crests.
    Ridged,
    /// Domain-warped fBm: swirled, eroded-looking shapes.
    Warped,
}

/// The base noise of a [`HeightfieldParams`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BasisKind {
    /// Perlin gradient noise.
    #[default]
    Perlin,
    /// Simplex gradient noise.
    Simplex,
}

/// Parameters of `terrain.heightfield`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct HeightfieldParams {
    /// Samples per side (2 to 2049). The field is centred on the source.
    pub size: u32,
    /// Distance between samples, in metres.
    pub cell: f32,
    /// Noise frequency: noise units per metre (smaller is broader).
    pub scale: f32,
    /// Height of the highest possible point, in metres (heights run from 0 to this).
    pub height: f32,
    /// The noise: `fbm`, `ridged` or `warped`.
    pub noise: NoiseKind,
    /// The base noise: `perlin` or `simplex`.
    pub basis: BasisKind,
    /// Number of octaves (1 to 16).
    pub octaves: u32,
    /// Frequency multiplier per octave.
    pub lacunarity: f32,
    /// Amplitude multiplier per octave.
    pub gain: f32,
    /// Warp strength in noise units (`warped` only).
    pub warp: f32,
    /// Moves the noise pattern, in metres `[x, z]`.
    pub offset: [f32; 2],
}

impl Default for HeightfieldParams {
    fn default() -> Self {
        HeightfieldParams {
            size: 128,
            cell: 1.0,
            scale: 0.01,
            height: 30.0,
            noise: NoiseKind::Fbm,
            basis: BasisKind::Perlin,
            octaves: 5,
            lacunarity: 2.0,
            gain: 0.5,
            warp: 1.0,
            offset: [0.0, 0.0],
        }
    }
}

fn check_size(size: u32) -> Result<(), String> {
    if (2..=MAX_FIELD_SIZE).contains(&size) {
        Ok(())
    } else {
        Err(format!("`size` must be 2 to {MAX_FIELD_SIZE}, got {size}"))
    }
}

impl Params for HeightfieldParams {
    fn validate(&self) -> Result<(), String> {
        check_size(self.size)?;
        positive("cell", self.cell)?;
        positive("scale", self.scale)?;
        within("height", self.height, -10_000.0, 10_000.0)?;
        if !(1..=16).contains(&self.octaves) {
            return Err(format!("`octaves` must be 1 to 16, got {}", self.octaves));
        }
        within("lacunarity", self.lacunarity, 1.0, 8.0)?;
        within("gain", self.gain, 0.0, 1.0)?;
        within("warp", self.warp, 0.0, 100.0)?;
        for v in self.offset {
            within("offset", v, -1.0e6, 1.0e6)?;
        }
        Ok(())
    }
}

fn heightfield(ctx: &super::NodeCtx, p: &HeightfieldParams) -> Result<Output, String> {
    if let Some(d) = ctx.domain {
        return heightfield_window(ctx, p, &d);
    }
    let n = p.size as usize;
    let f = fractal(p);
    let half = Heightfield::half_extent(p.size, p.cell);
    let seed = ctx.seed;
    let mut values = vec![0.0f32; n * n];
    ctx.par_rows(&mut values, n, |z, row| {
        let wz = z as f32 * p.cell - half + p.offset[1];
        for (x, out) in row.iter_mut().enumerate() {
            let wx = x as f32 * p.cell - half + p.offset[0];
            *out = noise_height(p, &f, seed, wx, wz);
        }
    });
    Ok(Output::Field(Arc::new(crate::value::Field::new(
        p.size, p.cell, values,
    )?)))
}

fn fractal(p: &HeightfieldParams) -> Fractal {
    Fractal {
        basis: match p.basis {
            BasisKind::Perlin => Basis::Perlin,
            BasisKind::Simplex => Basis::Simplex,
        },
        octaves: p.octaves,
        lacunarity: p.lacunarity,
        gain: p.gain,
    }
}

/// `terrain.heightfield` over a streamed window (ADR-0042): the window gives
/// the samples and spacing (`size` and `cell` are not used), and each sample
/// sits at its global position, so windows agree on the samples they share.
fn heightfield_window(
    ctx: &super::NodeCtx,
    p: &HeightfieldParams,
    d: &super::Domain,
) -> Result<Output, String> {
    let n = d.n as usize;
    let f = fractal(p);
    let seed = ctx.seed;
    let mut values = vec![0.0f32; n * n];
    ctx.par_rows(&mut values, n, |z, row| {
        let wz = d.position(d.origin[1] + z as i64) + p.offset[1];
        for (x, out) in row.iter_mut().enumerate() {
            let wx = d.position(d.origin[0] + x as i64) + p.offset[0];
            *out = noise_height(p, &f, seed, wx, wz);
        }
    });
    Ok(Output::Field(Arc::new(crate::value::Field::at(d, values)?)))
}

fn noise_height(p: &HeightfieldParams, f: &Fractal, seed: u64, wx: f32, wz: f32) -> f32 {
    let (sx, sz) = (wx * p.scale, wz * p.scale);
    let unit = match p.noise {
        NoiseKind::Fbm => fbm2(seed, f, sx, sz) * 0.5 + 0.5,
        NoiseKind::Ridged => ridged2(seed, f, sx, sz),
        NoiseKind::Warped => warped_fbm2(seed, f, p.warp, sx, sz) * 0.5 + 0.5,
    };
    unit.clamp(0.0, 1.0) * p.height
}

/// The outline of a [`MaskParams`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MaskShape {
    /// Round island.
    #[default]
    Circle,
    /// Square island.
    Square,
}

/// Parameters of `terrain.island_mask`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct MaskParams {
    /// Samples per side (2 to 2049). `field.combine` resamples it to the
    /// other field's size, so it rarely needs changing.
    pub size: u32,
    /// Distance between samples, in metres.
    pub cell: f32,
    /// The island's radius as a fraction of the half extent (1 reaches the
    /// middle of each edge).
    pub radius: f32,
    /// The fraction of the radius over which the mask falls from 1 to 0.
    pub falloff: f32,
    /// `circle` or `square`.
    pub shape: MaskShape,
}

impl Default for MaskParams {
    fn default() -> Self {
        MaskParams {
            size: 128,
            cell: 1.0,
            radius: 0.9,
            falloff: 0.6,
            shape: MaskShape::Circle,
        }
    }
}

impl Params for MaskParams {
    fn validate(&self) -> Result<(), String> {
        check_size(self.size)?;
        positive("cell", self.cell)?;
        within("radius", self.radius, 0.01, 2.0)?;
        within("falloff", self.falloff, 0.0, 1.0)
    }
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    if e1 <= e0 {
        return if x < e0 { 0.0 } else { 1.0 };
    }
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn island_mask(ctx: &super::NodeCtx, p: &MaskParams) -> Result<Output, String> {
    let n = p.size as usize;
    let last = (n - 1) as f32;
    let inner = p.radius * (1.0 - p.falloff);
    let mut values = vec![0.0f32; n * n];
    ctx.par_rows(&mut values, n, |z, row| {
        let v = z as f32 / last * 2.0 - 1.0;
        for (x, out) in row.iter_mut().enumerate() {
            let u = x as f32 / last * 2.0 - 1.0;
            let d = match p.shape {
                MaskShape::Circle => (u * u + v * v).sqrt(),
                MaskShape::Square => u.abs().max(v.abs()),
            };
            *out = 1.0 - smoothstep(inner, p.radius, d);
        }
    });
    Ok(Output::Field(Arc::new(crate::value::Field::new(
        p.size, p.cell, values,
    )?)))
}

/// Parameters of `terrain.mesh`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct TerrainMeshParams {
    /// Also add a heightfield `Collider` built from the same samples.
    pub collider: bool,
    /// A material file (`materials/grass.material.json`), or empty.
    pub material: String,
    /// The entity's name.
    pub name: String,
}

impl Default for TerrainMeshParams {
    fn default() -> Self {
        TerrainMeshParams {
            collider: false,
            material: String::new(),
            name: "Terrain".into(),
        }
    }
}

impl Params for TerrainMeshParams {}

fn terrain_mesh(ctx: &super::NodeCtx, p: &TerrainMeshParams) -> Result<Output, String> {
    let field = ctx.need_field("field")?;
    // Streamed (version 2), the entity sits at the window's corner and the
    // mesh and collider are local to it, so both keep their precision
    // kilometres out (ADR-0042's floating origin). Version 1 stays in world
    // space.
    let (hf, transform) = match ctx.domain {
        Some(_) if ctx.version >= 2 => {
            let mut local = field.0.clone();
            let corner = Vec3::new(local.offset.x, 0.0, local.offset.z);
            local.offset -= corner;
            (
                local,
                vec![("translation".to_string(), Value::Vec3(corner))],
            )
        }
        _ => (field.0.clone(), Vec::new()),
    };
    let hf = &hf;
    let mesh = Arc::new(hf.to_mesh());
    let mesh_name = derived_name(&ctx.key);
    let mut components = vec![
        ("Transform".to_string(), transform),
        (
            "MeshRef".to_string(),
            vec![("asset".to_string(), Value::Str(mesh_name.clone()))],
        ),
    ];
    let mut assets = vec![(mesh_name.clone(), DerivedAsset::Mesh(mesh))];
    if !p.material.is_empty() {
        components.push((
            "MaterialRef".into(),
            vec![("asset".into(), Value::Str(p.material.clone()))],
        ));
    }
    if p.collider {
        let hf_name = collider_name(&mesh_name).expect("a derived name");
        components.push((
            "Collider".into(),
            vec![
                ("shape".into(), Value::Enum("Heightfield".into())),
                ("source".into(), Value::Str(hf_name.clone())),
            ],
        ));
        assets.push((hf_name, DerivedAsset::Heightfield(Arc::new(hf.clone()))));
    }
    Ok(Output::Fragment(Arc::new(Fragment {
        items: vec![FragItem {
            key: format!("{}/terrain", ctx.node_id),
            name: p.name.clone(),
            parent_key: None,
            components,
            prefab: None,
        }],
        assets,
    })))
}

/// Parameters of `terrain.chunks`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ChunksParams {
    /// A material file (`materials/grass.material.json`), or empty.
    pub material: String,
    /// The coarsest level of detail (0 to 8): root tiles are 64 × 2^max_level
    /// samples of the field's spacing.
    pub max_level: u32,
    /// The entity's name when the graph is not streamed.
    pub name: String,
}

impl Default for ChunksParams {
    fn default() -> Self {
        ChunksParams {
            material: String::new(),
            max_level: 5,
            name: "Terrain".into(),
        }
    }
}

impl Params for ChunksParams {
    fn validate(&self) -> Result<(), String> {
        if self.max_level > 8 {
            return Err(format!(
                "`max_level` must be 0 to 8, got {}",
                self.max_level
            ));
        }
        Ok(())
    }
}

/// `terrain.chunks`: in a streamed window it adds nothing, because world
/// streaming draws the field as level-of-detail chunks and adds collision
/// chunks near the focuses (ADR-0042). Not streamed, it is one mesh, as
/// `terrain.mesh` without a collider.
fn terrain_chunks(ctx: &super::NodeCtx, p: &ChunksParams) -> Result<Output, String> {
    if ctx.domain.is_some() {
        return Ok(Output::Fragment(Arc::new(Fragment::default())));
    }
    terrain_mesh(
        ctx,
        &TerrainMeshParams {
            collider: false,
            material: p.material.clone(),
            name: p.name.clone(),
        },
    )
}

/// Parameters of `terrain.water_plane`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct WaterParams {
    /// The water's height, in metres.
    pub level: f32,
    /// The side length of the square plane, in metres.
    pub size: f32,
    /// Cells per side of the plane mesh (1 to 1024).
    pub subdivisions: u32,
    /// A material file, or empty.
    pub material: String,
    /// The entity's name.
    pub name: String,
}

impl Default for WaterParams {
    fn default() -> Self {
        WaterParams {
            level: 2.0,
            size: 512.0,
            subdivisions: 1,
            material: String::new(),
            name: "Water".into(),
        }
    }
}

impl Params for WaterParams {
    fn validate(&self) -> Result<(), String> {
        within("level", self.level, -10_000.0, 10_000.0)?;
        positive("size", self.size)
    }
}

fn water_plane(ctx: &super::NodeCtx, p: &WaterParams) -> Result<Output, String> {
    // In a streamed window the plane covers the window (`size` is not used).
    let (size, centre) = match ctx.domain {
        Some(d) => {
            let (lo, hi) = (d.min(), d.max());
            (
                hi[0] - lo[0],
                Vec3::new((lo[0] + hi[0]) * 0.5, p.level, (lo[1] + hi[1]) * 0.5),
            )
        }
        None => (p.size, Vec3::new(0.0, p.level, 0.0)),
    };
    let plane = re_geom::plane(size, size, p.subdivisions).map_err(|e| e.to_string())?;
    let mesh = MeshValue {
        mesh: plane,
        material: p.material.clone(),
        asset: ctx.key,
    };
    let mut item =
        super::fragment::mesh_item(format!("{}/water", ctx.node_id), p.name.clone(), &mesh);
    item.0.components[0].1 = vec![("translation".into(), Value::Vec3(centre))];
    Ok(Output::Fragment(Arc::new(Fragment {
        items: vec![item.0],
        assets: vec![item.1],
    })))
}

const FIELD: &[PortType] = &[PortType::Field];

pub(super) fn register(r: &mut NodeRegistry) {
    r.register(NodeDef::new(
        NodeSpec {
            kind: "terrain.heightfield",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Pointwise,
            doc: "A noise heightfield (fbm, ridged or warped), centred on the source, with heights from 0 to `height` metres. Streamed: pointwise, over the window at global positions (`size` and `cell` come from the window).",
            inputs: &[],
            variadic: None,
            output: PortType::Field,
            uses_seed: true,
            file_params: &[],
        },
        heightfield,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "terrain.island_mask",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Whole,
            doc: "A radial falloff from 1 in the middle to 0 at the edges; multiply a heightfield by it to make an island. Not streamable: it is relative to the field's size.",
            inputs: &[],
            variadic: None,
            output: PortType::Field,
            uses_seed: false,
            file_params: &[],
        },
        island_mask,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "terrain.mesh",
            version: 2,
            oldest: 1,
            migrations: &[Migration {
                from: 1,
                note: "streamed cells put the entity at the window's corner, with the mesh and collider local to it",
                changes_output: true,
                params: |_| Ok(()),
            }],
            streaming: super::Streaming::Cell,
            doc: "Turns a field into a terrain entity: a derived mesh, an optional material and an optional heightfield collider.",
            inputs: &[PortDef {
                name: "field",
                accepts: FIELD,
                required: true,
                doc: "The heights, in metres.",
            }],
            variadic: None,
            output: PortType::Fragment,
            uses_seed: false,
            file_params: &[],
        },
        terrain_mesh,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "terrain.chunks",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Cell,
            doc: "The terrain of a streamed world: world streaming draws its field as level-of-detail chunks (64 × 64 cells each, stitched to coarser neighbours) and gives the chunks near each focus heightfield colliders. Not streamed, it is one mesh with the material.",
            inputs: &[PortDef {
                name: "field",
                accepts: FIELD,
                required: true,
                doc: "The heights, in metres.",
            }],
            variadic: None,
            output: PortType::Fragment,
            uses_seed: false,
            file_params: &[],
        },
        terrain_chunks,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "terrain.water_plane",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Cell,
            doc: "A flat square water entity at a height. Streamed: one plane per cell, covering it.",
            inputs: &[],
            variadic: None,
            output: PortType::Fragment,
            uses_seed: false,
            file_params: &[],
        },
        water_plane,
    ));
}
