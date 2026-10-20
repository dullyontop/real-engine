//! glTF 2.0 import into engine data: meshes, materials, textures, the node tree,
//! node animations and skins ([`skin`]).
//!
//! The importer does not use `gltf::import`, which reads files itself and decodes
//! every image with the formats gltf picks. It parses the document, resolves
//! buffers with `gltf::import_buffers`, and decodes images with the `image` crate
//! itself, so the asset system owns caching and formats.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use glam::{Quat, Vec3};
use re_core::{Code, Diagnostic, Location, Severity};
use re_types::anim::{Channel, ClipData, Interpolation, SkinData, TargetChannels};
use re_types::mesh::{MeshData, SkinWeights, Vertex};
use re_types::render::{AlphaMode, MaterialDesc, TextureData, TextureFormat, TextureRef};
use serde::{Deserialize, Serialize};

mod skin;

use crate::image_decode::decode_image;
use crate::{AssetId, codes};

/// The version of the glTF importer. Bump it whenever its output changes, so
/// cached imports are rebuilt. This includes new fields: the cache is postcard,
/// which ignores `#[serde(default)]`, so an old cache never reads into a new
/// struct.
pub const GLTF_IMPORTER_VERSION: u32 = 2;

/// Everything imported from one glTF file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportedGltf {
    /// One mesh per triangle primitive.
    pub meshes: Vec<ImportedMesh>,
    /// The materials, in file order.
    pub materials: Vec<ImportedMaterial>,
    /// The images, in file order, as textures with derived ids.
    pub textures: Vec<ImportedTexture>,
    /// Every node, in file order.
    pub nodes: Vec<ImportedNode>,
    /// The root nodes of the default scene.
    pub roots: Vec<usize>,
    /// The node animations. They animate skin joints too, by joint name.
    pub clips: Vec<ClipData>,
    /// The skins (ADR-0039), in file order.
    pub skins: Vec<SkinData>,
    /// Parts that were skipped, as warnings.
    pub warnings: Vec<Diagnostic>,
}

impl ImportedGltf {
    /// The clip with this name.
    pub fn clip(&self, name: &str) -> Option<&ClipData> {
        self.clips.iter().find(|c| c.name == name)
    }

    /// The skin with this name.
    pub fn skin(&self, name: &str) -> Option<&SkinData> {
        self.skins.iter().find(|s| s.name == name)
    }

    /// The meshes a skin draws: every mesh of every node that uses skin `index`
    /// and has skin weights, in node order, without repeats.
    pub fn skinned_meshes(&self, index: usize) -> Vec<usize> {
        let mut out = Vec::new();
        for n in self.nodes.iter().filter(|n| n.skin == Some(index)) {
            for &m in &n.meshes {
                if self.meshes[m].skin_weights.is_some() && !out.contains(&m) {
                    out.push(m);
                }
            }
        }
        out
    }

    /// The texture with this id.
    pub fn texture(&self, id: AssetId) -> Option<&ImportedTexture> {
        self.textures.iter().find(|t| t.id == id)
    }
}

/// A mesh: one glTF primitive.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedMesh {
    /// `<mesh name>`, or `<mesh name>/<primitive index>` when the mesh has
    /// several primitives. Unnamed meshes are `mesh_<index>`.
    pub name: String,
    /// The geometry.
    pub mesh: MeshData,
    /// Index into [`ImportedGltf::materials`]; `None` for the default material.
    pub material: Option<usize>,
    /// The skin weights, when the primitive has `JOINTS_0` and `WEIGHTS_0`. On a
    /// mesh drawn by a skinned node, joint indices refer to that node's skin.
    #[serde(default)]
    pub skin_weights: Option<SkinWeights>,
}

/// A material, with textures as [`TextureRef::Asset`] holding derived ids.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportedMaterial {
    /// The glTF name, or `material_<index>`.
    pub name: String,
    /// The material.
    pub desc: MaterialDesc,
}

/// An image of the file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedTexture {
    /// `AssetId::derived(<file id>, "image/<index>")`.
    pub id: AssetId,
    /// The glTF name, or `image_<index>`.
    pub name: String,
    /// The pixels. Base-colour and emissive images are sRGB; images used only as
    /// normal or metallic-roughness maps are linear.
    pub data: TextureData,
}

