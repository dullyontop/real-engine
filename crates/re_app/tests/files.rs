//! The island boots and plays from an in-memory copy with no project directory
//! (ADR-0037): a read that bypasses `ProjectFiles` would find nothing, so this
//! fails on any missed `std::fs` read site. The run matches the same replay
//! played from the directory.

use std::path::{Path, PathBuf};

use re_app::{App, AppConfig};
use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, Writable};

fn island() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/island")
}

fn replay() -> re_types::ReplayDoc {
    let path = island().join("tests/replays/regenerate.replay.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The island copied into memory, without its `.real/` folder (caches and
/// tool settings, not content).
fn in_memory() -> ProjectFiles {
    let mut layer = MemLayer::new(LayerKind::Memory);
    for path in ProjectFiles::project(island()).list("", "") {
        if !path.as_str().starts_with(".real/") {
            let bytes = std::fs::read(island().join(path.as_str())).unwrap();
            layer.insert(path, bytes, Form::Source);
        }
    }
    // `App::open_files` adds the engine layer itself.
    ProjectFiles::for_tests(layer)
}

#[test]
fn the_island_boots_and_plays_from_memory() {
    let files = in_memory();
    assert!(files.project_dir().is_none());
    let config = AppConfig::load_from(&files).unwrap();
    let mut app = App::open_files(files.clone(), config).unwrap();
    assert!(app.project().as_os_str().is_empty());
    let report = app.run_headless(Some(480), Some(replay())).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    // Procedural outputs were cached in the run's (in-memory) cache folder.
    let cached = files.writable(Writable::Cache).list("proc").unwrap();
    assert!(!cached.is_empty());
    // Writing project files is refused.
    assert!(app.save_scene().is_err());

    let project = island();
    let mut from_dir = App::open(&project, AppConfig::load(&project).unwrap()).unwrap();
    let expected = from_dir.run_headless(Some(480), Some(replay())).unwrap();
    assert_eq!(report.state_hash, expected.state_hash);
}
