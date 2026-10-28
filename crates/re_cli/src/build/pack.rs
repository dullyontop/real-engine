//! Packing a project into `game.rpak` (ADR-0037): which files go in, and in
//! which form.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use re_app::AppConfig;
use re_app::package::archive::compressed_size;
use re_app::package::config::glob_match;
use re_app::package::{
    AssetStorage, PackageConfig, PackageInfo, PackageWriter, ScriptStorage, StoredForm,
};
use re_assets::{AssetServer, ProcessedAsset};
use re_core::files::{ProjectFiles, ProjectPath};
use re_core::{Diagnostic, Location};
use re_types::render::{TextureData, TextureFormat};
use serde::Serialize;

use crate::codes;

/// Top-level files packed by default.
pub const DEFAULT_FILES: &[&str] = &["real.toml", ".luaurc"];

/// Folders packed by default: the project's runtime content.
pub const DEFAULT_DIRS: &[&str] = &[
    "scenes",
    "prefabs",
    "materials",
    "graphs",
    "anims",
    "ui",
    "scripts",
    "shaders",
    "assets",
    "data/seed",
];

/// Never packed, whatever `[package] include` says.
pub fn never_packed(path: &str) -> bool {
    let top = path.split('/').next().unwrap_or(path);
    matches!(top, "tests" | ".real" | ".git") || matches!(path, "AGENTS.md" | "CLAUDE.md")
}

/// Files the asset server imports (and so may store processed).
fn importable(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".gltf", ".glb", ".png", ".jpg", ".jpeg"]
        .iter()
        .any(|ext| lower.ends_with(ext))
}

/// Scripts compiled to bytecode; type definition files (`.d.luau`) are for
/// editors and stay source.
fn is_script(path: &str) -> bool {
    (path.ends_with(".luau") || path.ends_with(".lua")) && !path.ends_with(".d.luau")
}

/// Packed JSON files whose string values are checked for project paths:
/// every one but asset sidecars.
fn names_files(path: &str) -> bool {
    path.ends_with(".json") && !path.ends_with(".meta.json")
}

/// The project files the packed JSON files (scenes, prefabs, materials,
/// graphs) name in their string values (`assets/hut.glb#mesh0` names
/// `assets/hut.glb`), each with the first file that names it. Only strings
/// naming a file that exists count; `.real/` (the engine's defaults) never
/// does.
fn named_files(files: &ProjectFiles, paths: &BTreeSet<String>) -> BTreeMap<String, String> {
    fn walk(value: &serde_json::Value, out: &mut Vec<String>) {
        match value {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, out)),
            serde_json::Value::Object(map) => map.values().for_each(|v| walk(v, out)),
            _ => {}
        }
    }
    let mut named = BTreeMap::new();
    for path in paths.iter().filter(|p| names_files(p)) {
        let Ok(bytes) = files.read_source(path) else {
            continue;
        };
        let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            continue;
        };
        let mut strings = Vec::new();
        walk(&doc, &mut strings);
        for s in strings {
            let target = s.split('#').next().unwrap_or_default();
            if target == path
                || !target.contains('/')
                || target.starts_with(".real/")
                || ProjectPath::new(target).is_err()
                || !files.is_file(target)
            {
                continue;
            }
            named
                .entry(target.to_owned())
                .or_insert_with(|| path.clone());
        }
    }
    named
}

/// An `RE0957` error for each file a packed JSON file names that is not
/// packed: the packaged game would not find it.
fn unpacked_names(named: &BTreeMap<String, String>, paths: &BTreeSet<String>) -> Vec<Diagnostic> {
    named
        .iter()
        .filter(|(target, _)| !paths.contains(*target))
        .map(|(target, by)| {
            let hint = if never_packed(target) {
                format!("`{target}` is never packed (tests, agent notes): move it under `assets/`")
            } else {
                format!(
                    "add it to `[package] include` (or stop excluding it): include = [\"{target}\"]"
                )
            };
            Diagnostic::error(
                codes::NOT_PACKED,
                format!("`{by}` names `{target}`, which is not packed"),
            )
            .at(Location::file(by.clone()))
            .with_hint(hint)
        })
        .collect()
}

/// How one asset was stored, and why.
#[derive(Clone, Debug, Serialize)]
pub struct AssetChoice {
    /// The asset.
    pub path: String,
    /// `processed` or `source`.
    pub stored: &'static str,
    /// The processed copy's compressed size.
    pub processed_size: u64,
    /// The compressed size of the source, its sidecar and the files it pulls
    /// in.
    pub source_size: u64,
    /// Why this form.
    pub reason: String,
}