/// A node of the glTF scene tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImportedNode {
    /// The glTF name, or `node_<index>`. Animation targets use this name.
    pub name: String,
    /// The parent node.
    pub parent: Option<usize>,
    /// The child nodes.
    pub children: Vec<usize>,
    /// Indices into [`ImportedGltf::meshes`] drawn at this node.
    pub meshes: Vec<usize>,
    /// Local translation.
    pub translation: Vec3,
    /// Local rotation.
    pub rotation: Quat,
    /// Local scale.
    pub scale: Vec3,
    /// Index into [`ImportedGltf::skins`] when the node draws its meshes skinned.
    #[serde(default)]
    pub skin: Option<usize>,
}

/// Imports a `.glb`, or a `.gltf` whose external files are resolved against
/// `base`. `owner` is the file's asset id (texture ids derive from it) and
/// `file` its project-relative path, for diagnostics.
///
/// # Errors
///
/// [`codes::GLTF_INVALID`] for an invalid document or unloadable buffers,
/// [`codes::IMAGE_INVALID`] for an image that cannot be decoded and
/// [`codes::MISSING_POSITIONS`] for a primitive without positions and
/// [`codes::SKIN_INVALID`] for a malformed skin or skin weights. Skipped parts
/// are warnings in [`ImportedGltf::warnings`].
pub fn import_gltf(
    bytes: &[u8],
    base: Option<&Path>,
    owner: AssetId,
    file: &str,
) -> Result<ImportedGltf, Diagnostic> {
    let at = || Location::file(file);
    let invalid = |e: &dyn std::fmt::Display| {
        Diagnostic::error(codes::GLTF_INVALID, format!("invalid glTF: {e}")).at(at())
    };
    let gltf::Gltf { document, blob } = gltf::Gltf::from_slice(bytes).map_err(|e| invalid(&e))?;
    let buffers = gltf::import_buffers(&document, base, blob).map_err(|e| invalid(&e))?;
    let buffer = |b: gltf::Buffer<'_>| buffers.get(b.index()).map(|d| &d.0[..]);
    let mut warnings = Vec::new();
    let mut skin_warnings = Vec::new();
    let names = skin::node_names(&document, file, &mut skin_warnings);
    let mut warn = |message: String| {
        warnings.push(Diagnostic::warning(codes::GLTF_PART_SKIPPED, message).at(at()));
    };

    // Colour images are sRGB; images used only as data maps are linear.
    let mut linear = vec![false; document.images().len()];
    let mut srgb = vec![false; document.images().len()];
    for m in document.materials() {
        let pbr = m.pbr_metallic_roughness();
        for info in [pbr.base_color_texture(), m.emissive_texture()]
            .into_iter()
            .flatten()
        {
            srgb[info.texture().source().index()] = true;
        }
        if let Some(info) = pbr.metallic_roughness_texture() {
            linear[info.texture().source().index()] = true;
        }
        if let Some(n) = m.normal_texture() {
            linear[n.texture().source().index()] = true;
        }
    }

    let mut textures = Vec::new();
    for image in document.images() {
        let i = image.index();
        let name = image
            .name()
            .map_or_else(|| format!("image_{i}"), str::to_owned);
        let format = if linear[i] && !srgb[i] {
            TextureFormat::Rgba8Unorm
        } else {
            TextureFormat::Rgba8Srgb
        };
        let what = format!("image {i} (`{name}`) of `{file}`");
        let data = match image.source() {
            gltf::image::Source::View { view, .. } => {
                let bytes = buffer(view.buffer())
                    .and_then(|b| b.get(view.offset()..view.offset() + view.length()))
                    .ok_or_else(|| invalid(&format!("image {i} points outside its buffer")))?;
                decode_image(bytes, format, &what, file)?
            }
            gltf::image::Source::Uri { uri, .. } => {
                let bytes = uri_bytes(uri, base).map_err(|e| {
                    Diagnostic::error(codes::IMAGE_INVALID, format!("cannot load {what}: {e}"))
                        .at(at())
                })?;
                decode_image(&bytes, format, &what, file)?
            }
        };
        textures.push(ImportedTexture {
            id: image_id(owner, i),
            name,
            data,
        });
    }

    let tex = |t: gltf::Texture<'_>| {
        Some(TextureRef::Asset(
            image_id(owner, t.source().index()).to_string(),
        ))
    };
    let materials = document
        .materials()
        .enumerate()
        .map(|(i, m)| {
            let pbr = m.pbr_metallic_roughness();
            ImportedMaterial {
                name: m
                    .name()
                    .map_or_else(|| format!("material_{i}"), str::to_owned),
                desc: MaterialDesc {
                    base_color: pbr.base_color_factor(),
                    base_color_texture: pbr.base_color_texture().and_then(|t| tex(t.texture())),
                    metallic: pbr.metallic_factor(),
                    roughness: pbr.roughness_factor(),
                    metallic_roughness_texture: pbr
                        .metallic_roughness_texture()
                        .and_then(|t| tex(t.texture())),
                    normal_texture: m.normal_texture().and_then(|t| tex(t.texture())),
                    emissive: m.emissive_factor(),
                    emissive_strength: m.emissive_strength().unwrap_or(1.0),
                    alpha_mode: match m.alpha_mode() {
                        gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
                        gltf::material::AlphaMode::Mask => AlphaMode::Mask {
                            cutoff: m.alpha_cutoff().unwrap_or(0.5),
                        },
                        gltf::material::AlphaMode::Blend => AlphaMode::Blend,
                    },
                    double_sided: m.double_sided(),
                    unlit: false,
                },
            }
        })
        .collect();

    // glTF mesh index -> our mesh indices (its triangle primitives).
    let mut meshes = Vec::new();
    let mut mesh_map: Vec<Vec<usize>> = Vec::new();
    for mesh in document.meshes() {
        let base_name = mesh
            .name()
            .map_or_else(|| format!("mesh_{}", mesh.index()), str::to_owned);
        let count = mesh.primitives().len();
        let mut ours = Vec::new();
        for prim in mesh.primitives() {
            let name = if count > 1 {
                format!("{base_name}/{}", prim.index())
            } else {
                base_name.clone()
            };
            if prim.mode() != gltf::mesh::Mode::Triangles {
                warn(format!(
                    "mesh `{name}`: {:?} primitives are not supported; skipped",
                    prim.mode()
                ));
                continue;
            }
            let reader = prim.reader(buffer);
            let positions: Vec<[f32; 3]> = reader
                .read_positions()
                .ok_or_else(|| {
                    Diagnostic::error(
                        codes::MISSING_POSITIONS,
                        format!("mesh `{name}` has no POSITION attribute"),
                    )
                    .at(at())
                })?
                .collect();
            let n = positions.len();
            let normals: Option<Vec<[f32; 3]>> = reader.read_normals().map(Iterator::collect);
            let uvs: Option<Vec<[f32; 2]>> =
                reader.read_tex_coords(0).map(|t| t.into_f32().collect());
            let colors: Option<Vec<[f32; 4]>> =
                reader.read_colors(0).map(|c| c.into_rgba_f32().collect());
            let indices: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..n as u32).collect(),
            };
            if !indices.len().is_multiple_of(3) || indices.iter().any(|&i| i as usize >= n) {
                warn(format!(
                    "mesh `{name}`: indices are not whole triangles over its {n} vertices; skipped"
                ));
                continue;
            }
            let mut vertices: Vec<Vertex> = (0..n)
                .map(|v| Vertex {
                    pos: positions[v],
                    normal: normals
                        .as_ref()
                        .and_then(|ns| ns.get(v))
                        .copied()
                        .unwrap_or([0.0; 3]),
                    uv: uvs
                        .as_ref()
                        .and_then(|t| t.get(v))
                        .copied()
                        .unwrap_or([0.0; 2]),
                    color: colors
                        .as_ref()
                        .and_then(|c| c.get(v))
                        .copied()
                        .unwrap_or([1.0; 4]),
                })
                .collect();
            if normals.is_none() {
                compute_normals(&mut vertices, &indices);
            }
            let mut too_many = 0;
            let skin_weights = skin::read_weights(&prim, &reader, n, &name, file, &mut too_many)?;
            if too_many > 0 {
                skin_warnings.push(
                    Diagnostic::warning(
                        codes::SKIN_TOO_MANY_INFLUENCES,
                        format!(
                            "mesh `{name}`: {too_many} vertices have more than four joint influences; each keeps its four largest"
                        ),
                    )
                    .at(at()),
                );
            }
            ours.push(meshes.len());
            meshes.push(ImportedMesh {
                name,
                mesh: MeshData { vertices, indices },
                material: prim.material().index(),
                skin_weights,
            });
        }
        mesh_map.push(ours);
    }

    let node_name = |n: &gltf::Node<'_>| names[n.index()].clone();
    let mut nodes: Vec<ImportedNode> = document
        .nodes()
        .map(|n| {
            let (t, r, s) = n.transform().decomposed();
            ImportedNode {
                name: node_name(&n),
                parent: None,
                children: n.children().map(|c| c.index()).collect(),
                meshes: n
                    .mesh()
                    .map(|m| mesh_map[m.index()].clone())
                    .unwrap_or_default(),
                translation: Vec3::from_array(t),
                rotation: Quat::from_array(r).normalize(),
                scale: Vec3::from_array(s),
                skin: None,
            }
        })
        .collect();
    for i in 0..nodes.len() {
        for c in nodes[i].children.clone() {
            nodes[c].parent = Some(i);
        }
    }
    let roots = match document
        .default_scene()
        .or_else(|| document.scenes().next())
    {
        Some(scene) => scene.nodes().map(|n| n.index()).collect(),
        None => (0..nodes.len())
            .filter(|&i| nodes[i].parent.is_none())
            .collect(),
    };

    let mut clips = Vec::new();
    for anim in document.animations() {
        let name = anim
            .name()
            .map_or_else(|| format!("animation_{}", anim.index()), str::to_owned);
        let mut targets: BTreeMap<usize, TargetChannels> = BTreeMap::new();
        let mut duration = 0.0f32;
        for channel in anim.channels() {
            let node = channel.target().node();
            let reader = channel.reader(buffer);
            let interpolation = match channel.sampler().interpolation() {
                gltf::animation::Interpolation::Linear => Interpolation::Linear,
                gltf::animation::Interpolation::Step => Interpolation::Step,
                gltf::animation::Interpolation::CubicSpline => Interpolation::CubicSpline,
            };
            let (Some(inputs), Some(outputs)) = (reader.read_inputs(), reader.read_outputs())
            else {
                warn(format!(
                    "animation `{name}`: a channel of `{}` has no keyframe data; skipped",
                    node_name(&node)
                ));
                continue;
            };
            let times: Vec<f32> = inputs.collect();
            duration = times.iter().copied().fold(duration, f32::max);
            let entry = targets
                .entry(node.index())
                .or_insert_with(|| TargetChannels {
                    target: node_name(&node),
                    ..TargetChannels::default()
                });
            use gltf::animation::util::ReadOutputs;
            match outputs {
                ReadOutputs::Translations(v) => {
                    entry.translation = Some(Channel {
                        interpolation,
                        times,
                        values: v.map(Vec3::from_array).collect(),
                    });
                }
                ReadOutputs::Rotations(v) => {
                    let values = v.into_f32().map(Quat::from_array).collect();
                    entry.rotation = Some(Channel {
                        interpolation,
                        times,
                        values,
                    });
                }
                ReadOutputs::Scales(v) => {
                    entry.scale = Some(Channel {
                        interpolation,
                        times,
                        values: v.map(Vec3::from_array).collect(),
                    });
                }
                ReadOutputs::MorphTargetWeights(_) => {
                    warn(format!(
                        "animation `{name}`: morph-target weights are not supported; skipped"
                    ));
                }
            }
        }
        clips.push(ClipData {
            name,
            duration,
            targets: targets.into_values().collect(),
        });
    }

    let skins = skin::import_skins(
        &document,
        buffer,
        &names,
        &mut meshes,
        &mut nodes,
        file,
        &mut skin_warnings,
    )?;
    warnings.extend(skin_warnings);

    Ok(ImportedGltf {
        meshes,
        materials,
        textures,
        nodes,
        roots,
        clips,
        skins,
        warnings,
    })
}

