//! The modelling kinds of PR "polymesh" (ADR-0062 §7): primitives, whole-mesh
//! edits, face groups, materials and attributes on polygon meshes, and the
//! conversions to and from `mesh`.

use std::sync::Arc;

use glam::{Quat, Vec3};
use re_geom::math::{quat_from_euler_deg, sin_cos};
use re_geom::poly::{self, AttrDomain, AttrValue, Axis, Column, ColumnType, PolyMesh};
use schemars::JsonSchema;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value as Json;

use super::{
    NodeCtx, NodeDef, NodeError, NodeRegistry, NodeSpec, Params, PortDef, Streaming, natural_cmp,
    within,
};
use crate::value::{MeshValue, Output, PortType};

/// The shortest length a primitive takes, in metres.
const MIN_LENGTH: f32 = 0.001;
/// The longest length a primitive takes, in metres.
const MAX_LENGTH: f32 = 10_000.0;

fn length(name: &str, v: f32) -> Result<(), String> {
    within(name, v, MIN_LENGTH, MAX_LENGTH)
}

fn count(name: &str, v: u32, min: u32, max: u32) -> Result<(), String> {
    if (min..=max).contains(&v) {
        Ok(())
    } else {
        Err(format!("`{name}` must be {min} to {max}, got {v}"))
    }
}

/// An optional group or attribute name: empty, or a valid attribute name.
fn name_or_empty(param: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Ok(());
    }
    poly::check_name(name).map_err(|e| format!("`{param}`: {e}"))
}

fn unknown(message: String) -> NodeError {
    NodeError {
        code: crate::codes::UNKNOWN_ATTRIBUTE,
        message,
    }
}

/// `(its groups: a, b)`, to say which groups a mesh has.
fn groups_of(m: &PolyMesh) -> String {
    let names: Vec<&str> = m
        .faces
        .iter()
        .filter(|(_, c)| c.ty() == ColumnType::Bool)
        .map(|(n, _)| n)
        .collect();
    if names.is_empty() {
        "(it has no groups)".into()
    } else {
        format!("(its groups: {})", names.join(", "))
    }
}

/// The faces of group `name`, or every face when it is empty; `RE4150` when
/// the mesh has no such group.
fn faces_of(m: &PolyMesh, name: &str) -> Result<Vec<bool>, NodeError> {
    if name.is_empty() {
        return Ok(vec![true; m.face_count()]);
    }
    match m.faces.get(name) {
        Some(Column::Bool(v)) => Ok(v.clone()),
        Some(c) => Err(unknown(format!(
            "face attribute `{name}` is a `{}`, not a group",
            c.ty().name()
        ))),
        None => Err(unknown(format!(
            "the mesh has no face group `{name}` {}",
            groups_of(m)
        ))),
    }
}

fn output(m: PolyMesh) -> Result<Output, NodeError> {
    Ok(Output::PolyMesh(Arc::new(m)))
}

/// Parameters of `poly.box`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct BoxParams {
    /// The full size `[x, y, z]`, in metres (each 0.001 to 10000).
    #[schemars(inner(range(min = 0.001, max = 10000.0)))]
    pub size: [f32; 3],
    /// The radius of the rounded edges and corners, from 0 (six flat quads)
    /// to half the smallest side.
    #[schemars(range(min = 0.0))]
    pub radius: f32,
    /// Steps per quarter circle of a rounded edge: an even number, 2 to 64.
    #[schemars(range(min = 2, max = 64))]
    pub segments: u32,
}

impl Default for BoxParams {
    fn default() -> Self {
        BoxParams {
            size: [1.0; 3],
            radius: 0.0,
            segments: 4,
        }
    }
}

impl Params for BoxParams {
    fn validate(&self) -> Result<(), String> {
        for v in self.size {
            length("size", v)?;
        }
        let half = self.size.iter().fold(f32::MAX, |a, &b| a.min(b)) * 0.5;
        within("radius", self.radius, 0.0, half)?;
        count("segments", self.segments, 2, 64)?;
        if !self.segments.is_multiple_of(2) {
            return Err(format!(
                "`segments` must be an even number, got {}",
                self.segments
            ));
        }
        Ok(())
    }
}

fn box_(_: &NodeCtx, p: &BoxParams) -> Result<Output, NodeError> {
    output(poly::rounded_box(
        Vec3::from_array(p.size),
        p.radius,
        p.segments,
    )?)
}

/// Parameters of `poly.icosphere`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct IcosphereParams {
    /// The radius, in metres (0.001 to 10000).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub radius: f32,
    /// How many times each triangle is split in four (0 to 6): 20 × 4^n
    /// triangles.
    #[schemars(range(min = 0, max = 6))]
    pub subdivisions: u32,
}

