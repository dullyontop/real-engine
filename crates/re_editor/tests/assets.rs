//! The asset index (ADR-0064 §7): the project's files by kind, built on a
//! worker thread and kept current from the App's file changes, with each
//! texture's and model's sidecar id and import state, and each model's
//! clips and skins.
//!
//! No GPU is needed: the index reads the App's asset server, not its
//! renderer.

use std::path::Path;

use re_app::{App, AppConfig};
use re_assets::AssetChange;
use re_command::{AllowAll, Origin};
use re_editor::{AssetKind, Editor, ImportState};
use re_render::Image;
use serde_json::json;

const ROCK: &str = "assets/textures/rock.png";
const MOSS: &str = "assets/textures/moss.png";
const RIG: &str = "assets/models/rig.gltf";

/// A model with two animations and a skin, the second animation unnamed.
/// Its buffer is never read by the index.
const RIG_TEXT: &str = r#"{
  "asset": {"version": "2.0"},
  "nodes": [{"name": "hip"}, {"name": "knee"}],
  "skins": [{"name": "body", "joints": [0, 1]}],
  "buffers": [{"byteLength": 32, "uri": "rig.bin"}],
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

fn png(path: &Path, rgba: [u8; 4]) {
    Image {
        width: 2,
        height: 1,
        rgba: [rgba, rgba].concat(),
    }
    .save_png(path)
    .unwrap();
}

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let write = |path: &str, text: &str| {
        let p = root.join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    };
    write("real.toml", "[project]\nname = \"index\"\n");
    write(
        "scenes/main.scene.json",
        r#"{ "format": "real-engine/scene", "version": 1, "entities": [] }"#,
    );
    write("materials/rock.material.json", r#"{ "roughness": 0.8 }"#);
    write("scripts/game/main.luau", "print('hi')\n");
    write(RIG, RIG_TEXT);
    write("notes.txt", "not an asset");
    // Hidden folders and the engine's own are left out.
    write(".git/objects/x.png", "no");
    write(".real/cache/thumbs/x.png", "no");
    std::fs::create_dir_all(root.join("assets/textures")).unwrap();
    png(&root.join(ROCK), [200, 100, 50, 255]);
    png(&root.join(MOSS), [20, 160, 40, 255]);
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn kinds(ed: &Editor) -> Vec<(String, AssetKind)> {
    ed.assets()
        .files()
        .map(|f| (f.path.clone(), f.kind))
        .collect()
}

#[test]
fn the_index_knows_the_projects_files_by_kind() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = Editor::new();
    assert!(!ed.assets().ready());
    ed.wait_for_assets(&mut app);
    assert!(ed.assets().ready());
    assert_eq!(
        kinds(&ed),
        [
            (RIG.to_string(), AssetKind::Model),
            (MOSS.to_string(), AssetKind::Texture),
            (ROCK.to_string(), AssetKind::Texture),
            (
                "materials/rock.material.json".to_string(),
                AssetKind::Material
            ),
            ("scenes/main.scene.json".to_string(), AssetKind::Scene),
            ("scripts/game/main.luau".to_string(), AssetKind::Script),
        ]
    );
    let folders: Vec<&str> = ed.assets().folders().iter().map(String::as_str).collect();
    assert_eq!(
        folders,
        [
            "assets",
            "assets/models",
            "assets/textures",
            "materials",
            "scenes",
            "scripts",
            "scripts/game"
        ]
    );
    let rig = ed.assets().file(RIG).unwrap();
    assert_eq!(rig.size, RIG_TEXT.len() as u64);
    assert_eq!(rig.clips, ["walk", "animation_1"]);
    assert_eq!(rig.skins, ["body"]);
    assert_eq!(
        ed.assets().of_kind(AssetKind::Clip),
        [format!("{RIG}#walk"), format!("{RIG}#animation_1")]
    );
    assert_eq!(
        ed.assets().of_kind(AssetKind::Skin),
        [format!("{RIG}#body")]
    );
    assert_eq!(ed.assets().of_kind(AssetKind::Texture), [MOSS, ROCK]);

    // Nothing was imported yet: no sidecars, so no ids.
    let rock = ed.assets().file(ROCK).unwrap();
    assert_eq!(
        (rock.id.as_deref(), rock.import.clone()),
        (None, Some(ImportState::Source))
    );
    assert_eq!(ed.assets().state(&app, ROCK), Some(ImportState::Source));
    assert_eq!(
        ed.assets().state(&app, "materials/rock.material.json"),
        None,
        "a material has no import"
    );
    assert!(!dir.path().join(format!("{ROCK}.meta.json")).exists());
}

