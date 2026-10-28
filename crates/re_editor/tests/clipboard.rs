//! Copy, cut, paste, duplicate and delete (ADR-0044's amendment for WP42,
//! §6), on a small project in a temporary directory, headless.
//!
//! The GPU tests need an adapter (lavapipe in CI); run them with
//! `--test-threads=1`.

use std::path::Path;

use egui::{Event, Key, Modifiers};
use glam::{Mat4, Quat, Vec3};
use re_app::{App, AppConfig};
use re_command::AllowAll;
use re_editor::{HeadlessEditor, Selection};
use re_reflect::Value;
use re_scene::GlobalTransform;
use serde_json::{Value as Json, json};

const CONFIG: &str = r#"[project]
name = "fixture"
"#;

/// `crates` is turned a quarter about Y, so its crates' world transforms
/// differ from their local ones. `Follow.target` is an entity reference.
const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "script_components": [
    { "name": "Follow", "fields": [
        { "name": "target", "type": { "kind": "entity" }, "default": null } ] }
  ],
  "entities": [
    { "id": "box", "name": "Box", "components": {
        "MeshRef": { "asset": "builtin:cube" },
        "Transform": { "translation": [0.0, 4.0, 0.0] } } },
    { "id": "camera", "name": "Camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 5.0, 12.0] } } },
    { "id": "crates", "name": "Crates", "components": {
        "Transform": {
          "translation": [1.0, 0.0, 0.0],
          "rotation": [0.0, 0.70710677, 0.0, 0.70710677] } } },
    { "id": "crate_1", "name": "Crate", "parent": "crates", "components": {
        "Transform": { "translation": [2.0, 0.5, 0.0] },
        "Follow": { "target": "crate_2" } } },
    { "id": "crate_2", "name": "Crate", "parent": "crates", "components": {
        "Transform": { "translation": [-2.0, 0.5, 0.0] },
        "Follow": { "target": "box" } } },
    { "id": "lamp", "name": "Lamp", "components": {
        "Light": { "kind": "Point" },
        "Transform": { "translation": [3.0, 1.0, 0.0] } } }
  ]
}
"#;

fn project() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("ec")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    std::fs::create_dir_all(dir.path().join("scenes")).unwrap();
    std::fs::write(dir.path().join("real.toml"), CONFIG).unwrap();
    std::fs::write(dir.path().join("scenes/main.scene.json"), SCENE).unwrap();
    let mut app = open(dir.path());
    app.save_scene().unwrap();
    dir
}

fn open(dir: &Path) -> App {
    let config = AppConfig {
        persist_edits: true,
        ..AppConfig::load(dir).unwrap()
    };
    App::open(dir, config).unwrap()
}

fn input(events: Vec<Event>) -> egui::RawInput {
    egui::RawInput {
        events,
        ..egui::RawInput::default()
    }
}

fn shortcut(key: Key, modifiers: Modifiers) -> egui::RawInput {
    let event = |pressed| Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    };
    input(vec![
        Event::ModifiersChanged(modifiers),
        event(true),
        event(false),
        Event::ModifiersChanged(Modifiers::NONE),
    ])
}

fn frames(ed: &mut HeadlessEditor, app: &mut App, n: usize) {
    for _ in 0..n {
        ed.frame(app, &AllowAll, input(vec![])).unwrap();
    }
}

fn select(ed: &mut HeadlessEditor, app: &mut App, ids: &[&str]) {
    let ids = ids.iter().map(|s| s.to_string());
    ed.editor.set_selection(Selection::from_ids(ids, None));
    frames(ed, app, 1);
}

fn selected(ed: &HeadlessEditor) -> Vec<&str> {
    ed.editor
        .selection()
        .ids()
        .iter()
        .map(String::as_str)
        .collect()
}

fn children(app: &App, id: &str) -> Vec<String> {
    let world = app.world();
    let e = world.entity(id).unwrap();
    re_scene::hierarchy::children(&world.ecs, e)
        .iter()
        .filter_map(|c| world.id_of(*c).map(str::to_string))
        .collect()
}

fn parent(app: &App, id: &str) -> Option<String> {
    let world = app.world();
    let e = world.entity(id).unwrap();
    re_scene::hierarchy::parent(&world.ecs, e).and_then(|p| world.id_of(p).map(str::to_string))
}

fn field(app: &App, id: &str, component: &str, field: &str) -> Value {
    app.world().get(id, component, field).unwrap()
}

fn global(app: &App, id: &str) -> Mat4 {
    let world = app.world();
    let e = world.entity(id).unwrap();
    Mat4::from(world.ecs.get::<GlobalTransform>(e).unwrap().0)
}

fn same(a: Mat4, b: Mat4) -> bool {
    a.abs_diff_eq(b, 1e-4)
}

