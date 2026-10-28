//! Several selected entities (ADR-0044's amendment for WP42, §4): clicks in
//! the Hierarchy, walking the hierarchy from the selection, the gizmo moving
//! every topmost selected entity in one undo step, and deleting them, on a
//! small project in a temporary directory, headless.
//!
//! The GPU tests need an adapter (lavapipe in CI); run them with
//! `--test-threads=1`.

use std::path::Path;

use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use glam::Vec3;
use re_app::{App, AppConfig};
use re_command::AllowAll;
use re_editor::gizmo::{GizmoMode, GizmoPivot, Handle};
use re_editor::{HeadlessEditor, Selection, layout};
use re_reflect::Value;

const CONFIG: &str = r#"[project]
name = "fixture"
"#;

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "box", "name": "Box", "components": {
        "MeshRef": { "asset": "builtin:cube" },
        "Transform": { "translation": [0.0, 4.0, 0.0] } } },
    { "id": "camera", "name": "Camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 5.0, 12.0] } } },
    { "id": "crates", "name": "Crates", "components": { "Transform": {} } },
    { "id": "crate_1", "name": "Crate", "parent": "crates", "components": {
        "Transform": { "translation": [2.0, 0.5, 0.0] } } },
    { "id": "crate_2", "name": "Crate", "parent": "crates", "components": {
        "Transform": { "translation": [-2.0, 0.5, 0.0] } } },
    { "id": "lamp", "name": "Lamp", "components": {
        "Light": { "kind": "Point", "range": 4.0 },
        "Transform": { "translation": [3.0, 1.0, 0.0] } } },
    { "id": "sun", "name": "Sun", "components": {
        "Light": { "kind": "Directional", "intensity": 3.0 } } }
  ]
}
"#;

fn project() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("es")
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

/// A click of the left button at `at` with `modifiers` held (until the
/// next frame's input says otherwise).
fn click(at: Pos2, modifiers: Modifiers) -> egui::RawInput {
    let mut events = vec![Event::ModifiersChanged(modifiers), Event::PointerMoved(at)];
    for pressed in [true, false] {
        events.push(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers,
        });
    }
    input(events)
}

fn pointer(at: Pos2, pressed: Option<bool>) -> egui::RawInput {
    let mut events = vec![Event::PointerMoved(at)];
    if let Some(pressed) = pressed {
        events.push(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    input(events)
}

fn shortcut(key: Key, modifiers: Modifiers) -> egui::RawInput {
    input(vec![
        Event::ModifiersChanged(modifiers),
        Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        },
        Event::Key {
            key,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers,
        },
        Event::ModifiersChanged(Modifiers::NONE),
    ])
}

fn frames(ed: &mut HeadlessEditor, app: &mut App, n: usize) {
    for _ in 0..n {
        ed.frame(app, &AllowAll, input(vec![])).unwrap();
    }
}

/// Clicks the Hierarchy's row of `id` with `modifiers`, released in the
/// next frame, after which the selection has changed.
fn click_row(ed: &mut HeadlessEditor, app: &mut App, id: &str, modifiers: Modifiers) {
    let row = ed.editor.hierarchy_row(id).expect("the row is shown");
    ed.frame(app, &AllowAll, click(row.center(), modifiers))
        .unwrap();
    let release = input(vec![Event::ModifiersChanged(Modifiers::NONE)]);
    ed.frame(app, &AllowAll, release).unwrap();
}

fn selected(ed: &HeadlessEditor) -> (Vec<&str>, Option<&str>) {
    let s = ed.editor.selection();
    (s.ids().iter().map(String::as_str).collect(), s.active())
}

fn translation(app: &App, id: &str) -> Vec3 {
    match app.world().get(id, "Transform", "translation").unwrap() {
        Value::Vec3(v) => v,
        other => panic!("{other:?}"),
    }
}

fn scale(app: &App, id: &str) -> Vec3 {
    match app.world().get(id, "Transform", "scale").unwrap() {
        Value::Vec3(v) => v,
        other => panic!("{other:?}"),
    }
}

fn close(a: egui::Pos2, b: egui::Pos2) -> bool {
    a.distance(b) < 1.0
}

/// In the Hierarchy a click selects one entity, Ctrl+click adds or removes
/// one, and Shift+click adds the rows from the active one to the clicked
/// one. The layout saves the selection, and none of it is an edit.
#[test]
fn the_hierarchy_selects_several_with_ctrl_and_shift() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);
    let (hash, log) = (app.state_hash(), app.history().log().len());

    click_row(&mut ed, &mut app, "box", Modifiers::NONE);
    assert_eq!(selected(&ed), (vec!["box"], Some("box")));
    click_row(&mut ed, &mut app, "lamp", Modifiers::COMMAND);
    assert_eq!(selected(&ed), (vec!["box", "lamp"], Some("lamp")));
    // The rows are box, camera, crates (closed), lamp and sun: Shift+click on
    // camera adds camera, crates and lamp.
    click_row(&mut ed, &mut app, "camera", Modifiers::SHIFT);
    assert_eq!(
        selected(&ed),
        (vec!["box", "lamp", "camera", "crates"], Some("camera"))
    );
    // Ctrl+click on a selected row removes it; the active one stays.
    click_row(&mut ed, &mut app, "crates", Modifiers::COMMAND);
    assert_eq!(
        selected(&ed),
        (vec!["box", "lamp", "camera"], Some("camera"))
    );
    // Removing the active one makes the last remaining one active.
    click_row(&mut ed, &mut app, "camera", Modifiers::COMMAND);
    assert_eq!(selected(&ed), (vec!["box", "lamp"], Some("lamp")));

    // Saved with the layout, and read back by the next editor.
    frames(&mut ed, &mut app, 1);
    let text = std::fs::read_to_string(layout::layout_path(dir.path())).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        json["selection"],
        serde_json::json!({ "ids": ["box", "lamp"], "active": "lamp" })
    );
    drop(ed);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 1);
    assert_eq!(selected(&ed), (vec!["box", "lamp"], Some("lamp")));

    // A plain click selects one again.
    click_row(&mut ed, &mut app, "sun", Modifiers::NONE);
    assert_eq!(selected(&ed), (vec!["sun"], Some("sun")));
    assert_eq!(app.state_hash(), hash, "selecting is not an edit");
    assert_eq!(app.history().log().len(), log);
}

