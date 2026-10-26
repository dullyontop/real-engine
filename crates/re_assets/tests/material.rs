//! Material files: the fixture loads into the expected `MaterialDesc`, and
//! problems are located diagnostics.

mod common;

use std::path::Path;

use re_assets::{AssetId, AssetMeta, load_material, load_material_in};
use re_types::render::{AlphaMode, MaterialDesc, TextureRef};

const ROCK: &str = r#"{
  "$schema": "../schemas/material.schema.json",
  "base_color": [0.8, 0.7, 0.6, 1.0],
  "base_color_texture": "assets/textures/rock.png",
  "metallic": 0.1,
  "roughness": 0.9,
  "normal_texture": "assets/textures/rock_n.png",
  "emissive": [0.0, 0.1, 0.0],
  "alpha_mode": "mask",
  "alpha_cutoff": 0.25,
  "double_sided": true
}
"#;

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("real.toml"), "[project]\nname = \"t\"\n").unwrap();
    std::fs::create_dir_all(root.join("assets/textures")).unwrap();
    std::fs::create_dir_all(root.join("materials")).unwrap();
    std::fs::write(root.join("assets/textures/rock.png"), common::png_2x2()).unwrap();
    std::fs::write(root.join("assets/textures/rock_n.png"), common::png_2x2()).unwrap();
    std::fs::write(root.join("materials/rock.material.json"), ROCK).unwrap();
    dir
}

fn meta(root: &Path, file: &str) -> AssetMeta {
    let text = std::fs::read_to_string(root.join(format!("{file}.meta.json"))).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn fixture_loads_into_the_expected_desc() {
    let dir = project();
    let root = dir.path();
    let desc = load_material(&root.join("materials/rock.material.json")).unwrap();

    // Loading created a sidecar for each texture; the normal map's is linear
    // and marked as a normal map (stored as BC5, ADR-0047).
    let albedo = meta(root, "assets/textures/rock.png");
    let normal = meta(root, "assets/textures/rock_n.png");
    assert_eq!(albedo.settings, serde_json::json!({}));
    assert_eq!(
        normal.settings,
        serde_json::json!({ "color_space": "linear", "normal_map": true })
    );

    let expected = MaterialDesc {
        base_color: [0.8, 0.7, 0.6, 1.0],
        base_color_texture: Some(TextureRef::Asset(albedo.id().to_string())),
        metallic: 0.1,
        roughness: 0.9,
        metallic_roughness_texture: None,
        normal_texture: Some(TextureRef::Asset(normal.id().to_string())),
        emissive: [0.0, 0.1, 0.0],
        emissive_strength: 1.0,
        alpha_mode: AlphaMode::Mask { cutoff: 0.25 },
        double_sided: true,
        unlit: false,
    };
    assert_eq!(desc, expected);
    assert!(matches!(albedo.id(), AssetId::Source(_)));

    // Loading again keeps the ids (the sidecars are read, not rewritten).
    assert_eq!(
        load_material_in(root, "materials/rock.material.json").unwrap(),
        expected
    );
}

#[test]
fn empty_material_is_the_default() {
    let dir = project();
    std::fs::write(dir.path().join("materials/plain.material.json"), "{}").unwrap();
    assert_eq!(
        load_material_in(dir.path(), "materials/plain.material.json").unwrap(),
        MaterialDesc::default()
    );
}

#[test]
fn missing_texture_is_a_located_diagnostic() {
    let dir = project();
    let text = ROCK.replace("assets/textures/rock_n.png", "assets/textures/rok_n.png");
    std::fs::write(dir.path().join("materials/rock.material.json"), text).unwrap();
    let err = load_material_in(dir.path(), "materials/rock.material.json").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7011");
    assert!(
        err.message.contains("assets/textures/rok_n.png"),
        "{}",
        err.message
    );
    let rendered = err.render_human();
    assert!(
        rendered.contains("materials/rock.material.json:7:"),
        "{rendered}"
    );
}

#[test]
fn unknown_key_and_bad_values_are_diagnostics() {
    let dir = project();
    let path = dir.path().join("materials/bad.material.json");
    std::fs::write(&path, "{\n  \"colour\": [1, 0, 0, 1]\n}").unwrap();
    let err = load_material_in(dir.path(), "materials/bad.material.json").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7010");
    assert!(
        err.render_human().contains("bad.material.json:2:"),
        "{}",
        err.render_human()
    );

    std::fs::write(&path, "{\n  \"metallic\": 0.5,\n  \"roughness\": 1.5\n}").unwrap();
    let err = load_material_in(dir.path(), "materials/bad.material.json").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7012");
    assert!(
        err.render_human().contains("bad.material.json:3:3"),
        "{}",
        err.render_human()
    );

    std::fs::write(&path, "{ \"alpha_mode\": \"cutout\" }").unwrap();
    assert_eq!(
        load_material_in(dir.path(), "materials/bad.material.json")
            .unwrap_err()
            .code
            .to_string(),
        "RE7010"
    );

    std::fs::write(&path, "{ \"base_color_texture\": \"../outside.png\" }").unwrap();
    assert_eq!(
        load_material_in(dir.path(), "materials/bad.material.json")
            .unwrap_err()
            .code
            .to_string(),
        "RE7040"
    );

    assert_eq!(
        load_material_in(dir.path(), "materials/none.material.json")
            .unwrap_err()
            .code
            .to_string(),
        "RE7001"
    );
}
