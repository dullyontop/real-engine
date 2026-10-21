//! The asset server over `ProjectFiles` with no project directory (ADR-0037):
//! everything is read through the layers, nothing is written to the project.

mod common;

use re_assets::{AssetServer, codes};
use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, ProjectPath, Writable};

const SIDECAR: &str =
    r#"{ "version": 1, "uuid": "0b6d3f9e-7f5c-4c55-9a43-2f4b6b0e3c11", "settings": {} }"#;

fn files(entries: &[(&str, &[u8])]) -> ProjectFiles {
    let mut layer = MemLayer::new(LayerKind::Memory);
    for (path, bytes) in entries {
        layer.insert(ProjectPath::new(path).unwrap(), *bytes, Form::Source);
    }
    ProjectFiles::for_tests(layer)
}

#[test]
fn loads_from_memory_and_caches_in_the_writable_folder() {
    let png = common::png_2x2();
    let glb = common::fixture_glb();
    let files = files(&[
        ("assets/rock.png", &png),
        ("assets/rock.png.meta.json", SIDECAR.as_bytes()),
        ("assets/lid.glb", &glb),
        (
            "assets/lid.glb.meta.json",
            br#"{ "version": 1, "uuid": "1b6d3f9e-7f5c-4c55-9a43-2f4b6b0e3c11" }"#,
        ),
        (
            "materials/rock.material.json",
            br#"{ "base_color_texture": "assets/rock.png" }"#,
        ),
    ]);
    let mut server = AssetServer::with_files(files.clone());
    let mat = server
        .load_material("materials/rock.material.json")
        .unwrap();
    assert_eq!(server.material_textures(mat).unwrap().len(), 1);
    server.load_gltf("assets/lid.glb").unwrap();
    let cached = files.writable(Writable::Cache).list("assets").unwrap();
    assert_eq!(cached.len(), 2, "{cached:?}");
    assert!(server.root().as_os_str().is_empty());
}

#[test]
fn a_missing_sidecar_is_not_created_without_a_project_directory() {
    let png = common::png_2x2();
    let mut server = AssetServer::with_files(files(&[("assets/rock.png", &png)]));
    let err = server.load_texture("assets/rock.png").unwrap_err();
    assert_eq!(err.code, codes::WRITE_FAILED);
    assert!(err.message.contains("cannot create"), "{}", err.message);
}

#[test]
fn gltf_external_files_may_not_leave_the_project() {
    let gltf =
        br#"{"asset":{"version":"2.0"},"buffers":[{"uri":"../../secret.bin","byteLength":4}]}"#;
    let mut server = AssetServer::with_files(files(&[
        ("assets/a.gltf", gltf),
        ("assets/a.gltf.meta.json", SIDECAR.as_bytes()),
    ]));
    let err = server.load_gltf("assets/a.gltf").unwrap_err();
    assert_eq!(err.code, codes::GLTF_INVALID);
    assert!(
        err.message.contains("outside the project"),
        "{}",
        err.message
    );
}
