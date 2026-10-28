//! Scenes and saves in the running game (ADR-0059), on the scenes fixture:
//! its replays switch, load and unload scenes at the frames their `scenes`
//! lists name, with the same state hash on every run; a replay whose list
//! differs fails with `RE0571`; kept entities stand in for the scene's own;
//! `world.restart()` goes back to the scene the run began in, or to a play
//! session's start world; edit mode opens and saves another scene; and the
//! progress slot migrates from the seed's version 1, is saved and read back
//! at every switch, and reaches the data folder only in a run that keeps
//! data.

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
    for sub in ["", "scenes", "scripts", "tests/replays", "data/seed/saves"] {
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
    let dir = copy();
    let mut app = open(dir.path());
    app.persist_data();
    let doc = replay("tour");
    let (report, _, _) = app
        .play_copy(doc.frames, Some(doc.clone()), &[], false)
        .unwrap();
    assert!(report.ok(), "{:#?}", report.diagnostics);
    assert_eq!(Some(report.scenes), doc.scenes);
    // Nothing reached the live app, nor its data folder: the copy saved its
    // progress in memory.
    assert_eq!(app.scenes().main(), "title");
    assert!(app.scenes().log().is_empty());
    assert!(app.world().entity("floor").is_ok());
    assert!(!dir.path().join(".real/data/saves").exists());
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

#[test]
fn verify_checks_the_seeded_saves() {
    let dir = copy();
    let seed = dir.path().join("data/seed/saves");
    let progress = std::fs::read_to_string(seed.join("progress.json")).unwrap();
    std::fs::write(seed.join("Slot.json"), &progress).unwrap();
    std::fs::write(seed.join("broken.json"), "{").unwrap();
    std::fs::write(
        seed.join("future.json"),
        progress.replace("\"data_version\": 1", "\"data_version\": 3"),
    )
    .unwrap();
    let config = AppConfig::load(dir.path()).unwrap();
    let report = re_app::verify::content_stages(dir.path(), &config, &[])
        .into_iter()
        .find(|s| s.name == "schema")
        .unwrap();
    assert!(
        report
            .diagnostics
            .iter()
            .all(|d| d.code == codes::CONTENT_INVALID)
    );
    let messages: Vec<&str> = report
        .diagnostics
        .iter()
        .map(|d| d.message.as_str())
        .collect();
    assert_eq!(
        messages,
        [
            "data/seed/saves/Slot.json is not a save slot: a seeded slot is data/seed/saves/<slot>.json, with a slot name of 1 to 64 characters from a-z, 0-9, _ and -",
            "data/seed/saves/broken.json is not JSON (EOF while parsing an object at line 1 column 1)",
            "data/seed/saves/future.json holds version 3 data, newer than [saves] version 2 in real.toml; save.read would refuse it",
        ]
    );
    assert_eq!(report.data["seeded_saves"], 4);
}

#[test]
fn the_progress_slot_migrates_from_the_seed_and_is_saved_at_every_switch() {
    // A run that keeps no data starts from the seed: version 1 data, which
    // the game's step migrates to version 2.
    let mut app = open(&fixture());
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(text(&app, "progress"), "coins 3, visited title");
    let mut app = open(&fixture());
    let report = app.run_headless(None, Some(replay("tour"))).unwrap();
    assert!(report.ok(), "{:#?}", report.diagnostics);
    assert_eq!(
        text(&app, "progress"),
        "coins 5, visited title > town > title"
    );
    // A restart keeps the slots, as it keeps the data folder's files: the
    // script that starts again reads what the run saved before it.
    let mut app = open(&fixture());
    let report = app.run_headless(None, Some(replay("restart"))).unwrap();
    assert!(report.ok(), "{:#?}", report.diagnostics);
    assert_eq!(app.restarts(), 1);
    assert_eq!(
        text(&app, "progress"),
        "coins 5, visited title > town > town"
    );
    // Nothing reached the fixture's data folder.
    assert!(!fixture().join(".real/data/saves").exists());
}

#[test]
fn a_run_that_keeps_data_writes_its_slots_and_the_next_run_reads_them() {
    let dir = copy();
    let saves = dir.path().join(".real/data/saves");
    // The data folder starts empty, not from the seed: a new game.
    let mut app = open(dir.path());
    app.persist_data();
    let report = app.run_headless(None, Some(replay("tour"))).unwrap();
    assert!(report.ok(), "{:#?}", report.diagnostics);
    assert_eq!(
        text(&app, "progress"),
        "coins 2, visited title > town > title"
    );
    let file: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(saves.join("progress.json")).unwrap())
            .unwrap();
    assert_eq!(file["format"], "real-engine/save");
    assert_eq!(
        (&file["version"], &file["data_version"]),
        (&1.into(), &2.into())
    );
    assert_eq!(
        file["data"],
        serde_json::json!({ "coins": 2, "visited": ["title", "town", "title"] })
    );
    assert!(file["saved_at"].is_i64());

    // The next run reads the data folder, not the seed.
    let mut next = open(dir.path());
    next.persist_data();
    next.run_headless(Some(1), None).unwrap();
    assert_eq!(
        text(&next, "progress"),
        "coins 2, visited title > town > title"
    );

    // With `mode = "session"`, even a run that keeps data keeps no saves.
    std::fs::remove_dir_all(&saves).unwrap();
    let toml = dir.path().join("real.toml");
    let text_of = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(
        &toml,
        text_of.replace(
            "[saves]\nversion = 2\n",
            "[saves]\nversion = 2\nmode = \"session\"\n",
        ),
    )
    .unwrap();
    let mut session = open(dir.path());
    session.persist_data();
    let report = session.run_headless(None, Some(replay("tour"))).unwrap();
    assert!(report.ok(), "{:#?}", report.diagnostics);
    assert_eq!(
        text(&session, "progress"),
        "coins 5, visited title > town > title"
    );
    assert!(!saves.exists());
}
