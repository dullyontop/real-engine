//! `re_editor` tests on a small project in a temporary directory: the panels
//! rendered headless, the editor socket and lock, the inspector's undo and
//! the command palette.
//!
//! The GPU tests need an adapter (lavapipe in CI); run them with
//! `--test-threads=1`.

use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

use glam::Vec3;
#[cfg(unix)]
use re_agent::lock::{self, Holder, LockState};
use re_app::{App, AppConfig};
use re_command::AllowAll;
#[cfg(unix)]
use re_core::time::Instant;
#[cfg(unix)]
use re_editor::EditorSocket;
use re_editor::{Editor, HeadlessEditor, Panel};
use re_reflect::{FieldType, Value};
use serde_json::json;

const CONFIG: &str = r#"[project]
name = "fixture"

[render]
clear = [0.2, 0.4, 0.6, 1.0]
"#;

const SCENE: &str = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    { "id": "box", "name": "Box", "components": {
        "Collider": { "shape": "Box", "half_extents": [0.5, 0.5, 0.5] },
        "MeshRef": { "asset": "builtin:cube" },
        "RigidBody": { "kind": "Dynamic", "mass": 10.0 },
        "Transform": { "translation": [0.0, 4.0, 0.0] } } },
    { "id": "camera", "name": "Camera", "components": {
        "Camera": { "active": true },
        "Transform": { "translation": [0.0, 5.0, 12.0] } } },
    { "id": "crates", "name": "Crates", "components": { "Transform": {} } },
    { "id": "crate_1", "name": "Crate", "parent": "crates", "components": {
        "Transform": { "translation": [2.0, 0.5, 0.0] } } },
    { "id": "sun", "name": "Sun", "components": {
        "Light": { "kind": "Directional", "intensity": 3.0 } } }
  ]
}
"#;

/// Writes the project, with the scene in the canonical form `App::save_scene`
/// writes, and returns its directory.
fn project() -> tempfile::TempDir {
    // A short path: Unix socket paths are limited to about 100 bytes.
    let dir = tempfile::Builder::new()
        .prefix("ed")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    std::fs::create_dir_all(dir.path().join("scenes")).unwrap();
    std::fs::write(dir.path().join("real.toml"), CONFIG).unwrap();
    std::fs::write(dir.path().join("scenes/main.scene.json"), SCENE).unwrap();
    let mut app = open(dir.path(), false);
    app.save_scene().unwrap();
    dir
}

fn open(dir: &Path, persist_edits: bool) -> App {
    let config = AppConfig {
        persist_edits,
        ..AppConfig::load(dir).unwrap()
    };
    App::open(dir, config).unwrap()
}

fn scene_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("scenes/main.scene.json")).unwrap()
}

/// Every panel is laid out and painted: after the warm-up frame each has a
/// rectangle, and the pixels inside it are not all one colour.
#[test]
fn panels_render_headless() {
    let dir = project();
    let mut app = open(dir.path(), false);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.select(Some("box"));
    ed.editor.set_palette_open(true);
    ed.frame(&mut app, &AllowAll, egui::RawInput::default())
        .unwrap();
    let image = ed
        .frame(&mut app, &AllowAll, egui::RawInput::default())
        .unwrap();
    assert_eq!((image.width, image.height), (1280, 720));
    let rects = *ed.editor.rects();
    for panel in Panel::ALL {
        if panel == Panel::Canvas {
            // A tab behind the game view (tests/canvas.rs brings it forward).
            continue;
        }
        let rect = rects
            .get(panel)
            .unwrap_or_else(|| panic!("{panel:?} was not drawn"));
        assert!(
            rect.width() >= 40.0 && rect.height() >= 20.0,
            "{panel:?} is too small: {rect:?}"
        );
        if panel == Panel::Viewport {
            continue;
        }
        let colours = distinct_colours(&image, rect);
        assert!(
            colours > 2,
            "{panel:?} at {rect:?} looks empty ({colours} colours)"
        );
    }

    // The game view shows the scene's clear colour where no panel covers it.
    let view = rects.get(Panel::Viewport).unwrap();
    let palette = rects.get(Panel::Palette).unwrap();
    let probe = [
        view.left_bottom() + egui::vec2(8.0, -8.0),
        view.right_bottom() + egui::vec2(-8.0, -8.0),
    ]
    .into_iter()
    .find(|p| !palette.contains(*p))
    .expect("a corner of the viewport outside the palette");
    let px = image.pixel(probe.x as u32, probe.y as u32);
    let want = [0.2f32, 0.4, 0.6].map(re_render::capture::linear_to_srgb_u8);
    for c in 0..3 {
        assert!(
            px[c].abs_diff(want[c]) <= 3,
            "viewport pixel {px:?} at {probe:?} is not the clear colour {want:?}"
        );
    }
    assert!(ed.view.size().is_some());
}

