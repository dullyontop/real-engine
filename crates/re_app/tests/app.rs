//! `re_app` integration tests on small projects written to a temporary
//! directory: the frame loop with physics, edit mode, serving engine requests,
//! replays, verify and the host services.

use std::path::Path;
use std::time::Duration;

use glam::Vec3;
use re_app::{App, AppConfig, VerifyOptions};
use re_command::{AllowAll, Origin};
use re_reflect::Value;
use re_types::{GroupMask, HostServices, MaterialHandle, MeshHandle, ReplayDoc};
use serde_json::json;

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "box", "name": "Box", "components": {
        "Collider": { "shape": "Box", "half_extents": [0.5, 0.5, 0.5] },
        "MeshRef": { "asset": "assets/cube.glb" },
        "RigidBody": { "kind": "Dynamic", "mass": 10.0 },
        "Transform": { "translation": [0.0, 4.0, 0.0] } } },
    { "id": "camera", "name": "Camera", "components": {
        "Camera": { "active": true, "fov_y_deg": 50.0 },
        "Transform": { "translation": [0.0, 5.0, 12.0] } } },
    { "id": "ground", "name": "Ground", "components": {
        "Collider": { "shape": "Box", "half_extents": [20.0, 0.5, 20.0] },
        "Transform": { "translation": [0.0, -0.5, 0.0] } } },
    { "id": "hero", "name": "Hero", "components": {
        "CharacterController": { "height": 1.8, "radius": 0.4 },
        "Transform": { "translation": [5.0, 1.0, 0.0] } } },
    { "id": "hud", "name": "Hud", "components": { "HudText": { "text": "0 / 3" } } },
    { "id": "music", "name": "Music", "components": {
        "AudioSource": { "clip": "assets/music.wav", "autoplay": true, "looping": true } } },
    { "id": "sun", "name": "Sun", "components": {
        "Light": { "kind": "Directional", "intensity": 3.0 } } }
  ]
}
"#;

const CONFIG: &str = r#"[project]
name = "fixture"

[input.buttons]
jump = ["key:Space"]

[input.axes]
move = [{ composite = ["key:KeyW", "key:KeyS", "key:KeyA", "key:KeyD"] }]
"#;

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_project(dir.path(), SCENE);
    dir
}

fn write_project(dir: &Path, scene: &str) {
    std::fs::create_dir_all(dir.join("scenes")).unwrap();
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("real.toml"), CONFIG).unwrap();
    std::fs::write(dir.join("scenes/main.scene.json"), scene).unwrap();
    std::fs::write(dir.join("assets/music.wav"), wav(800)).unwrap();
}

