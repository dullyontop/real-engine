//! `AssetServer`: loads project assets by path, keeps them behind generational
//! handles, and re-imports them when their files change.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use re_core::files::{FileData, Form, ProjectFiles, ProjectPath, Writable};
use re_core::{Diagnostic, Location};
use re_types::anim::ClipData;
use re_types::render::{MaterialDesc, TextureData, TextureFormat, TextureRef};

use crate::cache::{ProcessedCache, cache_key, decode_entry, encode_entry};
use crate::gltf::{
    CachedGltf, GLTF_IMPORTER_VERSION, ImportedGltf, external_uris, import_gltf_with,
};
use crate::image_decode::decode_image;
use crate::material::load_material_from;
use crate::meta::AssetMeta;
use crate::texture::{self, AssetsConfig, TextureRole};
use crate::watch::AssetChange;
use crate::{AssetId, codes, id};

/// The version of the texture importer. Bump it whenever its output changes,
/// a new version of the encoder crate or a change to its presets included.
/// Version 2: BC7 and BC5 mip chains from block_compression 0.10.1 (ADR-0047).
pub const TEXTURE_IMPORTER_VERSION: u32 = 2;

/// Whether the texture importer is quick enough to repeat when a packaged game
/// loads. It is not: encoding blocks takes seconds a texture, so `[package]
/// assets = "auto"` always stores imported textures processed (ADR-0037,
/// ADR-0047).
pub const TEXTURE_REPEAT_AT_LOAD: bool = false;

/// The importer name stored with processed glTF files (ADR-0037).
pub const GLTF_IMPORTER: &str = "gltf";

/// The importer name stored with processed textures (ADR-0037).
pub const TEXTURE_IMPORTER: &str = "texture";

/// An asset's processed form, as `real build` stores it in a packaged archive
/// (ADR-0037).
#[derive(Clone, Debug)]
pub struct ProcessedAsset {
    /// The importer ([`GLTF_IMPORTER`] or [`TEXTURE_IMPORTER`]).
    pub importer: &'static str,
    /// The importer's version.
    pub version: u32,
    /// The processed value, encoded as a cache entry ([`encode_entry`]).
    pub bytes: Vec<u8>,
    /// The project files the source pulls in (a glTF's external buffers and
    /// images).
    pub dependencies: Vec<String>,
    /// The textures it holds, as stored.
    pub textures: Vec<ProcessedTexture>,
    /// False when importing it again is too slow for a packaged game's load:
    /// its textures are block-compressed ([`TEXTURE_REPEAT_AT_LOAD`]), so
    /// `[package] assets = "auto"` stores it processed (ADR-0037, ADR-0047).
    pub repeat_at_load: bool,
}

/// A texture of a [`ProcessedAsset`], for `real build`'s report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessedTexture {
    /// The texture file, or the name of the glTF image.
    pub name: String,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The stored format: block-compressed, or RGBA8 when it was not encoded.
    pub format: TextureFormat,
}

impl ProcessedTexture {
    fn of(name: &str, data: &TextureData) -> ProcessedTexture {
        ProcessedTexture {
            name: name.to_owned(),
            width: data.width,
            height: data.height,
            format: data.format,
        }
    }
}

/// Whether an import holding `textures` may be repeated at load.
fn repeatable(textures: &[ProcessedTexture]) -> bool {
    TEXTURE_REPEAT_AT_LOAD || !textures.iter().any(|t| t.format.is_compressed())
}

/// A reference to a loaded asset of type `T`.
///
/// A handle holds a slot index and the slot's generation. After
/// [`AssetServer::release`], the slot may be reused, and the old handle reads as
/// `None` instead of reaching the new asset.
pub struct Handle<T> {
    index: u32,
    generation: u32,
    _type: PhantomData<fn() -> T>,
}

impl<T> Handle<T> {
    /// The slot index.
    pub fn index(self) -> u32 {
        self.index
    }

    /// The slot generation.
    pub fn generation(self) -> u32 {
        self.generation
    }
}

impl<T> Clone for Handle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for Handle<T> {}

impl<T> PartialEq for Handle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}

impl<T> Eq for Handle<T> {}

impl<T> Hash for Handle<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        self.generation.hash(state);
    }
}

impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Handle({}v{})", self.index, self.generation)
    }
}

/// Any loaded asset, by kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LoadedAsset {
    /// A `.gltf` or `.glb` file.
    Gltf(Handle<ImportedGltf>),
    /// A `.png`, `.jpg` or `.jpeg` file.
    Texture(Handle<TextureData>),
    /// A `.material.json` file.
    Material(Handle<MaterialDesc>),
}

