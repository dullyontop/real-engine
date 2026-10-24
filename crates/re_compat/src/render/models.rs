//! `MeshPart`s drawn with model files (ADR-0048 §5).
//!
//! A `MeshPart`'s `MeshId` resolves through the `[compat.assets]` table to a
//! glTF file, which the host loads into a [`Model`] (the view itself reads no
//! files). The model is scaled to the part's `Size`. A model with a skin is a
//! skinned item on `re_anim`'s skeleton: each `Bone` under the part drives
//! the joint of the same name through its `Transform`, on top of the joint's
//! rest pose; a bone with no joint, or a joint with no bone, is `RE9440` once
//! per part. A model without a skin draws as a rigid mesh. An id the table
//! lacks draws the part as a block, as before; a file that does not load
//! draws the block too and warns once with `RE9350`.
//!
//! Skinned parts are few and their bones move every frame, so this pass looks
//! at every `MeshPart` each frame rather than through the change journal.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use glam::{Mat4, Vec3};
use re_core::Diagnostic;
use re_scene::Entity;
use re_types::anim::{SkinData, Trs};
use re_types::mesh::{Aabb, MeshData, SkinWeights};

use super::{FrameItem, Hub, MeshKey, RenderFrame, parts};
use crate::gui::props::Scope;
use crate::instance::tree;
use crate::scene::CompatConfig;
use crate::scene::asset_key;

/// One mesh of a model: in the skin's bind space when it has weights,
/// otherwise in the model's space.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelMesh {
    /// The geometry.
    pub data: MeshData,
    /// The skin weights, for a mesh the skin deforms.
    pub weights: Option<SkinWeights>,
}

/// A model file as the view draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    /// The meshes.
    pub meshes: Vec<ModelMesh>,
    /// The skin of the meshes that have weights.
    pub skin: Option<SkinData>,
    /// The bound of every vertex, in the model's space (bind pose): the box
    /// the part's `Size` stretches.
    pub bounds: Aabb,
}

impl Model {
    /// A model of `meshes`, computing its bounds. Weights are dropped from a
    /// mesh when there is no skin or they do not fit it.
    pub fn new(mut meshes: Vec<ModelMesh>, skin: Option<SkinData>) -> Model {
        let joints = skin.as_ref().map_or(0, |s| s.joints.len());
        for m in &mut meshes {
            if m.weights
                .as_ref()
                .is_some_and(|w| joints == 0 || !w.is_well_formed(&m.data, joints))
            {
                m.weights = None;
            }
        }
        let skin = skin.filter(|_| meshes.iter().any(|m| m.weights.is_some()));
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for v in meshes.iter().flat_map(|m| &m.data.vertices) {
            let p = Vec3::from(v.pos);
            min = min.min(p);
            max = max.max(p);
        }
        let bounds = if min.cmple(max).all() {
            Aabb { min, max }
        } else {
            Aabb {
                min: Vec3::ZERO,
                max: Vec3::ZERO,
            }
        };
        Model {
            meshes,
            skin,
            bounds,
        }
    }
}

/// Loads the model at a project path; the error says why it did not load.
pub type LoadModelFn = dyn Fn(&str) -> Result<Model, String> + Send + Sync;

/// How `MeshId`s become models: the substitute table and the host's loader.
/// Install one per VM with [`configure_models`]; without a loader every
/// `MeshPart` draws as a block.
#[derive(Clone, Default)]
pub struct ModelSettings {
    /// Asset id to project path, keyed as [`asset_key`] gives them (the
    /// `[compat] assets` table).
    pub assets: BTreeMap<String, String>,
    /// The host's loader.
    pub load: Option<Arc<LoadModelFn>>,
}

impl std::fmt::Debug for ModelSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelSettings")
            .field("assets", &self.assets.len())
            .field("load", &self.load.is_some())
            .finish()
    }
}

impl ModelSettings {
    /// The settings of a project: its `[compat] assets` table and a loader.
    pub fn from_config(config: &CompatConfig, load: Option<Arc<LoadModelFn>>) -> Self {
        ModelSettings {
            assets: config.assets.clone(),
            load,
        }
    }

    /// The project path of a `MeshId`, when there is a loader to read it.
    pub fn resolve(&self, id: &str) -> Option<&str> {
        self.load.as_ref()?;
        let id = id.trim();
        if id.is_empty() {
            return None;
        }
        self.assets.get(asset_key(id)).map(String::as_str)
    }
}

