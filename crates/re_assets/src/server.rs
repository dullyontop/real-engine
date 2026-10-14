//! `AssetServer`: loads project assets by path, keeps them behind generational
//! handles, and re-imports them when their files change.

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use re_core::{Diagnostic, Location};
use re_types::anim::ClipData;
use re_types::render::{MaterialDesc, TextureData, TextureFormat, TextureRef};

use crate::cache::{ProcessedCache, cache_key};
use crate::gltf_import::{
    CachedGltf, GLTF_IMPORTER_VERSION, ImportedGltf, external_files, import_gltf,
};
use crate::image_decode::decode_image;
use crate::material::load_material_in;
use crate::meta::AssetMeta;
use crate::watch::AssetChange;
use crate::{AssetId, codes, fsutil, id};

/// The version of the texture importer. Bump it whenever its output changes.
pub const TEXTURE_IMPORTER_VERSION: u32 = 1;

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
    /// Loaded assets whose file was deleted. Their last version stays loaded.
    pub removed: Vec<String>,
    /// Re-imports that failed (warnings, RE7050); the last good version stays loaded.
    pub problems: Vec<Diagnostic>,
}

/// Loads a project's assets.
///
/// Assets are named by project-relative path (`assets/models/tree.glb`) and
/// loaded synchronously; imports go through the processed-asset cache in
/// `.real/cache/assets/`, so loading an unchanged file again is a cache read.
/// Loading the same path twice gives the same handle. Feed file changes from an
/// [`AssetWatcher`](crate::AssetWatcher) to [`AssetServer::apply_changes`] for
/// hot reload: a failed re-import keeps the last good version.
pub struct AssetServer {
    root: PathBuf,
    cache: ProcessedCache,
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
        let root = root.into();
        let cache = root.join(".real").join("cache").join("assets");
        AssetServer::with_cache_dir(root, cache)
    }

    /// A server for the project at `root`, caching in `cache_dir`.
    pub fn with_cache_dir(root: impl Into<PathBuf>, cache_dir: impl Into<PathBuf>) -> AssetServer {
        AssetServer {
            root: root.into(),
            cache: ProcessedCache::new(cache_dir),
            gltfs: Assets::default(),
            textures: Assets::default(),
            materials: Assets::default(),
            by_path: HashMap::new(),
            id_paths: HashMap::new(),
        }
    }

    /// The project root.
    pub fn root(&self) -> &Path {
        &self.root
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
        let (old_meta, new_meta) = (
            AssetMeta::path_for(&self.root.join(from)),
            AssetMeta::path_for(&self.root.join(to)),
        );
        if old_meta.is_file()
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
        let path = self.root.join(rel);
        let bytes = fsutil::read(&path, rel)?;
        let meta = AssetMeta::read_or_create(&path, rel, serde_json::json!({}))?;
        let id = meta.id();
        self.id_paths.insert(id, rel.to_owned());
        let dir = path.parent().unwrap_or(&self.root).to_path_buf();
        let dep_paths = external_files(&bytes, &dir);
        let dep_bytes: Vec<Vec<u8>> = dep_paths
            .iter()
            .map(|p| std::fs::read(p).unwrap_or_default())
            .collect();
        let deps = dep_paths
            .iter()
            .filter_map(|p| p.strip_prefix(&self.root).ok().map(fsutil::slash))
            .collect();
        let mut sources: Vec<&[u8]> = vec![&bytes];
        sources.extend(dep_bytes.iter().map(Vec::as_slice));
        let key = cache_key(
            "gltf",
            GLTF_IMPORTER_VERSION,
            &meta.settings_bytes(),
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
        let imported = import_gltf(&bytes, Some(&dir), id, rel)?;
        if let Err(d) = self.cache.put(&key, &CachedGltf::from_imported(&imported)) {
            tracing::warn!(code = %d.code, "{}", d.message);
        }
        Ok((imported, id, deps))
    }

    fn import_texture_file(&mut self, rel: &str) -> Result<(TextureData, AssetId), Diagnostic> {
        let path = self.root.join(rel);
        let bytes = fsutil::read(&path, rel)?;
        let meta = AssetMeta::read_or_create(&path, rel, serde_json::json!({}))?;
        let id = meta.id();
        self.id_paths.insert(id, rel.to_owned());
        let format = match meta.settings.get("color_space").and_then(|v| v.as_str()) {
            None | Some("srgb") => TextureFormat::Rgba8Srgb,
            Some("linear") => TextureFormat::Rgba8Unorm,
            Some(other) => {
                return Err(Diagnostic::error(
                    codes::META_INVALID,
                    format!("unknown color_space `{other}` in the sidecar of `{rel}`"),
                )
                .at(Location::file(format!("{rel}.meta.json")))
                .with_hint(
                    "use \"srgb\" for colour textures or \"linear\" for normal and data maps",
                ));
            }
        };
        let key = cache_key(
            "texture",
            TEXTURE_IMPORTER_VERSION,
            &meta.settings_bytes(),
            &[&bytes],
        );
        let (data, _) = self.cache.get_or_insert_with(&key, || {
            decode_image(&bytes, format, &format!("`{rel}`"), rel)
        })?;
        Ok((data, id))
    }

    fn import_material_file(&mut self, rel: &str) -> Result<MaterialDesc, Diagnostic> {
        let desc = load_material_in(&self.root, rel)?;
        // Remember where each texture id lives, for `texture_by_id`.
        let file = std::fs::read_to_string(self.root.join(rel)).unwrap_or_default();
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