/// Facts about a loaded asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetInfo {
    /// The project-relative path.
    pub path: String,
    /// The id from the sidecar (`None` for material files, which have none).
    pub id: Option<AssetId>,
    /// 0 when first loaded, plus one per successful re-import.
    pub version: u32,
}

struct Entry<T> {
    value: T,
    info: AssetInfo,
    /// Other files the asset was built from (a `.gltf`'s buffers and images).
    deps: Vec<String>,
}

struct Slot<T> {
    generation: u32,
    entry: Option<Entry<T>>,
}

struct Assets<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
}

impl<T> Default for Assets<T> {
    fn default() -> Self {
        Assets {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }
}

impl<T> Assets<T> {
    fn insert(&mut self, entry: Entry<T>) -> Handle<T> {
        let index = match self.free.pop() {
            Some(i) => {
                self.slots[i as usize].entry = Some(entry);
                i
            }
            None => {
                self.slots.push(Slot {
                    generation: 0,
                    entry: Some(entry),
                });
                (self.slots.len() - 1) as u32
            }
        };
        Handle {
            index,
            generation: self.slots[index as usize].generation,
            _type: PhantomData,
        }
    }

    fn get(&self, h: Handle<T>) -> Option<&Entry<T>> {
        self.slots
            .get(h.index as usize)
            .filter(|s| s.generation == h.generation)?
            .entry
            .as_ref()
    }

    fn get_mut(&mut self, h: Handle<T>) -> Option<&mut Entry<T>> {
        self.slots
            .get_mut(h.index as usize)
            .filter(|s| s.generation == h.generation)?
            .entry
            .as_mut()
    }

    fn remove(&mut self, h: Handle<T>) -> Option<Entry<T>> {
        let slot = self
            .slots
            .get_mut(h.index as usize)
            .filter(|s| s.generation == h.generation)?;
        let entry = slot.entry.take()?;
        slot.generation = slot.generation.wrapping_add(1);
        self.free.push(h.index);
        Some(entry)
    }

    fn iter(&self) -> impl Iterator<Item = &Entry<T>> {
        self.slots.iter().filter_map(|s| s.entry.as_ref())
    }
}

/// What [`AssetServer::apply_changes`] did.
#[derive(Debug, Default)]
pub struct ReloadReport {
    /// Assets re-imported successfully (their version went up).
    pub reloaded: Vec<LoadedAsset>,
    /// The ids whose data changed: re-imported source files and the textures
    /// inside re-imported glTF files. Re-upload these.
    pub changed_ids: Vec<AssetId>,
    /// Changed Luau scripts under `scripts/`, for script hot reload.
    pub scripts: Vec<String>,
    /// Changed audio files (`.wav`, `.ogg`): created, modified or removed,
    /// and both paths of a rename. Drop the audio's cached clip of each
    /// (`re_audio`'s `Audio::remove_clip`), so it plays new at its next start
    /// (ADR-0066 §6).
    pub audio: Vec<String>,
    /// Loaded assets whose file was deleted. Their last version stays loaded.
    pub removed: Vec<String>,
    /// Re-imports that failed (warnings, RE7050); the last good version stays loaded.
    pub problems: Vec<Diagnostic>,
}

/// Loads a project's assets.
///
/// Assets are named by project-relative path (`assets/models/tree.glb`), read
/// through the project's [`ProjectFiles`] (ADR-0037) and loaded synchronously;
/// imports go through the processed-asset cache in the run's cache folder
/// (`.real/cache/assets/` in a project), so loading an unchanged file again is
/// a cache read.
/// Loading the same path twice gives the same handle. Feed file changes from an
/// [`AssetWatcher`](crate::AssetWatcher) to [`AssetServer::apply_changes`] for
/// hot reload: a failed re-import keeps the last good version.
pub struct AssetServer {
    files: ProjectFiles,
    cache: ProcessedCache,
    config: AssetsConfig,
    gltfs: Assets<ImportedGltf>,
    textures: Assets<TextureData>,
    materials: Assets<MaterialDesc>,
    by_path: HashMap<String, LoadedAsset>,
    /// Source ids seen in sidecars, to their files.
    id_paths: HashMap<AssetId, String>,
}

impl AssetServer {
    /// A server for the project at `root`, caching in `<root>/.real/cache/assets`.
    pub fn new(root: impl Into<PathBuf>) -> AssetServer {
        AssetServer::with_files(ProjectFiles::project(root))
    }

    /// A server for the project at `root`, caching in `cache_dir`.
    pub fn with_cache_dir(root: impl Into<PathBuf>, cache_dir: impl Into<PathBuf>) -> AssetServer {
        let mut server = AssetServer::new(root);
        server.cache = ProcessedCache::new(cache_dir);
        server
    }