impl Default for IcosphereParams {
    fn default() -> Self {
        IcosphereParams {
            radius: 0.5,
            subdivisions: 2,
        }
    }
}

impl Params for IcosphereParams {
    fn validate(&self) -> Result<(), String> {
        length("radius", self.radius)?;
        count("subdivisions", self.subdivisions, 0, 6)
    }
}

fn icosphere(_: &NodeCtx, p: &IcosphereParams) -> Result<Output, NodeError> {
    output(poly::icosphere(p.radius, p.subdivisions)?)
}

/// Parameters of `poly.cone`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ConeParams {
    /// The radius at the bottom, in metres (0 to 10000; 0 is a point, but not
    /// at both ends).
    #[schemars(range(min = 0.0, max = 10000.0))]
    pub radius: f32,
    /// The radius at the top, in metres (0 to 10000): 0 for a cone, the
    /// bottom radius for a cylinder.
    #[schemars(range(min = 0.0, max = 10000.0))]
    pub top_radius: f32,
    /// The height, in metres (0.001 to 10000).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub height: f32,
    /// Sides around (3 to 256).
    #[schemars(range(min = 3, max = 256))]
    pub segments: u32,
    /// Closes the ends with flat faces.
    pub caps: bool,
}

impl Default for ConeParams {
    fn default() -> Self {
        ConeParams {
            radius: 0.5,
            top_radius: 0.0,
            height: 1.0,
            segments: 24,
            caps: true,
        }
    }
}

impl Params for ConeParams {
    fn validate(&self) -> Result<(), String> {
        within("radius", self.radius, 0.0, MAX_LENGTH)?;
        within("top_radius", self.top_radius, 0.0, MAX_LENGTH)?;
        if self.radius == 0.0 && self.top_radius == 0.0 {
            return Err("`radius` and `top_radius` cannot both be 0".into());
        }
        length("height", self.height)?;
        count("segments", self.segments, 3, poly::MAX_SEGMENTS)
    }
}

fn cone(_: &NodeCtx, p: &ConeParams) -> Result<Output, NodeError> {
    output(poly::cone(
        p.radius,
        p.top_radius,
        p.height,
        p.segments,
        p.caps,
    )?)
}

/// Parameters of `poly.torus`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct TorusParams {
    /// The radius of the circle the tube follows, in metres (0.001 to 10000,
    /// larger than `tube`).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub radius: f32,
    /// The radius of the tube, in metres (0.001 to 10000, smaller than
    /// `radius`).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub tube: f32,
    /// Steps around the circle (3 to 256).
    #[schemars(range(min = 3, max = 256))]
    pub segments: u32,
    /// Steps around the tube (3 to 256).
    #[schemars(range(min = 3, max = 256))]
    pub sides: u32,
}

impl Default for TorusParams {
    fn default() -> Self {
        TorusParams {
            radius: 1.0,
            tube: 0.25,
            segments: 24,
            sides: 12,
        }
    }
}

impl Params for TorusParams {
    fn validate(&self) -> Result<(), String> {
        length("radius", self.radius)?;
        length("tube", self.tube)?;
        if self.tube >= self.radius {
            return Err(format!(
                "`tube` ({}) must be smaller than `radius` ({})",
                self.tube, self.radius
            ));
        }
        count("segments", self.segments, 3, poly::MAX_SEGMENTS)?;
        count("sides", self.sides, 3, poly::MAX_SEGMENTS)
    }
}

fn torus(_: &NodeCtx, p: &TorusParams) -> Result<Output, NodeError> {
    output(poly::torus(p.radius, p.tube, p.segments, p.sides)?)
}

/// Parameters of `poly.stairs`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct StairsParams {
    /// The number of steps (1 to 256).
    #[schemars(range(min = 1, max = 256))]
    pub steps: u32,
    /// The width along X, in metres (0.001 to 10000).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub width: f32,
    /// The height of each step, in metres (0.001 to 10000).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub rise: f32,
    /// The depth of each step, in metres (0.001 to 10000).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub run: f32,
}

impl Default for StairsParams {
    fn default() -> Self {
        StairsParams {
            steps: 8,
            width: 1.0,
            rise: 0.18,
            run: 0.28,
        }
    }
}

impl Params for StairsParams {
    fn validate(&self) -> Result<(), String> {
        count("steps", self.steps, 1, poly::MAX_SEGMENTS)?;
        length("width", self.width)?;
        length("rise", self.rise)?;
        length("run", self.run)
    }
}