/// A silent 16-bit mono PCM WAV of `samples` samples at 8 kHz.
fn wav(samples: u32) -> Vec<u8> {
    let data = samples * 2;
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&8000u32.to_le_bytes());
    w.extend_from_slice(&16000u32.to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    w.resize(w.len() + data as usize, 0);
    w
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn translation(app: &App, id: &str) -> Vec3 {
    match app.world().get(id, "Transform", "translation").unwrap() {
        Value::Vec3(v) => v,
        other => panic!("{other:?}"),
    }
}

fn id_of(app: &App, id: &str) -> re_types::EntityId {
    re_scene::entity_id(app.world().entity(id).unwrap())
}

#[test]
fn a_dynamic_box_falls_and_lands_on_static_ground() {
    let dir = project();
    let mut app = open(dir.path());
    let report = app.run_headless(Some(120), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(report.frames, 120);
    let y = translation(&app, "box").y;
    assert!(
        (y - 0.5).abs() < 0.05,
        "the box rests on the ground, y = {y}"
    );
    // Box, ground and the character's inner body.
    assert_eq!(app.physics().body_count(), 3);
    assert_eq!(app.physics().character_count(), 1);
    let perf = report.perf;
    assert_eq!(perf.frames, 120);
    assert_eq!(perf.physics_step.samples, 120);
    assert_eq!(perf.counts.characters, 1);
    // The scene's seven entities.
    assert_eq!(perf.counts.entities, 7);
}

#[test]
fn headless_runs_are_deterministic() {
    let dir = project();
    let a = open(dir.path()).run_headless(Some(90), None).unwrap();
    let b = open(dir.path()).run_headless(Some(90), None).unwrap();
    assert_eq!(a.state_hash, b.state_hash);
}

#[test]
fn disabling_a_collider_removes_its_body_and_enabling_adds_it_back() {
    let dir = project();
    let mut app = open(dir.path());
    app.run_headless(Some(2), None).unwrap();
    assert_eq!(app.physics().body_count(), 3);
    app.world_mut()
        .set("box", "Collider", "enabled", Value::Bool(false))
        .unwrap();
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(app.physics().body_count(), 2);
    assert!(app.physics_sync().kind(id_of(&app, "box")).is_none());
    app.world_mut()
        .set("box", "Collider", "enabled", Value::Bool(true))
        .unwrap();
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(app.physics().body_count(), 3);
}

#[test]
fn writing_a_transform_teleports_the_body() {
    let dir = project();
    let mut app = open(dir.path());
    app.run_headless(Some(5), None).unwrap();
    app.world_mut()
        .set(
            "box",
            "Transform",
            "translation",
            Value::Vec3(Vec3::new(3.0, 10.0, 0.0)),
        )
        .unwrap();
    app.run_headless(Some(1), None).unwrap();
    let p = translation(&app, "box");
    assert!((p.x - 3.0).abs() < 1e-3 && p.y > 9.5, "teleported to {p}");
    let (pos, _) = app.physics().pose(id_of(&app, "box")).unwrap();
    assert!((pos.x - 3.0).abs() < 1e-3);
}

#[test]
fn moving_a_parent_moves_its_static_collider() {
    let dir = tempfile::tempdir().unwrap();
    write_project(
        dir.path(),
        r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "rig", "components": { "Transform": { "translation": [0.0, 0.0, 0.0] } } },
    { "id": "deck", "parent": "rig", "components": {
        "Collider": { "shape": "Box", "half_extents": [1.0, 0.1, 1.0] },
        "Transform": { "translation": [0.0, 2.0, 0.0] } } }
  ]
}
"#,
    );
    let mut app = open(dir.path());
    app.run_headless(Some(2), None).unwrap();
    app.world_mut()
        .set(
            "rig",
            "Transform",
            "translation",
            Value::Vec3(Vec3::new(5.0, 0.0, 0.0)),
        )
        .unwrap();
    app.run_headless(Some(2), None).unwrap();
    let (pos, _) = app.physics().pose(id_of(&app, "deck")).unwrap();
    assert!(
        (pos - Vec3::new(5.0, 2.0, 0.0)).length() < 1e-3,
        "deck at {pos}"
    );
}

#[test]
fn changing_a_shape_replaces_the_body() {
    let dir = project();
    let mut app = open(dir.path());
    app.run_headless(Some(2), None).unwrap();
    app.world_mut()
        .set("box", "Collider", "shape", Value::Enum("Sphere".into()))
        .unwrap();
    app.run_headless(Some(60), None).unwrap();
    let y = translation(&app, "box").y;
    // A sphere of radius 0.5 rests with its centre at 0.5 too, and still one body.
    assert!((y - 0.5).abs() < 0.05, "y = {y}");
    assert_eq!(app.physics().body_count(), 3);
}

#[test]
fn an_out_of_range_layer_is_reported_once_and_gets_no_body() {
    let dir = project();
    let mut app = open(dir.path());
    app.world_mut()
        .set("box", "Collider", "layer", Value::Int(70_000))
        .unwrap();
    let report = app.run_headless(Some(3), None).unwrap();
    let hits: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code == re_app::codes::PHYSICS_FIELD_OUT_OF_RANGE)
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", report.diagnostics);
    assert!(app.physics_sync().kind(id_of(&app, "box")).is_none());
}

#[test]
fn a_missing_mesh_source_is_rs0506() {
    let dir = project();
    let mut app = open(dir.path());
    app.world_mut()
        .set("box", "Collider", "shape", Value::Enum("Convex".into()))
        .unwrap();
    let report = app.run_headless(Some(2), None).unwrap();
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code == re_app::codes::SHAPE_SOURCE_MISSING)
    );
}

