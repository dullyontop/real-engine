//! `real new DIR`: a new project with `real.toml`, an entry scene and the
//! standard folders (ARCHITECTURE §8).

use std::path::Path;

use re_app::AppConfig;
use re_core::Diagnostic;

use crate::codes;

/// The entry scene of a new project: a camera, a sun, static ground and one
/// dynamic box that falls onto it. Written in canonical form (load, then save).
const STARTER_SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "box", "name": "Box", "components": {
        "Collider": { "shape": "Box", "half_extents": [0.5, 0.5, 0.5] },
        "MaterialRef": { "asset": "materials/box.material.json" },
        "MeshRef": { "asset": "builtin:cube" },
        "RigidBody": { "kind": "Dynamic", "mass": 10.0 },
        "Transform": { "translation": [0.0, 4.0, 0.0], "rotation": [0.2, 0.3, 0.1, 0.93] } } },
    { "id": "camera", "name": "Camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 5.0, 12.0], "rotation": [-0.19509032, 0.0, 0.0, 0.98078528] } } },
    { "id": "ground", "name": "Ground", "components": {
        "Collider": { "shape": "Box", "half_extents": [0.5, 0.5, 0.5] },
        "MaterialRef": { "asset": "materials/ground.material.json" },
        "MeshRef": { "asset": "builtin:cube" },
        "Transform": { "translation": [0.0, -0.5, 0.0], "scale": [40.0, 1.0, 40.0] } } },
    { "id": "sun", "name": "Sun", "components": {
        "Light": { "kind": "Directional", "intensity": 1.0, "shadows": true },
        "Transform": { "rotation": [-0.5, 0.3, 0.1, 0.8] } } }
  ]
}"#;

fn material(base_color: [f32; 4], roughness: f32) -> String {
    let [r, g, b, a] = base_color;
    format!(
        "{{\n  \"$schema\": \"../schemas/material.schema.json\",\n  \"base_color\": [{r:?}, {g:?}, {b:?}, {a:?}],\n  \"roughness\": {roughness:?}\n}}\n"
    )
}

const FOLDERS: &[&str] = &[
    "scenes",
    "prefabs",
    "materials",
    "graphs",
    "scripts/game",
    "scripts/tools",
    "scripts/shared",
    "assets",
    "tests/replays",
    "tests/goldens",
];

fn canonical_scene() -> String {
    let doc = re_scene::scene::from_json(STARTER_SCENE).expect("the starter scene parses");
    let mut world = re_scene::World::new();
    world.load(&doc).expect("the starter scene loads");
    let mut saved = world.save(re_scene::SaveOptions::default());
    saved.schema = Some("../schemas/scene.schema.json".into());
    re_scene::scene::to_json(&saved)
}

/// Creates the project at `dir`, named `name` (the directory name when `None`).
/// Returns the files written, relative to `dir`.
///
/// # Errors
///
/// `RE0901` when `dir` exists and is not empty; `RE0902` when a file cannot be
/// written.
pub fn create(dir: &Path, name: Option<&str>) -> Result<Vec<String>, Diagnostic> {
    if dir.exists()
        && std::fs::read_dir(dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(true)
    {
        return Err(Diagnostic::error(
            codes::PROJECT_EXISTS,
            format!("{} already exists and is not empty", dir.display()),
        )
        .with_hint("choose a new directory; `real new` never overwrites files"));
    }
    let fail = |what: &str, e: std::io::Error| {
        Diagnostic::error(codes::WRITE_FAILED, format!("cannot write {what}: {e}"))
    };
    let name = name.map(str::to_string).unwrap_or_else(|| {
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "game".into())
    });
    for folder in FOLDERS {
        std::fs::create_dir_all(dir.join(folder)).map_err(|e| fail(folder, e))?;
    }
    let files = [
        ("real.toml", AppConfig::template(&name)),
        ("scenes/main.scene.json", canonical_scene()),
        (
            ".luaurc",
            "{\n  \"languageMode\": \"strict\",\n  \"aliases\": {\n    \"shared\": \"scripts/shared\"\n  }\n}\n".to_string(),
        ),
        (".gitignore", ".real/*\n!.real/agent-policy.toml\nout/\n".to_string()),
        (
            "materials/box.material.json",
            material([0.9, 0.45, 0.15, 1.0], 0.6),
        ),
        (
            "materials/ground.material.json",
            material([0.35, 0.5, 0.3, 1.0], 0.9),
        ),
    ];
    let mut written = Vec::new();
    for (path, text) in files {
        std::fs::write(dir.join(path), text).map_err(|e| fail(path, e))?;
        written.push(path.to_string());
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_scene_is_canonical_and_opens() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("demo");
        create(&project, None).unwrap();
        let text = std::fs::read_to_string(project.join("scenes/main.scene.json")).unwrap();
        let doc = re_scene::scene::from_json(&text).unwrap();
        let mut w = re_scene::World::new();
        w.load(&doc).unwrap();
        assert_eq!(
            re_scene::scene::to_json(&w.save(re_scene::SaveOptions::default())),
            text
        );
        let mut app = re_app::App::open_project(&project).unwrap();
        let report = app.run_headless(Some(60), None).unwrap();
        assert!(report.ok(), "{:?}", report.diagnostics);
        // The box fell onto the ground.
        let y = match app.world().get("box", "Transform", "translation").unwrap() {
            re_reflect::Value::Vec3(v) => v.y,
            other => panic!("{other:?}"),
        };
        assert!(y < 3.9 && y > 0.3, "box at y = {y}");
        for m in ["box", "ground"] {
            let path = format!("materials/{m}.material.json");
            re_assets::load_material_in(&project, &path).unwrap();
        }
    }

    #[test]
    fn refuses_a_non_empty_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("x"), "").unwrap();
        assert_eq!(
            create(dir.path(), None).unwrap_err().code,
            codes::PROJECT_EXISTS
        );
    }
}