fn stairs(_: &NodeCtx, p: &StairsParams) -> Result<Output, NodeError> {
    output(poly::stairs(p.steps, p.width, p.rise, p.run)?)
}

/// Parameters of `poly.arch`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ArchParams {
    /// The width of the opening at its feet, in metres (0.001 to 10000).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub span: f32,
    /// The height of the opening, in metres (0.001 to 10000, at most half
    /// the span: a semicircle).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub rise: f32,
    /// The thickness of the ring, in metres (0.001 to 10000).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub thickness: f32,
    /// The depth along Z, in metres (0.001 to 10000).
    #[schemars(range(min = 0.001, max = 10000.0))]
    pub depth: f32,
    /// Pieces along the curve (1 to 256).
    #[schemars(range(min = 1, max = 256))]
    pub segments: u32,
}

impl Default for ArchParams {
    fn default() -> Self {
        ArchParams {
            span: 2.0,
            rise: 1.0,
            thickness: 0.3,
            depth: 0.5,
            segments: 12,
        }
    }
}

impl Params for ArchParams {
    fn validate(&self) -> Result<(), String> {
        length("span", self.span)?;
        length("rise", self.rise)?;
        if self.rise > self.span * 0.5 {
            return Err(format!(
                "`rise` ({}) must be at most half the span ({})",
                self.rise, self.span
            ));
        }
        length("thickness", self.thickness)?;
        length("depth", self.depth)?;
        count("segments", self.segments, 1, poly::MAX_SEGMENTS)
    }
}

fn arch(_: &NodeCtx, p: &ArchParams) -> Result<Output, NodeError> {
    output(poly::arch(
        p.span,
        p.rise,
        p.thickness,
        p.depth,
        p.segments,
    )?)
}

/// Parameters of `poly.transform`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct TransformParams {
    /// Moves the mesh, in metres (each -1000000 to 1000000).
    #[schemars(inner(range(min = -1.0e6, max = 1.0e6)))]
    pub translation: [f32; 3],
    /// Euler angles in degrees, applied X, then Y, then Z (each -1000000 to
    /// 1000000).
    #[schemars(inner(range(min = -1.0e6, max = 1.0e6)))]
    pub rotation: [f32; 3],
    /// Scale per axis, applied first (each -1000000 to 1000000, not 0); an
    /// odd number of negative axes mirrors the mesh.
    #[schemars(inner(range(min = -1.0e6, max = 1.0e6)))]
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
        for v in self.translation {
            within("translation", v, -1.0e6, 1.0e6)?;
        }
        for v in self.rotation {
            within("rotation", v, -1.0e6, 1.0e6)?;
        }
        for v in self.scale {
            within("scale", v, -1.0e6, 1.0e6)?;
            if v == 0.0 {
                return Err("`scale` must not be 0 on any axis".into());
            }
        }
        Ok(())
    }
}

fn transform(ctx: &NodeCtx, p: &TransformParams) -> Result<Output, NodeError> {
    let m = ctx.need_polymesh("mesh")?;
    output(poly::transform(
        m,
        Vec3::from_array(p.translation),
        quat_from_euler_deg(Vec3::from_array(p.rotation)),
        Vec3::from_array(p.scale),
    )?)
}

/// A coordinate axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AxisParam {
    /// The X axis.
    #[default]
    X,
    /// The Y axis.
    Y,
    /// The Z axis.
    Z,
}

impl From<AxisParam> for Axis {
    fn from(a: AxisParam) -> Axis {
        match a {
            AxisParam::X => Axis::X,
            AxisParam::Y => Axis::Y,
            AxisParam::Z => Axis::Z,
        }
    }
}

/// Parameters of `poly.mirror`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct MirrorParams {
    /// The axis the mirror plane is square to.
    pub axis: AxisParam,
    /// Where the plane crosses the axis, in metres (-1000000 to 1000000).
    #[schemars(range(min = -1.0e6, max = 1.0e6))]
    pub offset: f32,
    /// Keeps the original too, joined to its reflection at the points on the
    /// plane (faces lying on the plane go); else only the reflection.
    pub keep: bool,
}

impl Default for MirrorParams {
    fn default() -> Self {
        MirrorParams {
            axis: AxisParam::X,
            offset: 0.0,
            keep: true,
        }
    }
}

impl Params for MirrorParams {
    fn validate(&self) -> Result<(), String> {
        within("offset", self.offset, -1.0e6, 1.0e6)
    }
}

fn mirror(ctx: &NodeCtx, p: &MirrorParams) -> Result<Output, NodeError> {
    let m = ctx.need_polymesh("mesh")?;
    output(poly::mirror(m, p.axis.into(), p.offset, p.keep)?)
}