    /// A server reading through `files`, caching in `assets/` of its cache folder.
    pub fn with_files(files: ProjectFiles) -> AssetServer {
        AssetServer {
            cache: ProcessedCache::in_dir(files.writable(Writable::Cache).sub("assets")),
            config: AssetsConfig::default(),
            files,
            gltfs: Assets::default(),
            textures: Assets::default(),
            materials: Assets::default(),
            by_path: HashMap::new(),
            id_paths: HashMap::new(),
        }
    }

    /// Sets how imported textures are stored (`[assets]` in `real.toml`,
    /// ADR-0047). Assets already loaded keep the form they were imported in.
    pub fn set_config(&mut self, config: AssetsConfig) {
        self.config = config;
    }

    /// How imported textures are stored.
    pub fn config(&self) -> AssetsConfig {
        self.config
    }

    /// The project directory; empty when the files have none (a packaged game).
    pub fn root(&self) -> &Path {
        self.files.project_dir().unwrap_or(Path::new(""))
    }

    /// The project's files.
    pub fn files(&self) -> &ProjectFiles {
        &self.files
    }

    /// The processed-asset cache (for its hit and miss counts).
    pub fn cache(&self) -> &ProcessedCache {
        &self.cache
    }

    /// Loads any supported file, chosen by extension: `.gltf`/`.glb`,
    /// `.png`/`.jpg`/`.jpeg` or `.material.json`.
    ///
    /// # Errors
    ///
    /// [`codes::UNSUPPORTED_ASSET`] for other extensions, and the errors of the
    /// specific loaders.
    pub fn load(&mut self, path: &str) -> Result<LoadedAsset, Diagnostic> {
        let rel = id::normalize_path(path)?;
        match Kind::of(&rel) {
            Some(Kind::Gltf) => self.load_gltf(&rel).map(LoadedAsset::Gltf),
            Some(Kind::Texture) => self.load_texture(&rel).map(LoadedAsset::Texture),
            Some(Kind::Material) => self.load_material(&rel).map(LoadedAsset::Material),
            None => Err(Diagnostic::error(
                codes::UNSUPPORTED_ASSET,
                format!("no importer for `{rel}`"),
            )
            .at(Location::file(&rel))
            .with_hint("supported files are .gltf, .glb, .png, .jpg, .jpeg and .material.json")),
        }
    }

    /// The processed form of the asset at `path`, imported through the cache;
    /// `None` for a file that has no processed form (materials and files the
    /// server does not import).
    ///
    /// # Errors
    ///
    /// The errors of the importer.
    pub fn processed(&mut self, path: &str) -> Result<Option<ProcessedAsset>, Diagnostic> {
        let rel = id::normalize_path(path)?;
        Ok(match Kind::of(&rel) {
            Some(Kind::Gltf) => {
                let (imported, _, dependencies) = self.import_gltf_file(&rel)?;
                let textures: Vec<ProcessedTexture> = imported
                    .textures
                    .iter()
                    .map(|t| ProcessedTexture::of(&t.name, &t.data))
                    .collect();
                Some(ProcessedAsset {
                    importer: GLTF_IMPORTER,
                    version: GLTF_IMPORTER_VERSION,
                    bytes: encode_entry(&CachedGltf::from_imported(&imported)),
                    dependencies,
                    repeat_at_load: repeatable(&textures),
                    textures,
                })
            }
            Some(Kind::Texture) => {
                let (data, _) = self.import_texture_file(&rel)?;
                let textures = vec![ProcessedTexture::of(&rel, &data)];
                Some(ProcessedAsset {
                    importer: TEXTURE_IMPORTER,
                    version: TEXTURE_IMPORTER_VERSION,
                    bytes: encode_entry(&data),
                    dependencies: Vec::new(),
                    repeat_at_load: repeatable(&textures),
                    textures,
                })
            }
            Some(Kind::Material) | None => None,
        })
    }

    /// Loads a glTF file (`.gltf` or `.glb`).
    ///
    /// # Errors
    ///
    /// [`codes::PATH_INVALID`], [`codes::READ_FAILED`], sidecar errors and the
    /// errors of [`import_gltf`].
    pub fn load_gltf(&mut self, path: &str) -> Result<Handle<ImportedGltf>, Diagnostic> {
        let rel = id::normalize_path(path)?;
        if let Some(LoadedAsset::Gltf(h)) = self.by_path.get(&rel) {
            return Ok(*h);
        }
        let (value, id, deps) = self.import_gltf_file(&rel)?;
        for w in &value.warnings {
            tracing::warn!(code = %w.code, file = %rel, "{}", w.message);
        }
        let h = self.gltfs.insert(Entry {
            value,
            info: AssetInfo {
                path: rel.clone(),
                id: Some(id),
                version: 0,
            },
            deps,
        });
        self.by_path.insert(rel, LoadedAsset::Gltf(h));
        Ok(h)
    }

