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
