//! glTF import of a fixture with a mesh, a material, a texture and an animation.

mod common;

use common::*;
use glam::{Quat, Vec3};
use re_assets::{AssetId, ImportedGltf, image_id, import_gltf};
use re_types::anim::Interpolation;
use re_types::render::{AlphaMode, TextureFormat, TextureRef};

fn check(g: &ImportedGltf, owner: AssetId) {
    assert!(g.warnings.is_empty(), "{:#?}", g.warnings);

    // Mesh.
    assert_eq!(g.meshes.len(), 1);
    let m = &g.meshes[0];
    assert_eq!(m.name, "tri");
    assert_eq!(m.material, Some(0));
    assert_eq!(m.mesh.indices, [0, 1, 2]);
    assert!(m.mesh.is_well_formed());
    for (i, v) in m.mesh.vertices.iter().enumerate() {
        assert_eq!(v.pos, POSITIONS[i]);
        assert_eq!(v.normal, NORMALS[i]);
        assert_eq!(v.uv, UVS[i]);
        assert_eq!(v.color, [1.0; 4], "white without vertex colours");
    }

    // Material.
    let mat = &g.materials[0];
    assert_eq!(mat.name, "red_metal");
    let d = &mat.desc;
    assert_eq!(d.base_color, [1.0, 0.5, 0.25, 1.0]);
    assert_eq!((d.metallic, d.roughness), (0.75, 0.2));
    assert_eq!(d.emissive, [0.1, 0.2, 0.3]);
    assert_eq!(d.emissive_strength, 4.0);
    assert_eq!(d.alpha_mode, AlphaMode::Mask { cutoff: 0.3 });
    assert!(d.double_sided);
    let tex_id = image_id(owner, 0);
    assert_eq!(
        d.base_color_texture,
        Some(TextureRef::Asset(tex_id.to_string()))
    );
    assert!(!d.is_resolved());

    // Texture.
    let t = &g.textures[0];
    assert_eq!(t.id, tex_id);
    assert_eq!(t.name, "albedo");
    assert_eq!((t.data.width, t.data.height), (2, 2));
    assert_eq!(t.data.format, TextureFormat::Rgba8Srgb);
    assert_eq!(&t.data.pixels[..8], &[255, 0, 0, 255, 0, 255, 0, 255]);
    assert_eq!(&t.data.pixels[12..], &[255, 255, 255, 128]);
    assert!(g.texture(tex_id).is_some());

    // Nodes.
    assert_eq!(g.roots, [0]);
    assert_eq!(g.nodes[0].name, "Root");
    assert_eq!(g.nodes[0].children, [1]);
    assert_eq!(g.nodes[0].translation, Vec3::Y);
    assert_eq!(g.nodes[1].parent, Some(0));
    assert_eq!(g.nodes[1].meshes, [0]);
    assert_eq!(g.nodes[1].rotation, Quat::IDENTITY);

    // Animation.
    let clip = g.clip("open").expect("clip `open`");
    assert_eq!(clip.duration, 2.0);
    assert_eq!(clip.targets.len(), 1);
    let target = &clip.targets[0];
    assert_eq!(target.target, "Lid");
    let tr = target.translation.as_ref().unwrap();
    assert_eq!(tr.interpolation, Interpolation::Linear);
    assert_eq!(tr.times, TIMES);
    assert_eq!(tr.values, [Vec3::ZERO, Vec3::new(4.0, 0.0, 0.0)]);
    let rot = target.rotation.as_ref().unwrap();
    assert_eq!(rot.interpolation, Interpolation::Step);
    assert_eq!(rot.values[1], Quat::from_array(ROTATIONS[1]));
    assert!(target.scale.is_none());
}

#[test]
fn glb_with_buffer_view_image_and_animation() {
    let owner = AssetId::new_source();
    let g = import_gltf(&fixture_glb(), None, owner, "assets/lid.glb").expect("import GLB");
    check(&g, owner);
}

#[test]
fn gltf_with_data_uris_written_to_disk() {
    let png = png_2x2();
    let (bin, views) = build_bin(None);
    let buffer_uri = format!("data:application/octet-stream;base64,{}", base64(&bin));
    let image = format!(
        r#"{{"uri": "data:image/png;base64,{}", "name": "albedo"}}"#,
        base64(&png)
    );
    let doc = json(Some(&buffer_uri), bin.len(), &views, &image);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("lid.gltf");
    std::fs::write(&path, &doc).unwrap();
    let owner = AssetId::new_source();
    let g = import_gltf(
        &std::fs::read(&path).unwrap(),
        Some(dir.path()),
        owner,
        "lid.gltf",
    )
    .expect("import .gltf");
    check(&g, owner);
}

