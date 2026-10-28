//! Luau scripts in the running game: `ScriptRef`s start on the first frame,
//! callbacks run in their phases, errors are reported without stopping the game,
//! `world.restart()` and hot reload start scripts again, and `play.run` plays
//! with fresh VMs.

use std::path::Path;

use re_app::{App, AppConfig};
use re_command::{AllowAll, Origin};
use re_reflect::Value;
use re_script::ContextKind;
use re_types::ReplayDoc;
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

#[test]
fn the_game_context_stays_attached_to_the_debugger_across_a_restart() {
    let dir = project(MOVER);
    let mut app = open(dir.path());
    let debugger = re_script::debug::Debugger::new();
    app.set_debugger(Some(debugger.clone()));
    // The VM starts on the first frame and is attached then.
    assert!(debugger.contexts().is_empty());
    app.frame(std::time::Duration::from_millis(16));
    let names: Vec<String> = debugger.contexts().into_iter().map(|c| c.name).collect();
    assert_eq!(names, ["game"]);

    // A restart replaces the VM: the old context ends, the new one attaches.
    app.restart();
    app.frame(std::time::Duration::from_millis(16));
    let contexts = debugger.contexts();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].name, "game");

    app.set_debugger(None);
    assert!(debugger.contexts().is_empty());
}

#[test]
fn a_running_script_stops_at_a_breakpoint_and_a_reload_ends_the_stop() {
    use re_script::debug::{Debugger, Event, Resume};
    let dir = project(MOVER);
    let mut app = open(dir.path());
    // The mover starts on the first frame, before there is a debugger.
    app.run_headless(Some(1), None).unwrap();
    let debugger = Debugger::new();
    app.set_debugger(Some(debugger.clone()));
    let game = debugger.contexts()[0].id;
    debugger.set_breakpoints("scripts/mover.luau", &[3]);

    // Its step callback stops at line 3 and is parked: the frame returns
    // with the stop shown, and the mover has not moved.
    let z = mover_z(&app);
    app.run_headless(Some(1), None).unwrap();
    let stop = debugger.stopped().expect("the mover stopped");
    assert_eq!(stop.context, game);
    let top = stop.top().unwrap();
    assert_eq!(top.source.as_deref(), Some("scripts/mover.luau"));
    assert_eq!(top.line, Some(3));
    assert!(debugger.last_unpaused().is_none());
    assert_eq!(mover_z(&app), z);
    // The game runs on: the next step's callback runs past the breakpoint
    // while the first stop is shown.
    app.run_headless(Some(1), None).unwrap();
    assert!(mover_z(&app) > z);
    assert_eq!(debugger.stopped().map(|s| s.context), Some(game));
    debugger.take_events();

    // Reloading the script drops the parked callback, which ends its stop.
    std::fs::write(
        dir.path().join("scripts/mover.luau"),
        MOVER.replace("mover started", "mover v2"),
    )
    .unwrap();
    app.apply_file_changes(&[re_assets::AssetChange::Modified(
        "scripts/mover.luau".into(),
    )]);
    assert!(debugger.stopped().is_none());
    assert!(
        debugger
            .take_events()
            .contains(&Event::Continued { context: game })
    );
    // The new run's callback stops at the same breakpoint.
    let report = app.run_headless(Some(1), None).unwrap();
    assert_eq!(report.output, ["[game] mover v2"]);
    let stop = debugger.stopped().expect("the new run stopped");
    assert_eq!(stop.top().unwrap().line, Some(3));

    // Continued with the breakpoint gone, it moves the mover again.
    debugger.set_breakpoints("scripts/mover.luau", &[]);
    assert!(debugger.resume(Resume::Continue));
    let z = mover_z(&app);
    app.run_headless(Some(2), None).unwrap();
    assert!(debugger.stopped().is_none());
    assert!(mover_z(&app) > z);
}

fn x_of(app: &App, id: &str) -> f32 {
    match app.world().get(id, "Transform", "translation").unwrap() {
        Value::Vec3(v) => v.x,
        other => panic!("{other:?}"),
    }
}

/// A body spawned this frame starts with the velocity a script gives it, and
/// the pushes for a body destroyed in the same step are dropped, not errors.
#[test]
fn a_body_spawned_this_step_starts_with_its_velocity() {
    let dir = project(
        r#"local function spawn(id: string): Entity
	return world.spawn({ id = id, components = {
		Transform = { translation = vector.create(20, 10, 0) },
		Collider = { shape = "Box", half_extents = vector.create(0.5, 0.5, 0.5) },
		RigidBody = { kind = "Dynamic", mass = 1 },
	} })
end
physics.set_velocity(spawn("thrown"), vector.create(3, 0, 0))
local gone = spawn("gone")
physics.set_velocity(gone, vector.create(3, 0, 0))
physics.impulse(gone, vector.create(0, 5, 0))
gone:destroy()
"#,
    );
    let mut app = open(dir.path());
    let report = app.run_headless(Some(60), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    // One second at 3 m/s.
    let x = x_of(&app, "thrown");
    assert!((x - 23.0).abs() < 0.1, "x = {x}");
}

/// `math.random` starts from the run's seed (`[scripts] seed`, or a replay's),
/// and `os.clock` is simulation time, so a run repeats.
#[test]
fn the_run_seed_starts_math_random_and_os_clock_is_simulation_time() {
    let draws = |toml: &str, replay: Option<ReplayDoc>| -> Vec<String> {
        let dir = project("print(math.random(1, 1000000), os.clock())\n");
        std::fs::write(dir.path().join("real.toml"), toml).unwrap();
        let report = open(dir.path()).run_headless(Some(2), replay).unwrap();
        assert!(report.ok(), "{:?}", report.diagnostics);
        report.output
    };
    let base = "[project]\nname = \"scripts\"\n";
    let seven = format!("{base}\n[scripts]\nseed = 7\n");
    let a = draws(&seven, None);
    assert_eq!(a, draws(&seven, None));
    assert_ne!(a, draws(&format!("{base}\n[scripts]\nseed = 8\n"), None));
    // A replay's seed takes the place of real.toml's.
    assert_eq!(a, draws(base, Some(ReplayDoc::new(7, 2))));
    assert!(a[0].ends_with("\t86400"), "{a:?}");
}

/// `time.set_scale` slows or stops the fixed steps, `on_frame` goes on, and a
/// restart sets the scale back to 1.
#[test]
fn the_time_scale_slows_and_pauses_the_steps_until_a_restart() {
    let dir = project(
        r#"print("scale", time.scale())
local steps, frames = 0, 0
time.on_step(function()
	steps += 1
end)
time.on_frame(function()
	frames += 1
	if frames == 1 and time.frame() < 10 then
		time.set_scale(0.5)
	elseif frames == 61 then
		print("half", steps)
		time.set_scale(0)
	elseif frames == 91 then
		print("paused", steps, time.scale())
		world.restart()
	end
end)
"#,
    );
    let mut app = open(dir.path());
    let report = app.run_headless(Some(100), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(
        report.output,
        [
            "[game] scale\t1",
            "[game] half\t30",
            "[game] paused\t30\t0",
            "[game] scale\t1"
        ]
    );
}
