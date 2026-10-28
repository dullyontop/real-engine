//! Procedural sources in the runtime: builder scripts and graph files fill the
//! GraphStore, terrain gets its mesh and heightfield collider, characters stand
//! on it and are re-grounded when it regenerates, released meshes leave the GPU,
//! and changed builders regenerate.

use std::path::Path;

use glam::Vec3;
use re_app::{App, AppConfig};
use re_command::{AllowAll, Origin};
use re_reflect::Value;
use serde_json::json;

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 30.0, 40.0] } } },
    { "id": "hero", "name": "Hero", "components": {
        "CharacterController": { "height": 1.8, "radius": 0.4 },
        "Transform": { "translation": [3.0, 25.0, 2.0] } } },
    { "id": "island", "name": "Island", "components": {
        "GraphSource": { "builder": "scripts/tools/island_graph.luau" },
        "Procedural": { "generator": "graph", "seed": 1 },
        "Transform": {} } }
  ]
}
"#;

const BUILDER: &str = r#"local g = proc.graph("island")
local h = g:node("terrain.heightfield", { size = 33, cell = 2.0, height = 12.0, scale = 0.03 }, nil, "h")
local m = g:node("terrain.island_mask", { size = 17 }, nil, "mask")
local land = g:node("field.combine", { op = "mul" }, { a = h, b = m }, "land")
g:node("terrain.mesh", { collider = true }, { field = land }, "ground")
return g
"#;

const TERRAIN: &str = "island/ground/terrain";

