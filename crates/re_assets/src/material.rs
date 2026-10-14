//! Material files: `materials/*.material.json`.
//!
//! ```json
//! {
//!   "base_color": [0.8, 0.8, 0.8, 1.0],
//!   "base_color_texture": "assets/textures/rock.png",
//!   "roughness": 0.9,
//!   "normal_texture": "assets/textures/rock_normal.png"
//! }
//! ```
//!
//! Every key is optional (the defaults are those of [`MaterialDesc::default`]) and
//! unknown keys are errors. Texture paths are relative to the project root. Each
//! texture becomes a [`TextureRef::Asset`] holding the texture's id from its
//! `.meta.json` sidecar (created when missing; normal and metallic-roughness maps
//! get `"color_space": "linear"`).

use std::path::{Path, PathBuf};

use re_core::{Diagnostic, Location};
use re_types::render::{AlphaMode, MaterialDesc, TextureRef};
use serde::{Deserialize, Serialize};

use crate::meta::AssetMeta;
use crate::{codes, fsutil, id};

/// The JSON form of a material file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MaterialFile {
    /// An optional JSON Schema reference, ignored.
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Linear RGBA base colour.
    pub base_color: [f32; 4],
    /// Base colour texture path.
    pub base_color_texture: Option<String>,
    /// Metalness in `[0, 1]`.
    pub metallic: f32,
    /// Perceptual roughness in `[0, 1]`.
    pub roughness: f32,
    /// Metallic (blue) and roughness (green) texture path.
    pub metallic_roughness_texture: Option<String>,
    /// Tangent-space normal map path.
    pub normal_texture: Option<String>,
    /// Linear RGB emissive colour.
    pub emissive: [f32; 3],
    /// Multiplier for `emissive`.
    pub emissive_strength: f32,
    /// `opaque`, `mask` or `blend`.
    pub alpha_mode: AlphaModeName,
    /// The alpha threshold of `mask` (0.5 when left out).
    pub alpha_cutoff: Option<f32>,
    /// Draw back faces too.
    pub double_sided: bool,
    /// Skip lighting.
    pub unlit: bool,
}

/// The alpha mode names of a material file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlphaModeName {
    /// Alpha is ignored.
    #[default]
    Opaque,
    /// Alpha below `alpha_cutoff` is discarded.
    Mask,
    /// Alpha blending.
    Blend,
}

impl Default for MaterialFile {
    fn default() -> Self {
        let d = MaterialDesc::default();
        MaterialFile {
            schema: None,
            base_color: d.base_color,
            base_color_texture: None,
            metallic: d.metallic,
            roughness: d.roughness,
            metallic_roughness_texture: None,
            normal_texture: None,
            emissive: d.emissive,
            emissive_strength: d.emissive_strength,
            alpha_mode: AlphaModeName::Opaque,
            alpha_cutoff: None,
            double_sided: d.double_sided,
            unlit: d.unlit,
        }
    }
}

/// Loads a material file. Texture paths are resolved against the project root:
/// the nearest directory above the file that holds a `real.toml`, or the file's
/// own directory when there is none.
///
/// # Errors
///
/// See [`load_material_in`].
pub fn load_material(path: &Path) -> Result<MaterialDesc, Diagnostic> {
    let root = project_root(path);
    let rel = path
        .strip_prefix(&root)
        .map_or_else(|_| fsutil::slash(path), fsutil::slash);
    load_material_in(&root, &rel)
}