/// Parameters of `poly.weld`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct WeldParams {
    /// Points closer than this merge, in metres (0 to 1000; 0 merges only
    /// points at the same position).
    #[schemars(range(min = 0.0, max = 1000.0))]
    pub distance: f32,
}

impl Default for WeldParams {
    fn default() -> Self {
        WeldParams { distance: 0.001 }
    }
}

impl Params for WeldParams {
    fn validate(&self) -> Result<(), String> {
        within("distance", self.distance, 0.0, 1000.0)
    }
}

fn weld(ctx: &NodeCtx, p: &WeldParams) -> Result<Output, NodeError> {
    let m = ctx.need_polymesh("mesh")?;
    output(poly::weld(m, p.distance)?)
}

/// Parameters of `poly.flip`.
#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct FlipParams {
    /// The face group to turn around; empty for every face.
    pub group: String,
}

impl Params for FlipParams {
    fn validate(&self) -> Result<(), String> {
        name_or_empty("group", &self.group)
    }
}

fn flip(ctx: &NodeCtx, p: &FlipParams) -> Result<Output, NodeError> {
    let m = ctx.need_polymesh("mesh")?;
    faces_of(m, &p.group)?;
    output(m.flip(&p.group)?)
}

/// Parameters of `poly.merge` (none).
#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeParams {}

impl Params for MergeParams {}

fn merge(ctx: &NodeCtx, _: &MergeParams) -> Result<Output, NodeError> {
    let mut inputs: Vec<(&str, &Arc<PolyMesh>)> = ctx
        .inputs
        .iter()
        .filter_map(|(port, o)| match o {
            Output::PolyMesh(m) => Some((port.as_str(), m)),
            _ => None,
        })
        .collect();
    if inputs.is_empty() {
        return Err("no polygon mesh is connected".to_string().into());
    }
    inputs.sort_by(|a, b| natural_cmp(a.0, b.0));
    output(poly::merge(inputs.iter().map(|(_, m)| &***m))?)
}

/// How `poly.group` chooses faces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    /// Faces whose normal is within `angle` degrees of `direction`.
    #[default]
    Direction,
    /// Faces whose centre (the average of their corners) is inside the box
    /// from `min` to `max`, edges included.
    Box,
    /// Faces drawn with `material` (empty: the default material).
    Material,
    /// Faces `start` to `end - 1`, in face order.
    Index,
    /// Each face with probability `fraction`, from the node's seed.
    Random,
    /// Faces in any of `groups`.
    Union,
    /// Faces in every one of `groups`.
    Intersection,
    /// Faces in none of `groups`.
    Complement,
}

/// Parameters of `poly.group`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct GroupParams {
    /// The group to write, replacing a group of that name.
    pub name: String,
    /// How faces are chosen.
    pub by: GroupBy,
    /// `direction`: the direction faces should face (not zero).
    pub direction: [f32; 3],
    /// `direction`: the largest angle between a face's normal and
    /// `direction`, in degrees (0 to 180).
    #[schemars(range(min = 0.0, max = 180.0))]
    pub angle: f32,
    /// `box`: the box's lowest corner, in metres.
    #[schemars(inner(range(min = -1.0e6, max = 1.0e6)))]
    pub min: [f32; 3],
    /// `box`: the box's highest corner, in metres.
    #[schemars(inner(range(min = -1.0e6, max = 1.0e6)))]
    pub max: [f32; 3],
    /// `material`: the material file, as `poly.material` set it.
    #[schemars(extend("x-real-asset" = "material"))]
    pub material: String,
    /// `index`: the first face chosen.
    pub start: u32,
    /// `index`: one past the last face chosen (faces past the mesh's last are
    /// ignored).
    pub end: u32,
    /// `random`: the probability that a face is chosen (0 to 1).
    #[schemars(range(min = 0.0, max = 1.0))]
    pub fraction: f32,
    /// `union`, `intersection` and `complement`: the groups combined.
    pub groups: Vec<String>,
}

impl Default for GroupParams {
    fn default() -> Self {
        GroupParams {
            name: "selection".into(),
            by: GroupBy::Direction,
            direction: [0.0, 1.0, 0.0],
            angle: 45.0,
            min: [-0.5; 3],
            max: [0.5; 3],
            material: String::new(),
            start: 0,
            end: 1,
            fraction: 0.5,
            groups: Vec::new(),
        }
    }
}