#[test]
fn gltf_with_external_files() {
    let png = png_2x2();
    let (bin, views) = build_bin(None);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("lid data.bin"), &bin).unwrap();
    std::fs::write(dir.path().join("albedo.png"), &png).unwrap();
    let doc = json(
        Some("lid%20data.bin"),
        bin.len(),
        &views,
        r#"{"uri": "albedo.png", "name": "albedo"}"#,
    );
    let owner = AssetId::new_source();
    let g = import_gltf(doc.as_bytes(), Some(dir.path()), owner, "lid.gltf").expect("import");
    check(&g, owner);
}

#[test]
fn normal_maps_are_linear() {
    let png = png_2x2();
    let (bin, views) = build_bin(Some(&png));
    let doc = json(
        None,
        bin.len(),
        &views,
        r#"{"bufferView": 7, "mimeType": "image/png"}"#,
    )
    .replace(
        r#""baseColorTexture": {"index": 0}}"#,
        r#""baseColorTexture": {"index": 0}}, "normalTexture": {"index": 1}"#,
    );
    // A second image used only as a normal map.
    let doc = doc
        .replace(r#""textures": [{"source": 0}]"#, r#""textures": [{"source": 0}, {"source": 1}]"#)
        .replace(r#""images": [{"bufferView": 7, "mimeType": "image/png"}]"#, r#""images": [{"bufferView": 7, "mimeType": "image/png"}, {"bufferView": 7, "mimeType": "image/png"}]"#);
    let owner = AssetId::new_source();
    let g = import_gltf(&glb(&doc, &bin), None, owner, "n.glb").unwrap();
    assert_eq!(g.textures[0].data.format, TextureFormat::Rgba8Srgb);
    assert_eq!(g.textures[1].data.format, TextureFormat::Rgba8Unorm);
    assert_eq!(
        g.materials[0].desc.normal_texture,
        Some(TextureRef::Asset(image_id(owner, 1).to_string()))
    );
}

#[test]
fn errors_are_diagnostics() {
    let png = png_2x2();
    let (bin, views) = build_bin(Some(&png));
    let owner = AssetId::new_source();

    // Truncated BIN chunk: the buffer claims 1000 bytes.
    let doc = json(
        None,
        1000,
        &views,
        r#"{"bufferView": 7, "mimeType": "image/png"}"#,
    );
    let err = import_gltf(&glb(&doc, &bin), None, owner, "a.glb").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7002");
    assert!(err.render_human().contains("a.glb"));

    // An image that is not PNG or JPEG.
    let mut bad_png = png.clone();
    bad_png[1] = b'X';
    let (bin2, views2) = build_bin(Some(&bad_png));
    let doc = json(
        None,
        bin2.len(),
        &views2,
        r#"{"bufferView": 7, "mimeType": "image/png"}"#,
    );
    let err = import_gltf(&glb(&doc, &bin2), None, owner, "a.glb").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7003", "{err:?}");

    // Not glTF at all.
    assert_eq!(
        import_gltf(b"hello", None, owner, "a.glb")
            .unwrap_err()
            .code
            .to_string(),
        "RE7002"
    );

    // An external file that is not there.
    let doc = json(
        Some("missing.bin"),
        bin.len(),
        &views,
        r#"{"bufferView": 7, "mimeType": "image/png"}"#,
    );
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        import_gltf(doc.as_bytes(), Some(dir.path()), owner, "a.gltf")
            .unwrap_err()
            .code
            .to_string(),
        "RE7002"
    );
}

#[test]
fn unsupported_primitives_are_skipped_with_a_warning() {
    let png = png_2x2();
    let (bin, views) = build_bin(Some(&png));
    let doc = json(
        None,
        bin.len(),
        &views,
        r#"{"bufferView": 7, "mimeType": "image/png"}"#,
    )
    .replace(
        r#""indices": 3, "material": 0}"#,
        r#""indices": 3, "material": 0, "mode": 1}"#,
    );
    let g = import_gltf(&glb(&doc, &bin), None, AssetId::new_source(), "l.glb").unwrap();
    assert!(g.meshes.is_empty());
    assert_eq!(g.nodes[1].meshes, Vec::<usize>::new());
    assert_eq!(g.warnings[0].code.to_string(), "RE7005");
    assert!(
        g.warnings[0].message.contains("Lines"),
        "{}",
        g.warnings[0].message
    );
}

#[test]
fn missing_normals_are_computed() {
    let png = png_2x2();
    let (bin, views) = build_bin(Some(&png));
    let doc = json(
        None,
        bin.len(),
        &views,
        r#"{"bufferView": 7, "mimeType": "image/png"}"#,
    )
    .replace(
        r#""POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2"#,
        r#""POSITION": 0, "TEXCOORD_0": 2"#,
    );
    let g = import_gltf(&glb(&doc, &bin), None, AssetId::new_source(), "l.glb").unwrap();
    assert!(
        g.meshes[0]
            .mesh
            .vertices
            .iter()
            .all(|v| v.normal == [0.0, 0.0, 1.0])
    );
}