/// The id of image `index` of the file `owner`.
pub fn image_id(owner: AssetId, index: usize) -> AssetId {
    AssetId::derived(owner, &format!("image/{index}"))
}

/// The external files (buffers and images, not `data:` URIs) a glTF document
/// names, resolved against `base`. They are part of the cache key.
pub(crate) fn external_files(bytes: &[u8], base: &Path) -> Vec<PathBuf> {
    let Ok(gltf) = gltf::Gltf::from_slice(bytes) else {
        return Vec::new();
    };
    let buffers = gltf.document.buffers().filter_map(|b| match b.source() {
        gltf::buffer::Source::Uri(uri) => Some(uri),
        gltf::buffer::Source::Bin => None,
    });
    let images = gltf.document.images().filter_map(|i| match i.source() {
        gltf::image::Source::Uri { uri, .. } => Some(uri),
        gltf::image::Source::View { .. } => None,
    });
    buffers
        .chain(images)
        .filter(|u| !u.starts_with("data:"))
        .map(|u| base.join(percent_decode(u)))
        .collect()
}

fn uri_bytes(uri: &str, base: Option<&Path>) -> Result<Vec<u8>, String> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let (header, payload) = rest.split_once(',').ok_or("malformed data URI")?;
        if !header.ends_with(";base64") {
            return Err("only base64 data URIs are supported".into());
        }
        return base64_decode(payload).ok_or_else(|| "invalid base64 in data URI".into());
    }
    if uri.contains("://") {
        return Err(format!("`{uri}` is not a relative file path"));
    }
    let base = base.ok_or("external files need the glTF file's directory")?;
    let path = base.join(percent_decode(uri));
    std::fs::read(&path).map_err(|e| format!("cannot read `{uri}`: {e}"))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let value = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let clean: Vec<u8> = text.bytes().filter(|c| !c.is_ascii_whitespace()).collect();
    let trimmed = clean
        .strip_suffix(b"==")
        .or_else(|| clean.strip_suffix(b"="))
        .unwrap_or(&clean);
    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    for chunk in trimmed.chunks(4) {
        if chunk.len() == 1 {
            return None;
        }
        let mut n = 0u32;
        for (k, &c) in chunk.iter().enumerate() {
            n |= value(c)? << (18 - 6 * k);
        }
        let b = n.to_be_bytes();
        out.extend_from_slice(&b[1..chunk.len()]);
    }
    Some(out)
}

