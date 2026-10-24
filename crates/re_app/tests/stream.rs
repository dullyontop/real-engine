//! World streaming in the runtime (ADR-0042): `[stream]` in `real.toml`, cells
//! loaded around the focus at fixed steps, colliders the character stands on,
//! and the same cells at the same ticks on every run.

use std::path::Path;

use glam::Vec3;
use re_app::{App, AppConfig};
use re_reflect::Value;

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 30.0, 40.0] } } },
    { "id": "hero", "name": "Hero", "components": {
        "CharacterController": { "height": 1.8, "radius": 0.4 },
        "StreamFocus": {},
        "Transform": { "translation": [20.0, 30.0, 20.0] } } },
    { "id": "land", "name": "Land", "components": {
        "GraphSource": { "builder": "scripts/tools/land.luau" },
        "Procedural": { "generator": "graph", "seed": 3 },
        "Streamed": { "bounds": 512.0 },
        "Transform": {} } }
  ]
}
"#;

const BUILDER: &str = r#"local g = proc.graph("land")
local h = g:node("terrain.heightfield", { cell = 4.0, height = 6.0, scale = 0.02 }, nil, "h")
g:node("terrain.mesh", { collider = true }, { field = h }, "ground")
return g
"#;

const CONFIG: &str = "[project]\nname = \"stream\"\n\n[render]\nwidth = 96\nheight = 64\n\n\
[stream]\ncell_size = 64.0\nradius = 64.0\nradius_ahead = 128.0\nunload_margin = 32.0\ndelay_ticks = 2\n";

fn project(config: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("scenes")).unwrap();
    std::fs::create_dir_all(p.join("scripts/tools")).unwrap();
    std::fs::write(p.join("real.toml"), config).unwrap();
    std::fs::write(p.join("scenes/main.scene.json"), SCENE).unwrap();
    std::fs::write(p.join("scripts/tools/land.luau"), BUILDER).unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn position(app: &App, id: &str) -> Vec3 {
    match app.world().get(id, "Transform", "translation").unwrap() {
        Value::Vec3(v) => v,
        other => panic!("{other:?}"),
    }
}

fn ground_under(app: &App, p: Vec3) -> Option<f32> {
    let filter = re_physics::QueryFilter {
        exclude: Some(re_scene::entity_id(app.world().entity("hero").unwrap())),
        ..re_physics::QueryFilter::default()
    };
    app.physics()
        .raycast(Vec3::new(p.x, 200.0, p.z), Vec3::NEG_Y, 400.0, &filter)
        .unwrap()
        .map(|hit| hit.point.y)
}

