//! `re_app` with a GPU: meshes and materials uploaded from the World, the HUD,
//! screenshots, derived mesh release, animation clips and the audio mixer.
//! Needs a Vulkan or GL adapter (lavapipe or llvmpipe on CI).

use std::path::Path;
use std::sync::Arc;

use re_app::{App, AppConfig};
use re_reflect::Value;

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 1.0, 5.0] } } },
    { "id": "cube", "components": {
        "MaterialRef": { "asset": "materials/red.material.json" },
        "MeshRef": { "asset": "builtin:cube" },
        "Transform": { "translation": [0.0, 1.0, 0.0] } } },
    { "id": "hud", "components": { "HudText": { "text": "3 / 7", "size": 14.0 } } },
    { "id": "sun", "components": {
        "Light": { "kind": "Directional", "intensity": 3.0 },
        "Transform": { "rotation": [-0.5, 0.3, 0.1, 0.8] } } }
  ]
}
"#;

fn project(scene: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("scenes")).unwrap();
    std::fs::create_dir_all(p.join("materials")).unwrap();
    std::fs::write(
        p.join("real.toml"),
        "[project]\nname = \"media\"\n\n[render]\nwidth = 160\nheight = 120\n",
    )
    .unwrap();
    std::fs::write(p.join("scenes/main.scene.json"), scene).unwrap();
    std::fs::write(
        p.join("materials/red.material.json"),
        r#"{ "base_color": [1.0, 0.0, 0.0, 1.0], "unlit": true }"#,
    )
    .unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