impl Params for GroupParams {
    fn validate(&self) -> Result<(), String> {
        poly::check_attribute(&self.name, AttrDomain::Face, ColumnType::Bool)
            .map_err(|e| format!("`name`: {e}"))?;
        if !self.direction.iter().all(|v| v.is_finite()) || self.direction == [0.0; 3] {
            return Err(format!(
                "`direction` must be finite and not zero, got {:?}",
                self.direction
            ));
        }
        within("angle", self.angle, 0.0, 180.0)?;
        for (lo, hi) in self.min.iter().zip(&self.max) {
            within("min", *lo, -1.0e6, 1.0e6)?;
            within("max", *hi, -1.0e6, 1.0e6)?;
            if lo > hi {
                return Err(format!(
                    "`min` ({:?}) is above `max` ({:?})",
                    self.min, self.max
                ));
            }
        }
        if self.start > self.end {
            return Err(format!(
                "`start` ({}) is after `end` ({})",
                self.start, self.end
            ));
        }
        within("fraction", self.fraction, 0.0, 1.0)?;
        for g in &self.groups {
            poly::check_name(g).map_err(|e| format!("`groups`: {e}"))?;
        }
        let combines = matches!(
            self.by,
            GroupBy::Union | GroupBy::Intersection | GroupBy::Complement
        );
        if combines && self.groups.is_empty() {
            return Err("`groups` must name at least one group to combine".into());
        }
        Ok(())
    }
}

/// Mixed into the seed of `poly.group`'s random choices.
const RANDOM_SALT: u64 = 0x6772_6f75_7073;

fn group(ctx: &NodeCtx, p: &GroupParams) -> Result<Output, NodeError> {
    let m = ctx.need_polymesh("mesh")?;
    let n = m.face_count();
    let sel: Vec<bool> = match p.by {
        GroupBy::Direction if p.angle >= 180.0 => vec![true; n],
        GroupBy::Direction => {
            let d = p.direction.map(f64::from);
            let d_len = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
            let (_, cos) = sin_cos(f64::from(p.angle).to_radians());
            (0..n)
                .map(|f| {
                    let a = m.face_area_vector(f);
                    let a_len = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
                    a[0] * d[0] + a[1] * d[1] + a[2] * d[2] >= cos * a_len * d_len
                })
                .collect()
        }
        GroupBy::Box => {
            let (lo, hi) = (Vec3::from_array(p.min), Vec3::from_array(p.max));
            (0..n)
                .map(|f| {
                    let c = m.face_center(f);
                    c.cmpge(lo).all() && c.cmple(hi).all()
                })
                .collect()
        }
        GroupBy::Material => {
            let index = match m.materials.iter().position(|x| *x == p.material) {
                Some(i) => i as i32,
                None if p.material.is_empty() && m.materials.is_empty() => 0,
                None if p.material.is_empty() => -1,
                None => {
                    return Err(unknown(format!(
                        "no face of the mesh has material `{}`",
                        p.material
                    )));
                }
            };
            m.face_materials().iter().map(|&i| i == index).collect()
        }
        GroupBy::Index => (0..n)
            .map(|f| f as u64 >= u64::from(p.start) && (f as u64) < u64::from(p.end))
            .collect(),
        GroupBy::Random => (0..n)
            .map(|f| ctx.random01(f as u64, RANDOM_SALT) < p.fraction)
            .collect(),
        GroupBy::Union | GroupBy::Intersection | GroupBy::Complement => {
            let groups = p
                .groups
                .iter()
                .map(|g| faces_of(m, g))
                .collect::<Result<Vec<_>, _>>()?;
            (0..n)
                .map(|f| match p.by {
                    GroupBy::Intersection => groups.iter().all(|g| g[f]),
                    GroupBy::Complement => !groups.iter().any(|g| g[f]),
                    _ => groups.iter().any(|g| g[f]),
                })
                .collect()
        }
    };
    if let Some(c) = m.faces.get(&p.name)
        && c.ty() != ColumnType::Bool
    {
        return Err(unknown(format!(
            "face attribute `{}` is a `{}`, so it cannot be a group",
            p.name,
            c.ty().name()
        )));
    }
    let mut out = (**m).clone();
    out.set_group(&p.name, sel)?;
    output(out)
}

/// Parameters of `poly.material`.
#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct MaterialParams {
    /// The face group to paint; empty for every face.
    pub group: String,
    /// A material file, relative to the project root
    /// (`materials/stone.material.json`); empty for the default material.
    #[schemars(extend("x-real-asset" = "material"))]
    pub material: String,
}

impl Params for MaterialParams {
    fn validate(&self) -> Result<(), String> {
        name_or_empty("group", &self.group)
    }
}

