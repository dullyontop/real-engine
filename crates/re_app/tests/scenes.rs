//! Scenes in the running game (ADR-0059), on the scenes fixture: its replays
//! switch, load and unload scenes at the frames their `scenes` lists name,
//! with the same state hash on every run; a replay whose list differs fails
//! with `RE0571`; kept entities stand in for the scene's own; `world.restart()`
//! goes back to the scene the run began in, or to a play session's start
//! world; and edit mode opens and saves another scene.

use std::path::{Path, PathBuf};

use re_app::{App, AppConfig, VerifyOptions, codes};
use re_reflect::Value;
use re_types::{ReplayDoc, SceneChange, SceneChangeKind};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scenes")
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn replay(name: &str) -> ReplayDoc {
    let path = fixture().join(format!("tests/replays/{name}.replay.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// A copy of the fixture to write into.
fn copy() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["", "scenes", "scripts", "tests/replays"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
        for entry in std::fs::read_dir(fixture().join(sub)).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_file() {
                std::fs::copy(entry.path(), dir.path().join(sub).join(entry.file_name())).unwrap();
            }
        }
    }
    dir
}

fn name(app: &App, id: &str) -> Option<String> {
    let e = app.world().entity(id).ok()?;
    app.world()
        .ecs
        .get::<re_scene::Name>(e)
        .map(|n| n.0.clone())
}

fn position(app: &App, id: &str) -> glam::Vec3 {
    match app.world().get(id, "Transform", "translation").unwrap() {
        Value::Vec3(v) => v,
        other => panic!("{other:?}"),
    }
}

fn text(app: &App, id: &str) -> String {
    match app.world().get(id, "HudText", "text").unwrap() {
        Value::Str(s) => s,
        other => panic!("{other:?}"),
    }
}

fn change(frame: u64, kind: SceneChangeKind, name: &str) -> SceneChange {
    SceneChange::new(frame, kind, name)
}

#[test]
fn the_tour_changes_scenes_where_its_replay_lists_them_with_the_same_hash_twice() {
    let doc = replay("tour");
    let mut hashes = Vec::new();
    for _ in 0..2 {
        let mut app = open(&fixture());
        let report = app.run_headless(None, Some(doc.clone())).unwrap();
        assert!(report.ok(), "{:#?}", report.diagnostics);
        assert_eq!(Some(report.scenes), doc.scenes);
        // Back in the title, with the player the title made, kept through
        // the town.
        assert_eq!(app.scenes().main(), "title");
        assert_eq!(name(&app, "player").as_deref(), Some("Player"));
        assert!(app.world().entity("crate_a").is_err());
        hashes.push(report.state_hash);
    }
    assert_eq!(hashes[0], hashes[1]);
}

#[test]
fn a_switch_applies_at_the_end_of_the_frame_that_asked_and_kept_entities_stand_in() {
    let mut app = open(&fixture());
    app.play_replay(replay("tour")).unwrap();
    let step = app.step();
    for _ in 0..30 {
        app.frame(step);
    }
    // The player walked in the title for 20 frames.
    let walked = position(&app, "player");
    assert!(walked.x > 0.5 && walked.z < -0.5, "{walked}");
    app.take_diagnostics();
    // Frame 30 presses "next": the town replaces the title at its EndFrame.
    app.frame(step);
    assert_eq!(app.scenes().main(), "town");
    assert_eq!(
        app.scenes().log(),
        [change(30, SceneChangeKind::Switch, "town")]
    );
    assert!(app.world().entity("crate_a").is_ok());
    assert!(app.world().entity("floor").is_err());
    // The kept player stood in for the town's, which was not spawned.
    assert_eq!(name(&app, "player").as_deref(), Some("Player"));
    assert!(position(&app, "player").distance(walked) < 0.5);
    let notes = app.take_diagnostics();
    assert!(
        notes
            .iter()
            .any(|d| d.code == codes::KEPT_STANDS_IN && d.message.contains("\"player\"")),
        "{notes:#?}"
    );
    // The director's thread waits for the switch, and resumes in the next
    // frame's first script phase.
    assert_eq!(text(&app, "director"), "title: title (loading)");
    app.frame(step);
    assert_eq!(text(&app, "director"), "town: town");
    // The HUD loads over the town at frame 90, and goes again at 150.
    for _ in 32..91 {
        app.frame(step);
    }
    assert_eq!(app.scenes().loaded(), ["town", "hud"]);
    assert!(app.world().entity("banner").is_ok());
    assert_eq!(text(&app, "director"), "town: town (loading)");
    app.frame(step);
    assert_eq!(text(&app, "director"), "town: town, hud");
    assert_eq!(text(&app, "banner"), "town, 1 steps");
    for _ in 92..151 {
        app.frame(step);
    }
    assert!(app.world().entity("banner").is_err());
    assert!(app.world().entity("crate_a").is_ok());
    assert_eq!(text(&app, "director"), "town: town, hud");
    app.frame(step);
    assert_eq!(text(&app, "director"), "town: town");
    let report = app.run_headless(Some(60), None).unwrap();
    assert_eq!(
        report.output,
        [
            "[game] town loaded",
            "[game] hud loaded",
            "[game] title loaded"
        ]
    );
}

#[test]
fn a_replay_whose_scene_changes_differ_fails_with_re0571() {
    let mut late = replay("tour");
    late.scenes.as_mut().unwrap()[1].frame = 91;
    let report = open(&fixture()).run_headless(None, Some(late)).unwrap();
    let differ: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code == codes::SCENE_CHANGES_DIFFER)
        .collect();
    assert_eq!(differ.len(), 1, "{:#?}", report.diagnostics);
    assert_eq!(
        differ[0].message,
        "the run's scene changes differ from the replay's: at the end of frame 90 the run made a load of \"hud\", where the replay lists a load of \"hud\" at frame 91"
    );

    let mut more = replay("tour");
    more.scenes
        .as_mut()
        .unwrap()
        .push(change(260, SceneChangeKind::Switch, "town"));
    let report = open(&fixture()).run_headless(None, Some(more)).unwrap();
    let differ: Vec<_> = report
        .diagnostics
        .iter()
        .filter(|d| d.code == codes::SCENE_CHANGES_DIFFER)
        .collect();
    assert_eq!(differ.len(), 1, "{:#?}", report.diagnostics);
    assert_eq!(
        differ[0].message,
        "the run's scene changes differ from the replay's: the replay lists a switch to \"town\" at the end of frame 260, which the run did not make"
    );

    // A replay without a list checks nothing.
    let mut none = replay("tour");
    none.scenes = None;
    assert!(
        open(&fixture())
            .run_headless(None, Some(none))
            .unwrap()
            .ok()
    );
}

