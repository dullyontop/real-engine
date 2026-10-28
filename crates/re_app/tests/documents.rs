//! The document commands (ADR-0064): materials and `real.toml` made and
//! edited by commands, each one undo step, each refusing what it cannot
//! make, and each file written back byte for byte when its edits are undone;
//! and a graph file made from a builder source's graph
//! (`proc.convert_builder`), which generates before it is saved.

use std::path::Path;

use re_app::{App, AppConfig};
use re_command::{AllowAll, CommandOutcome, LogKind, Origin};
use serde_json::{Value as Json, json};

const CONFIG: &str = "[project]\nname = \"documents\"\n\n# Simulation rate.\n[sim]\nfixed_hz = 60   # steps per second\n\n[render]\nwidth = 320\nheight = 200\n";

/// A hand-formatted material, which no writer would write.
const ROCK: &str = "{ \"base_color\": [0.5, 0.5, 0.5, 1.0],\n  \"roughness\": 0.7 }\n";

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 1.0, 5.0] } } }
  ]
}
"#;

/// A builder script: a small terrain.
const FLAT: &str = r#"local g = proc.graph("flat")
local h = g:node("terrain.heightfield", { size = 9, cell = 4.0, height = 2.0 }, nil, "h")
g:node("terrain.mesh", {}, { field = h }, "ground")
return g
"#;

/// A scene whose procedural source's graph a builder script makes.
const BUILT: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "island", "name": "Island", "components": {
        "GraphSource": { "builder": "scripts/tools/flat.luau" },
        "Procedural": { "generator": "graph", "seed": 1 },
        "Transform": {} } }
  ]
}
"#;

fn project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    for d in ["scenes", "materials", "assets"] {
        std::fs::create_dir_all(p.join(d)).unwrap();
    }
    std::fs::write(p.join("real.toml"), CONFIG).unwrap();
    std::fs::write(p.join("scenes/main.scene.json"), SCENE).unwrap();
    std::fs::write(p.join("materials/rock.material.json"), ROCK).unwrap();
    // Only its existence is checked.
    std::fs::write(p.join("assets/rock.png"), b"not decoded").unwrap();
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn run(app: &mut App, name: &str, args: Json) -> CommandOutcome {
    app.dispatch(name, args, false, &AllowAll, Origin::User)
}

fn dry(app: &mut App, name: &str, args: Json) -> CommandOutcome {
    app.dispatch(name, args, true, &AllowAll, Origin::User)
}

/// The command succeeded; its result data.
fn ok(outcome: CommandOutcome) -> Json {
    assert!(outcome.ok, "{:?}", outcome.diagnostics);
    outcome.data
}

/// How many transactions were applied: each is an undo step.
fn applied(app: &App) -> usize {
    app.history()
        .log()
        .iter()
        .filter(|e| e.kind == LogKind::Do)
        .count()
}

/// The command was refused with `code`, and nothing changed.
fn refused(app: &App, outcome: CommandOutcome, code: &str, steps: usize) {
    assert!(!outcome.ok, "{:?}", outcome.data);
    let codes: Vec<String> = outcome
        .diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect();
    assert!(codes.iter().any(|c| c == code), "{codes:?}");
    assert_eq!(applied(app), steps);
}

fn read(dir: &Path, path: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(path)).ok()
}

fn undo(app: &mut App) {
    ok(run(app, "undo", json!({})));
}