#[test]
fn host_moves_characters_and_refuses_impulses_on_static_bodies() {
    let dir = project();
    let mut app = open(dir.path());
    app.run_headless(Some(30), None).unwrap();
    let hero = id_of(&app, "hero");
    let ground = id_of(&app, "ground");
    let start = translation(&app, "hero");
    for _ in 0..30 {
        app.host()
            .character_move(hero, Vec3::new(0.0, 0.0, -3.0), false)
            .unwrap();
        app.frame(app.step());
    }
    let end = translation(&app, "hero");
    assert!(end.z < start.z - 1.0, "walked from {start} to {end}");
    let state = app.host().character_state(hero).unwrap();
    assert!(state.on_ground(), "{state:?}");

    let err = app.host().apply_impulse(ground, Vec3::Y).unwrap_err();
    assert_eq!(err.code, "RE0504");
    let err = app
        .host()
        .character_move(ground, Vec3::ZERO, false)
        .unwrap_err();
    assert_eq!(err.code, "RE0503");

    // A ray straight down from above the hero hits the hero; masking out its
    // group sees the ground instead.
    let hit = app
        .host()
        .raycast(end + Vec3::Y * 5.0, Vec3::NEG_Y, 20.0, GroupMask::ALL)
        .unwrap();
    assert_eq!(hit.entity, hero);
    let hit = app
        .host()
        .raycast(end + Vec3::Y * 5.0, Vec3::NEG_Y, 20.0, GroupMask(2))
        .map(|h| h.entity);
    assert_eq!(hit, None, "no body is in group 1");
}

#[test]
fn hud_text_and_impulses_apply_through_the_host() {
    let dir = project();
    let mut app = open(dir.path());
    app.run_headless(Some(60), None).unwrap();
    let b = id_of(&app, "box");
    let hud = id_of(&app, "hud");
    app.host()
        .apply_impulse(b, Vec3::new(0.0, 80.0, 0.0))
        .unwrap();
    app.host().set_hud_text(hud, "1 / 3").unwrap();
    app.frame(app.step());
    let (v, _) = app.host().body_velocity(b).unwrap();
    assert!(v.y > 5.0, "velocity after the impulse {v}");
    assert_eq!(
        app.world().get("hud", "HudText", "text").unwrap(),
        Value::Str("1 / 3".into())
    );
}

#[test]
fn velocities_and_the_time_scale_apply_through_the_host() {
    let dir = project();
    let mut app = open(dir.path());
    app.run_headless(Some(60), None).unwrap();
    let b = id_of(&app, "box");
    // Only a dynamic body takes a velocity, and only a finite one.
    let hero = id_of(&app, "hero");
    assert!(
        app.host()
            .set_body_velocity(hero, Vec3::X, Vec3::ZERO)
            .is_err()
    );
    let nan = Vec3::new(f32::NAN, 0.0, 0.0);
    assert!(app.host().set_body_velocity(b, nan, Vec3::ZERO).is_err());
    app.host()
        .set_body_velocity(b, Vec3::new(0.0, 6.0, 0.0), Vec3::ZERO)
        .unwrap();
    app.frame(app.step());
    let (v, _) = app.host().body_velocity(b).unwrap();
    assert!(v.y > 5.0, "velocity after set_velocity {v}");

    // At a time scale of 0 no fixed step runs: the box hangs in the air.
    for bad in [-1.0, 11.0, f64::NAN] {
        assert!(app.host().set_time_scale(bad).is_err(), "{bad}");
    }
    app.host().set_time_scale(0.0).unwrap();
    assert_eq!(app.host().time_scale(), 0.0);
    let y = translation(&app, "box").y;
    for _ in 0..10 {
        app.frame(app.step());
    }
    assert_eq!(translation(&app, "box").y, y);
    // A restart sets it back to 1.
    app.host().request_restart();
    app.frame(app.step());
    assert_eq!(app.host().time_scale(), 1.0);
}

