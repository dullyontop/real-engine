//! Luau scripts in the running game: `ScriptRef`s start on the first frame,
//! callbacks run in their phases, errors are reported without stopping the game,
//! `world.restart()` and hot reload start scripts again, and `play.run` plays
//! with fresh VMs.

use std::path::Path;

use re_app::{App, AppConfig};
use re_command::{AllowAll, Origin};
use re_reflect::Value;
use re_script::ContextKind;
use serde_json::json;

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "box", "name": "Box", "components": {
        "Collider": { "shape": "Box", "half_extents": [0.5, 0.5, 0.5] },
        "RigidBody": { "kind": "Dynamic", "mass": 1.0 },
        "Transform": { "translation": [0.0, 3.0, 0.0] } } },
    { "id": "pad", "name": "Pad", "components": {
        "Collider": { "shape": "Box", "half_extents": [5.0, 0.5, 5.0] },
        "HudText": { "text": "" },
        "ScriptRef": { "path": "scripts/pad.luau" },
        "Transform": { "translation": [0.0, -0.5, 0.0] } } },
    { "id": "mover", "name": "Mover", "components": {
        "ScriptRef": { "path": "scripts/mover.luau" },
        "Transform": { "translation": [10.0, 0.0, 0.0] } } }
  ]
}
"#;

const PAD: &str = r#"local touched = 0
physics.on_touch(script.entity, function(other)
	touched += 1
	ui.set_text(script.entity, `{other.name} landed {touched}`)
end)
"#;

const MOVER: &str = r#"print("mover started")
time.on_step(function(dt)
	local p: vector = script.entity:get("Transform", "translation")
	script.entity:set("Transform", "translation", p + vector.create(0, 0, dt))
end)
"#;

fn project(mover: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("scenes")).unwrap();
    std::fs::create_dir_all(d.join("scripts")).unwrap();
    std::fs::write(d.join("real.toml"), "[project]\nname = \"scripts\"\n").unwrap();
    std::fs::write(d.join("scenes/main.scene.json"), SCENE).unwrap();
    std::fs::write(d.join("scripts/pad.luau"), PAD).unwrap();
    std::fs::write(d.join("scripts/mover.luau"), mover).unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn mover_z(app: &App) -> f32 {
    match app
        .world()
        .get("mover", "Transform", "translation")
        .unwrap()
    {
        Value::Vec3(v) => v.z,
        other => panic!("{other:?}"),
    }
}

fn hud(app: &App) -> String {
    match app.world().get("pad", "HudText", "text").unwrap() {
        Value::Str(s) => s,
        other => panic!("{other:?}"),
    }
}

#[test]
fn attached_scripts_run_their_callbacks_in_the_frame_loop() {
    let dir = project(MOVER);
    let mut app = open(dir.path());
    let report = app.run_headless(Some(60), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(report.output, ["[game] mover started"]);
    // One fixed step per frame at 60 Hz, 1 m/s along +Z.
    let z = mover_z(&app);
    assert!((z - 1.0).abs() < 0.02, "z = {z}");
    // The box fell onto the pad, whose contact callback set its HUD text.
    assert_eq!(hud(&app), "Box landed 1");
}

#[test]
fn scripted_runs_are_deterministic() {
    let dir = project(MOVER);
    let a = open(dir.path()).run_headless(Some(90), None).unwrap();
    let b = open(dir.path()).run_headless(Some(90), None).unwrap();
    assert_eq!(a.state_hash, b.state_hash);
}

#[test]
fn a_script_error_is_reported_and_the_game_runs_on() {
    let dir = project("time.on_step(function() error(\"boom\") end)\n");
    let mut app = open(dir.path());
    let report = app.run_headless(Some(60), None).unwrap();
    assert!(!report.ok());
    let errors: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code.to_string() == "RE3002")
        .collect();
    // Reported once, not once per step.
    assert_eq!(errors.len(), 1, "{:?}", report.diagnostics);
    assert!(errors[0].message.contains("boom"), "{}", errors[0].message);
    // Other scripts and physics went on.
    assert_eq!(hud(&app), "Box landed 1");
}