#[test]
fn material_create_makes_a_material_and_refuses_what_it_cannot_make() {
    let dir = project();
    let mut app = open(dir.path());
    const NEW: &str = "materials/new.material.json";

    // A dry run says what it would make, and makes nothing.
    let data = ok(dry(
        &mut app,
        "material.create",
        json!({ "path": NEW, "set": { "roughness": 0.25 } }),
    ));
    assert_eq!(data["material"]["roughness"], 0.25);
    assert!(app.world().doc(NEW).is_none());

    let data = ok(run(
        &mut app,
        "material.create",
        json!({ "path": NEW, "set": { "roughness": 0.25, "base_color_texture": "assets/rock.png" } }),
    ));
    assert_eq!(data["path"], NEW);
    assert_eq!(applied(&app), 1);
    assert_eq!(app.world().unsaved_docs(), [NEW]);
    app.save_scene().unwrap();
    assert_eq!(
        read(dir.path(), NEW).unwrap(),
        "{\n  \"$schema\": \"../schemas/material.schema.json\",\n  \"roughness\": 0.25,\n  \"base_color_texture\": \"assets/rock.png\"\n}\n"
    );

    // Another from it: its keys, then `set`.
    ok(run(
        &mut app,
        "material.create",
        json!({ "path": "materials/copy.material.json", "from": "materials/rock.material.json", "set": { "metallic": 1.0 } }),
    ));
    let copy = &app
        .world()
        .doc("materials/copy.material.json")
        .unwrap()
        .value;
    assert_eq!(copy["roughness"], 0.7);
    assert_eq!(copy["metallic"], 1.0);

    // What it cannot make, it refuses, and nothing changes.
    let log = applied(&app);
    let cases = [
        (json!({ "path": NEW }), "RE2051"),
        (json!({ "path": "materials/rock.material.json" }), "RE2051"),
        (json!({ "path": "materials/new.json" }), "RE2052"),
        (json!({ "path": "../new.material.json" }), "RE2052"),
        (
            json!({ "path": "materials/bad.material.json", "set": { "roughness": 2.0 } }),
            "RE2008",
        ),
        (
            json!({ "path": "materials/bad.material.json", "set": { "shininess": 2.0 } }),
            "RE2008",
        ),
        (
            json!({ "path": "materials/bad.material.json", "set": { "normal_texture": "assets/none.png" } }),
            "RE2054",
        ),
    ];
    for (args, code) in cases {
        let outcome = run(&mut app, "material.create", args.clone());
        refused(&app, outcome, code, log);
        assert!(
            app.world().doc("materials/bad.material.json").is_none(),
            "{args}"
        );
    }
    assert!(read(dir.path(), "materials/bad.material.json").is_none());
    assert_eq!(
        read(dir.path(), "materials/rock.material.json").unwrap(),
        ROCK
    );
}

#[test]
fn material_edit_is_one_undo_step_and_undo_gives_the_file_back() {
    let dir = project();
    let mut app = open(dir.path());
    const ROCK_PATH: &str = "materials/rock.material.json";

    let data = ok(run(
        &mut app,
        "material.edit",
        json!({ "path": ROCK_PATH, "set": { "roughness": 0.3, "metallic": 1.0 }, "unset": ["base_color"] }),
    ));
    assert_eq!(data["changed"], true);
    assert_eq!(
        data["material"],
        json!({ "roughness": 0.3, "metallic": 1.0 })
    );
    assert_eq!(applied(&app), 1, "one undo step");
    app.save_scene().unwrap();
    assert_eq!(
        read(dir.path(), ROCK_PATH).unwrap(),
        "{\n  \"roughness\": 0.3,\n  \"metallic\": 1.0\n}\n"
    );

    // Setting what is there already changes nothing.
    let data = ok(run(
        &mut app,
        "material.edit",
        json!({ "path": ROCK_PATH, "set": { "roughness": 0.3 } }),
    ));
    assert_eq!(data["changed"], false);
    assert_eq!(applied(&app), 1);

    // Refusals change nothing.
    for (args, code) in [
        (
            json!({ "path": ROCK_PATH, "set": { "roughness": -1.0 } }),
            "RE2008",
        ),
        (
            json!({ "path": ROCK_PATH, "set": { "base_color_texture": "assets/none.png" } }),
            "RE2054",
        ),
        (
            json!({ "path": "materials/none.material.json", "set": { "roughness": 0.1 } }),
            "RE0520",
        ),
        (json!({ "path": "materials/rock.json" }), "RE2052"),
    ] {
        let outcome = run(&mut app, "material.edit", args);
        refused(&app, outcome, code, 1);
    }
    assert!(app.world().doc("materials/none.material.json").is_none());

    // Undo puts the content back, and the save the file's own text.
    undo(&mut app);
    app.save_scene().unwrap();
    assert_eq!(read(dir.path(), ROCK_PATH).unwrap(), ROCK);
}