/// `HudText.color` reaches the HUD's lines, its channels kept within 0 to 1.
#[test]
fn hud_lines_have_their_text_colour() {
    let dir = tempfile::tempdir().unwrap();
    let scene = SCENE.replace(
        r#""HudText": { "text": "0 / 3" }"#,
        r#""HudText": { "text": "0 / 3", "color": [1.0, 0.5, 0.25, 2.0] }"#,
    );
    write_project(dir.path(), &scene);
    let mut app = open(dir.path());
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(app.hud_lines().len(), 1);
    assert_eq!(app.hud_lines()[0].color, [1.0, 0.5, 0.25, 1.0]);
}

#[test]
fn restart_reloads_the_entry_scene() {
    let dir = project();
    let mut app = open(dir.path());
    app.run_headless(Some(60), None).unwrap();
    assert!(translation(&app, "box").y < 1.0);
    app.host().request_restart();
    app.frame(app.step());
    // The restart happens at the end of the frame: the box is back at its start,
    // and the next frame's physics phase adds the bodies to the new world.
    assert_eq!(translation(&app, "box").y, 4.0);
    assert_eq!(app.physics().body_count(), 0);
    app.frame(app.step());
    assert_eq!(app.physics().body_count(), 3);
    assert!(translation(&app, "box").y > 3.9);
}

#[test]
fn the_snapshot_has_the_camera_lights_and_uploaded_meshes() {
    let dir = project();
    let mut app = open(dir.path());
    app.run_headless(Some(1), None).unwrap();
    // Nothing is uploaded: nothing is drawn.
    assert!(app.snapshot().items.is_empty());
    assert_eq!(app.snapshot().lights.len(), 1);
    assert!((app.snapshot().camera.fov_y_radians - 50f32.to_radians()).abs() < 1e-6);
    assert_eq!(app.snapshot().camera.position(), Vec3::new(0.0, 5.0, 12.0));
    app.render_assets_mut()
        .insert_mesh("assets/cube.glb", MeshHandle::new(0, 1));
    app.render_assets_mut().default_material = Some(MaterialHandle::new(0, 1));
    app.run_headless(Some(1), None).unwrap();
    let items = &app.snapshot().items;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].entity, id_of(&app, "box"));
}

/// `real.toml`'s `[render.post]` and `[render.shadows]` reach a native
/// project's snapshots; without them the snapshot has the defaults.
#[test]
fn post_and_shadow_settings_come_from_real_toml() {
    let dir = project();
    let mut app = open(dir.path());
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(
        app.snapshot().post,
        re_types::render::PostSettings::default()
    );
    assert_eq!(
        app.snapshot().shadows,
        re_types::render::SunShadows::default()
    );

    let config = format!(
        "{CONFIG}\n[render.post]\nexposure = 0.5\ntone_mapping = \"PbrNeutral\"\n\n\
         [render.post.bloom]\nintensity = 0.3\n\n[render.shadows]\ncascades = 2\ndistance = 80.0\n"
    );
    std::fs::write(dir.path().join("real.toml"), config).unwrap();
    let mut app = open(dir.path());
    app.run_headless(Some(1), None).unwrap();
    let snapshot = app.snapshot();
    assert_eq!(snapshot.post.exposure, 0.5);
    assert_eq!(
        snapshot.post.tone_mapping,
        re_types::render::ToneMapping::PbrNeutral
    );
    assert_eq!(snapshot.post.bloom.intensity, 0.3);
    // Keys left out keep their defaults.
    assert_eq!(
        snapshot.post.bloom.radius,
        re_types::render::Bloom::default().radius
    );
    assert_eq!(
        (snapshot.shadows.cascades, snapshot.shadows.distance),
        (2, 80.0)
    );
    assert_eq!(snapshot.shadows.split_lambda, 0.75);
}

#[test]
fn autoplay_sources_start_once_and_the_listener_follows_the_camera() {
    let dir = project();
    let mut app = open(dir.path());
    app.run_headless(Some(1), None).unwrap();
    let cmds = app.audio_commands().to_vec();
    assert!(cmds.iter().any(|c| matches!(c, re_types::AudioCmd::StartSource { clip, looping: true, .. } if clip == "assets/music.wav")));
    assert!(
        cmds.iter()
            .any(|c| matches!(c, re_types::AudioCmd::SetListener { .. }))
    );
}

