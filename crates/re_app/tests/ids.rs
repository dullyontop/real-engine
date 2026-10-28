//! Run-time ids repeat (ADR-0036 amendment): a game that spawns without ids
//! makes the same ids, and ends with the same state hash, in every run with
//! the same seed: headless, after a restart and in `play.run`.

use std::path::Path;

use re_app::{App, AppConfig};
use re_command::{AllowAll, Origin};
use re_types::ReplayDoc;
use serde_json::json;

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "spawner", "name": "Spawner", "components": {
        "ScriptRef": { "path": "scripts/spawner.luau" } } }
  ]
}
"#;

/// Spawns a crate without an id every tenth step and prints its id.
const SPAWNER: &str = r#"local n = 0
time.on_step(function()
	n += 1
	if n % 10 == 0 then
		local e = world.spawn({
			name = `Crate {n}`,
			components = { Transform = { translation = vector.create(n, 0, 0) } },
		})
		print(e.id)
	end
end)
"#;

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    std::fs::create_dir_all(d.join("scenes")).unwrap();
    std::fs::create_dir_all(d.join("scripts")).unwrap();
    std::fs::write(d.join("real.toml"), "[project]\nname = \"ids\"\n").unwrap();
    std::fs::write(d.join("scenes/main.scene.json"), SCENE).unwrap();
    std::fs::write(d.join("scripts/spawner.luau"), SPAWNER).unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

#[test]
fn a_game_that_spawns_without_ids_repeats_its_ids_and_its_hash() {
    let dir = project();
    let a = open(dir.path()).run_headless(Some(60), None).unwrap();
    assert!(a.ok(), "{:?}", a.diagnostics);
    assert_eq!(a.output.len(), 6, "{:?}", a.output);
    let b = open(dir.path()).run_headless(Some(60), None).unwrap();
    assert_eq!(a.output, b.output);
    assert_eq!(a.state_hash, b.state_hash);
}

#[test]
fn a_replay_seed_chooses_the_ids() {
    let dir = project();
    let config = open(dir.path()).run_headless(Some(30), None).unwrap();
    // real.toml's seed is 0: a replay with seed 0 makes the same ids, and
    // one with another seed other ids.
    let zero = open(dir.path())
        .run_headless(Some(30), Some(ReplayDoc::new(0, 30)))
        .unwrap();
    assert_eq!(zero.output, config.output);
    let five = open(dir.path())
        .run_headless(Some(30), Some(ReplayDoc::new(5, 30)))
        .unwrap();
    assert!(five.ok(), "{:?}", five.diagnostics);
    assert_eq!(five.output.len(), config.output.len());
    assert_ne!(five.output, config.output);
}

#[test]
fn a_restart_repeats_the_first_runs_ids() {
    let dir = project();
    let mut app = open(dir.path());
    let first = app.run_headless(Some(30), None).unwrap();
    app.restart();
    let again = app.run_headless(Some(30), None).unwrap();
    assert!(again.ok(), "{:?}", again.diagnostics);
    assert_eq!(again.output, first.output);
    assert_eq!(again.state_hash, first.state_hash);
}

#[test]
fn play_run_makes_the_ids_a_headless_run_makes() {
    let dir = project();
    let mut app = open(dir.path());
    // The editing world's generator is random; play is seeded all the same.
    app.edit_frame();
    let mut play = || {
        let outcome = app.dispatch(
            "play.run",
            json!({ "frames": 30, "screenshot": false }),
            false,
            &AllowAll,
            Origin::User,
        );
        assert!(outcome.ok, "{:?}", outcome.diagnostics);
        outcome.data
    };
    let a = play();
    let b = play();
    assert_eq!(a["output"], b["output"]);
    assert_eq!(a["state_hash"], b["state_hash"]);
    let headless = open(dir.path()).run_headless(Some(30), None).unwrap();
    assert_eq!(a["output"], json!(headless.output));
    assert_eq!(a["state_hash"], json!(headless.state_hash));
}