/// Installs the model settings of this VM.
pub fn configure_models(lua: &mlua::Lua, settings: ModelSettings) {
    super::hub_mut(lua, |h| h.models.settings = settings);
}

/// A model as uploaded: its meshes' keys, in [`Model::meshes`] order.
struct Uploaded {
    model: Arc<Model>,
    keys: Vec<MeshKey>,
}

/// What the view keeps about models from frame to frame.
#[derive(Default)]
pub(super) struct Models {
    pub(super) settings: ModelSettings,
    /// Each loaded path (`None`: it did not load) and the frame it was last
    /// drawn.
    loaded: HashMap<String, (Option<Uploaded>, u64)>,
    /// The parts already warned about their bones.
    warned: HashSet<Entity>,
}

impl Models {
    /// Nothing loaded yet, with these settings.
    pub(super) fn with(settings: ModelSettings) -> Models {
        Models {
            settings,
            ..Models::default()
        }
    }

    /// The model path of a part this pass draws (a `MeshPart` whose
    /// `MeshId` resolves), so the parts pass leaves it alone.
    pub(super) fn path_of(&self, s: &Scope, e: Entity) -> Option<String> {
        if !s.is_a(e, "MeshPart") {
            return None;
        }
        let id = s.props(e).string("MeshId");
        self.settings.resolve(&id).map(str::to_string)
    }

    /// Releases models not drawn for `after` frames.
    pub(super) fn collect(&mut self, frame: u64, after: u64, out: &mut RenderFrame) {
        self.loaded.retain(|_, (up, last)| {
            let keep = frame.saturating_sub(*last) <= after;
            if !keep && let Some(up) = up {
                out.released_meshes.extend(&up.keys);
            }
            keep
        });
    }
}

/// The meshes of `path`, loaded and sent the first time.
fn model(hub: &mut Hub, path: &str, out: &mut RenderFrame) -> Option<(Arc<Model>, Vec<MeshKey>)> {
    let frame = hub.frame;
    if let Some((up, last)) = hub.models.loaded.get_mut(path) {
        *last = frame;
        return up.as_ref().map(|u| (u.model.clone(), u.keys.clone()));
    }
    let load = hub.models.settings.load.clone()?;
    let up = match load(path) {
        Ok(m) if !m.meshes.is_empty() => {
            let model = Arc::new(m);
            let keys = model
                .meshes
                .iter()
                .map(|m| match &m.weights {
                    Some(w) => hub.new_skinned_mesh(out, m.data.clone(), w.clone()),
                    None => hub.new_mesh(out, m.data.clone()),
                })
                .collect();
            Some(Uploaded { model, keys })
        }
        result => {
            let why = result.err().unwrap_or_else(|| "it has no meshes".into());
            out.diagnostics.push(
                Diagnostic::warning(
                    crate::codes::UNKNOWN_ASSET,
                    format!(
                        "the model {path:?} cannot be drawn ({why}); its MeshParts draw as blocks"
                    ),
                )
                .with_hint(
                    "give [compat.assets] a .glb file, or a .gltf with its buffers embedded",
                ),
            );
            None
        }
    };
    let found = up.as_ref().map(|u| (u.model.clone(), u.keys.clone()));
    hub.models.loaded.insert(path.to_string(), (up, frame));
    found
}

/// Adds the items of the `MeshPart`s in `list` that the parts pass left out.
pub(super) fn draw(
    s: &Scope,
    hub: &mut Hub,
    list: &[Entity],
    in_workspace: &dyn Fn(Entity) -> bool,
    out: &mut RenderFrame,
) {
    for &e in list {
        if !in_workspace(e) {
            continue;
        }
        let Some(path) = hub.models.path_of(s, e) else {
            continue;
        };
        let Some(info) = parts::info(s, e) else {
            continue;
        };
        hub.use_material(out, info.key);
        let Some((model, keys)) = model(hub, &path, out) else {
            let item = info.item(hub, out);
            out.items.push(item);
            continue;
        };
        let size = info.geom.size;
        let extent = model.bounds.max - model.bounds.min;
        let scale = Vec3::select(extent.cmpgt(Vec3::splat(1e-6)), size / extent, Vec3::ONE);
        let center = (model.bounds.min + model.bounds.max) * 0.5;
        let world = Mat4::from_rotation_translation(info.rot, info.pos)
            * Mat4::from_scale(scale)
            * Mat4::from_translation(-center);
        let entity = re_scene::entity_id(e);
        let skinned = model.skin.as_ref().map(|skin| {
            let palette = pose(s, hub, e, skin, out);
            let base = out.joint_palette.len();
            out.joint_palette.extend(&palette);
            let bounds = re_anim::skin::skinned_bounds(skin, &palette, &world)
                .unwrap_or_else(|| model.bounds.transformed(&world));
            (base as u32, palette.len() as u32, bounds)
        });
        for (m, key) in model.meshes.iter().zip(keys) {
            match (&m.weights, skinned) {
                (Some(_), Some((palette, joints, bounds))) => out.skinned.push(SkinnedFrameItem {
                    entity,
                    mesh: key,
                    material: info.key,
                    world,
                    cast_shadow: info.cast_shadow,
                    palette,
                    joints,
                    bounds,
                }),
                _ => out.items.push(FrameItem {
                    entity,
                    mesh: key,
                    material: info.key,
                    world,
                    cast_shadow: info.cast_shadow,
                }),
            }
        }
    }
}