fn material(ctx: &NodeCtx, p: &MaterialParams) -> Result<Output, NodeError> {
    let m = ctx.need_polymesh("mesh")?;
    let sel = faces_of(m, &p.group)?;
    let mut out = (**m).clone();
    out.set_material(&sel, &p.material)?;
    output(out)
}

/// What an attribute has one value per.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DomainParam {
    /// Each point the group's faces use.
    Point,
    /// Each corner of the group's faces.
    Corner,
    /// Each face of the group.
    #[default]
    Face,
}

impl From<DomainParam> for AttrDomain {
    fn from(d: DomainParam) -> AttrDomain {
        match d {
            DomainParam::Point => AttrDomain::Point,
            DomainParam::Corner => AttrDomain::Corner,
            DomainParam::Face => AttrDomain::Face,
        }
    }
}

/// The type of an attribute's values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TypeParam {
    /// A number.
    Float,
    /// A whole number.
    Int,
    /// `true` or `false`.
    Bool,
    /// Two numbers.
    Vec2,
    /// Three numbers.
    Vec3,
    /// Four numbers.
    Vec4,
    /// A rotation as a quaternion `[x, y, z, w]`, normalised.
    Quat,
}

/// A constant attribute value: a number, `true` or `false`, or a list of
/// two to four numbers.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Constant {
    /// `true` or `false`.
    Bool(bool),
    /// A number.
    Number(f64),
    /// Two to four numbers.
    List(Vec<f64>),
}

impl<'de> Deserialize<'de> for Constant {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let bad = || {
            D::Error::custom("`value` must be a number, true or false, or a list of 2 to 4 numbers")
        };
        match Json::deserialize(d)? {
            Json::Bool(b) => Ok(Constant::Bool(b)),
            Json::Number(n) => n.as_f64().map(Constant::Number).ok_or_else(bad),
            Json::Array(a) => a
                .iter()
                .map(Json::as_f64)
                .collect::<Option<Vec<f64>>>()
                .map(Constant::List)
                .ok_or_else(bad),
            _ => Err(bad()),
        }
    }
}

/// Parameters of `poly.attribute`.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct AttributeParams {
    /// What the attribute has one value per.
    pub domain: DomainParam,
    /// The attribute's name: lowercase letters, digits and `_`, starting with
    /// a letter (`color` is the vertex colour). Not `material`, which
    /// `poly.material` sets.
    pub name: String,
    /// The type of the values; when omitted, a number is a `float`, `true`
    /// or `false` a `bool`, and a list of 2, 3 or 4 numbers a `vec2`, `vec3`
    /// or `vec4`.
    #[serde(rename = "type")]
    pub ty: Option<TypeParam>,
    /// The value given to every element of the group's faces.
    pub value: Constant,
    /// The face group whose faces (or their corners or points) get the
    /// value; empty for every face.
    pub group: String,
}

impl Default for AttributeParams {
    fn default() -> Self {
        AttributeParams {
            domain: DomainParam::Face,
            name: poly::COLOR.into(),
            ty: None,
            value: Constant::List(vec![1.0, 1.0, 1.0]),
            group: String::new(),
        }
    }
}

impl AttributeParams {
    /// The value as the attribute's type.
    fn attr_value(&self) -> Result<AttrValue, String> {
        let float = |x: f64| -> Result<f32, String> {
            let v = x as f32;
            if v.is_finite() {
                Ok(v)
            } else {
                Err(format!("`value` must be finite, got {x}"))
            }
        };
        let list = |n: usize| -> Result<Vec<f32>, String> {
            match &self.value {
                Constant::List(v) if v.len() == n => v.iter().map(|&x| float(x)).collect(),
                _ => Err(format!("`value` must be a list of {n} numbers")),
            }
        };
        let ty = match (self.ty, &self.value) {
            (Some(t), _) => t,
            (None, Constant::Bool(_)) => TypeParam::Bool,
            (None, Constant::Number(_)) => TypeParam::Float,
            (None, Constant::List(v)) => match v.len() {
                2 => TypeParam::Vec2,
                3 => TypeParam::Vec3,
                4 => TypeParam::Vec4,
                n => return Err(format!("`value` lists {n} numbers; give 2, 3 or 4")),
            },
        };
        Ok(match ty {
            TypeParam::Float => match self.value {
                Constant::Number(x) => AttrValue::Float(float(x)?),
                _ => return Err("`value` must be a number for a `float`".into()),
            },
            TypeParam::Int => match self.value {
                Constant::Number(x)
                    if x.fract() == 0.0 && x >= f64::from(i32::MIN) && x <= f64::from(i32::MAX) =>
                {
                    AttrValue::Int(x as i32)
                }
                _ => return Err("`value` must be a whole number for an `int`".into()),
            },
            TypeParam::Bool => match self.value {
                Constant::Bool(b) => AttrValue::Bool(b),
                _ => return Err("`value` must be true or false for a `bool`".into()),
            },
            TypeParam::Vec2 => {
                let v = list(2)?;
                AttrValue::Vec2([v[0], v[1]])
            }
            TypeParam::Vec3 => AttrValue::Vec3(Vec3::from_slice(&list(3)?)),
            TypeParam::Vec4 => {
                let v = list(4)?;
                AttrValue::Vec4([v[0], v[1], v[2], v[3]])
            }
            TypeParam::Quat => {
                let Constant::List(v) = &self.value else {
                    return Err("`value` must be a list of 4 numbers for a `quat`".into());
                };
                list(4)?;
                let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2] + v[3] * v[3]).sqrt();
                if !(len > 0.0 && len.is_finite()) {
                    return Err("`value` must not be a zero quaternion".into());
                }
                AttrValue::Quat(Quat::from_xyzw(
                    (v[0] / len) as f32,
                    (v[1] / len) as f32,
                    (v[2] / len) as f32,
                    (v[3] / len) as f32,
                ))
            }
        })
    }
}