/// Ctrl+Up selects the selection's parents and Ctrl+Down their children;
/// an entity that is deleted leaves the selection.
#[test]
fn ctrl_up_and_down_walk_the_hierarchy() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.set_selection(Selection::from_ids(
        ["crate_1".to_string(), "box".to_string()],
        Some("crate_1".to_string()),
    ));
    frames(&mut ed, &mut app, 2);
    ed.frame(
        &mut app,
        &AllowAll,
        shortcut(Key::ArrowUp, Modifiers::COMMAND),
    )
    .unwrap();
    // A root stays itself.
    assert_eq!(selected(&ed), (vec!["crates", "box"], Some("crates")));
    ed.frame(
        &mut app,
        &AllowAll,
        shortcut(Key::ArrowDown, Modifiers::COMMAND),
    )
    .unwrap();
    assert_eq!(
        selected(&ed),
        (vec!["crate_1", "crate_2", "box"], Some("crate_1"))
    );

    // An entity that stops existing leaves the selection at the next frame.
    let out = ed.editor.run(
        &mut app,
        &AllowAll,
        "entity.delete",
        serde_json::json!({ "id": "crate_1" }),
        false,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    frames(&mut ed, &mut app, 1);
    assert_eq!(selected(&ed), (vec!["crate_2", "box"], Some("box")));
}

/// Dragging the gizmo moves every topmost selected entity (one whose parent
/// is selected moves with it), as one `scene.apply` per update and one undo
/// step for the whole drag.
#[test]
fn the_gizmo_moves_the_topmost_selected_entities_in_one_undo_step() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.set_selection(Selection::from_ids(
        [
            "crates".to_string(),
            "crate_1".to_string(),
            "box".to_string(),
        ],
        Some("box".to_string()),
    ));
    frames(&mut ed, &mut app, 2);
    let start = ed
        .editor
        .gizmo_handle(&app, Handle::Axis(0))
        .expect("the X arrow is on screen");
    let to = start + egui::vec2(120.0, 0.0);
    ed.frame(&mut app, &AllowAll, pointer(start, Some(true)))
        .unwrap();
    for t in [0.25, 0.5, 0.75, 1.0] {
        ed.frame(&mut app, &AllowAll, pointer(start + (to - start) * t, None))
            .unwrap();
        assert!(ed.editor.gizmo_dragging());
    }
    ed.frame(&mut app, &AllowAll, pointer(to, Some(false)))
        .unwrap();
    frames(&mut ed, &mut app, 1);
    assert!(!ed.editor.gizmo_dragging());

    let moved = translation(&app, "box") - Vec3::new(0.0, 4.0, 0.0);
    assert!(moved.x > 0.5 && moved.y == 0.0 && moved.z == 0.0, "{moved}");
    assert_eq!(translation(&app, "crates"), moved, "moved as far");
    assert_eq!(
        translation(&app, "crate_1"),
        Vec3::new(2.0, 0.5, 0.0),
        "moved with its parent, not twice"
    );
    assert_eq!(app.history().undo_label(), Some("move 2 entities"));
    let step = app.history().peek_undo().unwrap();
    assert_eq!(step.len(), 2, "one translation each: {step:?}");

    let out = ed.editor.undo(&mut app, &AllowAll);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(translation(&app, "box"), Vec3::new(0.0, 4.0, 0.0));
    assert_eq!(translation(&app, "crates"), Vec3::ZERO);
    assert!(!app.history().can_undo(), "the drag was one step");
}

