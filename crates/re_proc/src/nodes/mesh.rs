//! `mesh.primitive`, `mesh.transform`, `mesh.merge` and `material.assign`.

use std::sync::Arc;

use glam::Vec3;
use re_geom::math::quat_from_euler_deg;
use schemars::JsonSchema;
use serde::Deserialize;

use super::{
    NodeCtx, NodeDef, NodeRegistry, NodeSpec, Params, PortDef, natural_cmp, positive, within,
};
use crate::value::{MeshValue, Output, PortType};

/// The shape of a [`PrimitiveParams`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveShape {
    /// A box of `size`.
    #[default]
    Box,
    /// A sphere of `radius`.
    Sphere,
    /// A cylinder of `radius` and `height`.
    Cylinder,
    /// A capsule of `radius` whose straight part is `height` long.
    Capsule,
    /// A flat plane of `size[0]` by `size[2]`, facing +Y.
    Plane,
}

/// Parameters of `mesh.primitive`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct PrimitiveParams {
    /// `box`, `sphere`, `cylinder`, `capsule` or `plane`.
    pub shape: PrimitiveShape,
    /// `box`: the full size `[x, y, z]`; `plane`: `[x, _, z]`.
    pub size: [f32; 3],
    /// `sphere`, `cylinder`, `capsule`: the radius.
    pub radius: f32,
    /// `cylinder`: the full height; `capsule`: the length of the straight part.
    pub height: f32,
    /// Segments around (3 to 1024).
    pub segments: u32,
    /// Rings: pole to pole for a sphere, per end for a capsule.
    pub rings: u32,
    /// `plane`: cells per side.
    pub subdivisions: u32,
}

impl Default for PrimitiveParams {
    fn default() -> Self {
        PrimitiveParams {
            shape: PrimitiveShape::Box,
            size: [1.0, 1.0, 1.0],
            radius: 0.5,
            height: 1.0,
            segments: 24,
            rings: 12,
            subdivisions: 1,
        }
    }
}

impl Params for PrimitiveParams {
    /// Checks the parameters the shape uses, so a bad value is `RE4005` when
    /// the graph is loaded or an override is set, not a failure at
    /// evaluation.
    fn validate(&self) -> Result<(), String> {
        let count = |name: &str, v: u32, min: u32| {
            if (min..=re_geom::primitives::MAX_TESSELLATION).contains(&v) {
                Ok(())
            } else {
                Err(format!(
                    "`{name}` must be {min} to {}, got {v}",
                    re_geom::primitives::MAX_TESSELLATION
                ))
            }
        };
        match self.shape {
            PrimitiveShape::Box => {
                for v in self.size {
                    positive("size", v)?;
                }
            }
            PrimitiveShape::Plane => {
                positive("size", self.size[0])?;
                positive("size", self.size[2])?;
                count("subdivisions", self.subdivisions, 1)?;
            }
            PrimitiveShape::Sphere => {
                positive("radius", self.radius)?;
                count("segments", self.segments, 3)?;
                count("rings", self.rings, 2)?;
            }
            PrimitiveShape::Cylinder => {
                positive("radius", self.radius)?;
                positive("height", self.height)?;
                count("segments", self.segments, 3)?;
            }
            PrimitiveShape::Capsule => {
                positive("radius", self.radius)?;
                within("height", self.height, 0.0, f32::MAX)?;
                count("segments", self.segments, 3)?;
                count("rings", self.rings, 1)?;
            }
        }
        Ok(())
    }
}

fn primitive(ctx: &NodeCtx, p: &PrimitiveParams) -> Result<Output, String> {
    let s = Vec3::from_array(p.size);
    let mesh = match p.shape {
        PrimitiveShape::Box => re_geom::cuboid(s * 0.5),
        PrimitiveShape::Sphere => re_geom::uv_sphere(p.radius, p.segments, p.rings),
        PrimitiveShape::Cylinder => re_geom::cylinder(p.radius, p.height * 0.5, p.segments),
        PrimitiveShape::Capsule => re_geom::capsule(p.radius, p.height * 0.5, p.segments, p.rings),
        PrimitiveShape::Plane => re_geom::plane(s.x, s.z, p.subdivisions),
    }
    .map_err(|e| e.to_string())?;
    Ok(Output::Mesh(Arc::new(MeshValue {
        mesh,
        material: String::new(),
        asset: ctx.key,
    })))
}

/// Parameters of `mesh.transform`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct TransformParams {
    /// Moves the mesh, in metres.
    pub translation: [f32; 3],
    /// Euler angles in degrees, applied X, then Y, then Z.
    pub rotation: [f32; 3],
    /// Scale per axis (applied first).
    pub scale: [f32; 3],
}

