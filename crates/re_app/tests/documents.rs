//! The document commands (ADR-0064): materials and `real.toml` made and
//! edited by commands, each one undo step, each refusing what it cannot
//! make, and each file written back byte for byte when its edits are undone.

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