/// A texture the packaged game draws uncompressed, and why (ADR-0047).
#[derive(Clone, Debug, Serialize)]
pub struct UncompressedTexture {
    /// The texture file, or a glTF file and the name of its image.
    pub texture: String,
    /// Why it is not block-compressed.
    pub reason: String,
}

/// What [`pack`] decided.
#[derive(Debug, Default, Serialize)]
pub struct PackReport {
    /// The package.
    pub package: Option<PackageInfo>,
    /// Each importable asset's stored form.
    pub assets: Vec<AssetChoice>,
    /// The textures that are not block-compressed in the package.
    pub uncompressed_textures: Vec<UncompressedTexture>,
    /// Sidecars `real build` created in the project (commit them).
    pub created_sidecars: Vec<String>,
    /// Files left out because only processed glTF files use them.
    pub dropped: Vec<String>,
    /// Warnings and errors.
    pub diagnostics: Vec<Diagnostic>,
}

/// The package info and its checks: `RE0950` errors for invalid values, and
/// a warning when the identifier is defaulted.
pub fn package_info(config: &AppConfig) -> (PackageInfo, Vec<Diagnostic>) {
    let pc = config.package.clone().unwrap_or_default();
    let mut diags = Vec::new();
    let at = || Location::file("real.toml");
    let name = config.project.name.clone();
    let identifier = pc.identifier_or_default(&name);
    match &pc.identifier {
        None => diags.push(
            Diagnostic::warning(
                codes::PACKAGE_CONFIG,
                format!("[package] has no identifier; building as `{identifier}`"),
            )
            .at(at())
            .with_hint("set `[package] identifier = \"com.example.game\"`: it names the folders the game saves in"),
        ),
        Some(id) => {
            if let Some(why) = re_app::package::config::identifier_problem(id) {
                diags.push(
                    Diagnostic::error(
                        codes::PACKAGE_CONFIG,
                        format!("[package] identifier `{id}` is not valid: {why}"),
                    )
                    .at(at()),
                );
            }
        }
    }
    if let Some(icon) = &pc.icon
        && !icon_named(icon)
    {
        diags.push(
            Diagnostic::error(
                codes::PACKAGE_CONFIG,
                format!("[package] icon `{icon}` must be a PNG named by its project path"),
            )
            .at(at()),
        );
    }
    for pattern in pc.include.iter().chain(&pc.exclude).chain(&pc.notices) {
        if pattern.starts_with('/')
            || pattern.split('/').any(|s| s == "..")
            || pattern.contains('\\')
        {
            diags.push(
                Diagnostic::error(
                    codes::PACKAGE_CONFIG,
                    format!("[package] pattern `{pattern}` must be a project path pattern (`assets/raw/**`)"),
                )
                .at(at()),
            );
        }
    }
    let info = PackageInfo {
        name,
        identifier,
        version: pc.version_or_default().to_owned(),
    };
    (info, diags)
}

/// The smallest `[package] icon` side, in pixels.
pub const ICON_MIN: u32 = 512;

/// True when `[package] icon` names a PNG by a valid project path
/// ([`package_info`] reports the others).
fn icon_named(icon: &str) -> bool {
    icon.to_ascii_lowercase().ends_with(".png") && ProjectPath::new(icon).is_ok()
}

/// The `[package] icon` decoded, checked to be a square PNG of at least
/// [`ICON_MIN`] pixels.
///
/// # Errors
///
/// `RE0950` when the file is missing, not a PNG, not square or too small.
pub fn icon_image(files: &ProjectFiles, icon: &str) -> Result<TextureData, Diagnostic> {
    let problem = |why: String| {
        Diagnostic::error(
            codes::PACKAGE_CONFIG,
            format!("[package] icon `{icon}` {why}"),
        )
        .at(Location::file("real.toml"))
    };
    let bytes = files
        .read_source(icon)
        .map_err(|e| problem(format!("cannot be read: {e}")))?;
    let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .map_err(|e| problem(format!("is not a PNG image: {e}")))?
        .into_rgba8();
    let (width, height) = image.dimensions();
    if width != height || width < ICON_MIN {
        return Err(problem(format!(
            "is {width}×{height} px; it must be square and at least {ICON_MIN} px across"
        ))
        .with_hint("a 1024×1024 PNG gives every size the platforms ask for"));
    }
    Ok(TextureData {
        width,
        height,
        format: TextureFormat::Rgba8Srgb,
        pixels: image.into_raw(),
        levels: Vec::new(),
    })
}

