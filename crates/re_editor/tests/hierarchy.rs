//! The Hierarchy's context menu, renaming in place and revealing the active
//! entity, and notes in the Inspector (ADR-0044's amendment for WP42, §7),
//! on a small project in a temporary directory, headless.
//!
//! The GPU tests need an adapter (lavapipe in CI); run them with
//! `--test-threads=1`.

use std::path::Path;

use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use glam::Vec3;
use re_app::{App, AppConfig};
use re_command::AllowAll;
use re_editor::{HeadlessEditor, Panel, Selection};
use re_scene::Generated;

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
    { "id": "lamp", "name": "Lamp", "note": "Hangs over the door", "components": {
        "Light": { "kind": "Point" },
        "Transform": { "translation": [3.0, 1.0, 0.0] } } }
  ]
}
"#;

fn project() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("eh")
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

fn click(at: Pos2, button: PointerButton) -> egui::RawInput {
    let mut events = vec![Event::PointerMoved(at)];
    for pressed in [true, false] {
        events.push(Event::PointerButton {
            pos: at,
            button,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    input(events)
}

fn key(key: Key, modifiers: Modifiers) -> egui::RawInput {
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

fn typed(text: &str) -> egui::RawInput {
    input(vec![Event::Text(text.to_string())])
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

fn row(ed: &HeadlessEditor, id: &str) -> Pos2 {
    ed.editor
        .hierarchy_row(id)
        .unwrap_or_else(|| panic!("the row of {id} is shown"))
        .center()
}

/// Right-clicks `at`, and lets the selection the click made settle.
fn open_menu(ed: &mut HeadlessEditor, app: &mut App, at: Pos2) {
    ed.frame(app, &AllowAll, click(at, PointerButton::Secondary))
        .unwrap();
    frames(ed, app, 1);
}

/// Opens the menu of the row of `id`.
fn row_menu(ed: &mut HeadlessEditor, app: &mut App, id: &str) {
    let at = row(ed, id);
    open_menu(ed, app, at);
}

/// Clicks the open menu's item `label`.
fn choose(ed: &mut HeadlessEditor, app: &mut App, label: &str) {
    let item = ed
        .editor
        .menu_item(label)
        .unwrap_or_else(|| panic!("the menu shows {label}"));
    ed.frame(app, &AllowAll, click(item.center(), PointerButton::Primary))
        .unwrap();
    frames(ed, app, 1);
}

fn parent(app: &App, id: &str) -> Option<String> {
    let world = app.world();
    let e = world.entity(id).unwrap();
    re_scene::hierarchy::parent(&world.ecs, e).and_then(|p| world.id_of(p).map(str::to_string))
}

fn name(app: &App, id: &str) -> String {
    let world = app.world();
    world.name_of(world.entity(id).unwrap()).to_string()
}

fn note(app: &App, id: &str) -> String {
    let world = app.world();
    world.note_of(world.entity(id).unwrap()).to_string()
}

fn console_has(ed: &HeadlessEditor, text: &str) -> bool {
    ed.editor.console().iter().any(|l| l.text.contains(text))
}

/// Makes `id` look generated by `source`, as a procedural source's output is.
fn generated(app: &mut App, id: &str, source: &str) {
    let world = app.world_mut();
    let source = world.entity(source).unwrap();
    let e = world.entity(id).unwrap();
    world.ecs.entity_mut(e).insert(Generated {
        source,
        key: id.to_string(),
    });
}

/// A right click on a row that is not selected selects it; the menu copies,
/// pastes (reading the clipboard through the host), pastes as a child,
/// duplicates, cuts and deletes, each as one undo step, and the menu under
/// the rows pastes at the root. A pasted entity inside a folded row is
/// revealed.
#[test]
fn the_context_menu_copies_pastes_and_deletes() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);
    let hash = app.state_hash();

    // Nothing copied yet: Paste notes that and changes nothing.
    row_menu(&mut ed, &mut app, "box");
    assert_eq!(selected(&ed), ["box"], "a right click selects its row");
    choose(&mut ed, &mut app, "Paste");
    assert!(console_has(&ed, "RE8805"), "{:?}", ed.editor.console());
    assert_eq!(app.state_hash(), hash);
    assert!(ed.editor.menu_item("Paste").is_none(), "the menu closed");

    // Copy the lamp and paste it as a child of the box.
    row_menu(&mut ed, &mut app, "lamp");
    choose(&mut ed, &mut app, "Copy");
    assert!(ed.copied().is_some_and(|t| t.contains("\"lamp\"")));
    assert_eq!(app.state_hash(), hash, "copying is not an edit");
    row_menu(&mut ed, &mut app, "box");
    choose(&mut ed, &mut app, "Paste as child");
    assert_eq!(parent(&app, "lamp_2").as_deref(), Some("box"));
    assert_eq!(selected(&ed), ["lamp_2"]);
    assert_eq!(app.history().undo_label(), Some("paste lamp"));
    // The box's row unfolds to show the copy.
    frames(&mut ed, &mut app, 12);
    assert!(ed.editor.hierarchy_row("lamp_2").is_some());

    // Under the rows, Paste puts the copy at the root.
    let panel = ed.editor.rects().get(Panel::Hierarchy).unwrap();
    open_menu(
        &mut ed,
        &mut app,
        Pos2::new(panel.center().x, panel.bottom() - 8.0),
    );
    choose(&mut ed, &mut app, "Paste");
    assert_eq!(parent(&app, "lamp_3"), None);
    assert_eq!(selected(&ed), ["lamp_3"]);

    // A right click on a selected row keeps the selection; Delete removes
    // all of it in one step.
    select(&mut ed, &mut app, &["lamp_2", "lamp_3"]);
    frames(&mut ed, &mut app, 12);
    row_menu(&mut ed, &mut app, "lamp_3");
    assert_eq!(selected(&ed), ["lamp_2", "lamp_3"]);
    choose(&mut ed, &mut app, "Delete");
    assert!(app.world().entity("lamp_2").is_err() && app.world().entity("lamp_3").is_err());
    assert!(ed.editor.selection().is_empty());
    assert_eq!(app.history().undo_label(), Some("delete 2 entities"));

    // Duplicate puts the copy beside the lamp; Cut copies and deletes it.
    row_menu(&mut ed, &mut app, "lamp");
    choose(&mut ed, &mut app, "Duplicate");
    assert_eq!(selected(&ed), ["lamp_2"]);
    assert_eq!(parent(&app, "lamp_2"), None);
    assert_eq!(app.history().undo_label(), Some("duplicate lamp"));
    row_menu(&mut ed, &mut app, "lamp_2");
    choose(&mut ed, &mut app, "Cut");
    assert!(app.world().entity("lamp_2").is_err());
    assert!(ed.copied().is_some_and(|t| t.contains("\"lamp_2\"")));
    assert_eq!(app.history().undo_label(), Some("delete lamp_2"));

    // Select children, and Focus, which frames the selection.
    row_menu(&mut ed, &mut app, "crates");
    choose(&mut ed, &mut app, "Select children");
    assert_eq!(selected(&ed), ["crate_1", "crate_2"]);
    row_menu(&mut ed, &mut app, "lamp");
    choose(&mut ed, &mut app, "Focus");
    let pivot = ed.editor.camera().unwrap().pivot;
    assert!(pivot.abs_diff_eq(Vec3::new(3.0, 1.0, 0.0), 1e-4), "{pivot}");
}

/// A double-click, F2 or the menu's Rename edits an entity's name in its
/// row, with the whole name selected: Enter or a click elsewhere keeps the
/// new name (one undo step), Escape drops it. A generated entity is not
/// renamed, and the menu's Rename is off for it.
#[test]
fn rows_are_renamed_in_place() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);

    let at = row(&ed, "box");
    for _ in 0..2 {
        ed.frame(&mut app, &AllowAll, click(at, PointerButton::Primary))
            .unwrap();
    }
    frames(&mut ed, &mut app, 1);
    assert_eq!(ed.editor.renaming(), Some(("box", "Box")));
    ed.frame(&mut app, &AllowAll, typed("Big box")).unwrap();
    ed.frame(&mut app, &AllowAll, key(Key::Enter, Modifiers::NONE))
        .unwrap();
    frames(&mut ed, &mut app, 1);
    assert_eq!(name(&app, "box"), "Big box", "{:?}", ed.editor.console());
    assert_eq!(app.history().undo_label(), Some("rename box"));
    assert!(ed.editor.renaming().is_none());

    // F2 renames the active entity; Escape drops the edit.
    let log = app.history().log().len();
    ed.frame(&mut app, &AllowAll, key(Key::F2, Modifiers::NONE))
        .unwrap();
    frames(&mut ed, &mut app, 1);
    assert_eq!(ed.editor.renaming(), Some(("box", "Big box")));
    ed.frame(&mut app, &AllowAll, typed("Other")).unwrap();
    ed.frame(&mut app, &AllowAll, key(Key::Escape, Modifiers::NONE))
        .unwrap();
    frames(&mut ed, &mut app, 1);
    assert!(ed.editor.renaming().is_none());
    assert_eq!(name(&app, "box"), "Big box");
    assert_eq!(app.history().log().len(), log, "nothing was sent");

    // The menu's Rename, kept by a click on another row.
    row_menu(&mut ed, &mut app, "lamp");
    choose(&mut ed, &mut app, "Rename");
    assert_eq!(ed.editor.renaming(), Some(("lamp", "Lamp")));
    ed.frame(&mut app, &AllowAll, typed("Porch lamp")).unwrap();
    ed.frame(
        &mut app,
        &AllowAll,
        click(row(&ed, "camera"), PointerButton::Primary),
    )
    .unwrap();
    frames(&mut ed, &mut app, 1);
    assert_eq!(name(&app, "lamp"), "Porch lamp");
    assert_eq!(app.history().undo_label(), Some("rename lamp"));

    // A generated entity keeps its name.
    generated(&mut app, "crate_2", "crates");
    select(&mut ed, &mut app, &["crate_2"]);
    frames(&mut ed, &mut app, 12);
    ed.frame(&mut app, &AllowAll, key(Key::F2, Modifiers::NONE))
        .unwrap();
    frames(&mut ed, &mut app, 1);
    assert!(ed.editor.renaming().is_none());
    assert!(console_has(&ed, "crate_2 cannot be renamed"));
    row_menu(&mut ed, &mut app, "crate_2");
    choose(&mut ed, &mut app, "Rename");
    assert!(ed.editor.renaming().is_none(), "the item is off");
    assert_eq!(name(&app, "crate_2"), "Crate");
}

/// The Inspector's header edits the active entity's note: a click elsewhere
/// keeps it (one undo step), Escape drops it. A generated entity has no
/// note field.
#[test]
fn the_inspector_edits_notes() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);
    select(&mut ed, &mut app, &["lamp"]);
    frames(&mut ed, &mut app, 1);

    let field = ed.editor.note_field("lamp").expect("the note is shown");
    ed.frame(
        &mut app,
        &AllowAll,
        click(field.center(), PointerButton::Primary),
    )
    .unwrap();
    ed.frame(&mut app, &AllowAll, key(Key::A, Modifiers::COMMAND))
        .unwrap();
    ed.frame(&mut app, &AllowAll, typed("Over the porch"))
        .unwrap();
    assert_eq!(note(&app, "lamp"), "Hangs over the door", "not yet sent");
    ed.frame(
        &mut app,
        &AllowAll,
        click(row(&ed, "box"), PointerButton::Primary),
    )
    .unwrap();
    frames(&mut ed, &mut app, 1);
    assert_eq!(note(&app, "lamp"), "Over the porch");
    assert_eq!(app.history().undo_label(), Some("note on lamp"));
    assert!(ed.editor.undo(&mut app, &AllowAll).ok);
    assert_eq!(note(&app, "lamp"), "Hangs over the door");

    // Escape drops the edit.
    select(&mut ed, &mut app, &["lamp"]);
    frames(&mut ed, &mut app, 1);
    let log = app.history().log().len();
    let field = ed.editor.note_field("lamp").unwrap();
    ed.frame(
        &mut app,
        &AllowAll,
        click(field.center(), PointerButton::Primary),
    )
    .unwrap();
    ed.frame(&mut app, &AllowAll, typed(" and more")).unwrap();
    ed.frame(&mut app, &AllowAll, key(Key::Escape, Modifiers::NONE))
        .unwrap();
    frames(&mut ed, &mut app, 1);
    assert_eq!(note(&app, "lamp"), "Hangs over the door");
    assert_eq!(app.history().log().len(), log);

    // A generated entity keeps no note.
    generated(&mut app, "crate_2", "crates");
    select(&mut ed, &mut app, &["crate_2"]);
    frames(&mut ed, &mut app, 1);
    assert!(ed.editor.note_field("crate_2").is_none());
}
