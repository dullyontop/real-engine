//! The file stacks a run reads through (ADR-0037).
//!
//! A development run reads the engine layer, then the project directory
//! ([`dev_files`]). A packaged game reads the engine layer, then its archive, then
//! the folder that holds the archive; the archive format arrives with WP19 PR
//! "archive".

use std::path::Path;
use std::sync::Arc;

use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, ProjectPath};

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