fn project(builder: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    std::fs::create_dir_all(p.join("scenes")).unwrap();
    std::fs::create_dir_all(p.join("scripts/tools")).unwrap();
    std::fs::write(
        p.join("real.toml"),
        "[project]\nname = \"proc\"\n\n[render]\nwidth = 96\nheight = 64\n",
    )
    .unwrap();
    std::fs::write(p.join("scenes/main.scene.json"), SCENE).unwrap();
    std::fs::write(p.join("scripts/tools/island_graph.luau"), builder).unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn asset(app: &App, id: &str) -> String {
    match app.world().get(id, "MeshRef", "asset").unwrap() {
        Value::Str(s) => s,
        other => panic!("{other:?}"),
    }
}

fn position(app: &App, id: &str) -> Vec3 {
    match app.world().get(id, "Transform", "translation").unwrap() {
        Value::Vec3(v) => v,
        other => panic!("{other:?}"),
    }
}

/// The ground height under (x, z), from the physics world.
fn ground_under(app: &App, p: Vec3) -> f32 {
    let filter = re_physics::QueryFilter {
        exclude: Some(re_scene::entity_id(app.world().entity("hero").unwrap())),
        ..re_physics::QueryFilter::default()
    };
    app.physics()
        .raycast(Vec3::new(p.x, 200.0, p.z), Vec3::NEG_Y, 400.0, &filter)
        .unwrap()
        .expect("ground under the hero")
        .point
        .y
}

fn regenerate(app: &mut App, seed: i64) {
    let out = app.dispatch(
        "graph.regenerate",
        json!({ "source": "island", "seed": seed }),
        false,
        &AllowAll,
        Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
}

#[test]
fn a_builder_makes_terrain_the_character_stands_on() {
    let dir = project(BUILDER);
    let mut app = open(dir.path());
    assert!(asset(&app, TERRAIN).starts_with("derived:"));
    let report = app.run_headless(Some(150), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let hero = position(&app, "hero");
    let ground = ground_under(&app, hero);
    assert!(
        ground > 0.5,
        "the hero is over the island, ground at {ground}"
    );
    assert!(
        (hero.y - (ground + 0.9)).abs() < 0.25,
        "the hero stands on the terrain: y {} over ground {ground}",
        hero.y
    );
}

#[test]
fn regenerating_keeps_the_character_and_puts_it_on_the_new_ground() {
    let dir = project(BUILDER);
    let mut app = open(dir.path());
    app.run_headless(Some(150), None).unwrap();
    let hero = app.world().entity("hero").unwrap();
    let old = asset(&app, TERRAIN);
    let terrain = app.world().entity(TERRAIN).unwrap();
    regenerate(&mut app, 7);
    assert_ne!(asset(&app, TERRAIN), old);
    assert_eq!(app.world().entity(TERRAIN).unwrap(), terrain);
    app.run_headless(Some(1), None).unwrap();
    assert_eq!(app.world().entity("hero").unwrap(), hero);
    let p = position(&app, "hero");
    let ground = ground_under(&app, p);
    assert!(
        (p.y - (ground + 0.92)).abs() < 0.1,
        "re-grounded right after: y {} over ground {ground}",
        p.y
    );
    let report = app.run_headless(Some(60), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let p = position(&app, "hero");
    assert!(
        (p.y - (ground_under(&app, p) + 0.9)).abs() < 0.25,
        "still on the ground: {p}"
    );
}

#[test]
fn ten_regenerations_keep_the_gpu_mesh_count_bounded() {
    let dir = project(BUILDER);
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    app.run_headless(Some(2), None).unwrap();
    let count = |app: &App| app.graphics().unwrap().renderer().stats().meshes.count;
    let base = count(&app);
    assert!(base >= 1);
    for seed in 2..12 {
        regenerate(&mut app, seed);
        app.run_headless(Some(2), None).unwrap();
        assert_eq!(count(&app), base, "after seed {seed}");
    }
    assert!(app.take_diagnostics().iter().all(|d| !d.is_error()));
}

#[test]
fn a_graph_file_works_like_a_builder() {
    let dir = project(BUILDER);
    let graph = json!({
        "format": "real-engine/graph", "version": 1, "name": "flat", "seed": 1,
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 9, "cell": 4.0, "height": 0.0 } },
            { "id": "ground", "kind": "terrain.mesh", "version": 3, "params": { "collider": true } }
        ],
        "edges": [ { "from": ["h", "field"], "to": ["ground", "field"] } ]
    });
    std::fs::create_dir_all(dir.path().join("graphs")).unwrap();
    std::fs::write(dir.path().join("graphs/flat.graph.json"), graph.to_string()).unwrap();
    let scene = SCENE.replace(
        r#""builder": "scripts/tools/island_graph.luau""#,
        r#""graph": "graphs/flat.graph.json""#,
    );
    std::fs::write(dir.path().join("scenes/main.scene.json"), scene).unwrap();
    let mut app = open(dir.path());
    let report = app.run_headless(Some(150), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let y = position(&app, "hero").y;
    assert!((y - 0.9).abs() < 0.25, "on the flat ground: {y}");
}

#[test]
fn a_saved_and_closed_graph_document_keeps_generating_its_edit() {
    let dir = project(BUILDER);
    let graph = json!({
        "format": "real-engine/graph", "version": 1, "name": "flat", "seed": 1,
        "nodes": [
            { "id": "h", "kind": "terrain.heightfield", "version": 1, "params": { "size": 9, "cell": 4.0, "height": 0.0 } },
            { "id": "ground", "kind": "terrain.mesh", "version": 3, "params": { "collider": true } }
        ],
        "edges": [ { "from": ["h", "field"], "to": ["ground", "field"] } ]
    });
    // A graph file whose name does not end in `.graph.json`.
    let path = "graphs/flat.json";
    std::fs::create_dir_all(dir.path().join("graphs")).unwrap();
    std::fs::write(dir.path().join(path), graph.to_string()).unwrap();
    let scene = SCENE.replace(
        r#""builder": "scripts/tools/island_graph.luau""#,
        r#""graph": "graphs/flat.json""#,
    );
    std::fs::write(dir.path().join("scenes/main.scene.json"), scene).unwrap();
    let mut app = open(dir.path());
    app.world_mut().open_doc(path, graph).unwrap();
    let apply = |app: &mut App, op: serde_json::Value| {
        app.dispatch(
            "scene.apply",
            json!({ "ops": [op] }),
            false,
            &AllowAll,
            Origin::User,
        )
    };
    // The app knows the file is a graph: a breaking edit is refused.
    let bad = apply(
        &mut app,
        json!({ "op": "set_doc", "doc": path, "pointer": "/nodes/0" }),
    );
    assert!(!bad.ok);
    let out = apply(
        &mut app,
        json!({ "op": "set_doc", "doc": path, "pointer": "/nodes/0/params/size", "value": 5 }),
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    app.save_scene().unwrap();
    app.world_mut().close_doc(path);

    let size = |app: &App| {
        let store = app.world().ecs.resource::<re_proc::GraphStore>();
        let current = store.current(app.world(), "island").unwrap().unwrap();
        assert!(matches!(current, re_proc::CurrentGraph::Stored(_)));
        current.node("h").unwrap().params["size"].clone()
    };
    assert_eq!(size(&app), json!(5), "the stored graph is the saved file");
    let island = app.world().entity("island").unwrap();
    let report = app.world_mut().regenerate(island);
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
    assert_eq!(size(&app), json!(5));
}

#[test]
fn a_failing_builder_is_re0523_and_a_fixed_one_regenerates() {
    let dir = project("error(\"no island today\")\n");
    let mut app = open(dir.path());
    let diags = app.take_diagnostics();
    let d = diags
        .iter()
        .find(|d| d.code.to_string() == "RE0523")
        .unwrap_or_else(|| panic!("{diags:?}"));
    assert!(format!("{d:?}").contains("no island today"), "{d:?}");
    assert!(app.world().entity(TERRAIN).is_err());

    std::fs::write(dir.path().join("scripts/tools/island_graph.luau"), BUILDER).unwrap();
    let problems = app.apply_file_changes(&[re_assets::AssetChange::Modified(
        "scripts/tools/island_graph.luau".into(),
    )]);
    assert!(problems.is_empty(), "{problems:?}");
    app.run_headless(Some(1), None).unwrap();
    assert!(asset(&app, TERRAIN).starts_with("derived:"));
}

#[test]
fn play_run_regenerates_on_the_copy_only() {
    let dir = project(BUILDER);
    let mut app = open(dir.path());
    app.run_headless(Some(30), None).unwrap();
    let live = asset(&app, TERRAIN);
    let out = app.dispatch(
        "play.run",
        json!({ "frames": 30, "screenshot": false }),
        false,
        &AllowAll,
        Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(out.data["ok"], json!(true), "{}", out.data);
    assert_eq!(asset(&app, TERRAIN), live);
    let report = app.run_headless(Some(30), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
}

#[test]
fn ten_regenerations_keep_the_shape_library_bounded() {
    let dir = project(BUILDER);
    let mut app = open(dir.path());
    app.run_headless(Some(2), None).unwrap();
    let base = app.shapes_mut().len();
    assert!(base >= 1);
    for seed in 2..12 {
        regenerate(&mut app, seed);
        app.run_headless(Some(2), None).unwrap();
        assert_eq!(app.shapes_mut().len(), base, "after seed {seed}");
    }
}

#[test]
fn pointing_a_source_at_another_builder_loads_that_graph() {
    let dir = project(BUILDER);
    std::fs::write(
        dir.path().join("scripts/tools/flat.luau"),
        r#"local g = proc.graph("flat")
local h = g:node("terrain.heightfield", { size = 9, cell = 4.0, height = 0.0 }, nil, "h")
g:node("terrain.mesh", { collider = true }, { field = h }, "ground")
return g
"#,
    )
    .unwrap();
    let mut app = open(dir.path());
    app.run_headless(Some(30), None).unwrap();
    let out = app.dispatch(
        "entity.set",
        json!({ "id": "island", "component": "GraphSource", "field": "builder", "value": "scripts/tools/flat.luau" }),
        false,
        &AllowAll,
        Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let report = app.run_headless(Some(120), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let y = position(&app, "hero").y;
    assert!((y - 0.9).abs() < 0.25, "on the flat ground now: {y}");
}

#[test]
fn re_grounding_passes_through_what_is_above_the_terrain() {
    let dir = project(BUILDER);
    // A wide roof high over the hero: the ray must not put the hero on it.
    let scene = SCENE.replace(
        r#"    { "id": "island""#,
        r#"    { "id": "roof", "components": {
        "Collider": { "shape": "Box", "half_extents": [10.0, 0.5, 10.0] },
        "Transform": { "translation": [3.0, 80.0, 2.0] } } },
    { "id": "island""#,
    );
    std::fs::write(dir.path().join("scenes/main.scene.json"), scene).unwrap();
    let mut app = open(dir.path());
    app.run_headless(Some(150), None).unwrap();
    regenerate(&mut app, 7);
    app.run_headless(Some(1), None).unwrap();
    let p = position(&app, "hero");
    assert!(p.y < 60.0, "not on the roof: {p}");
}

#[test]
fn terrain_that_appears_late_still_catches_the_character() {
    let dir = project("error(\"no island yet\")\n");
    let mut app = open(dir.path());
    // No ground: the hero falls far below where the island will be.
    app.run_headless(Some(240), None).unwrap();
    let fallen = position(&app, "hero");
    assert!(fallen.y < -20.0, "{fallen}");
    std::fs::write(dir.path().join("scripts/tools/island_graph.luau"), BUILDER).unwrap();
    app.apply_file_changes(&[re_assets::AssetChange::Modified(
        "./scripts/tools/island_graph.luau".into(),
    )]);
    app.run_headless(Some(1), None).unwrap();
    app.take_diagnostics();
    let report = app.run_headless(Some(60), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let p = position(&app, "hero");
    let ground = ground_under(&app, p);
    assert!(
        (p.y - (ground + 0.9)).abs() < 0.25,
        "put on the island that appeared: y {} over ground {ground}",
        p.y
    );
}

/// A project whose `real.toml` ends with `[proc] disk_cache_mb = mb`.
fn project_with_cache_mb(builder: &str, mb: u32) -> tempfile::TempDir {
    let dir = project(builder);
    let toml = dir.path().join("real.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(&toml, format!("{text}\n[proc]\ndisk_cache_mb = {mb}\n")).unwrap();
    dir
}

fn cache_limit(app: &App) -> Option<u64> {
    let store = app.world().ecs.get_resource::<re_proc::GraphStore>();
    store.unwrap().evaluator().disk_cache_limit()
}

#[test]
fn the_proc_cache_keeps_to_disk_cache_mb() {
    // Two fields of 129 × 129 heights a seed: about 130 KB of new entries.
    let builder = BUILDER
        .replace("size = 33,", "size = 129,")
        .replace("size = 17 }", "size = 65 }");
    let dir = project_with_cache_mb(&builder, 1);
    let mut app = open(dir.path());
    assert_eq!(cache_limit(&app), Some(1 << 20));
    let cache = dir.path().join(".real/cache/proc");
    let held = || -> u64 {
        std::fs::read_dir(&cache)
            .unwrap()
            .map(|e| e.unwrap().metadata().unwrap().len())
            .sum()
    };
    let mut most = 0;
    for seed in 2..20 {
        regenerate(&mut app, seed);
        let now = held();
        assert!(now <= 1 << 20, "after seed {seed}: {now} bytes");
        most = most.max(now);
    }
    assert!(
        most > 900 << 10,
        "the cache filled up: {most} bytes at most"
    );
    // A seed whose entries were removed generates again.
    regenerate(&mut app, 2);
    let report = app.run_headless(Some(2), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
}

#[test]
fn a_zero_disk_cache_mb_keeps_no_disk_cache() {
    let dir = project_with_cache_mb(BUILDER, 0);
    let mut app = open(dir.path());
    assert_eq!(cache_limit(&app), None);
    regenerate(&mut app, 2);
    assert!(!dir.path().join(".real/cache/proc").exists());
}
