//! What the editor's asset index reads without importing anything (ADR-0064
//! §7): [`import_key`] tells an imported file from a stale one, and
//! [`gltf_names`] lists the clips and skins a model file holds.

mod common;

use re_assets::{AssetChange, AssetServer, codes, gltf_names, import_gltf, import_key};
use re_core::ProjectFiles;

const ROCK: &str = "assets/textures/rock.png";
const LID: &str = "assets/models/lid.glb";

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("real.toml"), "[project]\nname = \"t\"\n").unwrap();
    std::fs::create_dir_all(root.join("assets/models")).unwrap();
    std::fs::create_dir_all(root.join("assets/textures")).unwrap();
    std::fs::write(root.join(LID), common::fixture_glb()).unwrap();
    std::fs::write(root.join(ROCK), common::png_2x2()).unwrap();
    dir
}

#[test]
fn the_import_key_tells_an_imported_file_from_a_stale_one() {
    let dir = project();
    let root = dir.path();
    let files = ProjectFiles::project(root);
    let mut server = AssetServer::new(root);
    let config = server.config();

    // No sidecar yet, so no key, and nothing is written; other kinds have none.
    for path in [ROCK, LID, "real.toml"] {
        assert_eq!(import_key(&files, config, path).unwrap(), None, "{path}");
    }
    assert!(!root.join(format!("{ROCK}.meta.json")).exists());

    server.load_texture(ROCK).unwrap();
    server.load_gltf(LID).unwrap();
    let rock = import_key(&files, config, ROCK).unwrap().unwrap();
    let lid = import_key(&files, config, LID).unwrap().unwrap();
    assert_ne!(rock, lid);
    assert!(server.cache().contains(&rock), "the import used this key");
    assert!(server.cache().contains(&lid));

    // New bytes: a key the cache has not seen, until the file is imported again.
    std::fs::write(root.join(ROCK), common::png(1, 1, &[0, 0, 0, 255])).unwrap();
    let changed = import_key(&files, config, ROCK).unwrap().unwrap();
    assert_ne!(changed, rock);
    assert!(!server.cache().contains(&changed), "stale");
    let report = server.apply_changes(&[AssetChange::Modified(ROCK.into())]);
    assert!(report.problems.is_empty(), "{:?}", report.problems);
    assert!(server.cache().contains(&changed), "imported again");

    // The sidecar's settings are part of the key.
    let sidecar = root.join(format!("{ROCK}.meta.json"));
    let mut meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    meta["settings"] = serde_json::json!({ "color_space": "linear" });
    std::fs::write(&sidecar, meta.to_string()).unwrap();
    let linear = import_key(&files, config, ROCK).unwrap().unwrap();
    assert_ne!(linear, changed);
    assert!(!server.cache().contains(&linear));

    std::fs::write(&sidecar, "{ not json").unwrap();
    let err = import_key(&files, config, ROCK).unwrap_err();
    assert_eq!(err.code, codes::META_INVALID);
}

/// A glTF document with two animations and two skins, the second of each
/// unnamed. Its buffer is never read.
const NAMES: &str = r#"{
  "asset": {"version": "2.0"},
  "nodes": [{"name": "hip"}, {"name": "knee"}],
  "skins": [{"name": "body", "joints": [0, 1]}, {"joints": [1]}],
  "buffers": [{"byteLength": 32, "uri": "never-read.bin"}],
  "bufferViews": [{"buffer": 0, "byteLength": 8}, {"buffer": 0, "byteOffset": 8, "byteLength": 24}],
  "accessors": [
    {"bufferView": 0, "componentType": 5126, "count": 2, "type": "SCALAR", "min": [0], "max": [1]},
    {"bufferView": 1, "componentType": 5126, "count": 2, "type": "VEC3"}
  ],
  "animations": [
    {"name": "walk", "samplers": [{"input": 0, "output": 1}], "channels": [{"sampler": 0, "target": {"node": 0, "path": "translation"}}]},
    {"samplers": [{"input": 0, "output": 1}], "channels": [{"sampler": 0, "target": {"node": 1, "path": "translation"}}]}
  ]
}"#;

#[test]
fn a_models_clips_and_skins_are_named_as_the_importer_names_them() {
    assert_eq!(
        gltf_names(NAMES.as_bytes()),
        (
            vec!["walk".to_owned(), "animation_1".to_owned()],
            vec!["body".to_owned(), "skin_1".to_owned()]
        )
    );

    let glb = common::fixture_glb();
    let imported = import_gltf(&glb, None, re_assets::AssetId::new_source(), LID).unwrap();
    let (clips, skins) = gltf_names(&glb);
    assert_eq!(
        clips,
        imported
            .clips
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(clips, ["open"]);
    assert!(skins.is_empty());

    assert_eq!(gltf_names(b"not a model"), (Vec::new(), Vec::new()));
}