#[test]
fn dispatch_edits_and_persists_only_when_asked() {
    let dir = project();
    let scene_path = dir.path().join("scenes/main.scene.json");
    let before = std::fs::read_to_string(&scene_path).unwrap();

    let mut app = open(dir.path());
    let out = app.dispatch(
        "entity.spawn",
        json!({ "id": "crate_9", "name": "Crate" }),
        false,
        &AllowAll,
        Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(std::fs::read_to_string(&scene_path).unwrap(), before);

    let config = AppConfig {
        persist_edits: true,
        ..AppConfig::load(dir.path()).unwrap()
    };
    let mut app = App::open(dir.path(), config).unwrap();
    let dry = app.dispatch(
        "entity.spawn",
        json!({ "id": "crate_9", "dry_run": true }),
        false,
        &AllowAll,
        Origin::User,
    );
    assert!(dry.ok && !dry.changes.is_empty());
    assert_eq!(std::fs::read_to_string(&scene_path).unwrap(), before);
    let out = app.dispatch(
        "entity.spawn",
        json!({ "id": "crate_9" }),
        false,
        &AllowAll,
        Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let after = std::fs::read_to_string(&scene_path).unwrap();
    assert!(after.contains("\"crate_9\""));
    // The saved scene opens again and contains the new entity.
    let app = open(dir.path());
    assert!(app.world().entity("crate_9").is_ok());
}

#[test]
fn saving_writes_open_documents_and_status_reports_them() {
    let dir = project();
    std::fs::create_dir_all(dir.path().join("graphs")).unwrap();
    let path = "graphs/g.graph.json";
    let file = dir.path().join(path);
    let original = re_types::graph::GraphDoc::new("g", 3).to_json();
    std::fs::write(&file, &original).unwrap();
    let mut app = open(dir.path());
    let value = serde_json::from_str(&original).unwrap();
    app.world_mut().open_doc(path, value).unwrap();
    let edit = |app: &mut App, ops: serde_json::Value| {
        app.dispatch(
            "scene.apply",
            json!({ "ops": ops }),
            false,
            &AllowAll,
            Origin::User,
        )
    };
    // The app registered the graph validator: an invalid graph is refused.
    let bad = edit(
        &mut app,
        json!([{ "op": "set_doc", "doc": path, "pointer": "/version", "value": 0 }]),
    );
    assert!(!bad.ok);
    let out = edit(
        &mut app,
        json!([{ "op": "set_doc", "doc": path, "pointer": "/layout", "value": { "n": [1.0, 2.0] } }]),
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let status = app.dispatch("project.status", json!({}), false, &AllowAll, Origin::User);
    assert_eq!(status.data["unsaved"], json!([path]));
    // Without persist_edits, the file changes only on save.
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);
    app.save_scene().unwrap();
    let saved = std::fs::read_to_string(&file).unwrap();
    let doc = re_types::graph::GraphDoc::from_json(&saved).unwrap();
    assert_eq!(doc.layout["n"], [1.0, 2.0]);
    assert_eq!(saved, doc.to_json(), "written as graph files are");
    assert!(app.world().unsaved_docs().is_empty());
    let status = app.dispatch("project.status", json!({}), false, &AllowAll, Origin::User);
    assert_eq!(status.data["unsaved"], json!([]));
    // Undo and save again: the file is back, byte for byte.
    assert!(
        app.dispatch("undo", json!({}), false, &AllowAll, Origin::User)
            .ok
    );
    app.save_scene().unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), original);

    // The watcher seeing our own save changes nothing (undo still works).
    let changed =
        |app: &mut App| app.apply_file_changes(&[re_assets::AssetChange::Modified(path.into())]);
    let epoch = app.world().doc_epoch(path).unwrap();
    assert!(changed(&mut app).is_empty());
    assert_eq!(app.world().doc_epoch(path), Some(epoch));
    assert!(app.history().can_redo());

    // A clean document whose file changed is reloaded: new epoch and
    // revision, and the old history no longer applies.
    let revision = app.world().doc(path).unwrap().revision;
    let other = re_types::graph::GraphDoc::new("other", 4).to_json();
    std::fs::write(&file, &other).unwrap();
    assert!(changed(&mut app).is_empty());
    let doc = app.world().doc(path).unwrap();
    assert_eq!(doc.value["name"], "other");
    assert!(doc.revision > revision && doc.epoch > epoch && !doc.dirty);
    let redo = app.dispatch("redo", json!({}), false, &AllowAll, Origin::User);
    assert!(!redo.ok);
    assert_eq!(redo.diagnostics[0].code.to_string(), "RE2007");
    assert_eq!(app.world().doc(path).unwrap().value["name"], "other");

    // A document with unsaved edits is kept, with a warning.
    let out = edit(
        &mut app,
        json!([{ "op": "set_doc", "doc": path, "pointer": "/seed", "value": 9 }]),
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    std::fs::write(&file, &original).unwrap();
    let problems = changed(&mut app);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].code.to_string(), "RE0528");
    assert!(
        problems[0].message.contains("unsaved edits"),
        "{problems:?}"
    );
    assert_eq!(app.world().doc(path).unwrap().value["seed"], 9);
}

