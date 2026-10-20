//! The island demo (`examples/island`) in the runtime: its regeneration replay
//! (collect a gem, press G, keep walking) keeps the hero's `Entity`, puts it
//! on the new ground, keeps the collected gem collected and the game script
//! working.

use std::path::{Path, PathBuf};

use re_app::{App, AppConfig};
use re_reflect::Value;

fn island() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/island")
}

fn replay(name: &str) -> re_types::ReplayDoc {
    let path = island().join(format!("tests/replays/{name}.replay.json"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn hud(app: &App) -> String {
    match app.world().get("game", "HudText", "text").unwrap() {
        Value::Str(s) => s,
        other => panic!("{other:?}"),
    }
}

#[test]
fn regenerating_the_island_keeps_the_game_going() {
    let project = island();
    let mut app = App::open(&project, AppConfig::load(&project).unwrap()).unwrap();
    let doc = replay("regenerate");
    let hero = app.world().entity("hero").unwrap();
    let before = app
        .world()
        .get("island/ground/terrain", "MeshRef", "asset")
        .unwrap();

    // Up to just before G: one gem collected.
    let report = app.run_headless(Some(320), Some(doc)).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert!(hud(&app).starts_with("Gems: 1 / "), "{}", hud(&app));
    let collected: Vec<String> = app
        .world()
        .suppressions()
        .iter()
        .map(str::to_string)
        .collect();
    assert_eq!(collected.len(), 1, "{collected:?}");

    // G regenerates; the rest of the replay walks on the new island.
    let report = app.run_headless(Some(160), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let after = app
        .world()
        .get("island/ground/terrain", "MeshRef", "asset")
        .unwrap();
    assert_ne!(before, after, "the terrain was regenerated");
    assert_eq!(app.world().entity("hero").unwrap(), hero);
    let state = app
        .physics()
        .character(re_scene::entity_id(hero))
        .expect("the hero is still a character");
    assert_eq!(state.ground, re_types::GroundState::OnGround, "{state:?}");
    for id in &collected {
        assert!(app.world().entity(id).is_err(), "{id} came back");
    }
    // The game script still runs: it counted the new island's gems.
    assert_eq!(hud(&app), format!("Gems: 1 / {}", 1 + gems_left(&mut app)));

    // Another G (seed 3 makes a different number of gems): counted again.
    let before = gems_left(&mut app);
    let mut press = re_types::ReplayDoc::new(1, 4);
    press.input = vec![re_types::ReplayInput {
        frame: 0,
        action: "regenerate".into(),
        down: Some(true),
        axis: None,
    }];
    app.run_headless(None, Some(press)).unwrap();
    let report = app.run_headless(Some(60), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_ne!(
        gems_left(&mut app),
        before,
        "a new island with another count"
    );
    assert_eq!(hud(&app), format!("Gems: 1 / {}", 1 + gems_left(&mut app)));
}

/// The gems still on the island.
fn gems_left(app: &mut App) -> usize {
    let world = app.world_mut();
    let ids: Vec<String> = world
        .ecs
        .iter_entities()
        .filter_map(|e| world.id_of(e.id()).map(str::to_string))
        .filter(|id| id.ends_with("/gem"))
        .collect();
    ids.len()
}

#[test]
fn the_hero_pushes_a_crate() {
    let project = island();
    let mut app = App::open(&project, AppConfig::load(&project).unwrap()).unwrap();
    app.run_headless(Some(150), None).unwrap();
    let world = app.world_mut();
    let crate_id = world
        .ecs
        .iter_entities()
        .filter_map(|e| world.id_of(e.id()).map(str::to_string))
        .find(|id| id.ends_with("/crate"))
        .expect("the island has crates");
    let at = |app: &mut App, id: &str| {
        let world = app.world_mut();
        let e = world.entity(id).unwrap();
        glam::Vec3::from(
            world
                .ecs
                .get::<re_scene::GlobalTransform>(e)
                .unwrap()
                .0
                .translation,
        )
    };
    let start = at(&mut app, &crate_id);
    // Put the hero 3 m in front of the crate (towards -Z is forward at the
    // camera's starting yaw) and walk forward into it.
    app.world_mut()
        .set(
            "hero",
            "Transform",
            "translation",
            Value::Vec3(start + glam::Vec3::new(0.0, 1.5, 3.0)),
        )
        .unwrap();
    let mut walk = re_types::ReplayDoc::new(1, 120);
    walk.input = vec![re_types::ReplayInput {
        frame: 0,
        action: "move".into(),
        down: None,
        axis: Some([0.0, 1.0]),
    }];
    let report = app.run_headless(None, Some(walk)).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let moved = at(&mut app, &crate_id) - start;
    assert!(
        moved.z < -0.5,
        "the crate was pushed forward: moved {moved}"
    );
}

#[test]
fn collecting_every_gem_wins_and_r_restarts() {
    let project = island();
    let mut app = App::open(&project, AppConfig::load(&project).unwrap()).unwrap();
    let report = app.run_headless(Some(150), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let total: usize = hud(&app)
        .rsplit(' ')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{}", hud(&app)));
    assert!(total >= 5, "{}", hud(&app));

    // Walk into each gem (a teleport is enough for a sensor).
    loop {
        let world = app.world_mut();
        let gems: Vec<glam::Vec3> = {
            let ids: Vec<String> = world
                .ecs
                .iter_entities()
                .filter_map(|e| world.id_of(e.id()).map(str::to_string))
                .filter(|id| id.ends_with("/gem"))
                .collect();
            ids.iter()
                .filter_map(|id| world.entity(id).ok())
                .filter_map(|e| world.ecs.get::<re_scene::GlobalTransform>(e))
                .map(|g| glam::Vec3::from(g.0.translation))
                .collect()
        };
        let Some(at) = gems.first() else { break };
        world
            .set("hero", "Transform", "translation", Value::Vec3(*at))
            .unwrap();
        let report = app.run_headless(Some(6), None).unwrap();
        assert!(report.ok(), "{:?}", report.diagnostics);
    }
    assert_eq!(
        hud(&app),
        format!("All {total} gems collected! Press R to play again")
    );

    // R reloads the entry scene: every gem is back.
    let mut press = re_types::ReplayDoc::new(1, 4);
    press.input = vec![re_types::ReplayInput {
        frame: 0,
        action: "restart".into(),
        down: Some(true),
        axis: None,
    }];
    app.run_headless(None, Some(press)).unwrap();
    app.run_headless(Some(30), None).unwrap();
    assert_eq!(app.restarts(), 1);
    assert!(app.world().suppressions().is_empty());
    assert_eq!(hud(&app), format!("Gems: 0 / {total}"));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn an_edit_saved_to_the_scene_keeps_the_island_loading() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("island");
    copy_dir(&island(), &project);
    let config = AppConfig {
        persist_edits: true,
        ..AppConfig::load(&project).unwrap()
    };
    let mut app = App::open(&project, config).unwrap();
    let out = app.dispatch(
        "entity.spawn",
        serde_json::json!({ "id": "rock" }),
        false,
        &re_command::AllowAll,
        re_command::Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    // The gems' Collectible is declared only for generated entities: the
    // saved scene must still declare it.
    let scene = std::fs::read_to_string(project.join("scenes/main.scene.json")).unwrap();
    assert!(scene.contains("\"Collectible\""), "{scene}");

    let mut again = App::open(&project, AppConfig::load(&project).unwrap()).unwrap();
    let problems = again.take_diagnostics();
    assert!(problems.iter().all(|d| !d.is_error()), "{problems:?}");
    assert!(again.world().entity("rock").is_ok());
    assert!(gems_left(&mut again) > 0);
}

/// Records `tests/replays/full_game.replay.json`: walks into a crate and
/// pushes it, collects every gem (steering towards the nearest one each
/// frame), waits on the win message and presses R. Run with
/// `cargo test -p re_app --test island record_full_game -- --ignored` after
/// a change to the island; the replay is played back by `real verify` and by
/// [`the_full_game_replay_pushes_wins_and_restarts`]. The golden's name holds
/// the win frame: after recording, run `real verify --update-goldens` on each
/// backend (Vulkan, and `RE_BACKENDS=gl`) and delete the old `full_game_f*.png`.
#[test]
#[ignore = "writes the replay file; run by hand"]
fn record_full_game() {
    let project = island();
    let mut app = App::open(&project, AppConfig::load(&project).unwrap()).unwrap();
    let mut rec = Recorder::default();

    // Land first, as the other replays do.
    rec.run(&mut app, 150, [0.0, 0.0]);

    // A crate: walk at it and keep pushing.
    let crate_id = nearest(&mut app, "/crate")
        .expect("the island has crates")
        .0;
    let crate_start = pos(&mut app, &crate_id);
    let mut pushing = 0;
    while pushing < 90 {
        let to = pos(&mut app, &crate_id) - pos(&mut app, "hero");
        if glam::Vec2::new(to.x, to.z).length() < 2.0 || pushing > 0 {
            pushing += 10;
        }
        rec.run(&mut app, 10, towards(to));
        assert!(rec.frame < 1200, "never reached the crate");
    }
    let moved = pos(&mut app, &crate_id) - crate_start;
    assert!(
        glam::Vec2::new(moved.x, moved.z).length() > 0.5,
        "the crate moved {moved}"
    );

    // Every gem, nearest first; a sideways step when stuck.
    let mut last = (pos(&mut app, "hero"), rec.frame);
    while let Some((_, at)) = nearest(&mut app, "/gem") {
        let here = pos(&mut app, "hero");
        let to = at - here;
        let mut dir = towards(to);
        // Steer every 10 frames, every frame when close.
        let frames = if glam::Vec2::new(to.x, to.z).length() > 4.0 {
            10
        } else {
            1
        };
        if rec.frame - last.1 >= 90 {
            if (here - last.0).length() < 2.0 {
                rec.run(&mut app, 40, [dir[1], -dir[0]]);
            }
            last = (pos(&mut app, "hero"), rec.frame);
            dir = towards(at - pos(&mut app, "hero"));
        }
        rec.run(&mut app, frames, dir);
        assert!(rec.frame < 12_000, "stuck at {here}; {}", hud(&app));
    }
    rec.run(&mut app, 60, [0.0, 0.0]);
    assert!(hud(&app).starts_with("All "), "{}", hud(&app));
    let win_frame = rec.frame - 1;

    // R: a new game.
    rec.press(&mut app, "restart");
    rec.run(&mut app, 90, [0.0, 0.0]);
    assert_eq!(app.restarts(), 1);
    assert!(hud(&app).starts_with("Gems: 0 / "), "{}", hud(&app));

    let mut doc = re_types::ReplayDoc::new(1, rec.frame);
    doc.input = rec.input;
    doc.checks = vec![re_types::ReplayCheck {
        frame: win_frame,
        screenshot: Some(format!(
            "tests/goldens/{{backend}}/full_game_f{win_frame}.png"
        )),
    }];
    // One line per entry, like the hand-written replays.
    let input: Vec<String> = doc
        .input
        .iter()
        .map(|i| match (i.axis, i.down) {
            (Some([x, y]), _) => format!(
                "    {{ \"frame\": {}, \"action\": \"{}\", \"axis\": [{x:?}, {y:?}] }}",
                i.frame, i.action
            ),
            (_, Some(down)) => format!(
                "    {{ \"frame\": {}, \"action\": \"{}\", \"down\": {down} }}",
                i.frame, i.action
            ),
            _ => unreachable!(),
        })
        .collect();
    let text = format!(
        "{{\n  \"version\": 1,\n  \"seed\": 1,\n  \"frames\": {},\n  \"input\": [\n{}\n  ],\n  \"checks\": [\n    {{ \"frame\": {win_frame}, \"screenshot\": \"{}\" }}\n  ]\n}}\n",
        doc.frames,
        input.join(",\n"),
        doc.checks[0].screenshot.as_deref().unwrap(),
    );
    let back: re_types::ReplayDoc = serde_json::from_str(&text).unwrap();
    assert_eq!(
        serde_json::to_value(&back).unwrap(),
        serde_json::to_value(&doc).unwrap()
    );
    std::fs::write(project.join("tests/replays/full_game.replay.json"), text).unwrap();
    println!(
        "full_game: {} frames, win at {win_frame}, hash {}",
        rec.frame,
        app.world().state_hash().to_hex()
    );
}

/// Input recorded frame by frame: each frame is played as a one-frame replay
/// carrying the current input, and every change is kept for the file. Each
/// one-frame replay starts with every button up, so the frame after a press
/// has no release edge here while the file has one: equivalent for scripts
/// that use `on_pressed` and `down` (all the island's), not for `on_released`.
#[derive(Default)]
struct Recorder {
    frame: u64,
    axis: [f32; 2],
    input: Vec<re_types::ReplayInput>,
}

impl Recorder {
    fn run(&mut self, app: &mut App, frames: u64, axis: [f32; 2]) {
        let axis = axis.map(|v| (v * 10.0).round() / 10.0);
        if axis != self.axis {
            self.axis = axis;
            self.input.push(re_types::ReplayInput {
                frame: self.frame,
                action: "move".into(),
                down: None,
                axis: Some(axis),
            });
        }
        for _ in 0..frames {
            self.step(app, None);
        }
    }

    /// Holds `button` down for one frame.
    fn press(&mut self, app: &mut App, button: &str) {
        for down in [true, false] {
            self.input.push(re_types::ReplayInput {
                frame: self.frame,
                action: button.into(),
                down: Some(down),
                axis: None,
            });
            self.step(app, Some((button, down)));
        }
    }

    fn step(&mut self, app: &mut App, button: Option<(&str, bool)>) {
        let mut one = re_types::ReplayDoc::new(1, 1);
        one.input.push(re_types::ReplayInput {
            frame: 0,
            action: "move".into(),
            down: None,
            axis: Some(self.axis),
        });
        if let Some((name, down)) = button {
            one.input.push(re_types::ReplayInput {
                frame: 0,
                action: name.into(),
                down: Some(down),
                axis: None,
            });
        }
        let report = app.run_headless(None, Some(one)).unwrap();
        assert!(report.ok(), "{:?}", report.diagnostics);
        self.frame += 1;
    }
}

/// The move stick for walking along `to` (the camera starts looking down -Z:
/// stick x is +X, stick y is -Z).
fn towards(to: glam::Vec3) -> [f32; 2] {
    let d = glam::Vec2::new(to.x, to.z).normalize_or_zero();
    [d.x, -d.y]
}

fn pos(app: &mut App, id: &str) -> glam::Vec3 {
    let world = app.world_mut();
    let e = world.entity(id).unwrap();
    glam::Vec3::from(
        world
            .ecs
            .get::<re_scene::GlobalTransform>(e)
            .unwrap()
            .0
            .translation,
    )
}

/// The entity whose id ends with `suffix` nearest the hero, and where it is.
fn nearest(app: &mut App, suffix: &str) -> Option<(String, glam::Vec3)> {
    let hero = pos(app, "hero");
    let world = app.world_mut();
    let ids: Vec<String> = world
        .ecs
        .iter_entities()
        .filter_map(|e| world.id_of(e.id()).map(str::to_string))
        .filter(|id| id.ends_with(suffix))
        .collect();
    ids.into_iter()
        .map(|id| {
            let at = pos(app, &id);
            (id, at)
        })
        .min_by(|a, b| {
            let da = glam::Vec2::new(a.1.x - hero.x, a.1.z - hero.z).length();
            let db = glam::Vec2::new(b.1.x - hero.x, b.1.z - hero.z).length();
            da.total_cmp(&db)
        })
}

#[test]
#[cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    ignore = "the replay's inputs were recorded on Linux x86_64 and run open-loop; until D2 \
              (ADR-0036, WP18 PR \"d2\") other platforms simulate differently and reach other gems"
)]
fn the_full_game_replay_pushes_wins_and_restarts() {
    let project = island();
    let mut app = App::open(&project, AppConfig::load(&project).unwrap()).unwrap();
    let doc = replay("full_game");
    let win = doc.checks[0].frame;
    let crates: Vec<(String, glam::Vec3)> = {
        app.run_headless(Some(1), None).unwrap();
        let world = app.world_mut();
        let ids: Vec<String> = world
            .ecs
            .iter_entities()
            .filter_map(|e| world.id_of(e.id()).map(str::to_string))
            .filter(|id| id.ends_with("/crate"))
            .collect();
        ids.into_iter()
            .map(|id| {
                let at = pos(&mut app, &id);
                (id, at)
            })
            .collect()
    };
    let mut app = App::open(&project, AppConfig::load(&project).unwrap()).unwrap();
    let report = app.run_headless(Some(win + 1), Some(doc.clone())).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert!(hud(&app).starts_with("All "), "{}", hud(&app));
    let pushed = crates.iter().any(|(id, at)| {
        let moved = pos(&mut app, id) - *at;
        glam::Vec2::new(moved.x, moved.z).length() > 0.5
    });
    assert!(pushed, "a crate was pushed");

    let report = app.run_headless(Some(doc.frames - win - 1), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(app.restarts(), 1);
    assert!(hud(&app).starts_with("Gems: 0 / "), "{}", hud(&app));
}