#[test]
fn a_screenshot_draws_the_scene_and_the_hud() {
    let dir = project(SCENE);
    let mut app = open(dir.path());
    app.run_headless(Some(1), None).unwrap();
    let image = app.screenshot(Some(Path::new("out/shot.png"))).unwrap();
    assert!(dir.path().join("out/shot.png").is_file());
    assert_eq!((image.width, image.height), (160, 120));
    // The unlit red cube covers the centre.
    let [r, g, b, _] = image.pixel(80, 60);
    assert!(r > 200 && g < 40 && b < 40, "centre is {:?}", [r, g, b]);
    // The HUD panel darkens the top-left corner and has white text in it.
    let bright = (12..60)
        .flat_map(|x| (12..50).map(move |y| (x, y)))
        .filter(|&(x, y)| image.pixel(x, y)[0] > 200 && image.pixel(x, y)[1] > 200)
        .count();
    assert!(bright > 10, "no HUD text in the top-left corner");
    assert_eq!(app.snapshot().items.len(), 1);
    let report = app.run_headless(Some(1), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
}

#[test]
fn drawn_frames_report_their_submission_time() {
    let dir = project(SCENE);
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    // Headless frames draw nothing: no submission samples.
    let report = app.run_headless(Some(3), None).unwrap();
    assert_eq!(report.perf.render_submit.samples, 0);
    // Each drawn frame is one sample: here, two drawn into a capture
    // (as a window's are) and a screenshot.
    let capture = re_render::Capture::new(&app.graphics().unwrap().gpu().device, 160, 120).unwrap();
    for _ in 0..2 {
        app.frame(std::time::Duration::from_secs(1) / 60);
        app.draw_into(capture.view(), [160, 120], 1.0).unwrap();
    }
    // The editor's scene view is an extra view, with no submission sample
    // of its own.
    let snapshot = app.snapshot().clone();
    app.draw_view(
        capture.view(),
        &snapshot,
        &re_render::SceneExtras::default(),
    )
    .unwrap();
    app.screenshot(None).unwrap();
    let perf = app.perf_report();
    assert_eq!(perf.render_submit.samples, 3, "{perf:?}");
    assert_eq!(perf.queue_submit.samples, 3, "{perf:?}");
    let renderer = perf.renderer.as_ref().expect("the renderer drew");
    let gfx = app.graphics().unwrap();
    assert_eq!(renderer.backend, gfx.backend());
    assert_eq!((renderer.frames, renderer.extra_views), (3, 1));
    // The cube: one draw a frame, of twelve triangles.
    assert_eq!(renderer.per_frame["draw_calls"].max, 1, "{renderer:?}");
    assert_eq!(renderer.per_frame["triangles"].mean, 12.0, "{renderer:?}");
    assert!(renderer.resources.meshes.count >= 1 && renderer.resources.meshes.bytes > 0);
    assert!(renderer.warnings.is_empty(), "{:?}", renderer.warnings);
    // Where passes can be timed, the frames before the last have their times
    // by now: the screenshot's readback waited for them.
    match &renderer.gpu_ms {
        Some(gpu) => {
            assert!(gfx.renderer().times_gpu());
            assert_eq!(gpu.total.samples, 2, "{gpu:?}");
            assert!(gpu.total.mean_ms >= gpu.scene.mean_ms, "{gpu:?}");
        }
        None => assert!(!gfx.renderer().times_gpu()),
    }
    assert!(
        perf.render_submit.mean_ms > 0.0 && perf.render_submit.max_ms >= perf.render_submit.mean_ms
    );
    assert!(perf.queue_submit.mean_ms > 0.0);
    // Old reports without the fields still read.
    let mut json = serde_json::to_value(&perf).unwrap();
    json.as_object_mut().unwrap().remove("render_submit");
    json.as_object_mut().unwrap().remove("queue_submit");
    let old: re_app::PerfReport = serde_json::from_value(json).unwrap();
    assert_eq!(old.render_submit, re_app::TimeStats::default());
    assert_eq!(old.queue_submit, re_app::TimeStats::default());
    app.reset_perf();
    assert_eq!(app.perf_report().render_submit.samples, 0);
}

/// [`SCENE`] with ten point lights, two more than a frame draws (`RE6017`).
fn scene_with_ten_lamps() -> String {
    let lamps: String = (0..10)
        .map(|i| {
            format!(
                r#",
    {{ "id": "lamp{i}", "components": {{
        "Light": {{ "kind": "Point", "range": 4.0 }},
        "Transform": {{ "translation": [{i}.0, 1.0, 0.0] }} }} }}"#
            )
        })
        .collect();
    SCENE.replace("\n  ]\n}", &format!("{lamps}\n  ]\n}}"))
}

/// The renderer's warnings reach the app's diagnostics once per code, and the
/// PerfReport counts every one (ADR-0022).
#[test]
fn renderer_warnings_are_reported_once_and_counted() {
    let dir = project(&scene_with_ten_lamps());
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    app.run_headless(Some(1), None).unwrap();
    let too_many = |app: &mut App| {
        app.take_diagnostics()
            .iter()
            .filter(|d| d.code == re_render::codes::TOO_MANY_LIGHTS)
            .count()
    };
    for _ in 0..3 {
        app.screenshot(None).unwrap();
    }
    assert_eq!(too_many(&mut app), 1, "once");
    app.screenshot(None).unwrap();
    assert_eq!(too_many(&mut app), 0, "and not again");
    let renderer = app.perf_report().renderer.unwrap();
    let code = re_render::codes::TOO_MANY_LIGHTS.to_string();
    assert_eq!(renderer.warnings[&code], 4);
    assert_eq!(renderer.per_frame["point_lights"].max, 8);
    assert_eq!(renderer.per_frame["lights_dropped"].max, 2);
}

#[test]
fn an_unknown_mesh_is_reported_once() {
    let dir = project(&SCENE.replace("builtin:cube", "builtin:teapot"));
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    let report = app.run_headless(Some(5), None).unwrap();
    let hits = report
        .diagnostics
        .iter()
        .filter(|d| d.code == re_app::codes::ASSET_UNAVAILABLE)
        .count();
    assert_eq!(hits, 1, "{:?}", report.diagnostics);
    assert!(app.snapshot().items.is_empty());
}

#[test]
fn derived_meshes_are_released_when_nothing_names_them() {
    let dir = project(SCENE);
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    app.run_headless(Some(1), None).unwrap();
    let base = app.graphics().unwrap().renderer().stats().meshes.count;
    // Ten regenerations, each naming a new derived mesh.
    for i in 0..10 {
        let name = format!("derived:{i:064x}");
        let mut mesh = re_app::media::builtin_mesh("sphere").unwrap();
        mesh.vertices[0].pos[0] += i as f32 * 0.01;
        app.media_mut()
            .insert_derived_mesh(name.clone(), Arc::new(mesh));
        app.world_mut()
            .set("cube", "MeshRef", "asset", Value::Str(name))
            .unwrap();
        app.run_headless(Some(1), None).unwrap();
        let count = app.graphics().unwrap().renderer().stats().meshes.count;
        // The cube (no longer named, but not derived) stays; one derived mesh is live.
        assert_eq!(count, base + 1, "after regeneration {i}");
    }
}

/// A GLB with one triangle at node `Lid` and a clip `slide` that moves `Lid`
/// from x = 0 to x = 2 over one second.
fn door_glb() -> Vec<u8> {
    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }
    let parts: [Vec<u8>; 5] = [
        f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]),
        f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]),
        [0u16, 1, 2, 0]
            .iter()
            .flat_map(|i| i.to_le_bytes())
            .collect(),
        f32s(&[0.0, 1.0]),
        f32s(&[0.0, 0.0, 0.0, 2.0, 0.0, 0.0]),
    ];
    let mut bin = Vec::new();
    let mut views = Vec::new();
    for p in &parts {
        views.push(format!(
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{}}}"#,
            bin.len(),
            p.len()
        ));
        bin.extend_from_slice(p);
    }
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0]}}],
"nodes":[{{"name":"Lid","mesh":0}}],
"meshes":[{{"name":"tri","primitives":[{{"attributes":{{"POSITION":0,"NORMAL":1}},"indices":2}}]}}],
"animations":[{{"name":"slide","samplers":[{{"input":3,"output":4,"interpolation":"LINEAR"}}],
  "channels":[{{"sampler":0,"target":{{"node":0,"path":"translation"}}}}]}}],
