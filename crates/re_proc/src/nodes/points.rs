//! `points.filter`, `points.pick` and `points.instance`.

use std::f32::consts::TAU;
use std::sync::Arc;

use glam::{Quat, Vec3};
use indexmap::IndexMap;
use re_geom::math::{cross, quat_from_yaw};
use re_reflect::Value;
use re_scene::PrefabFile;
use schemars::JsonSchema;
use serde::Deserialize;

use super::scatter::{Limits, limits_of};
use super::{
    NodeCtx, NodeDef, NodeError, NodeRegistry, NodeSpec, Params, PortDef, positive, within,
};
use crate::value::{FragItem, Fragment, Output, Points, PortType, PrefabUse};

/// Parameters of `points.filter`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct FilterParams {
    /// Drop points below this height (metres).
    pub height_min: Option<f32>,
    /// Drop points above this height (metres).
    pub height_max: Option<f32>,
    /// Drop points flatter than this slope (rise over run: 0 is flat, 1 is 45°).
    pub slope_min: Option<f32>,
    /// Drop points steeper than this slope (rise over run).
    pub slope_max: Option<f32>,
    /// With a `mask` field connected: drop points where the mask is below this.
    pub mask_min: f32,
    /// Keep each point with this probability (decided per point id, so the
    /// same points survive whatever else changes).
    pub density: f32,
}

impl Default for FilterParams {
    fn default() -> Self {
        FilterParams {
            height_min: None,
            height_max: None,
            slope_min: None,
            slope_max: None,
            mask_min: 0.5,
            density: 1.0,
        }
    }
}

limits_of!(FilterParams);

impl Params for FilterParams {
    fn validate(&self) -> Result<(), String> {
        self.limits().validate()?;
        within("mask_min", self.mask_min, -1.0e6, 1.0e6)?;
        within("density", self.density, 0.0, 1.0)
    }
}

fn filter(ctx: &NodeCtx, p: &FilterParams) -> Result<Output, String> {
    let input = ctx.need_points("points")?;
    let mask = ctx.field("mask");
    let limits: Limits = p.limits();
    let points = input
        .points
        .iter()
        .filter(|pt| limits.keeps(pt))
        .filter(|pt| {
            mask.is_none_or(|m| {
                m.0.height_at(pt.position.x, pt.position.z)
                    .is_some_and(|v| v >= p.mask_min)
            })
        })
        .filter(|pt| p.density >= 1.0 || ctx.random01(u64::from(pt.id), 1) < p.density)
        .cloned()
        .collect();
    Ok(Output::Points(Arc::new(Points { points })))
}

/// One choice of [`PickParams`].
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    /// The tag given to the points that pick this choice.
    pub tag: String,
    /// The relative weight (0 or more).
    #[serde(default = "one")]
    pub weight: f32,
}

fn one() -> f32 {
    1.0
}

/// Parameters of `points.pick`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct PickParams {
    /// The choices; each point picks one at random by weight.
    pub choices: Vec<Choice>,
}

impl Params for PickParams {
    fn validate(&self) -> Result<(), String> {
        if self.choices.is_empty() {
            return Err("`choices` needs at least one `{ tag, weight }`".into());
        }
        let mut total = 0.0;
        for c in &self.choices {
            within("weight", c.weight, 0.0, 1.0e6)?;
            total += c.weight;
        }
        if total <= 0.0 {
            return Err("the weights of `choices` add up to 0".into());
        }
        Ok(())
    }
}

fn pick(ctx: &NodeCtx, p: &PickParams) -> Result<Output, String> {
    let input = ctx.need_points("points")?;
    let total: f32 = p.choices.iter().map(|c| c.weight).sum();
    let points = input
        .points
        .iter()
        .map(|pt| {
            let r = ctx.random01(u64::from(pt.id), 2) * total;
            let mut acc = 0.0;
            let mut tag = &p.choices[p.choices.len() - 1].tag;
            for c in &p.choices {
                acc += c.weight;
                if r < acc {
                    tag = &c.tag;
                    break;
                }
            }
            let mut out = pt.clone();
            out.tag = tag.clone();
            out
        })
        .collect();
    Ok(Output::Points(Arc::new(Points { points })))
}