#[test]
fn a_restart_goes_back_to_the_scene_the_run_began_in_with_everything_reset() {
    let doc = replay("restart");
    let mut app = open(&fixture());
    let report = app.run_headless(Some(101), Some(doc.clone())).unwrap();
    assert!(report.ok(), "{:#?}", report.diagnostics);
    assert_eq!(app.restarts(), 1);
    assert_eq!(app.scenes().main(), "title");
    assert_eq!(app.scenes().loaded(), ["title"]);
    assert!(app.world().entity("banner").is_err());
    // A fresh title: the player the scene file places.
    assert!(position(&app, "player").distance(glam::Vec3::new(0.0, 1.0, 0.0)) < 0.1);
    let rest = app.run_headless(None, None).unwrap();
    assert!(rest.ok(), "{:#?}", rest.diagnostics);
    assert_eq!(app.scenes().log(), doc.scenes.unwrap().as_slice());
    assert_eq!(app.scenes().main(), "town");
}

#[test]
fn the_fixture_passes_verify() {
    let dir = fixture();
    let config = AppConfig::load(&dir).unwrap();
    let options = VerifyOptions {
        boot_frames: 30,
        warmup_frames: 5,
        ..VerifyOptions::default()
    };
    let report = re_app::verify::run_project(&dir, &config, &options, &[]);
    assert!(
        report.ok,
        "{}",
        serde_json::to_string_pretty(&report).unwrap()
    );
    let runs = &report.stage("replays").unwrap().data["runs"];
    assert_eq!(runs.as_array().unwrap().len(), 2);
    assert!(
        runs.as_array()
            .unwrap()
            .iter()
            .all(|r| r["deterministic"] == true)
    );
}

#[test]
fn edit_mode_opens_another_scene_saves_it_and_restarts_into_it() {
    let dir = copy();
    let mut app = open(dir.path());
    assert_eq!(app.scene_path(), "scenes/title.scene.json");
    let problems = app.open_scene("scenes/town.scene.json").unwrap();
    assert!(problems.is_empty(), "{problems:#?}");
    assert_eq!(app.scene_path(), "scenes/town.scene.json");
    assert_eq!(app.scenes().main(), "town");
    assert!(app.world().entity("crate_a").is_ok());
    assert!(app.world().entity("director").is_err());

    app.world_mut()
        .set(
            "crate_a",
            "Transform",
            "translation",
            Value::Vec3(glam::Vec3::new(1.0, 9.0, 1.0)),
        )
        .unwrap();
    app.save_scene().unwrap();
    let town = std::fs::read_to_string(dir.path().join("scenes/town.scene.json")).unwrap();
    assert!(town.contains("9.0"), "{town}");
    let title = std::fs::read_to_string(dir.path().join("scenes/title.scene.json")).unwrap();
    assert_eq!(
        title,
        std::fs::read_to_string(fixture().join("scenes/title.scene.json")).unwrap()
    );

    // The opened scene is the one a restart goes back to, as saved.
    app.restart();
    assert_eq!(app.scenes().main(), "town");
    assert!(position(&app, "crate_a").distance(glam::Vec3::new(1.0, 9.0, 1.0)) < 1e-4);

    // A scene that does not load changes nothing.
    std::fs::write(dir.path().join("scenes/broken.scene.json"), "{").unwrap();
    let err = app.open_scene("scenes/broken.scene.json").unwrap_err();
    assert_eq!(err.code, codes::SCENE_LOAD_FAILED);
    assert_eq!(app.scene_path(), "scenes/town.scene.json");
    assert!(app.world().entity("crate_a").is_ok());
}