fn console_has(ed: &HeadlessEditor, code: &str) -> bool {
    ed.editor.console().iter().any(|l| l.text.contains(code))
}

/// Ctrl+C writes the selected subtrees to the clipboard without editing
/// anything; Ctrl+V pastes them after the active entity as one undo step,
/// with fresh ids (the trailing number moved on), references between the
/// pasted entities following them and others kept, and selects the copies.
#[test]
fn copy_and_paste_make_fresh_ids_in_one_undo_step() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);
    select(&mut ed, &mut app, &["crates"]);
    let (hash, log) = (app.state_hash(), app.history().log().len());
    ed.frame(&mut app, &AllowAll, input(vec![Event::Copy]))
        .unwrap();
    let text = ed.copied().expect("Ctrl+C wrote the clipboard").to_string();
    let json: Json = serde_json::from_str(&text).unwrap();
    assert_eq!(json["format"], "real-engine/entities");
    assert_eq!(json["version"], 1);
    let ids: Vec<&str> = json["subtrees"][0]["entities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["crates", "crate_1", "crate_2"]);
    assert_eq!(app.state_hash(), hash, "copying is not an edit");
    assert_eq!(app.history().log().len(), log);

    // Pasted beside the lamp, a root: at the root, keeping the transform.
    select(&mut ed, &mut app, &["lamp"]);
    ed.frame(&mut app, &AllowAll, input(vec![Event::Paste(text)]))
        .unwrap();
    frames(&mut ed, &mut app, 1);
    assert_eq!(selected(&ed), ["crates_2"]);
    assert_eq!(parent(&app, "crates_2"), None);
    assert_eq!(children(&app, "crates_2"), ["crate_3", "crate_4"]);
    assert!(same(global(&app, "crates_2"), global(&app, "crates")));
    assert!(same(global(&app, "crate_3"), global(&app, "crate_1")));
    // crate_1 followed crate_2, so its copy follows crate_2's copy; crate_2
    // followed the box, which was not copied, so its copy does too.
    let target = |id| field(&app, id, "Follow", "target");
    let entity = |id| Value::Entity(Some(app.world().entity(id).unwrap()));
    assert_eq!(target("crate_3"), entity("crate_4"));
    assert_eq!(target("crate_4"), entity("box"));
    let world = app.world();
    assert_eq!(world.name_of(world.entity("crate_3").unwrap()), "Crate");
    assert_eq!(app.history().undo_label(), Some("paste crates"));

    let out = ed.editor.undo(&mut app, &AllowAll);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(app.state_hash(), hash, "the paste was one undo step");
    assert!(!app.history().can_undo());
}

/// "Paste as child" puts the copies under the active entity, and each root
/// keeps its world transform: its local `Transform` is worked out for its
/// new parent. Ctrl+V with a child active pastes after it, under its parent.
#[test]
fn a_pasted_root_keeps_its_world_transform() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);
    select(&mut ed, &mut app, &["crate_1"]);
    let text = ed.editor.copy_selection(app.world()).unwrap();

    select(&mut ed, &mut app, &["box"]);
    let out = ed
        .editor
        .paste(&mut app, &AllowAll, &text, true)
        .expect("a paste was sent");
    assert!(out.ok, "{:?}", out.diagnostics);
    frames(&mut ed, &mut app, 1);
    assert_eq!(selected(&ed), ["crate_3"]);
    assert_eq!(parent(&app, "crate_3").as_deref(), Some("box"));
    assert!(
        same(global(&app, "crate_3"), global(&app, "crate_1")),
        "{} {}",
        global(&app, "crate_3"),
        global(&app, "crate_1")
    );
    // The box is not turned: the copy's local rotation is the crates' turn.
    let Value::Quat(q) = field(&app, "crate_3", "Transform", "rotation") else {
        panic!()
    };
    assert!(
        q.abs_diff_eq(Quat::from_rotation_y(90f32.to_radians()), 1e-5),
        "{q}"
    );
    // A reference to an entity that exists, outside the paste, is kept.
    let crate_2 = app.world().entity("crate_2").unwrap();
    assert_eq!(
        field(&app, "crate_3", "Follow", "target"),
        Value::Entity(Some(crate_2))
    );

    // Ctrl+V with crate_1 active: after it, under the crates.
    select(&mut ed, &mut app, &["crate_1"]);
    ed.frame(&mut app, &AllowAll, input(vec![Event::Paste(text)]))
        .unwrap();
    assert_eq!(children(&app, "crates"), ["crate_1", "crate_4", "crate_2"]);
    let Value::Vec3(t) = field(&app, "crate_4", "Transform", "translation") else {
        panic!()
    };
    assert!(
        t.abs_diff_eq(Vec3::new(2.0, 0.5, 0.0), 1e-6),
        "the same parent: {t}"
    );
}