impl Params for AttributeParams {
    fn validate(&self) -> Result<(), String> {
        if self.name == poly::MATERIAL {
            return Err(
                "`material` indexes the material table; set it with `poly.material`".into(),
            );
        }
        let value = self.attr_value()?;
        poly::check_attribute(&self.name, self.domain.into(), value.ty())
            .map_err(|e| format!("`name`: {e}"))?;
        name_or_empty("group", &self.group)
    }
}

fn attribute(ctx: &NodeCtx, p: &AttributeParams) -> Result<Output, NodeError> {
    let m = ctx.need_polymesh("mesh")?;
    let mut value = p.attr_value()?;
    let domain: AttrDomain = p.domain.into();
    let attrs = match domain {
        AttrDomain::Point => &m.points,
        AttrDomain::Corner => &m.corners,
        AttrDomain::Face => &m.faces,
    };
    if let Some(c) = attrs.get(&p.name) {
        // A colour without alpha is opaque.
        if let (ColumnType::Vec4, AttrValue::Vec3(v)) = (c.ty(), value)
            && p.name == poly::COLOR
        {
            value = AttrValue::Vec4([v.x, v.y, v.z, 1.0]);
        }
        if c.ty() != value.ty() {
            return Err(unknown(format!(
                "{} attribute `{}` is a `{}`, not a `{}`; give `type` to match it, or use another name",
                domain.name(),
                p.name,
                c.ty().name(),
                value.ty().name()
            )));
        }
    }
    let sel = faces_of(m, &p.group)?;
    let mut out = (**m).clone();
    out.fill_attribute(domain, &p.name, value, &sel)?;
    output(out)
}

/// Parameters of `poly.from_mesh` (none).
#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FromMeshParams {}

impl Params for FromMeshParams {}

fn from_mesh(ctx: &NodeCtx, _: &FromMeshParams) -> Result<Output, NodeError> {
    let input = ctx.need_mesh("mesh")?;
    let mut out = poly::from_mesh(&input.mesh)?;
    if !input.material.is_empty() {
        let all = vec![true; out.face_count()];
        out.set_material(&all, &input.material)?;
    }
    output(out)
}

/// Parameters of `poly.to_mesh`.
#[derive(Debug, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ToMeshParams {
    /// Only the faces drawn with this material file; empty for every face,
    /// drawn with the first material any face uses.
    #[schemars(extend("x-real-asset" = "material"))]
    pub material: String,
}

impl Params for ToMeshParams {}

fn to_mesh(ctx: &NodeCtx, p: &ToMeshParams) -> Result<Output, NodeError> {
    let m = ctx.need_polymesh("mesh")?;
    let (mesh, material) = if p.material.is_empty() {
        let used = m.face_materials();
        let material = (0..m.part_count())
            .map(|i| m.part_material(i))
            .enumerate()
            .find(|(i, path)| !path.is_empty() && used.contains(&(*i as i32)))
            .map_or(String::new(), |(_, path)| path.to_string());
        (m.to_mesh()?, material)
    } else {
        let Some(i) = m.materials.iter().position(|x| *x == p.material) else {
            return Err(unknown(format!(
                "no face of the mesh has material `{}`",
                p.material
            )));
        };
        let mut parts = m.to_meshes()?;
        (parts.swap_remove(i), p.material.clone())
    };
    Ok(Output::Mesh(Arc::new(MeshValue {
        mesh,
        material,
        asset: ctx.key,
    })))
}