/// Area-weighted vertex normals, for meshes that have none.
fn compute_normals(vertices: &mut [Vertex], indices: &[u32]) {
    let mut acc = vec![Vec3::ZERO; vertices.len()];
    for t in indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| i as usize);
        let [pa, pb, pc] = [a, b, c].map(|i| Vec3::from_array(vertices[i].pos));
        let n = (pb - pa).cross(pc - pa);
        for i in [a, b, c] {
            acc[i] += n;
        }
    }
    for (v, n) in vertices.iter_mut().zip(acc) {
        v.normal = n.try_normalize().unwrap_or(Vec3::Y).to_array();
    }
}

/// The cached form of [`ImportedGltf`]. postcard cannot read the internally
/// tagged enums of `MaterialDesc` (`AlphaMode`) and `Diagnostic` (`Location`),
/// so materials are stored as JSON text and warnings as (code, message).
#[derive(Serialize, Deserialize)]
pub(crate) struct CachedGltf {
    meshes: Vec<ImportedMesh>,
    materials: Vec<(String, String)>,
    textures: Vec<ImportedTexture>,
    nodes: Vec<ImportedNode>,
    roots: Vec<usize>,
    clips: Vec<ClipData>,
    skins: Vec<SkinData>,
    warnings: Vec<(u16, String)>,
}