    /// Loads a PNG or JPEG texture. Its sidecar's `color_space` setting
    /// (`"srgb"` by default, or `"linear"`) picks the texture format.
    ///
    /// # Errors
    ///
    /// [`codes::PATH_INVALID`], [`codes::READ_FAILED`], sidecar errors and
    /// [`codes::IMAGE_INVALID`].
    pub fn load_texture(&mut self, path: &str) -> Result<Handle<TextureData>, Diagnostic> {
        let rel = id::normalize_path(path)?;
        if let Some(LoadedAsset::Texture(h)) = self.by_path.get(&rel) {
            return Ok(*h);
        }
        let (value, id) = self.import_texture_file(&rel)?;
        let h = self.textures.insert(Entry {
            value,
            info: AssetInfo {
                path: rel.clone(),
                id: Some(id),
                version: 0,
            },
            deps: Vec::new(),
        });
        self.by_path.insert(rel, LoadedAsset::Texture(h));
        Ok(h)
    }

    /// Loads a material file. Its textures are [`TextureRef::Asset`] ids; load
    /// each with [`AssetServer::texture_by_id`].
    ///
    /// # Errors
    ///
    /// The errors of [`load_material_in`].
    pub fn load_material(&mut self, path: &str) -> Result<Handle<MaterialDesc>, Diagnostic> {
        let rel = id::normalize_path(path)?;
        if let Some(LoadedAsset::Material(h)) = self.by_path.get(&rel) {
            return Ok(*h);
        }
        let value = self.import_material_file(&rel)?;
        let h = self.materials.insert(Entry {
            value,
            info: AssetInfo {
                path: rel.clone(),
                id: None,
                version: 0,
            },
            deps: Vec::new(),
        });
        self.by_path.insert(rel, LoadedAsset::Material(h));
        Ok(h)
    }

    /// A loaded glTF file, or `None` for a released handle.
    pub fn gltf(&self, h: Handle<ImportedGltf>) -> Option<&ImportedGltf> {
        self.gltfs.get(h).map(|e| &e.value)
    }

    /// A loaded texture, or `None` for a released handle.
    pub fn texture(&self, h: Handle<TextureData>) -> Option<&TextureData> {
        self.textures.get(h).map(|e| &e.value)
    }

    /// A loaded material, or `None` for a released handle.
    pub fn material(&self, h: Handle<MaterialDesc>) -> Option<&MaterialDesc> {
        self.materials.get(h).map(|e| &e.value)
    }

    /// The path, id and version of a loaded asset.
    pub fn info(&self, asset: LoadedAsset) -> Option<&AssetInfo> {
        match asset {
            LoadedAsset::Gltf(h) => self.gltfs.get(h).map(|e| &e.info),
            LoadedAsset::Texture(h) => self.textures.get(h).map(|e| &e.info),
            LoadedAsset::Material(h) => self.materials.get(h).map(|e| &e.info),
        }
    }

    /// The loaded asset at a path.
    pub fn loaded(&self, path: &str) -> Option<LoadedAsset> {
        self.by_path.get(&id::normalize_path(path).ok()?).copied()
    }

    /// Unloads an asset. Its handle (and any copy of it) reads as `None` from now
    /// on; loading the path again imports it afresh (from the cache).
    pub fn release(&mut self, asset: impl Into<LoadedAsset>) {
        let path = match asset.into() {
            LoadedAsset::Gltf(h) => self.gltfs.remove(h).map(|e| e.info.path),
            LoadedAsset::Texture(h) => self.textures.remove(h).map(|e| e.info.path),
            LoadedAsset::Material(h) => self.materials.remove(h).map(|e| e.info.path),
        };
        if let Some(p) = path {
            self.by_path.remove(&p);
        }
    }

    /// The file of a source id that this server has seen (in a loaded asset's or
    /// a material texture's sidecar).
    pub fn path_of(&self, id: AssetId) -> Option<&str> {
        self.id_paths.get(&id).map(String::as_str)
    }