"accessors":[
 {{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}},
 {{"bufferView":1,"componentType":5126,"count":3,"type":"VEC3"}},
 {{"bufferView":2,"componentType":5123,"count":3,"type":"SCALAR"}},
 {{"bufferView":3,"componentType":5126,"count":2,"type":"SCALAR","min":[0],"max":[1]}},
 {{"bufferView":4,"componentType":5126,"count":2,"type":"VEC3"}}],
"bufferViews":[{}],"buffers":[{{"byteLength":{}}}]}}"#,
        views.join(","),
        bin.len()
    );
    let mut j = json.into_bytes();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let total = 12 + 8 + j.len() + 8 + bin.len();
    let mut out = Vec::new();
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(j.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&j);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

#[test]
fn a_clip_moves_the_entity_named_after_its_node_and_gltf_meshes_draw() {
    let scene = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "door", "components": {
        "AnimPlayer": { "clip": "assets/door.glb#slide", "looping": false },
        "Transform": {} } },
    { "id": "lid", "name": "Lid", "parent": "door", "components": {
        "MeshRef": { "asset": "assets/door.glb" },
        "Transform": {} } }
  ]
}
"#;
    let dir = project(scene);
    std::fs::create_dir_all(dir.path().join("assets")).unwrap();
    std::fs::write(dir.path().join("assets/door.glb"), door_glb()).unwrap();
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    let report = app.run_headless(Some(30), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let x = match app.world().get("lid", "Transform", "translation").unwrap() {
        Value::Vec3(v) => v.x,
        other => panic!("{other:?}"),
    };
    assert!((x - 1.0).abs() < 0.05, "lid at x = {x} after half a second");
    app.run_headless(Some(60), None).unwrap();
    let x = match app.world().get("lid", "Transform", "translation").unwrap() {
        Value::Vec3(v) => v.x,
        other => panic!("{other:?}"),
    };
    assert!(
        (x - 2.0).abs() < 1e-4,
        "a non-looping clip holds its end: {x}"
    );
    assert_eq!(app.snapshot().items.len(), 1, "the glTF mesh is drawn");
}