/// With several entities selected the gizmo stands on the active one, or at
/// the centre of their bounds when the pivot is set to the centre, and a
/// scale there moves each entity away from that centre as it grows.
#[test]
fn the_gizmo_stands_on_the_active_entity_or_the_centre() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.set_selection(Selection::from_ids(
        ["box".to_string(), "lamp".to_string()],
        Some("lamp".to_string()),
    ));
    frames(&mut ed, &mut app, 2);
    // The first frame reads the gizmo settings from the layout.
    let mut settings = *ed.editor.gizmo_settings();
    settings.mode = GizmoMode::Scale;
    ed.editor.set_gizmo_settings(settings);
    frames(&mut ed, &mut app, 1);
    let centre_box = |ed: &HeadlessEditor, app: &App| {
        ed.editor
            .gizmo_handle(app, Handle::View)
            .expect("the centre box")
    };
    let lamp = ed
        .editor
        .screen_point(&app, Vec3::new(3.0, 1.0, 0.0))
        .unwrap();
    assert!(close(centre_box(&ed, &app), lamp), "on the lamp");

    settings.pivot = GizmoPivot::Centre;
    ed.editor.set_gizmo_settings(settings);
    frames(&mut ed, &mut app, 1);
    // The box's mesh spans a metre around (0, 4, 0); the lamp counts as a
    // metre around (3, 1, 0).
    let middle = ed
        .editor
        .screen_point(&app, Vec3::new(1.5, 2.5, 0.0))
        .unwrap();
    assert!(
        close(centre_box(&ed, &app), middle),
        "{:?} {middle:?}",
        centre_box(&ed, &app)
    );

    // Dragging the centre box up and to the right scales both: each grows
    // by the same factor and is that many times farther from the centre.
    let start = centre_box(&ed, &app);
    let to = start + egui::vec2(60.0, -30.0);
    ed.frame(&mut app, &AllowAll, pointer(start, Some(true)))
        .unwrap();
    for t in [0.5, 1.0] {
        ed.frame(&mut app, &AllowAll, pointer(start + (to - start) * t, None))
            .unwrap();
    }
    ed.frame(&mut app, &AllowAll, pointer(to, Some(false)))
        .unwrap();
    frames(&mut ed, &mut app, 1);
    let grown = scale(&app, "box");
    assert!(grown.x > 1.2 && grown == Vec3::splat(grown.x), "{grown}");
    assert_eq!(scale(&app, "lamp"), grown);
    let centre = Vec3::new(1.5, 2.5, 0.0);
    for (id, was) in [
        ("box", Vec3::new(0.0, 4.0, 0.0)),
        ("lamp", Vec3::new(3.0, 1.0, 0.0)),
    ] {
        let now = translation(&app, id);
        let expected = centre + (was - centre) * grown.x;
        assert!(now.distance(expected) < 1e-3, "{id}: {now} {expected}");
    }
    assert_eq!(app.history().undo_label(), Some("scale 2 entities"));
}

/// Deleting the selection removes the topmost selected entities and
/// everything under them in one undo step.
#[test]
fn deleting_the_selection_is_one_undo_step() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.set_selection(Selection::from_ids(
        [
            "crates".to_string(),
            "crate_1".to_string(),
            "lamp".to_string(),
        ],
        None,
    ));
    frames(&mut ed, &mut app, 1);
    let hash = app.state_hash();
    let out = ed
        .editor
        .delete_selection(&mut app, &AllowAll)
        .expect("something to delete");
    assert!(out.ok, "{:?}", out.diagnostics);
    for id in ["crates", "crate_1", "crate_2", "lamp"] {
        assert!(app.world().entity(id).is_err(), "{id} is gone");
    }
    assert!(app.world().entity("box").is_ok());
    assert!(ed.editor.selection().is_empty());
    assert_eq!(app.history().undo_label(), Some("delete 2 entities"));
    let out = ed.editor.undo(&mut app, &AllowAll);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(app.state_hash(), hash);
    assert!(!app.history().can_undo());
    assert!(ed.editor.delete_selection(&mut app, &AllowAll).is_none());
}
