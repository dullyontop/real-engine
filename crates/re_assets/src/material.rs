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
//! get `"color_space": "linear"`, and normal maps `"normal_map": true`).

use std::path::{Path, PathBuf};

use re_core::files::ProjectFiles;
use re_core::{Diagnostic, Location};
use re_types::render::{AlphaMode, MaterialDesc, TextureRef};
use serde::{Deserialize, Serialize};

use crate::meta::AssetMeta;
use crate::{codes, fsutil, id};

/// The JSON form of a material file.
///
/// With the `schemars` feature it derives `JsonSchema`, from which `real schema`
/// writes `schemas/material.schema.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, default)]
pub struct MaterialFile {
    /// An optional JSON Schema reference, ignored.
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Linear RGBA base colour.
    #[cfg_attr(feature = "schemars", schemars(extend("x-real-widget" = "colour")))]
    pub base_color: [f32; 4],
    /// Base colour texture path.
    #[cfg_attr(feature = "schemars", schemars(extend("x-real-asset" = "texture")))]
    pub base_color_texture: Option<String>,
    /// Metalness in `[0, 1]`.
    #[cfg_attr(feature = "schemars", schemars(range(min = 0.0, max = 1.0)))]
    pub metallic: f32,
    /// Perceptual roughness in `[0, 1]`.
    #[cfg_attr(feature = "schemars", schemars(range(min = 0.0, max = 1.0)))]
    pub roughness: f32,
    /// Metallic (blue) and roughness (green) texture path.
    #[cfg_attr(feature = "schemars", schemars(extend("x-real-asset" = "texture")))]
    pub metallic_roughness_texture: Option<String>,
    /// Tangent-space normal map path.
    #[cfg_attr(feature = "schemars", schemars(extend("x-real-asset" = "texture")))]
    pub normal_texture: Option<String>,
    /// Linear RGB emissive colour.
    #[cfg_attr(feature = "schemars", schemars(extend("x-real-widget" = "colour")))]
    pub emissive: [f32; 3],
    /// Multiplier for `emissive`.
    #[cfg_attr(feature = "schemars", schemars(range(min = 0.0)))]
    pub emissive_strength: f32,
    /// `opaque`, `mask` or `blend`.
    pub alpha_mode: AlphaModeName,
    /// The alpha threshold of `mask` (0.5 when left out).
    #[cfg_attr(feature = "schemars", schemars(range(min = 0.0, max = 1.0)))]
    pub alpha_cutoff: Option<f32>,
    /// Draw back faces too.
    pub double_sided: bool,
    /// Skip lighting.
    pub unlit: bool,
}

/// The alpha mode names of a material file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
    load_material_from(&ProjectFiles::project(root), path)
}

/// Loads the material file `path` (project-relative) through `files`, the way
/// the asset server does (ADR-0037).
///
/// # Errors
///
/// As [`load_material_in`].
pub fn load_material_from(files: &ProjectFiles, path: &str) -> Result<MaterialDesc, Diagnostic> {
    let rel = id::normalize_path(path)?;
    let bytes = fsutil::read_in(files, &rel)?;
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

    check_ranges(&file).map_err(|(key, why)| {
        Diagnostic::error(
            codes::MATERIAL_VALUE_OUT_OF_RANGE,
            format!("material `{key}` {why}"),
        )
        .at(located(key))
    })?;

    let texture = |key: &str,
                   value: &Option<String>,
                   settings: serde_json::Value|
     -> Result<Option<TextureRef>, Diagnostic> {
        let Some(tex) = value else { return Ok(None) };
        let tex_rel = id::normalize_path(tex).map_err(|d| d.at(located(key)))?;
        if !files.is_file(&tex_rel) {
            return Err(Diagnostic::error(
                codes::MATERIAL_TEXTURE_MISSING,
                format!("material `{key}` names `{tex_rel}`, which does not exist"),
            )
            .at(located(key))
            .with_hint("texture paths are relative to the project root, for example `assets/textures/rock.png`"));
        }
        let meta = AssetMeta::read_or_create_in(files, &tex_rel, settings)?;
        Ok(Some(TextureRef::Asset(meta.id().to_string())))
    };

    Ok(MaterialDesc {
        base_color: file.base_color,
        base_color_texture: texture(
            "base_color_texture",
            &file.base_color_texture,
            serde_json::json!({}),
        )?,
        metallic: file.metallic,
        roughness: file.roughness,
        metallic_roughness_texture: texture(
            "metallic_roughness_texture",
            &file.metallic_roughness_texture,
            serde_json::json!({ "color_space": "linear" }),
        )?,
        // Normal maps are stored as BC5, two channels (ADR-0047).
        normal_texture: texture(
            "normal_texture",
            &file.normal_texture,
            serde_json::json!({ "color_space": "linear", "normal_map": true }),
        )?,
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

/// Checks a material file's content as [`load_material_from`] does, without
/// reading anything: its keys (an unknown one is refused) and the range of
/// each value. It is the validator of open `*.material.json` documents
/// (ADR-0064), so a material made or edited by a command loads.
///
/// # Errors
///
/// What is wrong, as the loader's message says it.
pub fn check_material(value: &serde_json::Value) -> Result<MaterialFile, String> {
    let file = MaterialFile::deserialize(value).map_err(|e| format!("invalid material: {e}"))?;
    check_ranges(&file).map_err(|(key, why)| format!("material `{key}` {why}"))?;
    Ok(file)
}

/// The first value of `file` outside its range: its key, and why.
fn check_ranges(file: &MaterialFile) -> Result<(), (&'static str, String)> {
    for (key, v) in [("metallic", file.metallic), ("roughness", file.roughness)] {
        if !(0.0..=1.0).contains(&v) {
            return Err((key, format!("is {v}; it must be in [0, 1]")));
        }
    }
    if let Some(c) = file.alpha_cutoff
        && !(0.0..=1.0).contains(&c)
    {
        return Err(("alpha_cutoff", format!("is {c}; it must be in [0, 1]")));
    }
    let colours = [
        ("base_color", &file.base_color[..]),
        ("emissive", &file.emissive[..]),
    ];
    for (key, values) in colours {
        if values.iter().any(|v| !v.is_finite() || *v < 0.0) {
            return Err((key, "has a negative or non-finite component".into()));
        }
    }
    if !file.emissive_strength.is_finite() || file.emissive_strength < 0.0 {
        return Err((
            "emissive_strength",
            format!("is {}; it must be >= 0", file.emissive_strength),
        ));
    }
    Ok(())
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