#[test]
fn play_run_plays_a_copy_and_leaves_the_live_world_alone() {
    let scene = SCENE.replace(
        r#""MeshRef": { "asset": "builtin:cube" },"#,
        r#""MeshRef": { "asset": "builtin:cube" }, "Collider": { "shape": "Box" }, "RigidBody": { "kind": "Dynamic" },"#,
    );
    let dir = project(&scene);
    let mut app = open(dir.path());
    app.run_headless(Some(10), None).unwrap();
    let before = app.world().state_hash();
    let frames = app.frame_count();
    let out = app.dispatch(
        "play.run",
        serde_json::json!({ "frames": 30 }),
        false,
        &re_command::AllowAll,
        re_command::Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let result = out.data.clone();
    assert_eq!(result["frames"], 30);
    assert_eq!(result["ok"], true);
    let shot = result["screenshot"].as_str().unwrap();
    assert!(dir.path().join(shot).is_file());
    assert_eq!(app.world().state_hash(), before);
    assert_eq!(app.frame_count(), frames);
    // The copy carried on from the live fall: the box ends lower in the copy.
    assert_ne!(result["state_hash"], before.to_hex().to_string());
}

#[test]
fn asset_import_copies_with_a_sidecar_and_refuses_to_overwrite() {
    let dir = project(SCENE);
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("door.glb"), door_glb()).unwrap();
    let mut app = open(dir.path());
    let source = src.path().join("door.glb").display().to_string();
    let import = |app: &mut App, dry: bool| {
        app.dispatch(
            "asset.import",
            serde_json::json!({ "source": source, "dest": "assets/models/door.glb" }),
            dry,
            &re_command::AllowAll,
            re_command::Origin::User,
        )
    };
    let out = import(&mut app, true);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert!(!dir.path().join("assets/models/door.glb").exists());
    let out = import(&mut app, false);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert!(dir.path().join("assets/models/door.glb").is_file());
    assert!(
        dir.path()
            .join("assets/models/door.glb.meta.json")
            .is_file()
    );
    let out = import(&mut app, false);
    assert!(!out.ok);
    assert_eq!(out.diagnostics[0].code, re_app::codes::ASSET_IMPORT_FAILED);
    let out = app.dispatch(
        "asset.import",
        serde_json::json!({ "source": source, "dest": "../escape.glb" }),
        false,
        &re_command::AllowAll,
        re_command::Origin::User,
    );
    assert!(!out.ok);
}

#[test]
fn the_goldens_stage_reports_missing_images_then_matches_written_ones() {
    let dir = project(SCENE);
    std::fs::create_dir_all(dir.path().join("tests/replays")).unwrap();
    std::fs::write(
        dir.path().join("tests/replays/look.replay.json"),
        r#"{ "version": 1, "seed": 1, "frames": 20,
             "checks": [ { "frame": 10, "screenshot": "tests/goldens/{backend}/look_f10.png" } ] }"#,
    )
    .unwrap();
    let config = AppConfig::load(dir.path()).unwrap();
    let options = re_app::VerifyOptions::default();
    let report = re_app::verify::run_project(dir.path(), &config, &options, &[]);
    let goldens = report.stage("goldens").unwrap();
    assert!(!goldens.ok);
    assert_eq!(goldens.diagnostics[0].code, re_app::codes::GOLDEN_MISSING);

    let update = re_app::VerifyOptions {
        update_goldens: true,
        ..re_app::VerifyOptions::default()
    };
    let report = re_app::verify::run_project(dir.path(), &config, &update, &[]);
    assert!(report.stage("goldens").unwrap().ok);
    let backend = report.stage("goldens").unwrap().data["backend"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        dir.path()
            .join(format!("tests/goldens/{backend}/look_f10.png"))
            .is_file()
    );
    let report = re_app::verify::run_project(dir.path(), &config, &options, &[]);
    assert!(report.ok, "{:#?}", report.stages);
}