const POLY: &[PortType] = &[PortType::PolyMesh];
const ONE_POLY: &[PortDef] = &[PortDef {
    name: "mesh",
    accepts: POLY,
    required: true,
    doc: "The input polygon mesh.",
}];
const ONE_MESH: &[PortDef] = &[PortDef {
    name: "mesh",
    accepts: &[PortType::Mesh],
    required: true,
    doc: "The input triangle mesh.",
}];

/// The spec of a modelling kind at version 1: whole (it streams once
/// ADR-0062's PR "stream" makes it global), with no file parameters.
fn spec(
    kind: &'static str,
    doc: &'static str,
    inputs: &'static [PortDef],
    output: PortType,
) -> NodeSpec {
    NodeSpec {
        kind,
        version: 1,
        oldest: 1,
        migrations: &[],
        streaming: Streaming::Whole,
        doc,
        inputs,
        variadic: None,
        output,
        uses_seed: false,
        file_params: &[],
    }
}

pub(super) fn register(r: &mut NodeRegistry) {
    let mesh = PortType::PolyMesh;
    r.register(NodeDef::new(
        spec(
            "poly.box",
            "A box centred on the origin, its edges and corners rounded with `radius` (0 gives six quads), with UVs in metres and the groups `top`, `bottom` and `side`.",
            &[],
            mesh,
        ),
        box_,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.icosphere",
            "A sphere of nearly equal triangles centred on the origin: an icosahedron split `subdivisions` times, with smooth normals and UVs in metres.",
            &[],
            mesh,
        ),
        icosphere,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.cone",
            "A cone, cylinder or frustum about +Y centred on the origin, with optional flat ends, smooth normals on its side, UVs in metres and the groups `side`, `top` and `bottom`.",
            &[],
            mesh,
        ),
        cone,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.torus",
            "A torus about +Y centred on the origin, with smooth normals and UVs in metres.",
            &[],
            mesh,
        ),
        torus,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.stairs",
            "A closed flight of stairs along -Z from the origin, `width` wide, with UVs in metres and the groups `tread`, `riser`, `side`, `back` and `bottom`.",
            &[],
            mesh,
        ),
        stairs,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.arch",
            "A round arch in the XY plane standing on y = 0, `depth` deep along Z, with smooth normals on its curves, UVs in metres and the groups `front`, `back`, `inner`, `outer` and `bottom`.",
            &[],
            mesh,
        ),
        arch,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.transform",
            "Scales, rotates (Euler degrees, X then Y then Z) and moves a polygon mesh; normals follow, and a mirroring scale rewinds the faces.",
            ONE_POLY,
            mesh,
        ),
        transform,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.mirror",
            "Reflects a polygon mesh across a plane square to an axis, keeping its faces' fronts; with `keep`, the original stays, joined to its reflection on the plane.",
            ONE_POLY,
            mesh,
        ),
        mirror,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.weld",
            "Merges the points of a polygon mesh closer than `distance`, keeping the lowest-numbered one; faces that collapse go.",
            ONE_POLY,
            mesh,
        ),
        weld,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.flip",
            "Turns the faces of a group (or every face) around, so their fronts face the other way.",
            ONE_POLY,
            mesh,
        ),
        flip,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            variadic: Some(POLY),
            ..spec(
                "poly.merge",
                "Merges any number of polygon meshes (inputs on any port names, in natural name order) into one; attributes and material tables are unioned.",
                &[],
                mesh,
            )
        },
        merge,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            uses_seed: true,
            ..spec(
                "poly.group",
                "Writes a face group: the faces facing a direction, inside a box, of a material, in an index range, a random fraction (from the seed), or other groups' union, intersection or complement.",
                ONE_POLY,
                mesh,
            )
        },
        group,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.material",
            "Sets the material file the faces of a group (or every face) are drawn with.",
            ONE_POLY,
            mesh,
        ),
        material,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.attribute",
            "Sets an attribute to a constant on the faces of a group, their corners or their points (every face when the group is empty): a colour, for example.",
            ONE_POLY,
            mesh,
        ),
        attribute,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.from_mesh",
            "A polygon mesh of a triangle mesh: one face per triangle, points shared where positions are equal, and its UVs, normals, colours and material kept.",
            ONE_MESH,
            mesh,
        ),
        from_mesh,
    ));
    r.register(NodeDef::new(
        spec(
            "poly.to_mesh",
            "Triangulates a polygon mesh into a mesh: every face, or only the faces of one material.",
            ONE_POLY,
            PortType::Mesh,
        ),
        to_mesh,
    ));
}