#[test]
fn config_set_keeps_real_toml_as_written_and_undo_gives_it_back() {
    let dir = project();
    let mut app = open(dir.path());

    let data = ok(run(
        &mut app,
        "config.get",
        json!({ "key": "sim.fixed_hz" }),
    ));
    assert_eq!(data["value"], 60);
    let data = ok(run(
        &mut app,
        "config.get",
        json!({ "key": "render.vsync" }),
    ));
    assert_eq!(data["set"], false);

    ok(run(
        &mut app,
        "config.set",
        json!({ "key": "sim.fixed_hz", "value": 30 }),
    ));
    ok(run(
        &mut app,
        "config.set",
        json!({ "key": "proc.luau_budget", "value": 1000 }),
    ));
    assert_eq!(applied(&app), 2);
    let data = ok(run(
        &mut app,
        "config.get",
        json!({ "key": "sim.fixed_hz" }),
    ));
    assert_eq!(data["value"], 30, "read from the open document");
    app.save_scene().unwrap();
    assert_eq!(
        read(dir.path(), "real.toml").unwrap(),
        CONFIG.replace("fixed_hz = 60 ", "fixed_hz = 30 ") + "\n[proc]\nluau_budget = 1000\n"
    );

    // What real.toml cannot hold, or the App would refuse at start, is
    // refused.
    for (args, code) in [
        (json!({ "key": "sim.fixed_hz", "value": "fast" }), "RE2008"),
        (json!({ "key": "sim.speed", "value": 2 }), "RE2008"),
        (json!({ "key": "render.width", "value": null }), "RE2057"),
        (json!({ "key": "render..width", "value": 1 }), "RE2057"),
        (json!({ "key": "sim.fixed_hz.x", "value": 1 }), "RE2057"),
    ] {
        let outcome = run(&mut app, "config.set", args);
        refused(&app, outcome, code, 2);
    }

    // Removing a key, then undoing everything: the file's own bytes.
    ok(run(
        &mut app,
        "config.set",
        json!({ "key": "render.height" }),
    ));
    app.save_scene().unwrap();
    assert!(!read(dir.path(), "real.toml").unwrap().contains("height"));
    for _ in 0..3 {
        undo(&mut app);
    }
    app.save_scene().unwrap();
    assert_eq!(read(dir.path(), "real.toml").unwrap(), CONFIG);
}

/// The hash of the fragment the island's graph made at its last generation.
fn fragment(app: &App) -> String {
    app.world()
        .ecs
        .resource::<re_proc::GraphStore>()
        .report("island")
        .and_then(|r| r.fragment_hash)
        .expect("a fragment hash")
}

#[test]
fn a_builder_source_converted_to_a_graph_file_generates_the_same_before_the_save() {
    let dir = project();
    let p = dir.path();
    std::fs::create_dir_all(p.join("scripts/tools")).unwrap();
    std::fs::write(p.join("scripts/tools/flat.luau"), FLAT).unwrap();
    std::fs::write(p.join("scenes/main.scene.json"), BUILT).unwrap();
    let mut app = open(p);
    let built = fragment(&app);
    app.take_diagnostics();

    let data = ok(run(
        &mut app,
        "proc.convert_builder",
        json!({ "source": "island" }),
    ));
    assert_eq!(data["graph"], "graphs/island.graph.json");
    assert_eq!(applied(&app), 1);
    // The graph file is not written yet: the source reads its document.
    assert!(read(p, "graphs/island.graph.json").is_none());
    assert_eq!(app.take_diagnostics(), []);
    ok(run(
        &mut app,
        "graph.regenerate",
        json!({ "source": "island" }),
    ));
    assert_eq!(fragment(&app), built);

    // Undo gives the source its builder again.
    undo(&mut app);
    assert_eq!(app.take_diagnostics(), []);
    assert!(
        app.world()
            .doc("graphs/island.graph.json")
            .unwrap()
            .is_absent()
    );
    ok(run(
        &mut app,
        "graph.regenerate",
        json!({ "source": "island" }),
    ));
    assert_eq!(fragment(&app), built);
}
