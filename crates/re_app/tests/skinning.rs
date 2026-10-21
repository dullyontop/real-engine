//! `re_app` with a GPU: a `SkinnedMesh` posed by the `AnimPlayer` on its
//! entity, drawn as a skinned item (ADR-0039). Needs a Vulkan or GL adapter.

use std::path::Path;

use glam::{Mat4, Quat, Vec3};
use re_app::{App, AppConfig};
use re_reflect::Value;

/// Appends `bytes` as one buffer view and accessor; returns the accessor index.
struct Bin {
    bin: Vec<u8>,
    views: Vec<String>,
    accessors: Vec<String>,
}

impl Bin {
    fn push(&mut self, bytes: &[u8], component: u32, count: usize, ty: &str, extra: &str) -> usize {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        self.views.push(format!(
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{}}}"#,
            self.bin.len(),
            bytes.len()
        ));
        self.bin.extend_from_slice(bytes);
        self.accessors.push(format!(
            r#"{{"bufferView":{},"componentType":{component},"count":{count},"type":"{ty}"{extra}}}"#,
            self.views.len() - 1
        ));
        self.accessors.len() - 1
    }

    fn floats(&mut self, v: &[f32], per: usize, ty: &str, extra: &str) -> usize {
        let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.push(&bytes, 5126, v.len() / per, ty, extra)
    }
}

/// An arm: a 0.4 m wide strip from y = 0 to y = 2 facing +Z, skinned to
/// `Shoulder` (at the origin) and `Elbow` (at y = 1). Clip `bend` turns the
/// elbow 90 degrees about +Z over one second; clip `wave` animates `Prop`, a
/// node that is not a joint.
fn arm_glb() -> Vec<u8> {
    let mut b = Bin {
        bin: Vec::new(),
        views: Vec::new(),
        accessors: Vec::new(),
    };
    let mut pos = Vec::new();
    let mut joints: Vec<u8> = Vec::new();
    let mut weights = Vec::new();
    for (y, w) in [(0.0f32, 0.0f32), (1.0, 0.5), (2.0, 1.0)] {
        for x in [-0.2f32, 0.2] {
            pos.extend_from_slice(&[x, y, 0.0]);
            for j in [0u16, 1, 0, 0] {
                joints.extend_from_slice(&j.to_le_bytes());
            }
            weights.extend_from_slice(&[1.0 - w, w, 0.0, 0.0]);
        }
    }
    let p = b.floats(&pos, 3, "VEC3", r#","min":[-0.2,0,0],"max":[0.2,2,0]"#);
    let n = b.floats(&[0.0, 0.0, 1.0].repeat(6), 3, "VEC3", "");
    let j = b.push(&joints, 5123, 6, "VEC4", "");
    let w = b.floats(&weights, 4, "VEC4", "");
    let idx: Vec<u8> = [0u16, 1, 3, 0, 3, 2, 2, 3, 5, 2, 5, 4]
        .iter()
        .flat_map(|i| i.to_le_bytes())
        .collect();
    let i = b.push(&idx, 5123, 12, "SCALAR", "");
    let ibm: Vec<f32> = [
        Mat4::IDENTITY,
        Mat4::from_translation(Vec3::new(0.0, -1.0, 0.0)),
    ]
    .iter()
    .flat_map(|m| m.to_cols_array())
    .collect();
    let ibm = b.floats(&ibm, 16, "MAT4", "");
    let t = b.floats(&[0.0, 1.0], 1, "SCALAR", r#","min":[0],"max":[1]"#);
    let bent = Quat::from_rotation_z(std::f32::consts::FRAC_PI_2);
    let rot = b.floats(
        &[0.0, 0.0, 0.0, 1.0, bent.x, bent.y, bent.z, bent.w],
        4,
        "VEC4",
        "",
    );
    let mv = b.floats(&[0.0, 0.0, 0.0, 0.0, 1.0, 0.0], 3, "VEC3", "");
    while !b.bin.len().is_multiple_of(4) {
        b.bin.push(0);
    }
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0,2,3]}}],
"nodes":[{{"name":"Shoulder","children":[1]}},{{"name":"Elbow","translation":[0,1,0]}},{{"name":"Arm","mesh":0,"skin":0}},{{"name":"Prop"}}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":{p},"NORMAL":{n},"JOINTS_0":{j},"WEIGHTS_0":{w}}},"indices":{i}}}]}}],
"skins":[{{"name":"Rig","joints":[0,1],"inverseBindMatrices":{ibm}}}],
"animations":[{{"name":"bend","samplers":[{{"input":{t},"output":{rot}}}],"channels":[{{"sampler":0,"target":{{"node":1,"path":"rotation"}}}}]}},
{{"name":"wave","samplers":[{{"input":{t},"output":{mv}}}],"channels":[{{"sampler":0,"target":{{"node":3,"path":"translation"}}}}]}}],
"accessors":[{}],"bufferViews":[{}],"buffers":[{{"byteLength":{}}}]}}"#,
        b.accessors.join(","),
        b.views.join(","),
        b.bin.len()
    );
    let mut j = json.into_bytes();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&((12 + 8 + j.len() + 8 + b.bin.len()) as u32).to_le_bytes());
    out.extend_from_slice(&(j.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&j);
    out.extend_from_slice(&(b.bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&b.bin);
    out
}

fn project(clip: &str) -> tempfile::TempDir {
    let scene = format!(
        r#"{{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    {{ "id": "camera", "components": {{
        "Camera": {{ "active": true }},
        "Transform": {{ "translation": [0.0, 1.0, 3.0] }} }} }},
    {{ "id": "arm", "components": {{
        "SkinnedMesh": {{ "asset": "assets/arm.glb#Rig" }},
        "AnimPlayer": {{ "clip": "{clip}", "looping": false }},
        "MaterialRef": {{ "asset": "materials/red.material.json" }},
        "Transform": {{}} }} }}
  ]
}}
"#
    );
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    for d in ["scenes", "materials", "assets"] {
        std::fs::create_dir_all(p.join(d)).unwrap();
    }
    std::fs::write(
        p.join("real.toml"),
        "[project]\nname = \"skins\"\n\n[render]\nwidth = 160\nheight = 120\n",
    )
    .unwrap();
    std::fs::write(p.join("scenes/main.scene.json"), scene).unwrap();
    std::fs::write(
        p.join("materials/red.material.json"),
        r#"{ "base_color": [1.0, 0.0, 0.0, 1.0], "unlit": true, "double_sided": true }"#,
    )
    .unwrap();
    std::fs::write(p.join("assets/arm.glb"), arm_glb()).unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