impl CachedGltf {
    pub(crate) fn from_imported(g: &ImportedGltf) -> CachedGltf {
        CachedGltf {
            meshes: g.meshes.clone(),
            materials: g
                .materials
                .iter()
                .map(|m| {
                    (
                        m.name.clone(),
                        serde_json::to_string(&m.desc).expect("materials serialize"),
                    )
                })
                .collect(),
            textures: g.textures.clone(),
            nodes: g.nodes.clone(),
            roots: g.roots.clone(),
            clips: g.clips.clone(),
            skins: g.skins.clone(),
            warnings: g
                .warnings
                .iter()
                .map(|d| (d.code.number(), d.message.clone()))
                .collect(),
        }
    }

    /// `None` when a stored material no longer parses (a stale entry).
    pub(crate) fn into_imported(self, file: &str) -> Option<ImportedGltf> {
        let materials = self
            .materials
            .into_iter()
            .map(|(name, json)| {
                Some(ImportedMaterial {
                    name,
                    desc: serde_json::from_str(&json).ok()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let warnings = self
            .warnings
            .into_iter()
            .filter_map(|(code, message)| {
                Some(
                    Diagnostic::new(Code::from_number(code)?, Severity::Warning, message)
                        .at(Location::file(file)),
                )
            })
            .collect();
        Some(ImportedGltf {
            meshes: self.meshes,
            materials,
            textures: self.textures,
            nodes: self.nodes,
            roots: self.roots,
            clips: self.clips,
            skins: self.skins,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_and_percent_decoding() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8h").unwrap(), b"hello!");
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert!(base64_decode("a").is_none());
        assert!(base64_decode("a*==").is_none());
        assert_eq!(percent_decode("my%20tex.png"), "my tex.png");
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn normals_face_the_winding() {
        let mut v = vec![
            Vertex {
                pos: [0.0, 0.0, 0.0],
                ..Vertex::default()
            },
            Vertex {
                pos: [1.0, 0.0, 0.0],
                ..Vertex::default()
            },
            Vertex {
                pos: [0.0, 1.0, 0.0],
                ..Vertex::default()
            },
        ];
        compute_normals(&mut v, &[0, 1, 2]);
        assert_eq!(v[0].normal, [0.0, 0.0, 1.0]);
    }
}