#[test]
fn serve_answers_requests_from_another_thread() {
    let dir = project();
    let config = AppConfig {
        persist_edits: true,
        ..AppConfig::load(dir.path()).unwrap()
    };
    let mut app = App::open(dir.path(), config).unwrap();
    let (handle, receiver) = re_command::channel();
    let client = std::thread::spawn(move || {
        let agent = handle.with_origin(Origin::Agent("test".into()));
        let specs = agent.list_specs().unwrap();
        assert!(specs.iter().any(|s| s.name == "verify"));
        let out = agent.call("entity.spawn", json!({ "id": "from_agent" }), false);
        assert!(out.ok, "{:?}", out.diagnostics);
        let out = agent.call("entity.get", json!({ "id": "from_agent" }), false);
        assert!(out.ok);
        let out = agent.call("undo", json!({}), false);
        assert!(out.ok);
        agent.call("entity.get", json!({ "id": "from_agent" }), false)
    });
    app.serve(receiver, &AllowAll);
    let last = client.join().unwrap();
    assert!(!last.ok, "the spawn was undone");
    let log = app.history().log();
    assert!(log.iter().any(|e| e.origin == Origin::Agent("test".into())));
    // Undo persisted too: the saved scene no longer has the entity.
    let text = std::fs::read_to_string(dir.path().join("scenes/main.scene.json")).unwrap();
    assert!(!text.contains("from_agent"));
}

#[test]
fn serve_pending_does_not_block() {
    let dir = project();
    let mut app = open(dir.path());
    let (handle, receiver) = re_command::channel();
    assert_eq!(app.serve_pending(&receiver, &AllowAll), Some(0));
    drop(handle);
    assert_eq!(app.serve_pending(&receiver, &AllowAll), None);
}

fn replay(frames: u64) -> ReplayDoc {
    let mut doc = ReplayDoc::new(7, frames);
    doc.input = vec![
        re_types::ReplayInput {
            frame: 10,
            action: "move".into(),
            down: None,
            axis: Some([0.0, 1.0]),
        },
        re_types::ReplayInput {
            frame: 20,
            action: "jump".into(),
            down: Some(true),
            axis: None,
        },
    ];
    doc.commands = vec![re_types::ReplayCommand {
        frame: 30,
        name: "entity.set".into(),
        args: json!({ "id": "box", "component": "Transform", "field": "translation", "value": [1.0, 6.0, 0.0] }),
    }];
    doc
}