    /// The texture named by the text form of an asset id, as found in
    /// [`TextureRef::Asset`]: a texture file (loaded on first use) or an image
    /// inside a loaded glTF file.
    ///
    /// # Errors
    ///
    /// [`codes::UNKNOWN_ASSET`] when the id is malformed or names no texture this
    /// server knows; the loading errors of [`AssetServer::load_texture`].
    pub fn texture_by_id(&mut self, id: &str) -> Result<&TextureData, Diagnostic> {
        let asset = AssetId::parse(id)?;
        if let AssetId::Source(_) = asset
            && let Some(path) = self.id_paths.get(&asset).cloned()
        {
            let h = self.load_texture(&path)?;
            return Ok(&self.textures.get(h).expect("just loaded").value);
        }
        for e in self.gltfs.iter() {
            if let Some(t) = e.value.texture(asset) {
                return Ok(&t.data);
            }
        }
        Err(Diagnostic::error(
            codes::UNKNOWN_ASSET,
            format!("no loaded texture has the id `{id}`"),
        )
        .with_hint("load the material or glTF file that refers to it first"))
    }

    /// The texture references of a material, resolved and loaded: one
    /// `(slot, id, texture)` per texture the material uses.
    ///
    /// # Errors
    ///
    /// As [`AssetServer::texture_by_id`].
    pub fn material_textures(
        &mut self,
        h: Handle<MaterialDesc>,
    ) -> Result<Vec<(usize, String, TextureData)>, Diagnostic> {
        let Some(desc) = self.material(h).cloned() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for (slot, t) in desc.textures().into_iter().enumerate() {
            if let Some(TextureRef::Asset(id)) = t {
                out.push((slot, id.clone(), self.texture_by_id(id)?.clone()));
            }
        }
        Ok(out)
    }

    /// A clip, named as `AnimPlayer.clip` names it: `<glTF path>#<clip name>`,
    /// for example `assets/models/door.glb#open`. Loads the file if needed.
    ///
    /// # Errors
    ///
    /// [`codes::UNKNOWN_ASSET`] when the reference has no `#` or the file has no
    /// such clip (the message lists the clips it has), and the errors of
    /// [`AssetServer::load_gltf`].
    pub fn clip(&mut self, reference: &str) -> Result<&ClipData, Diagnostic> {
        let Some((path, name)) = reference.rsplit_once('#') else {
            return Err(Diagnostic::error(codes::UNKNOWN_ASSET, format!("clip reference `{reference}` has no `#<clip name>`"))
                .with_hint("clips are named `<glTF path>#<clip name>`, for example `assets/models/door.glb#open`"));
        };
        let h = self.load_gltf(path)?;
        let gltf = &self.gltfs.get(h).expect("just loaded").value;
        gltf.clip(name).ok_or_else(|| {
            let names: Vec<&str> = gltf.clips.iter().map(|c| c.name.as_str()).collect();
            Diagnostic::error(
                codes::UNKNOWN_ASSET,
                format!("`{path}` has no clip named `{name}`"),
            )
            .at(Location::file(path))
            .with_hint(if names.is_empty() {
                "the file has no animations".to_owned()
            } else {
                format!("its clips are: {}", names.join(", "))
            })
        })
    }

    /// Applies file changes (from [`AssetWatcher::poll`](crate::AssetWatcher::poll)):
    /// re-imports loaded assets whose file, sidecar or dependency changed, follows
    /// renames, and reports changed scripts. Call between frames.
    pub fn apply_changes(&mut self, changes: &[AssetChange]) -> ReloadReport {
        let mut report = ReloadReport::default();
        let mut dirty: Vec<String> = Vec::new();
        let mark = |p: &str, dirty: &mut Vec<String>| {
            if !dirty.iter().any(|d| d == p) {
                dirty.push(p.to_owned());
            }
        };
        for change in changes {
            if change.is_script() && !report.scripts.iter().any(|s| s == change.path()) {
                report.scripts.push(change.path().to_owned());
            }
            let from = match change {
                AssetChange::Renamed { from, .. } => Some(from.as_str()),
                _ => None,
            };
            for p in from.into_iter().chain([change.path()]) {
                if crate::is_audio(p) && !report.audio.iter().any(|a| a == p) {
                    report.audio.push(p.to_owned());
                }
            }
            match change {
                AssetChange::Created(p) | AssetChange::Modified(p) => {
                    // A sidecar change re-imports its file (new settings).
                    let target = p.strip_suffix(".meta.json").unwrap_or(p);
                    mark(target, &mut dirty);
                }
                AssetChange::Removed(p) => {
                    if self.by_path.contains_key(p) {
                        report.removed.push(p.clone());
                    }
                }
                AssetChange::Renamed { from, to } => {
                    self.follow_rename(from, to);
                    mark(to, &mut dirty);
                }
            }
        }
        // Files that loaded glTF files pull in (buffers, images) dirty their owner.
        let owners: Vec<String> = self
            .gltfs
            .iter()
            .filter(|e| e.deps.iter().any(|d| dirty.contains(d)))
            .map(|e| e.info.path.clone())
            .collect();
        for o in owners {
            mark(&o, &mut dirty);
        }
        for path in dirty {
            if let Some(&asset) = self.by_path.get(&path) {
                self.reload(asset, &path, &mut report);
            }
        }
        report
    }

