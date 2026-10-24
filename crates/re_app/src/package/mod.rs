//! The file stacks a run reads through, and packaged games (ADR-0037).
//!
//! A development run reads the engine layer, then the project directory
//! ([`dev_files`]). A packaged game reads the engine layer, then its archive
//! ([`archive`]), then the folder that holds the archive ([`open_package`]),
//! and writes only to its per-user folders ([`dirs`]).

pub mod archive;
pub mod config;
pub mod dirs;

use std::path::Path;
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
use re_core::Diagnostic;
#[cfg(not(target_arch = "wasm32"))]
use re_core::files::DirLayer;
use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, ProjectPath};

pub use archive::{ARCHIVE_NAME, Archive, PackageInfo, PackageWriter, RuntimeVersions, StoredForm};
pub use config::{AssetStorage, PackageConfig, ScriptStorage};

#[cfg(not(target_arch = "wasm32"))]
use crate::codes;

/// The project path of a compat default script (`.real/defaults/<container>/<name>.luau`).
pub fn default_script_path(container: &str, name: &str) -> String {
    format!(".real/defaults/{container}/{name}.luau")
}

/// The files the engine provides in every run: the compat default scripts under
/// `.real/defaults/`, from the sources `re_compat` compiles in. A compat boot
/// therefore writes nothing into the project.
pub fn engine_layer() -> MemLayer {
    let mut layer = MemLayer::new(LayerKind::Engine);
    for script in re_compat::humanoid::scripts::DEFAULT_SCRIPTS {
        let path = default_script_path(script.container, script.name);
        layer.insert(
            ProjectPath::new(&path).expect("default script paths are valid"),
            script.source.as_bytes(),
            Form::Source,
        );
    }
    layer
}

/// A development run of the project in `project`: the engine layer, then the
/// project directory, with the writable folders under `project/.real/`.
pub fn dev_files(project: &Path) -> ProjectFiles {
    ProjectFiles::project(project).with_first_layer(Arc::new(engine_layer()))
}

/// `files` with the engine layer asked first (for stacks built elsewhere, such
/// as a test's in-memory project).
pub fn with_engine_layer(files: &ProjectFiles) -> ProjectFiles {
    files.with_first_layer(Arc::new(engine_layer()))
}

/// Where a game executable at `exe` looks for its archive: beside it, or on
/// macOS, in the app's `Contents/Resources/` when the executable is in
/// `Contents/MacOS/`.
#[cfg(not(target_arch = "wasm32"))]
pub fn archive_beside(exe: &Path) -> PathBuf {
    let dir = exe.parent().unwrap_or(Path::new("."));
    if dir.file_name().is_some_and(|n| n == "MacOS")
        && let Some(contents) = dir.parent()
    {
        let resources = contents.join("Resources").join(archive::ARCHIVE_NAME);
        if resources.is_file() {
            return resources;
        }
    }
    dir.join(archive::ARCHIVE_NAME)
}

/// A packaged game, open.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct Package {
    /// What the game reads and writes: the engine layer, the archive, the
    /// folder that holds it, and the per-user folders.
    pub files: ProjectFiles,
    /// The archive, for its index and its case-only matches (`RE0603`).
    pub archive: Arc<Archive>,
}

/// Opens the packaged game whose archive is at `archive_path`. With
/// `data_dir` (`--data-dir`), its writable folders are there instead of the
/// per-user folders.
///
/// # Errors
///
/// `RE0604` when there is no archive, `RE0600` or `RE0602` when it cannot be
/// read, `RE0605` when there is no per-user folder.
#[cfg(not(target_arch = "wasm32"))]
pub fn open_package(archive_path: &Path, data_dir: Option<&Path>) -> Result<Package, Diagnostic> {
    if !archive_path.is_file() {
        return Err(Diagnostic::error(
            codes::PACKAGE_MISSING,
            format!("there is no {} at {}", archive::ARCHIVE_NAME, archive_path.display()),
        )
        .with_hint("ship the whole folder `real build` writes; run a project in development with `real run`"));
    }
    let archive = Arc::new(Archive::open(archive_path)?);
    let writable = dirs::user_dirs(&archive.index().package.identifier, data_dir)?;
    let folder = archive_path
        .parent()
        .unwrap_or(Path::new("."))
        .to_path_buf();
    let files = ProjectFiles::new(
        vec![
            Arc::new(engine_layer()),
            archive.clone(),
            Arc::new(DirLayer::folder(folder)),
        ],
        writable,
        None,
    );
    Ok(Package { files, archive })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_layer_serves_the_default_scripts_first() {
        let dir = std::env::temp_dir().join(format!("re_app_package_{}", std::process::id()));
        let script = &re_compat::humanoid::scripts::DEFAULT_SCRIPTS[0];
        let rel = default_script_path(script.container, script.name);
        std::fs::create_dir_all(dir.join(&rel).parent().unwrap()).unwrap();
        std::fs::write(dir.join(&rel), "-- an old copy").unwrap();
        let files = dev_files(&dir);
        let data = files.read(&rel).unwrap();
        assert_eq!(data.layer, LayerKind::Engine);
        assert_eq!(&*data.bytes, script.source.as_bytes());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