/// The project paths to pack, sorted.
fn collect(files: &ProjectFiles, pc: &PackageConfig) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    for f in DEFAULT_FILES {
        if files.is_file(f) {
            paths.insert((*f).to_owned());
        }
    }
    for dir in DEFAULT_DIRS {
        for p in files.list(dir, "") {
            paths.insert(p.as_str().to_owned());
        }
    }
    if !pc.include.is_empty() {
        for p in files.list("", "") {
            let p = p.as_str();
            if !never_packed(p) && pc.include.iter().any(|g| glob_match(g, p)) {
                paths.insert(p.to_owned());
            }
        }
    }
    paths.retain(|p| {
        !never_packed(p) && !p.starts_with(".real/") && !pc.exclude.iter().any(|g| glob_match(g, p))
    });
    // The engine layer serves `.real/defaults/`; it is never packed.
    paths
}

/// Packs the project at `project` (a development run's files) into a
/// [`PackageWriter`]. Creates missing sidecars in the project, as a
/// development run does, and lists them in the report.
pub fn pack(project: &Path, config: &AppConfig) -> (PackageWriter, PackReport) {
    let (info, diags) = package_info(config);
    let mut report = PackReport {
        package: Some(info.clone()),
        diagnostics: diags,
        ..PackReport::default()
    };
    let mut writer = PackageWriter::new(info);
    let pc = config.package.clone().unwrap_or_default();
    let files = re_app::package::dev_files(project);
    let mut paths = collect(&files, &pc);
    // The icon is packed wherever it is, and stored as source: the game reads
    // it as its window icon. One outside the packed folders is not imported.
    let icon = pc.icon.clone().filter(|i| icon_named(i));
    let icon_only = icon.clone().filter(|i| !paths.contains(i));
    if let Some(icon) = &icon {
        match icon_image(&files, icon) {
            Ok(_) => {
                paths.insert(icon.clone());
            }
            Err(d) => report.diagnostics.push(d),
        }
    }
    let mut named = named_files(&files, &paths);
    // `[ui] theme` names the native UI's theme file (ADR-0058).
    let theme = &config.ui.theme;
    if !theme.is_empty() && files.is_file(theme) {
        named
            .entry(theme.clone())
            .or_insert_with(|| "real.toml".to_owned());
    }
    report.diagnostics.extend(unpacked_names(&named, &paths));

    // Two paths that differ only by case cannot both be found ignoring case.
    let mut folded: BTreeMap<String, &str> = BTreeMap::new();
    for p in &paths {
        if let Some(other) = folded.insert(p.to_ascii_lowercase(), p) {
            report.diagnostics.push(
                Diagnostic::error(
                    codes::CASE_COLLISION,
                    format!("`{other}` and `{p}` differ only by case"),
                )
                .at(Location::file(p.clone())),
            );
        }
    }

    // Assets: import each (creating missing sidecars), then choose its form.
    let mut server = AssetServer::with_files(files.clone());
    server.set_config(config.assets);
    let mut processed: BTreeMap<String, ProcessedAsset> = BTreeMap::new();
    for p in paths
        .iter()
        .filter(|p| importable(p) && icon_only.as_ref() != Some(*p))
    {
        let sidecar = format!("{p}.meta.json");
        let had_sidecar = files.is_file(&sidecar);
        match server.processed(p) {
            Ok(Some(asset)) => {
                processed.insert(p.clone(), asset);
            }
            Ok(None) => {}
            Err(d) => report.diagnostics.push(d),
        }
        if !had_sidecar && files.is_file(&sidecar) {
            report.created_sidecars.push(sidecar);
        }
    }
    let read = |p: &str| files.read_source(p).map(|b| b.to_vec());
    let mut stored_processed: BTreeMap<String, bool> = BTreeMap::new();
    for (path, asset) in &processed {
        let processed_size = compressed_size(&asset.bytes);
        let mut source_size = read(path).map(|b| compressed_size(&b)).unwrap_or(0);
        source_size += read(&format!("{path}.meta.json"))
            .map(|b| compressed_size(&b))
            .unwrap_or(0);
        for dep in &asset.dependencies {
            source_size += read(dep).map(|b| compressed_size(&b)).unwrap_or(0);
        }
        let (keep, reason) = match pc.assets {
            AssetStorage::Processed => (true, "[package] assets = \"processed\"".to_owned()),
            AssetStorage::Source => (false, "[package] assets = \"source\"".to_owned()),
            AssetStorage::Auto if !asset.repeat_at_load => (
                true,
                "its textures are block-compressed, which is too slow to repeat at load".to_owned(),
            ),
            AssetStorage::Auto if processed_size <= source_size.saturating_mul(2) => (
                true,
                "the processed copy is at most twice the source's size".to_owned(),
            ),
            AssetStorage::Auto => (
                false,
                "the processed copy is more than twice the source's size; it is imported on first run".to_owned(),
            ),
        };
        stored_processed.insert(path.clone(), keep);
        report.assets.push(AssetChoice {
            path: path.clone(),
            stored: if keep { "processed" } else { "source" },
            processed_size,
            source_size,
            reason,
        });
    }
    // A glTF stored as source reads its external files as source.
    let mut needed_as_source = BTreeSet::new();
    let mut only_for_processed = BTreeSet::new();
    for (path, asset) in &processed {
        for dep in &asset.dependencies {
            if stored_processed[path] {
                only_for_processed.insert(dep.clone());
            } else {
                needed_as_source.insert(dep.clone());
            }
        }
    }
    for dep in &needed_as_source {
        if stored_processed.get(dep) == Some(&true) {
            stored_processed.insert(dep.clone(), false);
            if let Some(choice) = report.assets.iter_mut().find(|c| &c.path == dep) {
                choice.stored = "source";
                choice.reason = "a glTF file stored as source reads it".to_owned();
            }
        }
    }

    if let Some(icon) = &icon
        && stored_processed.get(icon) == Some(&true)
    {
        stored_processed.insert(icon.clone(), false);
        if let Some(choice) = report.assets.iter_mut().find(|c| &c.path == icon) {
            choice.stored = "source";
            choice.reason = "[package] icon: the game reads it as its window icon".to_owned();
        }
    }

    // Textures the game draws as RGBA8: stored as source (the runtime has no
    // encoder) or not encoded at build time.
    for (path, asset) in &processed {
        let as_source = stored_processed.get(path) != Some(&true);
        for t in &asset.textures {
            let reason = if as_source {
                "stored as source, which a packaged game imports as RGBA8".to_owned()
            } else if t.format.is_compressed() {
                continue;
            } else if !re_assets::TEXTURE_ENCODER {
                "this `real` was built without the texture encoder".to_owned()
            } else if !config.assets.encodes() {
                "[assets] texture_compression = \"none\"".to_owned()
            } else {
                format!(
                    "{}x{} is not a multiple of 4, which block compression needs",
                    t.width, t.height
                )
            };
            let texture = if &t.name == path {
                path.clone()
            } else {
                format!("{path} image `{}`", t.name)
            };
            report
                .uncompressed_textures
                .push(UncompressedTexture { texture, reason });
        }
    }

    // Write the entries.
    let mut add = |path: &str, bytes: Vec<u8>, form: StoredForm, report: &mut PackReport| {
        let Ok(pp) = ProjectPath::new(path) else {
            return;
        };
        if writer.contains(&pp) {
            return;
        }
        if let Err(e) = writer.add(pp, bytes, form) {
            report.diagnostics.push(
                Diagnostic::error(codes::BUILD_STOPPED, e).at(Location::file(path.to_owned())),
            );
        }
    };
    let mut sidecars: BTreeSet<String> = BTreeSet::new();
    for path in &paths {
        if only_for_processed.contains(path)
            && !needed_as_source.contains(path)
            && !named.contains_key(path)
            && !importable(path)
        {
            report.dropped.push(path.clone());
            continue;
        }
        if importable(path) {
            sidecars.insert(format!("{path}.meta.json"));
        }
        if stored_processed.get(path) == Some(&true) {
            let asset = &processed[path];
            let form = StoredForm::Processed {
                importer: asset.importer.to_owned(),
                version: asset.version,
            };
            add(path, asset.bytes.clone(), form, &mut report);
            continue;
        }
        let bytes = match read(path) {
            Ok(b) => b,
            Err(e) => {
                report.diagnostics.push(
                    Diagnostic::error(codes::BUILD_STOPPED, format!("cannot pack `{path}`: {e}"))
                        .at(Location::file(path.clone())),
                );
                continue;
            }
        };
        if is_script(path) && pc.scripts == ScriptStorage::Bytecode {
            let source = String::from_utf8_lossy(&bytes);
            match re_script::compile_bytecode(&source, path) {
                Ok(code) => add(
                    path,
                    code,
                    StoredForm::Bytecode {
                        compiler: re_script::COMPILER_ID.to_owned(),
                    },
                    &mut report,
                ),
                Err(ds) => report.diagnostics.extend(ds),
            }
            continue;
        }
        add(path, bytes, StoredForm::Source, &mut report);
    }
    // Sidecars created during this build were not listed above.
    for sidecar in sidecars {
        if let Ok(bytes) = read(&sidecar) {
            add(&sidecar, bytes, StoredForm::Source, &mut report);
        }
    }
    (writer, report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tests_and_agent_notes_are_never_packed() {
        assert!(never_packed("tests/replays/a.replay.json"));
        assert!(never_packed(".real/cache/x"));
        assert!(never_packed("AGENTS.md"));
        assert!(!never_packed("scripts/AGENTS.md"));
        assert!(!never_packed("scenes/main.scene.json"));
    }
}