#[test]
fn a_play_session_restarts_from_its_start_world() {
    let mut app = open(&fixture());
    app.world_mut()
        .set(
            "player",
            "Transform",
            "translation",
            Value::Vec3(glam::Vec3::new(2.0, 1.0, 2.0)),
        )
        .unwrap();
    let start = re_app::scenes::fork_world(app.world());
    app.set_start_world(start);
    app.world_mut()
        .set(
            "player",
            "Transform",
            "translation",
            Value::Vec3(glam::Vec3::new(7.0, 1.0, 7.0)),
        )
        .unwrap();
    app.restart();
    // The start world's player, not the scene file's or the edited one.
    assert!(position(&app, "player").distance(glam::Vec3::new(2.0, 1.0, 2.0)) < 1e-4);
    assert_eq!(app.restarts(), 1);
    assert_eq!(
        app.scenes().log(),
        [change(0, SceneChangeKind::Restart, "title")]
    );
}

#[test]
fn play_run_plays_a_copy_with_the_same_scenes() {
    let mut app = open(&fixture());
    let doc = replay("tour");
    let (report, _, _) = app
        .play_copy(doc.frames, Some(doc.clone()), &[], false)
        .unwrap();
    assert!(report.ok(), "{:#?}", report.diagnostics);
    assert_eq!(Some(report.scenes), doc.scenes);
    // Nothing reached the live app.
    assert_eq!(app.scenes().main(), "title");
    assert!(app.scenes().log().is_empty());
    assert!(app.world().entity("floor").is_ok());
}

#[test]
fn verify_checks_the_listed_scenes_and_the_scenes_replays_name() {
    let dir = copy();
    let config = AppConfig::load(dir.path()).unwrap();
    let schema = |dir: &Path, config: &AppConfig| {
        re_app::verify::content_stages(dir, config, &[])
            .into_iter()
            .find(|s| s.name == "schema")
            .unwrap()
    };
    assert!(schema(dir.path(), &config).ok);

    // A replay naming a scene the project does not have.
    let mut doc = replay("tour");
    doc.scenes.as_mut().unwrap()[0].switch = Some("harbour".into());
    std::fs::write(
        dir.path().join("tests/replays/tour.replay.json"),
        serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();
    let report = schema(dir.path(), &config);
    assert!(!report.ok);
    assert_eq!(report.diagnostics[0].code, codes::CONTENT_INVALID);
    assert!(
        report.diagnostics[0]
            .message
            .contains("scene change 1 (frame 30) names scene \"harbour\""),
        "{:#?}",
        report.diagnostics
    );

    // Two changes in one entry, and a change past the end.
    doc.scenes.as_mut().unwrap()[0].load = Some("hud".into());
    std::fs::write(
        dir.path().join("tests/replays/tour.replay.json"),
        serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();
    let report = schema(dir.path(), &config);
    assert!(
        report.diagnostics[0].message.contains("exactly one of"),
        "{:#?}",
        report.diagnostics
    );
    let mut doc = replay("tour");
    doc.scenes.as_mut().unwrap()[3].frame = 300;
    std::fs::write(
        dir.path().join("tests/replays/tour.replay.json"),
        serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();
    let report = schema(dir.path(), &config);
    assert!(
        report.diagnostics[0]
            .message
            .contains("past the replay's last frame (299)"),
        "{:#?}",
        report.diagnostics
    );
    std::fs::copy(
        fixture().join("tests/replays/tour.replay.json"),
        dir.path().join("tests/replays/tour.replay.json"),
    )
    .unwrap();

    // A listed scene that is missing, and one outside scenes/ that does not
    // load.
    let toml = std::fs::read_to_string(dir.path().join("real.toml")).unwrap();
    let toml = toml.replace(
        "hud = \"scenes/hud.scene.json\"",
        "hud = \"scenes/hud.scene.json\"\nharbour = \"levels/harbour.scene.json\"\ndocks = \"levels/docks.scene.json\"",
    );
    std::fs::write(dir.path().join("real.toml"), toml).unwrap();
    std::fs::create_dir_all(dir.path().join("levels")).unwrap();
    std::fs::write(
        dir.path().join("levels/docks.scene.json"),
        r#"{ "format": "real-engine/scene", "version": 1, "entities": [ { "id": "a", "components": { "Colider": {} } } ] }"#,
    )
    .unwrap();
    let config = AppConfig::load(dir.path()).unwrap();
    let report = schema(dir.path(), &config);
    let messages: Vec<&str> = report
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "levels/docks.scene.json does not load",
            "the scene \"harbour\" that [scenes] lists in real.toml is missing: levels/harbour.scene.json",
        ]
    );
}
