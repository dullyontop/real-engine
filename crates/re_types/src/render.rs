//! What the renderer draws: one [`RenderSnapshot`] per frame, plus resources
//! uploaded when an asset loads.
//!
//! `re_app` builds the snapshot from the World in its sync step; `re_render`
//! never sees the scene. The snapshot owns its data and is `Send`, which is the
//! seam for a future render thread (ADR-0007). UI is not part of the snapshot:
//! egui output is painted separately.
//!
//! Meshes and textures are uploaded once (`Renderer::upload_*`), materials are
//! created once (`Renderer::create_material`), and all three are referred to by
//! handles. A handle carries a generation, so a handle to a released and reused
//! slot is detected as stale.

use glam::{Mat4, Vec3};
use serde::{Deserialize, Serialize};

use crate::ids::EntityId;

/// Everything the renderer needs for one frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RenderSnapshot {
    /// The camera to draw from.
    pub camera: Camera,
    /// The lights of the frame.
    pub lights: Vec<Light>,
    /// The objects to draw.
    pub items: Vec<DrawItem>,
    /// Debug lines in world space (drawn without depth test against UI).
    pub debug_lines: Vec<DebugLine>,
    /// The clear colour, linear RGBA.
    pub clear: [f32; 4],
    /// The skinned objects to draw (ADR-0039).
    #[serde(default)]
    pub skinned: Vec<SkinnedDrawItem>,
    /// The joint matrices of every skinned item, one run per item
    /// ([`SkinnedDrawItem::palette`]): `joint_model * inverse_bind`, in the skinned
    /// mesh's model space.
    #[serde(default)]
    pub joint_palette: Vec<Mat4>,
}

// The snapshot must stay `Send` (ADR-0007): it is the seam for a render thread.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<RenderSnapshot>();
};

/// A perspective camera.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    /// Camera-to-world transform (the camera entity's global transform). The camera
    /// looks down its local −Z axis with +Y up.
    pub world: Mat4,
    /// Vertical field of view in radians.
    pub fov_y_radians: f32,
    /// Near clip distance (positive).
    pub near: f32,
    /// Far clip distance (greater than `near`).
    pub far: f32,
}

impl Camera {
    /// The world-to-view matrix.
    pub fn view(&self) -> Mat4 {
        self.world.inverse()
    }

    /// The camera position in world space.
    pub fn position(&self) -> Vec3 {
        self.world.w_axis.truncate()
    }
}

impl Default for Camera {
    /// At the origin, looking down −Z, 60° vertical field of view, 0.1 to 1000.
    fn default() -> Self {
        Camera {
            world: Mat4::IDENTITY,
            fov_y_radians: 60f32.to_radians(),
            near: 0.1,
            far: 1000.0,
        }
    }
}

/// The kind of a [`Light`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LightKind {
    /// Parallel rays along `direction` (a sun). `position` and `range` are ignored.
    Directional,
    /// Rays from `position` in every direction, fading out at `range`.
    #[default]
    Point,
}

/// A light.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Light {
    /// Directional or point.
    pub kind: LightKind,
    /// World position (point lights).
    pub position: Vec3,
    /// Direction the light travels, normalised (directional lights).
    pub direction: Vec3,
    /// Linear RGB colour.
    pub color: [f32; 3],
    /// Intensity: lux for directional lights, lumens for point lights.
    pub intensity: f32,
    /// Distance at which a point light's contribution reaches zero.
    pub range: f32,
    /// Whether the light casts shadows.
    pub shadows: bool,
}

impl Default for Light {
    /// A white point light at the origin, matching the scene's `Light` defaults.
    fn default() -> Self {
        Light {
            kind: LightKind::Point,
            position: Vec3::ZERO,
            direction: Vec3::NEG_Y,
            color: [1.0, 1.0, 1.0],
            intensity: 800.0,
            range: 20.0,
            shadows: false,
        }
    }
}

/// One object to draw: a mesh with a material at a world transform.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DrawItem {
    /// The entity the item comes from (for picking and debugging).
    pub entity: EntityId,
    /// The uploaded mesh.
    pub mesh: MeshHandle,
    /// The uploaded material.
    pub material: MaterialHandle,
    /// Model-to-world transform.
    pub world: Mat4,
    /// Per-item flags.
    pub flags: DrawFlags,
}