/// Parameters of `points.instance`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct InstanceParams {
    /// The prefab placed at untagged points (or at every point when
    /// `prefabs` is empty), relative to the project root.
    pub prefab: String,
    /// A prefab per tag (from `points.pick`); points whose tag has none and
    /// that `prefab` does not cover are skipped.
    #[schemars(with = "std::collections::BTreeMap<String, String>")]
    pub prefabs: IndexMap<String, String>,
    /// Turn each instance about Y by a random angle.
    pub random_yaw: bool,
    /// Tilt each instance so its +Y follows the surface normal.
    pub align_to_normal: bool,
    /// The smallest random uniform scale.
    pub scale_min: f32,
    /// The largest random uniform scale.
    pub scale_max: f32,
    /// Raise (or sink) each instance by this many metres.
    pub offset_y: f32,
    /// The instance entities' name; the prefab file's name when empty.
    pub name: String,
}

impl Default for InstanceParams {
    fn default() -> Self {
        InstanceParams {
            prefab: String::new(),
            prefabs: IndexMap::new(),
            random_yaw: true,
            align_to_normal: false,
            scale_min: 1.0,
            scale_max: 1.0,
            offset_y: 0.0,
            name: String::new(),
        }
    }
}

impl Params for InstanceParams {
    fn validate(&self) -> Result<(), String> {
        if self.prefab.is_empty() && self.prefabs.is_empty() {
            return Err("give `prefab` (a prefab file) or `prefabs` (a prefab per tag)".into());
        }
        positive("scale_min", self.scale_min)?;
        positive("scale_max", self.scale_max)?;
        if self.scale_min > self.scale_max {
            return Err(format!(
                "`scale_min` ({}) is above `scale_max` ({})",
                self.scale_min, self.scale_max
            ));
        }
        within("offset_y", self.offset_y, -1.0e4, 1.0e4)
    }
}

/// The shortest rotation taking +Y to `n`.
fn align_y_to(n: Vec3) -> Quat {
    let d = n.y;
    if d >= 1.0 - 1e-6 {
        return Quat::IDENTITY;
    }
    if d <= -1.0 + 1e-6 {
        return Quat::from_xyzw(1.0, 0.0, 0.0, 0.0);
    }
    let axis = cross(Vec3::Y, n);
    let (x, y, z, w) = (axis.x, axis.y, axis.z, 1.0 + d);
    let len = (x * x + y * y + z * z + w * w).sqrt();
    Quat::from_xyzw(x / len, y / len, z / len, w / len)
}

fn file_stem(path: &str) -> &str {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    name.split('.').next().unwrap_or(name)
}

fn load_prefab(ctx: &NodeCtx, path: &str) -> Result<Arc<PrefabUse>, NodeError> {
    load_prefab_text(ctx, path).map_err(|message| NodeError {
        code: crate::codes::PREFAB_FAILED,
        message,
    })
}

fn load_prefab_text(ctx: &NodeCtx, path: &str) -> Result<Arc<PrefabUse>, String> {
    let bytes = match ctx.files.get(path) {
        Some(Ok(b)) => b.clone(),
        Some(Err(e)) => return Err(format!("prefab `{path}` could not be read: {e}")),
        None => return Err(format!("prefab `{path}` was not loaded")),
    };
    let text =
        std::str::from_utf8(&bytes).map_err(|_| format!("prefab `{path}` is not UTF-8 text"))?;
    let file =
        PrefabFile::from_json(text).map_err(|e| format!("prefab `{path}` is not valid: {e}"))?;
    Ok(Arc::new(PrefabUse {
        path: path.to_string(),
        file,
    }))
}