/// Red pixels left of the arm's rest column (screen x < 60 of 160).
fn red_on_the_left(app: &mut App) -> usize {
    let image = app.screenshot(None).unwrap();
    (0..60)
        .flat_map(|x| (0..image.height).map(move |y| (x, y)))
        .filter(|&(x, y)| {
            let [r, g, b, _] = image.pixel(x, y);
            r > 200 && g < 40 && b < 40
        })
        .count()
}

#[test]
fn an_anim_player_bends_a_skinned_mesh() {
    let dir = project("assets/arm.glb#bend");
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    let report = app.run_headless(Some(1), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    // A headless run builds the snapshot when it draws, as for a screenshot.
    assert_eq!(red_on_the_left(&mut app), 0, "at rest the arm is upright");
    assert!(
        app.snapshot().items.is_empty(),
        "skinned meshes are not plain items"
    );
    assert_eq!(app.snapshot().skinned.len(), 1);
    assert_eq!(app.snapshot().joint_palette.len(), 2);

    // Half a second in: the elbow is at 45 degrees.
    app.run_headless(Some(29), None).unwrap();
    app.screenshot(None).unwrap();
    let palette = &app.snapshot().joint_palette;
    assert_eq!(palette.len(), 2, "one run of the skin's joints per frame");
    let expected = Mat4::from_translation(Vec3::Y)
        * Mat4::from_quat(Quat::from_rotation_z(std::f32::consts::FRAC_PI_4))
        * Mat4::from_translation(-Vec3::Y);
    assert!(
        palette[0].abs_diff_eq(Mat4::IDENTITY, 1e-5),
        "{}",
        palette[0]
    );
    assert!(
        palette[1].abs_diff_eq(expected, 1e-4),
        "{} vs {expected}",
        palette[1]
    );

    // At the end the forearm points along -X, left of the rest column.
    app.run_headless(Some(60), None).unwrap();
    let left = red_on_the_left(&mut app);
    assert!(left > 20, "{left} red pixels left of the rest column");
    let item = app.snapshot().skinned[0];
    assert!(item.bounds.min.x < -0.9, "{:?}", item.bounds);
    let stats = app.graphics().unwrap().renderer().stats().last_frame;
    assert_eq!(stats.skinned_items, 1, "{stats:?}");

    // Clearing the clip returns the skeleton to its rest pose.
    app.world_mut()
        .set("arm", "AnimPlayer", "clip", Value::Str(String::new()))
        .unwrap();
    app.run_headless(Some(1), None).unwrap();
    app.screenshot(None).unwrap();
    assert!(app.snapshot().joint_palette[1].abs_diff_eq(Mat4::IDENTITY, 1e-5));
}

#[test]
fn a_clip_target_that_is_not_a_joint_is_reported_once() {
    let dir = project("assets/arm.glb#wave");
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    let report = app.run_headless(Some(10), None).unwrap();
    let missing: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code == re_app::codes::ANIM_TARGET_MISSING)
        .collect();
    assert_eq!(missing.len(), 1, "{:?}", report.diagnostics);
    assert!(
        missing[0].message.contains("`Prop`"),
        "{}",
        missing[0].message
    );
    // The skin still draws, at rest.
    app.screenshot(None).unwrap();
    assert_eq!(app.snapshot().skinned.len(), 1);
}

#[test]
fn an_unknown_skin_is_reported_once_and_not_drawn() {
    let dir = project("assets/arm.glb#bend");
    let scene = dir.path().join("scenes/main.scene.json");
    let text = std::fs::read_to_string(&scene)
        .unwrap()
        .replace("#Rig", "#Nope");
    std::fs::write(&scene, text).unwrap();
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    let report = app.run_headless(Some(5), None).unwrap();
    let hits: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code == re_app::codes::ASSET_UNAVAILABLE)
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", report.diagnostics);
    assert!(
        hits[0].render_human().contains("Rig"),
        "{}",
        hits[0].render_human()
    );
    app.screenshot(None).unwrap();
    assert!(app.snapshot().skinned.is_empty());
}
