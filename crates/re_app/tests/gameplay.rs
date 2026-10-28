//! ADR-0057 in the running game: the LateFrame phase and drawn poses (§7),
//! and what a run keeps of script output (§11, B5).

use std::path::Path;
use std::time::Duration;

use glam::{Affine3A, Vec3};
use re_app::{App, AppConfig};
use re_reflect::Value;

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "box", "name": "Box", "components": {
        "Collider": { "shape": "Box", "half_extents": [0.5, 0.5, 0.5] },
        "RigidBody": { "kind": "Dynamic", "mass": 1.0 },
        "Transform": { "translation": [0.0, 30.0, 0.0] } } },
    { "id": "lid", "name": "Lid", "parent": "box", "components": {
        "Transform": { "translation": [0.0, 0.5, 0.0] } } },
    { "id": "camera", "name": "Camera", "components": {
        "ScriptRef": { "path": "scripts/camera.luau" },
        "Transform": {} } }
  ]
}
"#;

const CAMERA: &str = r#"local box = world.get("box")
time.on_late_frame(function()
	local at = box:drawn_transform()
	script.entity:set("Transform", "translation", at + vector.create(0, 0, 5))
end)
"#;

fn project(camera: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("scenes")).unwrap();
    std::fs::create_dir_all(d.join("scripts")).unwrap();
    std::fs::write(d.join("real.toml"), "[project]\nname = \"gameplay\"\n").unwrap();
    std::fs::write(d.join("scenes/main.scene.json"), SCENE).unwrap();
    std::fs::write(d.join("scripts/camera.luau"), camera).unwrap();
    dir
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

fn drawn(app: &App, id: &str) -> Affine3A {
    let e = app.world().ids().get(id).unwrap();
    app.drawn_poses()
        .get(e)
        .unwrap_or_else(|| panic!("{id} has no drawn pose"))
}

#[test]
fn a_late_frame_camera_follows_where_the_body_is_drawn() {
    let dir = project(CAMERA);
    let mut app = open(dir.path());
    // A frame and a half of fixed steps per frame: alpha is 0.5 on every
    // other frame, so the falling box is drawn between its last two steps.
    let frame_time = Duration::from_secs_f64(1.5 / 60.0);
    let mut between = 0;
    for _ in 0..20 {
        app.frame(frame_time);
        let body = drawn(&app, "box");
        let at = Vec3::from(body.translation);
        assert!(
            (translation(&app, "camera") - (at + Vec3::Z * 5.0)).length() < 1e-5,
            "the camera follows the drawn box"
        );
        // The lid rides on the box: drawn with the box's correction.
        let lid = Vec3::from(drawn(&app, "lid").translation);
        assert!((lid - (at + Vec3::Y * 0.5)).length() < 1e-4, "{lid} {at}");
        if (at - translation(&app, "box")).length() > 1e-4 {
            between += 1;
        }
    }
    assert!(between >= 5, "drawn between steps on {between} frames");
    assert!(app.take_diagnostics().is_empty());
}

#[test]
fn late_frame_is_timed_in_the_perf_report() {
    let dir = project(CAMERA);
    let mut app = open(dir.path());
    let report = app.run_headless(Some(10), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert!(report.perf.phases.contains_key("LateFrame"));
}

#[test]
fn a_run_keeps_the_newest_script_output_and_counts_what_it_dropped() {
    let dir = project(
        r#"local n = 0
time.on_frame(function()
	for _ = 1, 250 do
		n += 1
		print(n)
	end
end)
"#,
    );
    let mut app = open(dir.path());
    // 60 frames of 250 lines: 15,000, of which a run keeps the last 10,000.
    let report = app.run_headless(Some(60), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(report.output.len(), re_script::vm::MAX_OUTPUT_LINES + 1);
    assert_eq!(
        report.output[0],
        "[app] 5000 earlier lines were dropped: a run keeps the last 10000"
    );
    assert_eq!(report.output[1], "[game] 5001");
    assert_eq!(report.output.last().unwrap(), "[game] 15000");
}

#[test]
fn a_console_takes_the_script_output_of_each_frame() {
    let dir = project(
        r#"local n = 0
time.on_frame(function()
	n += 1
	print(n)
end)
"#,
    );
    let mut app = open(dir.path());
    let frame_time = Duration::from_secs_f64(1.0 / 60.0);
    app.frame(frame_time);
    app.frame(frame_time);
    assert_eq!(app.take_script_output(), ["[game] 1", "[game] 2"]);
    app.frame(frame_time);
    assert_eq!(app.take_script_output(), ["[game] 3"]);
    assert!(app.take_script_output().is_empty());
}