    fn follow_rename(&mut self, from: &str, to: &str) {
        let Some(asset) = self.by_path.remove(from) else {
            return;
        };
        // Keep the id: move the sidecar along when the file moved without it.
        // Only a development run writes project files.
        if let Some(root) = self.files.project_dir()
            && let (old_meta, new_meta) = (
                AssetMeta::path_for(&root.join(from)),
                AssetMeta::path_for(&root.join(to)),
            )
            && old_meta.is_file()
            && !new_meta.exists()
            && let Err(e) = std::fs::rename(&old_meta, &new_meta)
        {
            tracing::warn!(error = %e, "could not move the sidecar of `{from}` to `{to}`");
        }
        let info = match asset {
            LoadedAsset::Gltf(h) => self.gltfs.get_mut(h).map(|e| &mut e.info),
            LoadedAsset::Texture(h) => self.textures.get_mut(h).map(|e| &mut e.info),
            LoadedAsset::Material(h) => self.materials.get_mut(h).map(|e| &mut e.info),
        };
        if let Some(info) = info {
            info.path = to.to_owned();
            if let Some(id) = info.id {
                self.id_paths.insert(id, to.to_owned());
            }
        }
        self.by_path.insert(to.to_owned(), asset);
    }

    fn reload(&mut self, asset: LoadedAsset, path: &str, report: &mut ReloadReport) {
        let failed = |d: Diagnostic| {
            Diagnostic::warning(
                codes::RELOAD_FAILED,
                format!(
                    "`{path}` changed but could not be re-imported; keeping the last good version"
                ),
            )
            .at(Location::file(path))
            .with_related(d)
        };
        match asset {
            LoadedAsset::Gltf(h) => match self.import_gltf_file(path) {
                Ok((value, id, deps)) => {
                    let e = self
                        .gltfs
                        .get_mut(h)
                        .expect("loaded assets have live handles");
                    if e.value != value {
                        report.changed_ids.push(id);
                        report
                            .changed_ids
                            .extend(value.textures.iter().map(|t| t.id));
                        e.value = value;
                        e.info.version += 1;
                        e.info.id = Some(id);
                        report.reloaded.push(asset);
                    }
                    e.deps = deps;
                }
                Err(d) => report.problems.push(failed(d)),
            },
            LoadedAsset::Texture(h) => match self.import_texture_file(path) {
                Ok((value, id)) => {
                    let e = self
                        .textures
                        .get_mut(h)
                        .expect("loaded assets have live handles");
                    if e.value != value {
                        e.value = value;
                        e.info.version += 1;
                        e.info.id = Some(id);
                        report.changed_ids.push(id);
                        report.reloaded.push(asset);
                    }
                }
                Err(d) => report.problems.push(failed(d)),
            },
            LoadedAsset::Material(h) => match self.import_material_file(path) {
                Ok(value) => {
                    let e = self
                        .materials
                        .get_mut(h)
                        .expect("loaded assets have live handles");
                    if e.value != value {
                        e.value = value;
                        e.info.version += 1;
                        report.reloaded.push(asset);
                    }
                }
                Err(d) => report.problems.push(failed(d)),
            },
        }
    }