impl Default for TransformParams {
    fn default() -> Self {
        TransformParams {
            translation: [0.0; 3],
            rotation: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}

impl Params for TransformParams {
    fn validate(&self) -> Result<(), String> {
        for v in self.translation.iter().chain(&self.rotation) {
            within("translation and rotation", *v, -1.0e6, 1.0e6)?;
        }
        for v in self.scale {
            if !v.is_finite() || v == 0.0 {
                return Err(format!("`scale` must be finite and not zero, got {v}"));
            }
        }
        Ok(())
    }
}

fn transform(ctx: &NodeCtx, p: &TransformParams) -> Result<Output, String> {
    let input = ctx.need_mesh("mesh")?;
    let mut mesh = input.mesh.clone();
    re_geom::transform(
        &mut mesh,
        Vec3::from_array(p.translation),
        quat_from_euler_deg(Vec3::from_array(p.rotation)),
        Vec3::from_array(p.scale),
    );
    Ok(Output::Mesh(Arc::new(MeshValue {
        mesh,
        material: input.material.clone(),
        asset: ctx.key,
    })))
}

/// Parameters of `mesh.merge` (none).
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeParams {}

impl Params for MergeParams {}

fn merge(ctx: &NodeCtx, _: &MergeParams) -> Result<Output, String> {
    let mut inputs: Vec<(&str, &Arc<MeshValue>)> = ctx
        .inputs
        .iter()
        .filter_map(|(port, o)| match o {
            Output::Mesh(m) => Some((port.as_str(), m)),
            _ => None,
        })
        .collect();
    if inputs.is_empty() {
        return Err("no mesh is connected".into());
    }
    inputs.sort_by(|a, b| natural_cmp(a.0, b.0));
    let mesh = re_geom::merge(inputs.iter().map(|(_, m)| &m.mesh)).map_err(|e| e.to_string())?;
    let material = inputs
        .iter()
        .map(|(_, m)| m.material.as_str())
        .find(|m| !m.is_empty())
        .unwrap_or_default()
        .to_string();
    Ok(Output::Mesh(Arc::new(MeshValue {
        mesh,
        material,
        asset: ctx.key,
    })))
}

/// Parameters of `material.assign`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct MaterialParams {
    /// A material file, relative to the project root
    /// (`materials/stone.material.json`); `re_app` loads it through `re_assets`.
    pub material: String,
}

impl Params for MaterialParams {
    fn validate(&self) -> Result<(), String> {
        if self.material.is_empty() {
            return Err(
                "`material` is empty; give a material file such as `materials/stone.material.json`"
                    .into(),
            );
        }
        Ok(())
    }
}

fn assign(ctx: &NodeCtx, p: &MaterialParams) -> Result<Output, String> {
    let input = ctx.need_mesh("mesh")?;
    // The geometry is unchanged, so the derived mesh asset is shared.
    Ok(Output::Mesh(Arc::new(MeshValue {
        mesh: input.mesh.clone(),
        material: p.material.clone(),
        asset: input.asset,
    })))
}

const MESH: &[PortType] = &[PortType::Mesh];
const ONE_MESH: &[PortDef] = &[PortDef {
    name: "mesh",
    accepts: MESH,
    required: true,
    doc: "The input mesh.",
}];

pub(super) fn register(r: &mut NodeRegistry) {
    r.register(NodeDef::new(
        NodeSpec {
            kind: "mesh.primitive",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Whole,
            doc: "A box, sphere, cylinder, capsule or plane mesh, centred on the origin.",
            inputs: &[],
            variadic: None,
            output: PortType::Mesh,
            uses_seed: false,
            file_params: &[],
        },
        primitive,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "mesh.transform",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Whole,
            doc: "Scales, rotates (Euler degrees, X then Y then Z) and moves a mesh.",
            inputs: ONE_MESH,
            variadic: None,
            output: PortType::Mesh,
            uses_seed: false,
            file_params: &[],
        },
        transform,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "mesh.merge",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Whole,
            doc: "Merges any number of meshes (inputs on any port names, in natural name order) into one; the material is the first non-empty one.",
            inputs: &[],
            variadic: Some(MESH),
            output: PortType::Mesh,
            uses_seed: false,
            file_params: &[],
        },
        merge,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "material.assign",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Whole,
            doc: "Sets the material file a mesh is drawn with.",
            inputs: ONE_MESH,
            variadic: None,
            output: PortType::Mesh,
            uses_seed: false,
            file_params: &[],
        },
        assign,
    ));
}