/// Loads the material file `path` (project-relative) of the project at `root`.
///
/// # Errors
///
/// [`codes::READ_FAILED`] when the file cannot be read,
/// [`codes::MATERIAL_INVALID`] for invalid JSON or unknown keys,
/// [`codes::MATERIAL_VALUE_OUT_OF_RANGE`] for values outside their range and
/// [`codes::MATERIAL_TEXTURE_MISSING`] for a texture that does not exist. Each
/// is located at the line of the problem.
pub fn load_material_in(root: &Path, path: &str) -> Result<MaterialDesc, Diagnostic> {
    let rel = id::normalize_path(path)?;
    let bytes = fsutil::read(&root.join(&rel), &rel)?;
    let text = String::from_utf8_lossy(&bytes);
    let file: MaterialFile = serde_json::from_str(&text).map_err(|e| {
        Diagnostic::error(codes::MATERIAL_INVALID, format!("invalid material: {e}"))
            .at(Location::file_at(&rel, e.line() as u32, e.column() as u32))
            .with_hint("material keys are base_color, base_color_texture, metallic, roughness, metallic_roughness_texture, normal_texture, emissive, emissive_strength, alpha_mode, alpha_cutoff, double_sided and unlit")
    })?;
    let located = |key: &str| match fsutil::find_line(&text, &format!("\"{key}\"")) {
        Some((line, col)) => Location::file_at(&rel, line, col),
        None => Location::file(&rel),
    };

    let out_of_range = |key: &str, why: String| {
        Diagnostic::error(
            codes::MATERIAL_VALUE_OUT_OF_RANGE,
            format!("material `{key}` {why}"),
        )
        .at(located(key))
    };
    for (key, v) in [("metallic", file.metallic), ("roughness", file.roughness)] {
        if !(0.0..=1.0).contains(&v) {
            return Err(out_of_range(key, format!("is {v}; it must be in [0, 1]")));
        }
    }
    if let Some(c) = file.alpha_cutoff
        && !(0.0..=1.0).contains(&c)
    {
        return Err(out_of_range(
            "alpha_cutoff",
            format!("is {c}; it must be in [0, 1]"),
        ));
    }
    let colours = [
        ("base_color", &file.base_color[..]),
        ("emissive", &file.emissive[..]),
    ];
    for (key, values) in colours {
        if values.iter().any(|v| !v.is_finite() || *v < 0.0) {
            return Err(out_of_range(
                key,
                "has a negative or non-finite component".into(),
            ));
        }
    }
    if !file.emissive_strength.is_finite() || file.emissive_strength < 0.0 {
        return Err(out_of_range(
            "emissive_strength",
            format!("is {}; it must be >= 0", file.emissive_strength),
        ));
    }

    let texture = |key: &str,
                   value: &Option<String>,
                   linear: bool|
     -> Result<Option<TextureRef>, Diagnostic> {
        let Some(tex) = value else { return Ok(None) };
        let tex_rel = id::normalize_path(tex).map_err(|d| d.at(located(key)))?;
        let tex_path = root.join(&tex_rel);
        if !tex_path.is_file() {
            return Err(Diagnostic::error(
                codes::MATERIAL_TEXTURE_MISSING,
                format!("material `{key}` names `{tex_rel}`, which does not exist"),
            )
            .at(located(key))
            .with_hint("texture paths are relative to the project root, for example `assets/textures/rock.png`"));
        }
        let settings = if linear {
            serde_json::json!({ "color_space": "linear" })
        } else {
            serde_json::json!({})
        };
        let meta = AssetMeta::read_or_create(&tex_path, &tex_rel, settings)?;
        Ok(Some(TextureRef::Asset(meta.id().to_string())))
    };

    Ok(MaterialDesc {
        base_color: file.base_color,
        base_color_texture: texture("base_color_texture", &file.base_color_texture, false)?,
        metallic: file.metallic,
        roughness: file.roughness,
        metallic_roughness_texture: texture(
            "metallic_roughness_texture",
            &file.metallic_roughness_texture,
            true,
        )?,
        normal_texture: texture("normal_texture", &file.normal_texture, true)?,
        emissive: file.emissive,
        emissive_strength: file.emissive_strength,
        alpha_mode: match file.alpha_mode {
            AlphaModeName::Opaque => AlphaMode::Opaque,
            AlphaModeName::Mask => AlphaMode::Mask {
                cutoff: file.alpha_cutoff.unwrap_or(0.5),
            },
            AlphaModeName::Blend => AlphaMode::Blend,
        },
        double_sided: file.double_sided,
        unlit: file.unlit,
    })
}

/// The nearest ancestor directory of `file` holding a `real.toml`, or the file's
/// directory.
fn project_root(file: &Path) -> PathBuf {
    let dir = file.parent().unwrap_or(Path::new("."));
    dir.ancestors()
        .find(|d| d.join("real.toml").is_file())
        .unwrap_or(dir)
        .to_path_buf()
}