#[test]
fn a_changed_material_file_is_uploaded_again() {
    let dir = project(SCENE);
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    app.enable_hot_reload().unwrap();
    app.run_headless(Some(1), None).unwrap();
    let before = app.snapshot().items[0].material;
    std::fs::write(
        dir.path().join("materials/red.material.json"),
        r#"{ "base_color": [0.0, 0.0, 1.0, 1.0], "unlit": true }"#,
    )
    .unwrap();
    let deadline = re_core::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.snapshot().items[0].material == before {
        assert!(
            re_core::time::Instant::now() < deadline,
            "the change never arrived"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
        app.run_headless(Some(1), None).unwrap();
    }
    let image = app.screenshot(None).unwrap();
    let [r, _, b, _] = image.pixel(80, 60);
    assert!(
        b > 200 && r < 40,
        "the cube is blue now: {:?}",
        image.pixel(80, 60)
    );
}

/// The goldens stage is the only one that draws, so it reports the
/// renderer's warnings, once per code over all its replays, without failing
/// on them (ADR-0022).
#[test]
fn the_goldens_stage_reports_the_renderers_warnings_once() {
    let dir = project(&scene_with_ten_lamps());
    std::fs::create_dir_all(dir.path().join("tests/replays")).unwrap();
    for name in ["a", "b"] {
        std::fs::write(
            dir.path().join(format!("tests/replays/{name}.replay.json")),
            format!(
                r#"{{ "version": 1, "seed": 1, "frames": 10,
                     "checks": [ {{ "frame": 5, "screenshot": "tests/goldens/{{backend}}/{name}.png" }} ] }}"#
            ),
        )
        .unwrap();
    }
    let config = AppConfig::load(dir.path()).unwrap();
    let update = re_app::VerifyOptions {
        update_goldens: true,
        ..re_app::VerifyOptions::default()
    };
    let report = re_app::verify::run_project(dir.path(), &config, &update, &[]);
    let goldens = report.stage("goldens").unwrap();
    assert!(goldens.ok, "{goldens:#?}");
    let too_many: Vec<_> = goldens
        .diagnostics
        .iter()
        .filter(|d| d.code == re_render::codes::TOO_MANY_LIGHTS)
        .collect();
    assert_eq!(too_many.len(), 1, "{goldens:#?}");
    assert!(!too_many[0].is_error());
    // The boot run draws nothing, so its stage has no renderer warning.
    let boot = report.stage("boot").unwrap();
    assert!(
        boot.diagnostics
            .iter()
            .all(|d| !re_app::gfx::is_renderer_warning(d)),
        "{boot:#?}"
    );
}

#[test]
fn a_screenshot_check_past_the_end_or_outside_the_project_fails_the_goldens_stage() {
    let dir = project(SCENE);
    std::fs::create_dir_all(dir.path().join("tests/replays")).unwrap();
    let config = AppConfig::load(dir.path()).unwrap();
    for (frame, path) in [
        (20, "tests/goldens/{backend}/late.png"),
        (5, "../outside.png"),
    ] {
        std::fs::write(
            dir.path().join("tests/replays/x.replay.json"),
            format!(
                r#"{{ "version": 1, "seed": 1, "frames": 20,
                     "checks": [ {{ "frame": {frame}, "screenshot": "{path}" }} ] }}"#
            ),
        )
        .unwrap();
        let update = re_app::VerifyOptions {
            update_goldens: true,
            ..re_app::VerifyOptions::default()
        };
        let report = re_app::verify::run_project(dir.path(), &config, &update, &[]);
        assert!(!report.stage("goldens").unwrap().ok, "{path}");
    }
    assert!(!dir.path().parent().unwrap().join("outside.png").exists());
}

#[test]
fn asset_import_refuses_files_it_cannot_import() {
    let dir = project(SCENE);
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("notes.txt"), "hello").unwrap();
    let mut app = open(dir.path());
    let out = app.dispatch(
        "asset.import",
        serde_json::json!({ "source": src.path().join("notes.txt").display().to_string() }),
        false,
        &re_command::AllowAll,
        re_command::Origin::User,
    );
    assert!(!out.ok);
    assert!(!dir.path().join("assets/notes.txt").exists());
}

#[test]
fn edit_frames_upload_and_draw_with_a_renderer_the_caller_made() {
    let dir = project(SCENE);
    let mut app = open(dir.path());
    // What an editor does: its own device and renderer, handed to the app.
    let backends = re_app::gfx::backends(&[]).unwrap();
    let gpu = re_render::Gpu::headless(backends).unwrap();
    let renderer = re_render::Renderer::new(&gpu, re_render::Capture::FORMAT);
    app.set_graphics(re_app::Graphics::from_parts(gpu, renderer));
    app.edit_frame();
    assert_eq!(app.snapshot().items.len(), 1);
    let capture = {
        let gpu = app.graphics().unwrap().gpu();
        re_render::Capture::new(&gpu.device, 160, 120).unwrap()
    };
    app.draw_into(capture.view(), [160, 120], 1.0).unwrap();
    let gpu = app.graphics().unwrap().gpu();
    let image = capture.read(&gpu.device, &gpu.queue).unwrap();
    let [r, g, b, _] = image.pixel(80, 60);
    assert!(r > 200 && g < 40 && b < 40, "centre is {:?}", [r, g, b]);
    // Edit mode simulates nothing and runs no scripts.
    assert_eq!(app.frame_count(), 0);
    assert!(app.scripts().is_none());
}