    fn import_gltf_file(
        &mut self,
        rel: &str,
    ) -> Result<(ImportedGltf, AssetId, Vec<String>), Diagnostic> {
        let data = self.read_asset(rel)?;
        let meta = AssetMeta::read_or_create_in(&self.files, rel, serde_json::json!({}))?;
        let id = meta.id();
        self.id_paths.insert(id, rel.to_owned());
        if matches!(data.form, Form::Processed { .. }) {
            let cached: CachedGltf =
                processed_value(rel, &data, GLTF_IMPORTER, GLTF_IMPORTER_VERSION)?;
            let imported = cached.into_imported(rel).ok_or_else(|| {
                Diagnostic::error(
                    codes::READ_FAILED,
                    format!("the processed copy of `{rel}` does not decode"),
                )
                .at(Location::file(rel))
            })?;
            return Ok((imported, id, Vec::new()));
        }
        let bytes = source_bytes(rel, data)?;
        let gltf_path = ProjectPath::new(rel).ok();
        // External files resolve against the glTF's folder and may not leave
        // the project.
        let resolve = |uri: &str| gltf_path.as_ref().and_then(|p| p.join_relative(uri).ok());
        let mut deps = Vec::new();
        let mut dep_bytes: Vec<Vec<u8>> = Vec::new();
        for uri in external_uris(&bytes) {
            if let Some(p) = resolve(&uri) {
                dep_bytes.push(
                    self.files
                        .read_source(p.as_str())
                        .map(|b| b.to_vec())
                        .unwrap_or_default(),
                );
                deps.push(p.as_str().to_owned());
            }
        }
        let mut sources: Vec<&[u8]> = vec![&bytes];
        sources.extend(dep_bytes.iter().map(Vec::as_slice));
        let key = cache_key(
            "gltf",
            GLTF_IMPORTER_VERSION,
            &self.config.cache_settings(&meta.settings_bytes()),
            &sources,
        );
        if let Some(cached) = self
            .cache
            .get::<CachedGltf>(&key)
            .and_then(|c| c.into_imported(rel))
        {
            self.cache.count(true);
            return Ok((cached, id, deps));
        }
        self.cache.count(false);
        let files = &self.files;
        let read = |uri: &str| -> Result<Vec<u8>, String> {
            let p = resolve(uri).ok_or_else(|| format!("`{uri}` is outside the project"))?;
            files
                .read_source(p.as_str())
                .map(|b| b.to_vec())
                .map_err(|e| e.to_string())
        };
        let mut imported = import_gltf_with(&bytes, &read, id, rel)?;
        encode_gltf_textures(&mut imported, &self.config);
        if let Err(d) = self.cache.put(&key, &CachedGltf::from_imported(&imported)) {
            tracing::warn!(code = %d.code, "{}", d.message);
        }
        Ok((imported, id, deps))
    }

    fn import_texture_file(&mut self, rel: &str) -> Result<(TextureData, AssetId), Diagnostic> {
        let data = self.read_asset(rel)?;
        let meta = AssetMeta::read_or_create_in(&self.files, rel, serde_json::json!({}))?;
        let id = meta.id();
        self.id_paths.insert(id, rel.to_owned());
        if matches!(data.form, Form::Processed { .. }) {
            let texture = processed_value(rel, &data, TEXTURE_IMPORTER, TEXTURE_IMPORTER_VERSION)?;
            return Ok((texture, id));
        }
        let bytes = source_bytes(rel, data)?;
        let sidecar = || Location::file(format!("{rel}.meta.json"));
        let normal_map = match meta.settings.get("normal_map") {
            None => false,
            Some(serde_json::Value::Bool(b)) => *b,
            Some(other) => {
                return Err(Diagnostic::error(
                    codes::META_INVALID,
                    format!(
                        "normal_map is {other} in the sidecar of `{rel}`; it must be true or false"
                    ),
                )
                .at(sidecar()));
            }
        };
        let format = match (
            meta.settings.get("color_space").and_then(|v| v.as_str()),
            normal_map,
        ) {
            (None | Some("srgb"), false) => TextureFormat::Rgba8Srgb,
            (Some("linear"), _) | (None, true) => TextureFormat::Rgba8Unorm,
            (Some("srgb"), true) => {
                return Err(Diagnostic::error(
                    codes::META_INVALID,
                    format!("`{rel}` is a normal map, which holds linear data, but its sidecar sets color_space \"srgb\""),
                )
                .at(sidecar())
                .with_hint("remove color_space, or set it to \"linear\""));
            }
            (Some(other), _) => {
                return Err(Diagnostic::error(
                    codes::META_INVALID,
                    format!("unknown color_space `{other}` in the sidecar of `{rel}`"),
                )
                .at(sidecar())
                .with_hint(
                    "use \"srgb\" for colour textures or \"linear\" for normal and data maps",
                ));
            }
        };
        let role = if normal_map {
            TextureRole::Normal
        } else {
            TextureRole::Rgba
        };
        let key = cache_key(
            "texture",
            TEXTURE_IMPORTER_VERSION,
            &self.config.cache_settings(&meta.settings_bytes()),
            &[&bytes],
        );
        let config = self.config;
        let (data, _) = self.cache.get_or_insert_with(&key, || {
            decode_image(&bytes, format, &format!("`{rel}`"), rel)
                .map(|data| texture::process(data, role, &config))
        })?;
        Ok((data, id))
    }

    /// The file at `rel` in whatever form it is stored, as
    /// [`codes::READ_FAILED`] on error.
    /// The format, channels, rate and length of the audio file at `path`
    /// (`.wav` or `.ogg`), from its headers (ADR-0066 §6).
    ///
    /// # Errors
    ///
    /// [`codes::READ_FAILED`] when it cannot be read, and
    /// [`codes::UNSUPPORTED_AUDIO`] for a file the engine does not play.
    pub fn audio_info(&self, path: &str) -> Result<crate::AudioInfo, Diagnostic> {
        let data = self.read_asset(path)?;
        crate::audio_info(path, &data.bytes)
    }