/// One skinned object to draw: a skinned mesh posed by a run of
/// [`RenderSnapshot::joint_palette`] (ADR-0039).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SkinnedDrawItem {
    /// The entity the item comes from (for picking and debugging).
    pub entity: EntityId,
    /// The uploaded skinned mesh (`Renderer::upload_skinned_mesh`).
    pub mesh: MeshHandle,
    /// The uploaded material.
    pub material: MaterialHandle,
    /// Model-to-world transform, applied after skinning.
    pub world: Mat4,
    /// Per-item flags.
    pub flags: DrawFlags,
    /// The index of the item's first matrix in [`RenderSnapshot::joint_palette`].
    pub palette: u32,
    /// The number of palette matrices (the skin's joint count).
    pub joints: u32,
    /// A conservative world-space bound of the posed mesh, for culling.
    pub bounds: crate::mesh::Aabb,
}

/// Per-item draw flags, a small bit set.
///
/// Serialized as its bits (a number).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DrawFlags(pub u32);

impl DrawFlags {
    /// No flags.
    pub const NONE: DrawFlags = DrawFlags(0);
    /// The item casts shadows.
    pub const CAST_SHADOWS: DrawFlags = DrawFlags(1 << 0);
    /// The item receives shadows.
    pub const RECEIVE_SHADOWS: DrawFlags = DrawFlags(1 << 1);
    /// The item is selected in the editor and drawn highlighted.
    pub const SELECTED: DrawFlags = DrawFlags(1 << 2);

    /// True when every flag of `other` is set in `self`.
    pub const fn contains(self, other: DrawFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// `self` with the flags of `other` added.
    #[must_use]
    pub const fn with(self, other: DrawFlags) -> DrawFlags {
        DrawFlags(self.0 | other.0)
    }

    /// `self` with the flags of `other` removed.
    #[must_use]
    pub const fn without(self, other: DrawFlags) -> DrawFlags {
        DrawFlags(self.0 & !other.0)
    }

    /// True when no flag is set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl std::ops::BitOr for DrawFlags {
    type Output = DrawFlags;
    fn bitor(self, rhs: DrawFlags) -> DrawFlags {
        self.with(rhs)
    }
}

impl std::ops::BitOrAssign for DrawFlags {
    fn bitor_assign(&mut self, rhs: DrawFlags) {
        *self = self.with(rhs);
    }
}

/// A line segment drawn for debugging (physics shapes, rays, gizmos).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DebugLine {
    /// Start, in world space.
    pub a: Vec3,
    /// End, in world space.
    pub b: Vec3,
    /// Linear RGBA colour.
    pub color: [f32; 4],
}

macro_rules! handle {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        ///
        /// Handles are issued by the renderer; `index` is a slot and `generation`
        /// changes every time the slot is reused, so a stale handle is detected.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        pub struct $name {
            /// The slot.
            pub index: u32,
            /// The slot's generation when the handle was issued.
            pub generation: u32,
        }

        impl $name {
            /// A handle for `index` at `generation`.
            pub const fn new(index: u32, generation: u32) -> Self {
                Self { index, generation }
            }
        }
    };
}

handle!(
    /// An uploaded mesh.
    MeshHandle
);
handle!(
    /// An uploaded material.
    MaterialHandle
);
handle!(
    /// An uploaded texture.
    TextureHandle
);

/// How a material's alpha is used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum AlphaMode {
    /// Alpha is ignored.
    #[default]
    Opaque,
    /// Pixels with alpha below `cutoff` are discarded; the rest are opaque.
    Mask {
        /// The alpha threshold.
        cutoff: f32,
    },
    /// Alpha blending.
    Blend,
}

/// How a [`MaterialDesc`] names one of its textures.
///
/// `re_assets` cannot hold renderer handles (subsystem crates never depend on each
/// other, ARCHITECTURE §2), so it produces [`TextureRef::Asset`]. `re_app` loads and
/// uploads that texture (`Renderer::upload_texture`) and replaces the reference with
/// [`TextureRef::Handle`] before it calls `Renderer::create_material`. The renderer
/// only accepts handles: it rejects a material that still has an asset reference.
///
/// Serialized externally tagged: `{"asset": "<id>"}` or
/// `{"handle": {"index": 1, "generation": 0}}`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextureRef {
    /// A texture asset, by the text form of its `re_assets::AssetId`.
    Asset(String),
    /// A texture already uploaded to the renderer.
    Handle(TextureHandle),
}