/// Ctrl+D puts a copy of each selected entity after its original, in one
/// undo step, without touching the clipboard, and selects the copies.
/// Delete removes the selection in one undo step.
#[test]
fn duplicate_and_delete_are_one_undo_step_each() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);
    select(&mut ed, &mut app, &["crate_1", "crate_2"]);
    let hash = app.state_hash();
    ed.frame(&mut app, &AllowAll, shortcut(Key::D, Modifiers::COMMAND))
        .unwrap();
    frames(&mut ed, &mut app, 1);
    assert_eq!(
        children(&app, "crates"),
        ["crate_1", "crate_3", "crate_2", "crate_4"],
        "{:?}",
        ed.editor.console()
    );
    assert_eq!(selected(&ed), ["crate_3", "crate_4"]);
    assert_eq!(
        field(&app, "crate_3", "Transform", "translation"),
        field(&app, "crate_1", "Transform", "translation")
    );
    let crate_4 = app.world().entity("crate_4").unwrap();
    assert_eq!(
        field(&app, "crate_3", "Follow", "target"),
        Value::Entity(Some(crate_4)),
        "both were duplicated, so the copy follows the copy"
    );
    assert_eq!(app.history().undo_label(), Some("duplicate 2 entities"));
    assert!(ed.copied().is_none(), "the clipboard is not touched");

    // Delete: the copies go, in one step.
    ed.frame(&mut app, &AllowAll, shortcut(Key::Delete, Modifiers::NONE))
        .unwrap();
    assert!(app.world().entity("crate_3").is_err() && app.world().entity("crate_4").is_err());
    assert!(ed.editor.selection().is_empty());
    assert_eq!(app.history().undo_label(), Some("delete 2 entities"));
    for _ in 0..2 {
        assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    }
    assert!(app.world().entity("crate_3").is_err());
    assert_eq!(app.state_hash(), hash);
}

/// Ctrl+X copies and deletes; text that is not entities, a component the
/// project does not define, and a reference to an entity that is not there
/// are each reported, and a scene file's text pastes its root entities.
#[test]
fn cut_and_what_the_clipboard_refuses() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);
    select(&mut ed, &mut app, &["lamp"]);
    ed.frame(&mut app, &AllowAll, input(vec![Event::Cut]))
        .unwrap();
    assert!(app.world().entity("lamp").is_err());
    assert_eq!(app.history().undo_label(), Some("delete lamp"));
    let text = ed.copied().unwrap().to_string();
    ed.frame(&mut app, &AllowAll, input(vec![Event::Paste(text)]))
        .unwrap();
    assert!(
        app.world().entity("lamp_2").is_ok(),
        "pasted back, renumbered"
    );

    let log = app.history().log().len();
    let paste = |ed: &mut HeadlessEditor, app: &mut App, text: String| {
        ed.frame(app, &AllowAll, input(vec![Event::Paste(text)]))
            .unwrap();
    };
    paste(&mut ed, &mut app, "a line of a script".into());
    assert!(console_has(&ed, "RE8805"), "{:?}", ed.editor.console());
    let one = |entities: Json| {
        json!({ "format": "real-engine/entities", "version": 1,
                "subtrees": [{ "world": Mat4::IDENTITY.to_cols_array(), "entities": entities }] })
        .to_string()
    };
    paste(
        &mut ed,
        &mut app,
        one(json!([{ "id": "door", "components": { "Hinge": { "angle": 1.0 } } }])),
    );
    assert!(console_has(&ed, "RE8806"), "{:?}", ed.editor.console());
    assert!(app.world().entity("door_2").is_err());
    assert_eq!(app.history().log().len(), log, "nothing was pasted");

    paste(
        &mut ed,
        &mut app,
        one(json!([{ "id": "door", "components": { "Follow": { "target": "gone" } } }])),
    );
    assert!(console_has(&ed, "RE8808"), "{:?}", ed.editor.console());
    assert_eq!(
        field(&app, "door_2", "Follow", "target"),
        Value::Entity(None)
    );

    let scene = json!({ "format": "real-engine/scene", "version": 1, "entities": [
        { "id": "shed", "components": { "Transform": { "translation": [5.0, 0.0, 0.0] } } },
        { "id": "shed_door", "parent": "shed" }
    ] });
    paste(&mut ed, &mut app, scene.to_string());
    assert_eq!(children(&app, "shed_2"), ["shed_door_2"]);
    assert_eq!(
        field(&app, "shed_2", "Transform", "translation"),
        Value::Vec3(Vec3::new(5.0, 0.0, 0.0))
    );
}