#[test]
fn replays_drive_actions_and_commands() {
    let dir = project();
    let mut app = open(dir.path());
    app.play_replay(replay(40)).unwrap();
    for _ in 0..21 {
        app.frame(app.step());
    }
    assert_eq!(app.actions().axis("move"), glam::Vec2::new(0.0, 1.0));
    assert!(app.actions().pressed("jump"));
    let report = app.run_headless(Some(19), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert!(
        translation(&app, "box").x > 0.9,
        "the replay command moved the box"
    );
}

#[test]
fn a_replay_naming_an_unknown_action_is_refused() {
    let dir = project();
    let mut app = open(dir.path());
    let mut doc = replay(10);
    doc.input[0].action = "mvoe".into();
    let e = app.play_replay(doc).unwrap_err();
    assert_eq!(e.code, re_app::codes::REPLAY_FAILED);
}

#[test]
fn verify_passes_on_a_good_project_and_reports_broken_content() {
    let dir = project();
    std::fs::create_dir_all(dir.path().join("tests/replays")).unwrap();
    std::fs::write(
        dir.path().join("tests/replays/walk.replay.json"),
        serde_json::to_string(&replay(60)).unwrap(),
    )
    .unwrap();
    let config = AppConfig::load(dir.path()).unwrap();
    let options = VerifyOptions {
        boot_frames: 30,
        warmup_frames: 5,
        ..VerifyOptions::default()
    };
    let report = re_app::verify::run_project(dir.path(), &config, &options, &[]);
    assert!(
        report.ok,
        "{}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    let names: Vec<&str> = report.stages.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names,
        [
            "schema", "compile", "scan", "boot", "replays", "goldens", "perf"
        ]
    );
    let replays = report.stage("replays").unwrap();
    assert_eq!(replays.data["runs"][0]["deterministic"], true);
    assert_eq!(report.perf.as_ref().unwrap().frames, 25);

    std::fs::create_dir_all(dir.path().join("prefabs")).unwrap();
    std::fs::write(
        dir.path().join("prefabs/bad.prefab.json"),
        r#"{ "format": "real-engine/prefab", "version": 1, "entities": [ { "id": "a", "components": { "Colider": {} } } ] }"#,
    )
    .unwrap();
    let report = re_app::verify::run_project(dir.path(), &config, &options, &[]);
    assert!(!report.ok);
    let schema = report.stage("schema").unwrap();
    assert!(!schema.ok);
    assert_eq!(schema.diagnostics[0].code, re_app::codes::CONTENT_INVALID);
    assert_eq!(report.exit_code(), 1);
}

struct ScanStage;

impl re_app::VerifyStage for ScanStage {
    fn name(&self) -> &str {
        "scan"
    }
    fn run(&self, ctx: &re_app::verify::VerifyContext<'_>) -> re_app::StageReport {
        re_app::StageReport::new(
            "scan",
            Vec::new(),
            json!({ "project": ctx.config.project.name }),
        )
    }
}

#[test]
fn extra_stages_run_after_compile_and_through_the_command() {
    let dir = project();
    let mut app = open(dir.path());
    app.add_verify_stage(std::sync::Arc::new(ScanStage));
    let out = app.dispatch(
        "verify",
        json!({ "boot_frames": 10 }),
        false,
        &AllowAll,
        Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let stages = out.data["stages"].as_array().unwrap();
    assert_eq!(stages[2]["name"], "scan");
    assert_eq!(stages[2]["data"]["project"], "fixture");
    assert_eq!(out.data["ok"], true);
}

#[test]
fn a_broken_entry_scene_is_re0502_with_the_cause() {
    let dir = tempfile::tempdir().unwrap();
    write_project(
        dir.path(),
        r#"{ "format": "real-engine/scene", "version": 1, "entities": [ { "id": "a", "parent": "b" } ] }"#,
    );
    let e = App::open(dir.path(), AppConfig::load(dir.path()).unwrap())
        .err()
        .unwrap();
    assert_eq!(e.code, re_app::codes::ENTRY_SCENE_FAILED);
    assert_eq!(e.related.len(), 1);
    let config = AppConfig::load(dir.path()).unwrap();
    let report = re_app::verify::run_project(dir.path(), &config, &VerifyOptions::default(), &[]);
    assert!(!report.stage("boot").unwrap().ok);
    assert!(
        report.stage("replays").unwrap().diagnostics[0]
            .message
            .contains("did not boot")
    );
}

#[test]
fn a_directory_without_real_toml_is_re0500() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        AppConfig::load(dir.path()).unwrap_err().code,
        re_app::codes::PROJECT_NOT_FOUND
    );
}

#[test]
fn frames_take_variable_time_with_catch_up_steps() {
    let dir = project();
    let mut app = open(dir.path());
    app.frame(Duration::from_millis(50));
    // 50 ms holds three 1/60 s steps.
    assert_eq!(app.perf().frames(), 1);
    let report = app.perf_report();
    assert_eq!(report.physics_step.samples, 3);
}