impl TextureRef {
    /// The handle, when the reference has been resolved.
    pub fn handle(&self) -> Option<TextureHandle> {
        match self {
            TextureRef::Handle(h) => Some(*h),
            TextureRef::Asset(_) => None,
        }
    }

    /// The asset id, when the reference has not been resolved yet.
    pub fn asset(&self) -> Option<&str> {
        match self {
            TextureRef::Asset(id) => Some(id),
            TextureRef::Handle(_) => None,
        }
    }
}

impl From<TextureHandle> for TextureRef {
    fn from(handle: TextureHandle) -> Self {
        TextureRef::Handle(handle)
    }
}

/// A PBR-lite material.
///
/// `re_assets` produces it from `materials/*.material.json` files and glTF
/// materials, with textures as [`TextureRef::Asset`]. `re_app` resolves every
/// texture to a [`TextureRef::Handle`] (see [`MaterialDesc::is_resolved`]) and
/// gives the result to the renderer through `Renderer::create_material`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaterialDesc {
    /// Linear RGBA base colour (multiplied with the base colour texture).
    pub base_color: [f32; 4],
    /// Base colour texture (sRGB).
    pub base_color_texture: Option<TextureRef>,
    /// Metalness in `[0, 1]`.
    pub metallic: f32,
    /// Perceptual roughness in `[0, 1]`.
    pub roughness: f32,
    /// Metallic (blue) and roughness (green) texture, as in glTF.
    pub metallic_roughness_texture: Option<TextureRef>,
    /// Tangent-space normal map.
    pub normal_texture: Option<TextureRef>,
    /// Linear RGB emissive colour.
    pub emissive: [f32; 3],
    /// Multiplier for `emissive` (glTF `KHR_materials_emissive_strength`).
    pub emissive_strength: f32,
    /// How alpha is used.
    pub alpha_mode: AlphaMode,
    /// Draw back faces too.
    pub double_sided: bool,
    /// Skip lighting: output the base colour as is.
    pub unlit: bool,
}

impl MaterialDesc {
    /// The three texture slots: base colour, metallic-roughness and normal.
    pub fn textures(&self) -> [Option<&TextureRef>; 3] {
        [
            self.base_color_texture.as_ref(),
            self.metallic_roughness_texture.as_ref(),
            self.normal_texture.as_ref(),
        ]
    }

    /// True when no texture is still an asset reference, so the renderer accepts it.
    pub fn is_resolved(&self) -> bool {
        self.textures()
            .into_iter()
            .flatten()
            .all(|t| matches!(t, TextureRef::Handle(_)))
    }
}

impl Default for MaterialDesc {
    /// Opaque white, non-metallic, roughness 0.5, no textures.
    fn default() -> Self {
        MaterialDesc {
            base_color: [1.0, 1.0, 1.0, 1.0],
            base_color_texture: None,
            metallic: 0.0,
            roughness: 0.5,
            metallic_roughness_texture: None,
            normal_texture: None,
            emissive: [0.0, 0.0, 0.0],
            emissive_strength: 1.0,
            alpha_mode: AlphaMode::Opaque,
            double_sided: false,
            unlit: false,
        }
    }
}

/// The pixel format of [`TextureData`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TextureFormat {
    /// 8-bit RGBA, sRGB-encoded colour (base colour and emissive textures).
    #[default]
    Rgba8Srgb,
    /// 8-bit RGBA, linear data (normal maps, metallic-roughness).
    Rgba8Unorm,
}

/// Decoded texture pixels, as given to `Renderer::upload_texture`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextureData {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The pixel format.
    pub format: TextureFormat,
    /// Tightly packed rows, top row first: `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

impl TextureData {
    /// True when `pixels` has exactly `width * height * 4` bytes.
    pub fn is_well_formed(&self) -> bool {
        u64::from(self.width) * u64::from(self.height) * 4 == self.pixels.len() as u64
    }
}