#[test]
fn streamed_cells_load_around_the_focus_and_carry_the_character() {
    let dir = project(CONFIG);
    let mut app = open(dir.path());
    assert!(app.world().entity("land/c/0_0/ground/terrain").is_err());
    let report = app.run_headless(Some(120), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert!(app.world().entity("land/c/0_0/ground/terrain").is_ok());
    let hero = position(&app, "hero");
    let ground = ground_under(&app, hero).expect("a streamed cell's collider");
    assert!(
        (hero.y - (ground + 0.9)).abs() < 0.25,
        "the hero stands on the streamed terrain: y {} over ground {ground}",
        hero.y
    );
    // Far cells are not loaded.
    assert!(app.world().entity("land/c/7_7/ground/terrain").is_err());
    assert!(ground_under(&app, Vec3::new(460.0, 0.0, 460.0)).is_none());
}

/// The land as level-of-detail terrain: no mesh per cell, collision chunks
/// near the focus.
const CHUNKED: &str = r#"local g = proc.graph("land")
local h = g:node("terrain.heightfield", { cell = 4.0, height = 6.0, scale = 0.02 }, nil, "h")
g:node("terrain.chunks", { max_level = 2 }, { field = h }, "ground")
return g
"#;

#[test]
fn the_character_stands_on_terrain_collision_chunks() {
    let dir = project(&format!("{CONFIG}physics_radius = 100.0\n"));
    std::fs::write(dir.path().join("scripts/tools/land.luau"), CHUNKED).unwrap();
    let mut app = open(dir.path());
    let report = app.run_headless(Some(120), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    // The hero at (20, 20) is in chunk t/0_0 (256 m at 4 m spacing).
    let chunk = app.world().entity("land/t/0_0").expect("a collision chunk");
    let c = app.world().ecs.get::<re_scene::Collider>(chunk).unwrap();
    assert!(re_proc::stream::collider_precision(&c.source).is_some());
    let hero = position(&app, "hero");
    let ground = ground_under(&app, hero).expect("the chunk's collider");
    assert!(
        (hero.y - (ground + 0.9)).abs() < 0.25,
        "the hero stands on the terrain: y {} over ground {ground}",
        hero.y
    );
    // Chunks beyond physics_radius have no collider.
    assert!(app.world().entity("land/t/1_1").is_err());
    assert!(ground_under(&app, Vec3::new(460.0, 0.0, 460.0)).is_none());
}

#[test]
fn cells_unload_when_the_focus_leaves() {
    let dir = project(CONFIG);
    let mut app = open(dir.path());
    app.run_headless(Some(10), None).unwrap();
    assert!(app.world().entity("land/c/0_0/ground/terrain").is_ok());
    // The camera takes over as the focus once the hero is gone.
    app.world_mut()
        .set(
            "camera",
            "Transform",
            "translation",
            Value::Vec3(Vec3::new(400.0, 30.0, 400.0)),
        )
        .unwrap();
    let hero = app.world().entity("hero").unwrap();
    app.world_mut()
        .ecs
        .entity_mut(hero)
        .remove::<re_scene::StreamFocus>();
    let report = app.run_headless(Some(10), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert!(app.world().entity("land/c/0_0/ground/terrain").is_err());
    assert!(app.world().entity("land/c/6_6/ground/terrain").is_ok());
}

#[test]
fn two_runs_stream_the_same_world() {
    let hash = || {
        let dir = project(CONFIG);
        let mut app = open(dir.path());
        app.run_headless(Some(60), None).unwrap();
        app.world().state_hash()
    };
    assert_eq!(hash(), hash());
}

#[test]
fn stream_config_is_checked() {
    let bad =
        AppConfig::parse("[project]\nname = \"x\"\n\n[stream]\ncell_size = -1.0\n").unwrap_err();
    assert_eq!(bad.code.to_string(), "RE4300", "{bad:?}");
    let unknown =
        AppConfig::parse("[project]\nname = \"x\"\n\n[stream]\nradius_behind = 3.0\n").unwrap_err();
    assert_eq!(unknown.code.to_string(), "RE0501", "{unknown:?}");
    let config = AppConfig::parse(CONFIG).unwrap();
    assert_eq!(config.stream.cell_size, 64.0);
    assert_eq!(
        config.stream.memory_mb, 1024,
        "unset keys keep their defaults"
    );
}

#[test]
fn streamed_cells_are_drawn_and_released() {
    let dir = project(CONFIG);
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    let count = |app: &App| app.graphics().unwrap().renderer().stats().meshes.count;
    app.run_headless(Some(1), None).unwrap();
    let empty = count(&app);
    app.run_headless(Some(10), None).unwrap();
    let loaded = count(&app);
    assert!(loaded > empty, "{empty} -> {loaded}");
    let hero = app.world().entity("hero").unwrap();
    app.world_mut()
        .ecs
        .entity_mut(hero)
        .remove::<re_scene::StreamFocus>();
    app.world_mut()
        .set(
            "camera",
            "Transform",
            "translation",
            Value::Vec3(Vec3::new(4000.0, 30.0, 4000.0)),
        )
        .unwrap();
    app.run_headless(Some(10), None).unwrap();
    assert_eq!(count(&app), empty, "unloaded cells leave the GPU");
}

#[test]
fn scripts_see_cells_load_in_key_order_before_the_step() {
    let dir = project(CONFIG);
    let scene = SCENE.replace(
        r#"    { "id": "land","#,
        r#"    { "id": "watcher", "components": { "ScriptRef": { "path": "scripts/watch.luau" } } },
    { "id": "land","#,
    );
    std::fs::write(dir.path().join("scenes/main.scene.json"), scene).unwrap();
    std::fs::write(
        dir.path().join("scripts/watch.luau"),
        r#"local seen = 0
stream.on_unloaded(function(cell) print("unloaded " .. cell.key) end)
stream.on_loaded(function(cell)
	seen += 1
	print(`loaded {cell.key} {cell.coord[1]} {cell.coord[2]} {cell.sources[1]} {stream.is_loaded(cell.key)}`)
end)
local reported = false
time.on_step(function()
	if seen > 0 and not reported then
		reported = true
		print(`step sees {#stream.cells()} cells`)
	end
end)
"#,
    )
    .unwrap();
    let mut app = open(dir.path());
    let report = app.run_headless(Some(20), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let loaded: Vec<&String> = report
        .output
        .iter()
        .filter(|l| l.starts_with("[game] loaded "))
        .collect();
    assert!(
        loaded
            .iter()
            .any(|l| *l == "[game] loaded c/0_0 0 0 land true"),
        "{:?}",
        report.output
    );
    let mut sorted = loaded.clone();
    sorted.sort();
    assert_eq!(loaded, sorted, "key order");
    let step = report
        .output
        .iter()
        .position(|l| l.starts_with("[game] step sees"))
        .expect("the step callback ran");
    assert!(
        step > report
            .output
            .iter()
            .position(|l| l.starts_with("[game] loaded"))
            .unwrap()
    );
    assert_eq!(
        report.output[step],
        format!("[game] step sees {} cells", loaded.len()),
        "{:?}",
        report.output
    );
}

#[test]
fn a_tools_script_loads_a_far_cell_before_the_first_step() {
    let dir = project(CONFIG);
    let mut app = open(dir.path());
    app.exec_script(
        re_script::ContextKind::Tools,
        "probe",
        r#"stream.load("c/6_6")"#,
    )
    .unwrap();
    let report = app.run_headless(Some(10), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert!(app.world().entity("land/c/6_6/ground/terrain").is_ok());
    app.exec_script(
        re_script::ContextKind::Tools,
        "probe",
        r#"stream.release("c/6_6")"#,
    )
    .unwrap();
    app.run_headless(Some(3), None).unwrap();
    assert!(app.world().entity("land/c/6_6/ground/terrain").is_err());
}

#[test]
fn only_a_live_run_applies_late_cells() {
    use re_proc::stream::{Lateness, Streamer};
    let dir = project(CONFIG);
    let mut app = open(dir.path());
    app.run_headless(Some(2), None).unwrap();
    let lateness = |app: &App| app.world().ecs.resource::<Streamer>().lateness();
    assert_eq!(lateness(&app), Lateness::Wait, "the default waits");
    app.set_stream_lateness(Lateness::Late);
    app.run_headless(Some(2), None).unwrap();
    assert_eq!(lateness(&app), Lateness::Late);
    // A play copy is a check: it waits, whatever the live run does.
    let (report, _, _) = app.play_copy(5, None, &[], false).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(lateness(&app), Lateness::Late);
}

/// Seven kilometres out, beyond Jolt's 5 km from the world origin: the
/// floating origin follows the hero, so collision chunks there still load
/// and carry the character.
#[test]
fn the_floating_origin_carries_the_character_far_out() {
    let dir = project(&format!("{CONFIG}physics_radius = 100.0\n"));
    let scene = SCENE
        .replace(
            "\"translation\": [20.0, 30.0, 20.0]",
            "\"translation\": [7020.0, 30.0, 20.0]",
        )
        .replace("\"bounds\": 512.0", "\"bounds\": 0.0");
    std::fs::write(dir.path().join("scenes/main.scene.json"), scene).unwrap();
    std::fs::write(dir.path().join("scripts/tools/land.luau"), CHUNKED).unwrap();
    let mut app = open(dir.path());
    let report = app.run_headless(Some(120), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(app.physics().origin(), Vec3::new(7168.0, 0.0, 0.0));
    let hero = position(&app, "hero");
    assert!((hero.x - 7020.0).abs() < 1.0, "{hero}");
    let ground = ground_under(&app, hero).expect("a collision chunk 7 km out");
    assert!(
        (hero.y - (ground + 0.9)).abs() < 0.25,
        "the hero stands on the terrain: y {} over ground {ground}",
        hero.y
    );
}

/// The render snapshot is relative to the floating origin: far out, the
/// camera and the terrain it sees are drawn near zero.
#[test]
fn far_terrain_is_drawn_relative_to_the_render_origin() {
    let dir = project(&format!("{CONFIG}terrain_view = 512.0\n"));
    let scene = SCENE
        .replace(
            "\"translation\": [20.0, 30.0, 20.0]",
            "\"translation\": [7020.0, 30.0, 20.0]",
        )
        .replace(
            "\"translation\": [0.0, 30.0, 40.0]",
            "\"translation\": [7000.0, 30.0, 40.0]",
        )
        .replace("\"bounds\": 512.0", "\"bounds\": 0.0");
    std::fs::write(dir.path().join("scenes/main.scene.json"), scene).unwrap();
    std::fs::write(dir.path().join("scripts/tools/land.luau"), CHUNKED).unwrap();
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    app.run_headless(Some(60), None).unwrap();
    assert_eq!(app.render_origin(), Vec3::new(7168.0, 0.0, 0.0));
    let snapshot = app.snapshot();
    let eye = snapshot.camera.position();
    assert!(
        (eye - Vec3::new(-168.0, 30.0, 40.0)).length() < 1e-3,
        "{eye}"
    );
    assert!(!snapshot.items.is_empty(), "the terrain is drawn");
    for item in &snapshot.items {
        let at = item.world.w_axis.truncate();
        assert!(at.abs().max_element() < 2048.0, "{at}");
    }
}