/// The game view draws the World's meshes: a cube spawned in front of the
/// camera, with a built-in mesh the app uploads to the editor's renderer,
/// covers the middle of the viewport.
#[test]
fn game_view_draws_meshes() {
    let dir = project();
    let mut app = open(dir.path(), false);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    let clear = [0.2f32, 0.4, 0.6].map(re_render::capture::linear_to_srgb_u8);
    let is_clear = |px: [u8; 4]| (0..3).all(|c| px[c].abs_diff(clear[c]) <= 3);
    ed.frame(&mut app, &AllowAll, egui::RawInput::default())
        .unwrap();
    let image = ed
        .frame(&mut app, &AllowAll, egui::RawInput::default())
        .unwrap();
    let centre = ed.editor.rects().get(Panel::Viewport).unwrap().center();
    let before = image.pixel(centre.x as u32, centre.y as u32);
    assert!(is_clear(before), "the empty view is {before:?}");

    let out = ed.editor.run(
        &mut app,
        &AllowAll,
        "entity.spawn",
        json!({ "id": "near", "components": {
            "MeshRef": { "asset": "builtin:cube" },
            "Transform": { "translation": [0.0, 5.0, 9.0] } } }),
        false,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let image = ed
        .frame(&mut app, &AllowAll, egui::RawInput::default())
        .unwrap();
    let after = image.pixel(centre.x as u32, centre.y as u32);
    assert!(!is_clear(after), "the cube is not drawn: {after:?}");
}

/// A screenshot of a scene with a HUD (drawn by the app's renderer, with the
/// HUD's own egui context) leaves the editor's panels as they were: the two
/// egui contexts do not share a UI painter.
#[test]
fn a_hud_screenshot_leaves_the_panels_alone() {
    let dir = project();
    let mut app = open(dir.path(), false);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    let out = ed.editor.run(
        &mut app,
        &AllowAll,
        "entity.spawn",
        json!({ "id": "hud", "components": { "HudText": { "text": "3 / 7", "size": 14.0 } } }),
        false,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    ed.editor.select(Some("box"));
    for _ in 0..2 {
        ed.frame(&mut app, &AllowAll, egui::RawInput::default())
            .unwrap();
    }
    let before = ed
        .frame(&mut app, &AllowAll, egui::RawInput::default())
        .unwrap();
    let out = ed
        .editor
        .run(&mut app, &AllowAll, "screenshot", json!({}), false);
    assert!(out.ok, "{:?}", out.diagnostics);
    let after = ed
        .frame(&mut app, &AllowAll, egui::RawInput::default())
        .unwrap();
    for panel in [Panel::Hierarchy, Panel::Toolbar, Panel::Inspector] {
        let rect = ed.editor.rects().get(panel).unwrap();
        let (x0, y0) = (rect.left().max(0.0) as u32, rect.top().max(0.0) as u32);
        let x1 = (rect.right() as u32).min(after.width);
        let y1 = (rect.bottom() as u32).min(after.height);
        let changed = (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| before.pixel(x, y) != after.pixel(x, y))
            .count();
        assert_eq!(changed, 0, "{panel:?} changed after the screenshot");
    }
}

fn distinct_colours(image: &re_render::Image, rect: egui::Rect) -> usize {
    let mut seen = std::collections::BTreeSet::new();
    let x0 = rect.left().max(0.0) as u32;
    let y0 = rect.top().max(0.0) as u32;
    let x1 = (rect.right() as u32).min(image.width);
    let y1 = (rect.bottom() as u32).min(image.height);
    for y in (y0..y1).step_by(2) {
        for x in (x0..x1).step_by(2) {
            seen.insert(image.pixel(x, y));
            if seen.len() > 16 {
                return seen.len();
            }
        }
    }
    seen.len()
}

/// The panels draw every field type of the built-in components and take
/// keyboard input without dispatching anything by themselves.
#[test]
fn every_component_inspects_without_changes() {
    let dir = project();
    let mut app = open(dir.path(), false);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    let names: Vec<String> = app
        .world()
        .registry
        .schemas()
        .map(|s| s.name.clone())
        .collect();
    let mut editor = Editor::new();
    for name in &names {
        let out = editor.run(
            &mut app,
            &AllowAll,
            "component.add",
            json!({ "id": "crates", "component": name }),
            false,
        );
        // `Transform` is already there; every other built-in is added.
        assert!(
            out.ok || name == "Transform",
            "{name}: {:?}",
            out.diagnostics
        );
    }
    let before = app.history().log().len();
    ed.editor.select(Some("crates"));
    for _ in 0..3 {
        ed.frame(&mut app, &AllowAll, egui::RawInput::default())
            .unwrap();
    }
    assert_eq!(
        app.history().log().len(),
        before,
        "drawing changed the scene"
    );
    // The only error is the empty `Procedural` that was just added: it names
    // no generator (`RE1207`).
    let errors: Vec<_> = ed
        .editor
        .console()
        .iter()
        .filter(|l| l.kind == re_editor::LineKind::Error)
        .collect();
    assert!(
        errors.iter().all(|l| l.text.contains("RE1207")),
        "{errors:?}"
    );
    assert!(ed.editor.rects().get(Panel::Graph).is_some());
}

/// With the editor socket served, a command sent through
/// `re_agent::remote::connect` is applied to the editor's world, and a second
/// writer is refused with `RE8003`.
#[cfg(unix)]
#[test]
fn socket_round_trip_and_second_writer() {
    let dir = project();
    let mut app = open(dir.path(), false);
    let socket = EditorSocket::open(dir.path()).unwrap();
    assert_eq!(
        lock::probe(dir.path()),
        LockState::Editor(lock::socket_path(dir.path()))
    );

    let path = socket.path().expect("the socket is served on Unix");
    let client = std::thread::spawn(move || {
        let handle = re_agent::remote::connect(&path).unwrap();
        let spawn = handle.call(
            "entity.spawn",
            json!({ "id": "rock", "name": "Rock", "parent": "crates" }),
            false,
        );
        let specs = handle.list_specs().unwrap();
        (spawn, specs.len())
    });
    let deadline = Instant::now() + Duration::from_secs(20);
    while !client.is_finished() {
        socket.serve_pending(&mut app, &AllowAll);
        assert!(Instant::now() < deadline, "the socket call did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    let (spawn, specs) = client.join().unwrap();
    assert!(spawn.ok, "{:?}", spawn.diagnostics);
    assert!(specs > 10);
    let rock = app.world().entity("rock").expect("the socket's spawn");
    assert_eq!(app.world().name_of(rock), "Rock");

    // The editor holds the lock: a second writer is refused.
    let refused = lock::acquire(dir.path(), Holder::headless("real do")).unwrap_err();
    assert_eq!(refused.code.to_string(), "RE8003");
    assert!(
        refused.message.contains("the editor"),
        "{}",
        refused.message
    );
    // ...and the editor's own lock cannot be taken twice either.
    let twice = EditorSocket::open(dir.path()).unwrap_err();
    assert_eq!(twice.code.to_string(), "RE8003");

    // Closing the editor removes the socket and frees the project.
    drop(socket);
    assert!(!lock::socket_path(dir.path()).exists());
    assert_eq!(lock::probe(dir.path()), LockState::Free);
    lock::acquire(dir.path(), Holder::headless("real do")).unwrap();
}

/// An inspector edit is one undo step, and after `undo` the saved scene is
/// byte-identical to the one before the edit.
#[test]
fn inspector_edit_is_undone_byte_identically() {
    let dir = project();
    let before = scene_text(dir.path());
    let mut app = open(dir.path(), true);
    let mut editor = Editor::new();
    let out = editor.set_field(
        &mut app,
        &AllowAll,
        "crate_1",
        "Transform",
        "translation",
        &FieldType::Vec3,
        &Value::Vec3(Vec3::new(3.25, 0.5, -1.0)),
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let edited = scene_text(dir.path());
    assert_ne!(edited, before);
    assert!(edited.contains("3.25"), "{edited}");

    let out = editor.undo(&mut app, &AllowAll);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert!(!app.history().can_undo(), "the edit was more than one step");
    assert_eq!(scene_text(dir.path()), before);

    let out = editor.redo(&mut app, &AllowAll);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(scene_text(dir.path()), edited);
}

/// The palette runs any command with JSON arguments; arguments that are not
/// a JSON object are refused with `RE8802` and run nothing.
#[test]
fn palette_runs_commands_and_refuses_bad_arguments() {
    let dir = project();
    let mut app = open(dir.path(), false);
    let mut editor = Editor::new();
    editor.set_palette_command("entity.spawn", r#"{"id": "p1", "name": "From palette"}"#);
    let out = editor.run_palette(&mut app, &AllowAll).unwrap();
    assert!(out.ok, "{:?}", out.diagnostics);
    assert!(app.world().entity("p1").is_ok());

    let log = app.history().log().len();
    editor.set_palette_command("entity.spawn", "[1, 2]");
    assert!(editor.run_palette(&mut app, &AllowAll).is_none());
    editor.set_palette_command("entity.spawn", "{\"id\": ");
    assert!(editor.run_palette(&mut app, &AllowAll).is_none());
    assert_eq!(app.history().log().len(), log);
    let errors: Vec<_> = editor
        .console()
        .iter()
        .filter(|l| l.text.contains("RE8802"))
        .collect();
    assert_eq!(errors.len(), 2, "{:?}", editor.console());

    // A read command shows its result in the console.
    editor.set_palette_command("entity.get", r#"{"id": "p1"}"#);
    let out = editor.run_palette(&mut app, &AllowAll).unwrap();
    assert!(out.ok);
    assert!(
        editor
            .console()
            .last()
            .is_some_and(|l| l.text.contains("From palette")),
        "{:?}",
        editor.console().last()
    );
}

/// Commands from the panels go through the editor's policy.
#[test]
fn the_policy_applies_to_editor_actions() {
    struct ReadOnly;
    impl re_command::PolicyCheck for ReadOnly {
        fn check(
            &self,
            spec: &re_command::CommandSpec,
            _args: &serde_json::Value,
            dry_run: bool,
        ) -> Result<(), re_core::Diagnostic> {
            if dry_run || spec.tier == re_command::Tier::Read {
                Ok(())
            } else {
                Err(re_core::Diagnostic::error(
                    re_core::Code::new("RE2005"),
                    "read only",
                ))
            }
        }
    }
    let dir = project();
    let mut app = open(dir.path(), false);
    let mut editor = Editor::new();
    let out = editor.set_field(
        &mut app,
        &ReadOnly,
        "box",
        "Transform",
        "translation",
        &FieldType::Vec3,
        &Value::Vec3(Vec3::ZERO),
    );
    assert!(!out.ok);
    assert!(!app.history().can_undo());
}

fn click(at: egui::Pos2) -> Vec<egui::Event> {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    vec![egui::Event::PointerMoved(at), button(true), button(false)]
}

fn input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        events,
        ..egui::RawInput::default()
    }
}

/// Typing a number into the Y box of a vector field, one key per frame as a
/// person types, then Enter, is one `entity.set` and one undo step; undo
/// restores the saved scene byte for byte.
#[test]
fn typing_into_an_inspector_field_is_one_undo_step() {
    let dir = project();
    let before = scene_text(dir.path());
    let mut app = open(dir.path(), true);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.select(Some("crate_1"));
    for _ in 0..2 {
        ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    }
    let rects = ed
        .editor
        .field_rects("crate_1", "Transform", "translation")
        .to_vec();
    assert_eq!(rects.len(), 3, "x, y and z boxes");
    let log = app.history().log().len();

    ed.frame(&mut app, &AllowAll, input(click(rects[1].center())))
        .unwrap();
    // Select what the box shows, then type over it.
    let select_all = egui::Event::Key {
        key: egui::Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    };
    ed.frame(&mut app, &AllowAll, input(vec![select_all]))
        .unwrap();
    for ch in ["1", "2", ".", "5"] {
        ed.frame(
            &mut app,
            &AllowAll,
            input(vec![egui::Event::Text(ch.into())]),
        )
        .unwrap();
        assert_eq!(
            app.history().log().len(),
            log,
            "committed while typing {ch}"
        );
    }
    let enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    for _ in 0..2 {
        ed.frame(&mut app, &AllowAll, input(vec![enter.clone()]))
            .unwrap();
    }
    ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    assert_eq!(
        app.history().log().len(),
        log + 1,
        "{:?}",
        ed.editor.console()
    );
    assert_eq!(
        app.world()
            .get("crate_1", "Transform", "translation")
            .unwrap(),
        Value::Vec3(Vec3::new(2.0, 12.5, 0.0))
    );
    assert_ne!(scene_text(dir.path()), before);

    let out = ed.editor.undo(&mut app, &AllowAll);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert!(!app.history().can_undo());
    assert_eq!(scene_text(dir.path()), before);
}

/// Clicking another entity while a field is being typed into commits the
/// typed value to the entity it belongs to, then selects the other one.
#[test]
fn changing_selection_commits_the_field_being_edited() {
    let dir = project();
    let mut app = open(dir.path(), false);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.select(Some("crate_1"));
    for _ in 0..2 {
        ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    }
    let rects = ed
        .editor
        .field_rects("crate_1", "Transform", "translation")
        .to_vec();
    ed.frame(&mut app, &AllowAll, input(click(rects[0].center())))
        .unwrap();
    let select_all = egui::Event::Key {
        key: egui::Key::A,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::COMMAND,
    };
    ed.frame(&mut app, &AllowAll, input(vec![select_all]))
        .unwrap();
    ed.frame(
        &mut app,
        &AllowAll,
        input(vec![egui::Event::Text("7".into())]),
    )
    .unwrap();
    let sun = ed.editor.hierarchy_row("sun").expect("the Sun row");
    ed.frame(&mut app, &AllowAll, input(click(sun.center())))
        .unwrap();
    for _ in 0..3 {
        ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    }
    assert_eq!(ed.editor.selected(), Some("sun"));
    assert_eq!(
        app.world()
            .get("crate_1", "Transform", "translation")
            .unwrap(),
        Value::Vec3(Vec3::new(7.0, 0.5, 0.0)),
        "{:?}",
        ed.editor.console()
    );
}

fn pointer(pos: egui::Pos2, pressed: Option<bool>) -> Vec<egui::Event> {
    let mut events = vec![egui::Event::PointerMoved(pos)];
    if let Some(pressed) = pressed {
        events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
    }
    events
}

fn key(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

fn translation(app: &App, id: &str) -> Vec3 {
    match app.world().get(id, "Transform", "translation").unwrap() {
        Value::Vec3(v) => v,
        other => panic!("{other:?}"),
    }
}

/// Dragging the gizmo's X arrow moves only the preview while the button is
/// held; releasing it sends one `scene.apply`, which moves the entity along
/// X only and is one undo step. Escape during a drag sends nothing.
#[test]
fn a_gizmo_drag_is_one_transaction() {
    use re_editor::gizmo::Handle;
    let dir = project();
    let before = scene_text(dir.path());
    let mut app = open(dir.path(), true);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.select(Some("box"));
    for _ in 0..2 {
        ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    }
    let log = app.history().log().len();
    let start = ed
        .editor
        .gizmo_handle(&app, Handle::Axis(0))
        .expect("the X arrow is on screen");
    let to = start + egui::vec2(120.0, 40.0);

    let drag = |ed: &mut HeadlessEditor, app: &mut App, last: Vec<egui::Event>| {
        ed.frame(app, &AllowAll, input(pointer(start, Some(true))))
            .unwrap();
        for t in [0.25, 0.5, 1.0] {
            ed.frame(
                app,
                &AllowAll,
                input(pointer(start + (to - start) * t, None)),
            )
            .unwrap();
            assert!(ed.editor.gizmo_dragging());
            assert_eq!(app.history().log().len(), log, "an edit while dragging");
        }
        ed.frame(app, &AllowAll, input(last)).unwrap();
        ed.frame(app, &AllowAll, input(vec![])).unwrap();
    };

    // Escape cancels: nothing is sent, nothing moved.
    drag(&mut ed, &mut app, vec![key(egui::Key::Escape)]);
    ed.frame(&mut app, &AllowAll, input(pointer(to, Some(false))))
        .unwrap();
    assert!(!ed.editor.gizmo_dragging());
    assert_eq!(app.history().log().len(), log, "{:?}", ed.editor.console());
    assert_eq!(translation(&app, "box"), Vec3::new(0.0, 4.0, 0.0));

    drag(&mut ed, &mut app, pointer(to, Some(false)));
    assert!(!ed.editor.gizmo_dragging());
    assert_eq!(
        app.history().log().len(),
        log + 1,
        "{:?}",
        ed.editor.console()
    );
    let entry = app.history().log().last().unwrap();
    assert_eq!(entry.label, "move box");
    assert_eq!(entry.ops.len(), 1, "only the translation: {:?}", entry.ops);
    let t = translation(&app, "box");
    assert!(t.x > 0.5, "moved along X: {t}");
    assert_eq!((t.y, t.z), (4.0, 0.0), "only along X");

    let out = ed.editor.undo(&mut app, &AllowAll);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(translation(&app, "box"), Vec3::new(0.0, 4.0, 0.0));
    assert_eq!(scene_text(dir.path()), before);
}

/// A child's gizmo edit is written in its parent's space: dragging the
/// crate under `crates` (moved to x = 1) sets the crate's local translation.
#[test]
fn a_gizmo_drag_on_a_child_sets_its_local_transform() {
    use re_editor::gizmo::Handle;
    let dir = project();
    let mut app = open(dir.path(), false);
    let out = app.dispatch(
        "entity.set",
        json!({ "id": "crates", "component": "Transform", "field": "translation", "value": [1.0, 0.0, 0.0] }),
        false,
        &AllowAll,
        re_command::Origin::User,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.select(Some("crate_1"));
    // Up the Y arrow.
    for _ in 0..2 {
        ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    }
    let start = ed
        .editor
        .gizmo_handle(&app, Handle::Axis(1))
        .expect("the Y arrow");
    let to = start - egui::vec2(0.0, 80.0);
    ed.frame(&mut app, &AllowAll, input(pointer(start, Some(true))))
        .unwrap();
    for t in [0.5, 1.0] {
        ed.frame(
            &mut app,
            &AllowAll,
            input(pointer(start + (to - start) * t, None)),
        )
        .unwrap();
        assert!(
            ed.editor.gizmo_dragging(),
            "{start:?} {:?}",
            ed.editor.rects()
        );
    }
    ed.frame(&mut app, &AllowAll, input(pointer(to, Some(false))))
        .unwrap();
    let t = translation(&app, "crate_1");
    assert_eq!((t.x, t.z), (2.0, 0.0), "local X is kept: {t}");
    assert!(t.y > 0.6, "moved up: {t}");
}

/// The dock layout and gizmo settings are saved per project and read back
/// by the next editor; an unreadable file gives the default layout and an
/// `RE8804` warning, and is replaced when the layout changes.
#[test]
fn the_layout_is_saved_per_project() {
    use re_editor::gizmo::GizmoMode;
    use re_editor::layout::{self, PanelId};
    let dir = project();
    let path = layout::layout_path(dir.path());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "{ not json").unwrap();

    let mut app = open(dir.path(), false);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    assert!(
        ed.editor
            .console()
            .iter()
            .any(|l| l.text.contains("RE8804")),
        "{:?}",
        ed.editor.console()
    );
    ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{ not json",
        "kept until a change"
    );
    assert!(ed.editor.rects().get(Panel::Console).is_some());

    // R picks the scale gizmo; the console moves into the hierarchy's place.
    ed.frame(&mut app, &AllowAll, input(vec![key(egui::Key::R)]))
        .unwrap();
    assert_eq!(ed.editor.gizmo_settings().mode, GizmoMode::Scale);
    let dock = ed.editor.dock_mut();
    let at = dock.find_tab(&PanelId::Console).unwrap();
    dock.remove_tab(at);
    let hier = dock.find_tab(&PanelId::Hierarchy).unwrap().node_path();
    dock.set_focused_node_and_surface(hier);
    dock.push_to_focused_leaf(PanelId::Console);
    ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"Scale\""), "{text}");
    drop(ed);

    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    assert!(
        !ed.editor
            .console()
            .iter()
            .any(|l| l.text.contains("RE8804"))
    );
    assert_eq!(ed.editor.gizmo_settings().mode, GizmoMode::Scale);
    let dock = ed.editor.dock();
    let console = dock.find_tab(&PanelId::Console).unwrap().node_path();
    let hierarchy = dock.find_tab(&PanelId::Hierarchy).unwrap().node_path();
    assert_eq!(
        console, hierarchy,
        "the console shares the hierarchy's leaf"
    );
    // One leaf, two tabs: the console is the one shown.
    assert!(ed.editor.rects().get(Panel::Console).is_some());
    assert!(ed.editor.rects().get(Panel::Hierarchy).is_none());
    drop(ed);

    // A small window clamps the splits, but that is not a change to save.
    let saved = std::fs::read_to_string(&path).unwrap();
    let mut ed = HeadlessEditor::new(&mut app, 500, 300).expect("a headless GPU");
    for _ in 0..4 {
        ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    }
    ed.frame(&mut app, &AllowAll, input(click(egui::pos2(250.0, 10.0))))
        .unwrap();
    ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), saved);
}

/// The debugger panel draws a recorded stop: a script VM with a breakpoint
/// and no DAP client records its stack, and the panel (brought to the front
/// of its leaf) shows it.
#[test]
fn the_debugger_panel_shows_a_recorded_stop() {
    use re_editor::layout::PanelId;
    use re_script::debug::{self, Debugger};
    let dir = project();
    let mut app = open(dir.path(), false);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    let debugger = Debugger::new();
    ed.editor.set_debugger(Some(debugger.clone()));

    let lua = mlua::Lua::new();
    debugger.attach(&lua, "game", None).unwrap();
    let bytecode = mlua::chunk::Compiler::new()
        .set_debug_level(2)
        .compile("local speed = 12\nlocal t = { a = 1 }\nreturn speed\n")
        .unwrap();
    let chunk = lua
        .load(&bytecode[..])
        .set_name("@scripts/probe.luau")
        .into_function()
        .unwrap();
    debug::register_chunk(&lua, "scripts/probe.luau", &chunk);
    debugger.set_breakpoints("scripts/probe.luau", &[3]);
    debug::service(&lua);
    assert_eq!(chunk.call::<i64>(()).unwrap(), 12);
    let s = debugger.last_unpaused().expect("a recorded stop");
    assert_eq!(s.local("speed").unwrap().value, "12");

    // The first frame reads the project's layout; rearrange after it.
    ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    let dock = ed.editor.dock_mut();
    let at = dock.find_tab(&PanelId::Debugger).unwrap();
    dock.set_active_tab(at).unwrap();
    for _ in 0..2 {
        ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    }
    // The console shares the leaf and is now behind the debugger.
    assert!(ed.editor.rects().get(Panel::Console).is_none());
    assert!(ed.editor.debugger().is_some());
}