fn instance(ctx: &NodeCtx, p: &InstanceParams) -> Result<Output, NodeError> {
    let input = ctx.need_points("points")?;
    let mut loaded: IndexMap<String, Arc<PrefabUse>> = IndexMap::new();
    let mut items = Vec::with_capacity(input.points.len());
    for pt in &input.points {
        let path = match p.prefabs.get(&pt.tag) {
            Some(path) => path.as_str(),
            None if !p.prefab.is_empty() => p.prefab.as_str(),
            None => continue,
        };
        let prefab = match loaded.get(path) {
            Some(u) => u.clone(),
            None => {
                let u = load_prefab(ctx, path)?;
                loaded.insert(path.to_string(), u.clone());
                u
            }
        };
        let id = u64::from(pt.id);
        let mut rotation = if p.random_yaw {
            quat_from_yaw(ctx.random01(id, 3) * TAU)
        } else {
            Quat::IDENTITY
        };
        if p.align_to_normal {
            rotation = align_y_to(pt.normal) * rotation;
        }
        let scale = p.scale_min + (p.scale_max - p.scale_min) * ctx.random01(id, 4);
        let name = if p.name.is_empty() {
            file_stem(path).to_string()
        } else {
            p.name.clone()
        };
        items.push(FragItem {
            key: format!("{}/{}", ctx.node_id, pt.id),
            name,
            parent_key: None,
            components: vec![(
                "Transform".into(),
                vec![
                    (
                        "translation".into(),
                        Value::Vec3(pt.position + Vec3::new(0.0, p.offset_y, 0.0)),
                    ),
                    ("rotation".into(), Value::Quat(rotation)),
                    ("scale".into(), Value::Vec3(Vec3::splat(scale))),
                ],
            )],
            prefab: Some(prefab),
        });
    }
    Ok(Output::Fragment(Arc::new(Fragment {
        items,
        assets: Vec::new(),
    })))
}

const POINTS: &[PortType] = &[PortType::Points];
const FIELD: &[PortType] = &[PortType::Field];

pub(super) fn register(r: &mut NodeRegistry) {
    r.register(NodeDef::new(
        NodeSpec {
            kind: "points.filter",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Cell,
            doc: "Keeps the points within height and slope limits, above `mask_min` on an optional mask field, and with probability `density`.",
            inputs: &[
                PortDef {
                    name: "points",
                    accepts: POINTS,
                    required: true,
                    doc: "The points to filter.",
                },
                PortDef {
                    name: "mask",
                    accepts: FIELD,
                    required: false,
                    doc: "A field sampled at each point; points below `mask_min` are dropped.",
                },
            ],
            variadic: None,
            output: PortType::Points,
            uses_seed: true,
            file_params: &[],
        },
        filter,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "points.pick",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Cell,
            doc: "Gives each point a tag chosen at random by weight (stable per point id).",
            inputs: &[PortDef {
                name: "points",
                accepts: POINTS,
                required: true,
                doc: "The points to tag.",
            }],
            variadic: None,
            output: PortType::Points,
            uses_seed: true,
            file_params: &[],
        },
        pick,
    ));
    r.register(NodeDef::new(
        NodeSpec {
            kind: "points.instance",
            version: 1,
            oldest: 1,
            migrations: &[],
            streaming: super::Streaming::Cell,
            doc: "Places a prefab file at each point: one entity `<node>/<point id>` per point, with the prefab's entities under it.",
            inputs: &[PortDef {
                name: "points",
                accepts: POINTS,
                required: true,
                doc: "Where to place instances.",
            }],
            variadic: None,
            output: PortType::Fragment,
            uses_seed: true,
            file_params: &["prefab", "prefabs"],
        },
        instance,
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_takes_y_to_the_normal() {
        let n = Vec3::new(0.3, 0.9, -0.2).normalize();
        let q = align_y_to(n);
        assert!((q * Vec3::Y - n).length() < 1e-5);
        assert_eq!(file_stem("prefabs/tree.prefab.json"), "tree");
    }
}