/// The joint palette of a skinned part: each joint at its rest pose, moved
/// by the `Transform` of the bone with its name.
fn pose(s: &Scope, hub: &mut Hub, e: Entity, skin: &SkinData, out: &mut RenderFrame) -> Vec<Mat4> {
    let mut bones: BTreeMap<String, Mat4> = BTreeMap::new();
    for b in tree::descendants(s.w, e) {
        if !s.is_a(b, "Bone") {
            continue;
        }
        let t = s
            .props(b)
            .get("Transform")
            .and_then(|v| crate::physics::pose_of(&v))
            .unwrap_or(re_physics::Pose::IDENTITY);
        bones
            .entry(tree::name(s.w, b))
            .or_insert(Mat4::from_rotation_translation(t.rot, t.pos));
    }
    let mut pose = re_anim::skin::Pose::rest(skin);
    let mut unmatched_joints = Vec::new();
    for (j, joint) in skin.joints.iter().enumerate() {
        match bones.remove(&joint.name) {
            Some(t) => {
                let (scale, rotation, translation) =
                    (joint.rest.to_mat4() * t).to_scale_rotation_translation();
                pose.joints[j] = Trs {
                    translation,
                    rotation: rotation.normalize(),
                    scale,
                };
            }
            None => unmatched_joints.push(joint.name.as_str()),
        }
    }
    if (!bones.is_empty() || !unmatched_joints.is_empty()) && hub.models.warned.insert(e) {
        let list = |names: Vec<&str>| {
            let more = names.len().saturating_sub(5);
            let mut s = names.into_iter().take(5).collect::<Vec<_>>().join(", ");
            if more > 0 {
                s.push_str(&format!(" and {more} more"));
            }
            s
        };
        let mut parts = Vec::new();
        if !bones.is_empty() {
            parts.push(format!(
                "the bones [{}] name no joint of the skin `{}`",
                list(bones.keys().map(String::as_str).collect()),
                skin.name
            ));
        }
        if !unmatched_joints.is_empty() {
            parts.push(format!(
                "the joints [{}] have no bone; those joints stay at rest",
                list(unmatched_joints)
            ));
        }
        out.diagnostics.push(
            Diagnostic::warning(
                crate::codes::BONE_MISMATCH,
                format!("{}: {}", tree::full_name(s.w, e), parts.join(", and ")),
            )
            .with_hint("name each Bone after a joint of the model, or run `real explain RE9440`"),
        );
    }
    let mut model = re_anim::skin::ModelPose::default();
    model.compute(skin, &pose);
    let mut palette = Vec::with_capacity(skin.joints.len());
    re_anim::skin::palette_into(skin, &model, &mut palette);
    palette
}

/// One skinned thing to draw: a skinned mesh posed by a run of
/// [`RenderFrame::joint_palette`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SkinnedFrameItem {
    /// The part.
    pub entity: re_types::EntityId,
    /// The mesh, uploaded by a frame's [`RenderFrame::new_skinned_meshes`].
    pub mesh: MeshKey,
    /// The material.
    pub material: super::MaterialKey,
    /// Model-to-world transform, applied after skinning.
    pub world: Mat4,
    /// `CastShadow`.
    pub cast_shadow: bool,
    /// The index of the item's first matrix in the palette.
    pub palette: u32,
    /// The number of palette matrices.
    pub joints: u32,
    /// A world-space bound of the posed mesh.
    pub bounds: Aabb,
}