#[test]
fn a_syntax_error_does_not_stop_other_scripts() {
    let dir = project("this is not luau\n");
    let mut app = open(dir.path());
    let report = app.run_headless(Some(60), None).unwrap();
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.code.to_string() == "RE3001"),
        "{:?}",
        report.diagnostics
    );
    assert_eq!(hud(&app), "Box landed 1");
}

#[test]
fn restart_starts_scripts_again() {
    let dir = project(MOVER);
    let mut app = open(dir.path());
    app.run_headless(Some(30), None).unwrap();
    assert!(mover_z(&app) > 0.4);
    app.restart();
    let report = app.run_headless(Some(30), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(report.output, ["[game] mover started"]);
    let z = mover_z(&app);
    // Only the new run's steps moved it: the old callback is gone.
    assert!((z - 0.5).abs() < 0.02, "z = {z}");
}

#[test]
fn a_changed_script_is_reloaded() {
    let dir = project(MOVER);
    let mut app = open(dir.path());
    app.run_headless(Some(10), None).unwrap();
    let before = mover_z(&app);
    std::fs::write(
        dir.path().join("scripts/mover.luau"),
        "print(\"mover v2\")\ntime.on_step(function() end)\n",
    )
    .unwrap();
    app.apply_file_changes(&[re_assets::AssetChange::Modified(
        "scripts/mover.luau".into(),
    )]);
    let report = app.run_headless(Some(10), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(report.output, ["[game] mover v2"]);
    // The old on_step was disconnected: it no longer moves.
    assert_eq!(mover_z(&app), before);
}

#[test]
fn exec_runs_a_snippet_against_the_running_game() {
    let dir = project(MOVER);
    let mut app = open(dir.path());
    app.run_headless(Some(2), None).unwrap();
    let value = app
        .exec_script(
            ContextKind::Game,
            "probe",
            "return world.get(\"mover\").name",
        )
        .unwrap();
    assert_eq!(value, json!("Mover"));
    let err = app
        .exec_script(ContextKind::Game, "probe", "error(\"nope\")")
        .unwrap_err();
    assert!(err.iter().any(|d| d.message.contains("nope")), "{err:?}");
}

#[test]
fn play_run_uses_fresh_scripts_and_leaves_the_live_world_alone() {
    let dir = project(MOVER);
    let mut app = open(dir.path());
    app.run_headless(Some(30), None).unwrap();
    let live = mover_z(&app);
    let outcome = app.dispatch(
        "play.run",
        json!({ "frames": 30, "screenshot": false }),
        false,
        &AllowAll,
        Origin::User,
    );
    assert!(outcome.ok, "{:?}", outcome.diagnostics);
    assert_eq!(outcome.data["ok"], json!(true));
    assert_eq!(mover_z(&app), live);
    // The live scripts keep running where they were.
    app.run_headless(Some(30), None).unwrap();
    assert!((mover_z(&app) - 1.0).abs() < 0.02);
}

#[test]
fn edit_mode_runs_no_scripts() {
    let dir = project(MOVER);
    let mut app = open(dir.path());
    app.edit_frame();
    app.edit_frame();
    assert!(app.scripts().is_none());
    assert_eq!(mover_z(&app), 0.0);
}

#[test]
fn verify_compiles_every_script() {
    let dir = project(MOVER);
    std::fs::create_dir_all(dir.path().join("scripts/lib")).unwrap();
    std::fs::write(dir.path().join("scripts/lib/bad.luau"), "local x = = 1\n").unwrap();
    let app = open(dir.path());
    let report = re_app::verify::run(&app, &re_app::VerifyOptions::default(), &[]);
    let compile = report.stages.iter().find(|s| s.name == "compile").unwrap();
    assert!(!compile.ok);
    assert_eq!(compile.data["scripts"], json!(3));
    let d = &compile.diagnostics[0];
    assert_eq!(d.code.to_string(), "RE3001");
    assert!(format!("{:?}", d.location).contains("scripts/lib/bad.luau"));
}