    fn read_asset(&self, rel: &str) -> Result<FileData, Diagnostic> {
        self.files.read(rel).map_err(|e| {
            Diagnostic::error(codes::READ_FAILED, format!("cannot read `{rel}`: {e}"))
                .at(Location::file(rel))
        })
    }

    fn import_material_file(&mut self, rel: &str) -> Result<MaterialDesc, Diagnostic> {
        let desc = load_material_from(&self.files, rel)?;
        // Remember where each texture id lives, for `texture_by_id`.
        let file = self.files.read_str(rel).unwrap_or_default();
        if let Ok(parsed) = serde_json::from_str::<crate::material::MaterialFile>(&file) {
            let paths = [
                parsed.base_color_texture,
                parsed.metallic_roughness_texture,
                parsed.normal_texture,
            ];
            for (t, p) in desc.textures().into_iter().zip(paths) {
                if let (Some(TextureRef::Asset(id)), Some(p)) = (t, p)
                    && let (Ok(id), Ok(p)) = (AssetId::parse(id), id::normalize_path(&p))
                {
                    self.id_paths.insert(id, p);
                }
            }
        }
        Ok(desc)
    }
}

impl From<Handle<ImportedGltf>> for LoadedAsset {
    fn from(h: Handle<ImportedGltf>) -> Self {
        LoadedAsset::Gltf(h)
    }
}

impl From<Handle<TextureData>> for LoadedAsset {
    fn from(h: Handle<TextureData>) -> Self {
        LoadedAsset::Texture(h)
    }
}

impl From<Handle<MaterialDesc>> for LoadedAsset {
    fn from(h: Handle<MaterialDesc>) -> Self {
        LoadedAsset::Material(h)
    }
}

/// Encodes the images of a glTF import as `config` stores textures: an image
/// used only as a normal map becomes BC5, the others BC7 (ADR-0047).
fn encode_gltf_textures(imported: &mut ImportedGltf, config: &AssetsConfig) {
    if !config.encodes() {
        return;
    }
    let id = |t: &Option<TextureRef>| t.as_ref().and_then(TextureRef::asset).map(str::to_owned);
    let mut normal = Vec::new();
    let mut other = Vec::new();
    for m in &imported.materials {
        normal.extend(id(&m.desc.normal_texture));
        other.extend(id(&m.desc.base_color_texture));
        other.extend(id(&m.desc.metallic_roughness_texture));
    }
    for t in &mut imported.textures {
        let name = t.id.to_string();
        let role = if normal.contains(&name) && !other.contains(&name) {
            TextureRole::Normal
        } else {
            TextureRole::Rgba
        };
        t.data = texture::process(std::mem::take(&mut t.data), role, config);
    }
}

/// The source bytes of `data`, or an error for another form.
fn source_bytes(rel: &str, data: FileData) -> Result<std::sync::Arc<[u8]>, Diagnostic> {
    match data.form {
        Form::Source => Ok(data.bytes),
        other => Err(Diagnostic::error(
            codes::READ_FAILED,
            format!("cannot import `{rel}`: it is stored as {other:?}"),
        )
        .at(Location::file(rel))),
    }
}

/// The value of a processed entry from `importer` at `version`.
fn processed_value<T: serde::de::DeserializeOwned>(
    rel: &str,
    data: &FileData,
    importer: &str,
    version: u32,
) -> Result<T, Diagnostic> {
    let failed = |why: String| {
        Diagnostic::error(
            codes::READ_FAILED,
            format!("cannot use the processed copy of `{rel}`: {why}"),
        )
        .at(Location::file(rel))
        .with_hint("rebuild the package with this engine's `real build`")
    };
    match &data.form {
        Form::Processed {
            importer: i,
            version: v,
        } if &**i == importer && *v == version => decode_entry(&data.bytes).map_err(failed),
        Form::Processed {
            importer: i,
            version: v,
        } => Err(failed(format!(
            "it was made by importer `{i}` version {v}; this engine has `{importer}` version {version}"
        ))),
        other => Err(failed(format!("it is stored as {other:?}"))),
    }
}

enum Kind {
    Gltf,
    Texture,
    Material,
}

impl Kind {
    fn of(path: &str) -> Option<Kind> {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".material.json") {
            Some(Kind::Material)
        } else if lower.ends_with(".gltf") || lower.ends_with(".glb") {
            Some(Kind::Gltf)
        } else if lower.ends_with(".png") || lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
            Some(Kind::Texture)
        } else {
            None
        }
    }
}