#[test]
fn the_index_follows_the_apps_file_changes() {
    let dir = project();
    let root = dir.path();
    let mut app = open(root);
    let mut ed = Editor::new();
    ed.wait_for_assets(&mut app);
    let revision = ed.assets().revision();

    // A load writes the sidecar; the watcher reports it.
    app.media_mut().server().load_texture(ROCK).unwrap();
    app.apply_file_changes(&[AssetChange::Created(format!("{ROCK}.meta.json"))]);
    ed.wait_for_assets(&mut app);
    assert!(ed.assets().revision() > revision);
    let rock = ed.assets().file(ROCK).unwrap().clone();
    let id = rock.id.clone().expect("the sidecar's id");
    assert_eq!(rock.import, Some(ImportState::Imported));
    assert_eq!(ed.assets().state(&app, ROCK), Some(ImportState::Loaded(0)));
    assert_eq!(
        ed.assets().file(ROCK).map(|f| f.path.as_str()),
        Some(ROCK),
        "the sidecar is not an asset of its own"
    );
    assert!(ed.assets().file(&format!("{ROCK}.meta.json")).is_none());

    // A loaded texture that changes is imported again (version 1); one
    // that is not loaded is stale until its next load.
    png(&root.join(ROCK), [1, 2, 3, 255]);
    let moss_meta = json!({ "version": 1, "uuid": "8c5b6f7e-2f1d-4a8e-9a63-3f4c2b1d0e9a" });
    std::fs::write(
        root.join(format!("{MOSS}.meta.json")),
        moss_meta.to_string(),
    )
    .unwrap();
    app.apply_file_changes(&[
        AssetChange::Modified(ROCK.into()),
        AssetChange::Created(format!("{MOSS}.meta.json")),
    ]);
    ed.wait_for_assets(&mut app);
    assert_eq!(ed.assets().state(&app, ROCK), Some(ImportState::Loaded(1)));
    assert_eq!(
        ed.assets().file(ROCK).unwrap().import,
        Some(ImportState::Imported)
    );
    assert_eq!(ed.assets().file(ROCK).unwrap().id, Some(id), "same id");
    let moss = ed.assets().file(MOSS).unwrap();
    assert_eq!(moss.import, Some(ImportState::Stale));
    assert_eq!(
        moss.id.as_deref(),
        Some("8c5b6f7e-2f1d-4a8e-9a63-3f4c2b1d0e9a")
    );

    // A sidecar that cannot be read fails the import.
    std::fs::write(root.join(format!("{MOSS}.meta.json")), "{ not json").unwrap();
    app.apply_file_changes(&[AssetChange::Modified(format!("{MOSS}.meta.json"))]);
    ed.wait_for_assets(&mut app);
    assert!(matches!(
        ed.assets().file(MOSS).unwrap().import,
        Some(ImportState::Failed(_))
    ));

    // Renames, removals (of a file and of a whole folder) and new files.
    std::fs::rename(
        root.join("materials/rock.material.json"),
        root.join("materials/stone.material.json"),
    )
    .unwrap();
    std::fs::remove_dir_all(root.join("assets/models")).unwrap();
    std::fs::remove_file(root.join("scripts/game/main.luau")).unwrap();
    app.apply_file_changes(&[
        AssetChange::Renamed {
            from: "materials/rock.material.json".into(),
            to: "materials/stone.material.json".into(),
        },
        AssetChange::Removed("assets/models".into()),
        AssetChange::Removed("scripts/game/main.luau".into()),
    ]);
    // A command's change: asset.import copies a file in and says so.
    let outside = tempfile::tempdir().unwrap();
    png(&outside.path().join("bark.png"), [90, 60, 30, 255]);
    let out = app.dispatch(
        "asset.import",
        json!({ "source": outside.path().join("bark.png").display().to_string(), "dest": "assets/textures/bark.png" }),
        false,
        &AllowAll,
        Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    ed.wait_for_assets(&mut app);
    assert_eq!(
        kinds(&ed),
        [
            ("assets/textures/bark.png".to_string(), AssetKind::Texture),
            (MOSS.to_string(), AssetKind::Texture),
            (ROCK.to_string(), AssetKind::Texture),
            (
                "materials/stone.material.json".to_string(),
                AssetKind::Material
            ),
            ("scenes/main.scene.json".to_string(), AssetKind::Scene),
        ]
    );
    assert!(ed.assets().of_kind(AssetKind::Clip).is_empty());
    assert!(!ed.assets().folders().contains("assets/models"));
    assert!(!ed.assets().folders().contains("scripts"));
    assert_eq!(
        ed.assets().state(&app, "assets/textures/bark.png"),
        Some(ImportState::Loaded(0)),
        "asset.import loads what it copies"
    );
}
