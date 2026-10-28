//! The editor camera and the view's overlays (ADR-0044's amendment for
//! WP42, §2 and §3), on a small project in a temporary directory, headless.
//!
//! The GPU tests need an adapter (lavapipe in CI); run them with
//! `--test-threads=1`.

use std::path::Path;

use egui::{Event, Key, Modifiers, PointerButton, Pos2};
use glam::{Mat4, Quat, Vec3, Vec4Swizzles};
use re_app::{App, AppConfig};
use re_command::AllowAll;
use re_editor::camera::EditorCamera;
use re_editor::gizmo::GizmoMode;
use re_editor::layout;
use re_editor::{HeadlessEditor, Panel, ViewSettings};
use re_reflect::Value;
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
        "MeshRef": { "asset": "builtin:cube" },
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

fn project() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("ev")
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

fn scene_text(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("scenes/main.scene.json")).unwrap()
}

fn input(events: Vec<Event>) -> egui::RawInput {
    egui::RawInput {
        events,
        ..egui::RawInput::default()
    }
}

/// The pointer at `at`, with `button` pressed or released there while
/// `modifiers` are held.
fn button(at: Pos2, button: PointerButton, pressed: bool, modifiers: Modifiers) -> egui::RawInput {
    input(vec![
        Event::ModifiersChanged(modifiers),
        Event::PointerMoved(at),
        Event::PointerButton {
            pos: at,
            button,
            pressed,
            modifiers,
        },
    ])
}

/// A click of the left button at `at`.
fn click(at: Pos2) -> egui::RawInput {
    let mut events = button(at, PointerButton::Primary, true, Modifiers::NONE).events;
    events.extend(button(at, PointerButton::Primary, false, Modifiers::NONE).events);
    input(events)
}

fn moved(at: Pos2, modifiers: Modifiers) -> egui::RawInput {
    input(vec![
        Event::ModifiersChanged(modifiers),
        Event::PointerMoved(at),
    ])
}

fn key(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
    Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers,
    }
}

fn wheel(at: Pos2, lines: f32) -> egui::RawInput {
    input(vec![
        Event::PointerMoved(at),
        Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, lines),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        },
    ])
}

/// Drags with `which` held from `from` by `by`, in four moves.
fn drag(
    ed: &mut HeadlessEditor,
    app: &mut App,
    which: PointerButton,
    modifiers: Modifiers,
    from: Pos2,
    by: egui::Vec2,
) {
    ed.frame(app, &AllowAll, button(from, which, true, modifiers))
        .unwrap();
    for t in [0.25, 0.5, 0.75, 1.0] {
        ed.frame(app, &AllowAll, moved(from + by * t, modifiers))
            .unwrap();
    }
    ed.frame(app, &AllowAll, button(from + by, which, false, modifiers))
        .unwrap();
    ed.frame(app, &AllowAll, moved(from + by, Modifiers::NONE))
        .unwrap();
}

fn camera(ed: &HeadlessEditor) -> EditorCamera {
    *ed.editor.camera().expect("the editor has a camera")
}

fn close(a: Vec3, b: Vec3, within: f32) -> bool {
    a.distance(b) <= within
}

fn frames(ed: &mut HeadlessEditor, app: &mut App, n: usize) {
    for _ in 0..n {
        ed.frame(app, &AllowAll, input(vec![])).unwrap();
    }
}

fn saved_camera(dir: &Path) -> Option<EditorCamera> {
    let text = std::fs::read_to_string(layout::layout_path(dir)).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["version"], 2, "{text}");
    serde_json::from_value(json.get("camera")?.clone()).ok()
}

/// The camera starts where the game's camera is. Orbiting, panning and
/// zooming move it without a command: the scene file, the undo history and
/// `state_hash` stay as they were. The camera is saved in the layout (a
/// zoom once it settles) and read back by the next editor.
#[test]
fn the_camera_moves_without_editing_the_scene_and_is_saved() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);
    let start = camera(&ed);
    assert!(
        close(start.eye(), Vec3::new(0.0, 5.0, 12.0), 1e-4),
        "{start:?}"
    );
    assert!(close(start.forward(), Vec3::NEG_Z, 1e-4), "{start:?}");
    assert_eq!(saved_camera(dir.path()), None, "nothing changed yet");
    let (hash, log, scene) = (
        app.state_hash(),
        app.history().log().len(),
        scene_text(dir.path()),
    );
    let centre = ed.editor.rects().get(Panel::Viewport).unwrap().center();

    // Orbit: the middle button.
    drag(
        &mut ed,
        &mut app,
        PointerButton::Middle,
        Modifiers::NONE,
        centre,
        egui::vec2(100.0, 0.0),
    );
    let orbited = camera(&ed);
    assert!(close(orbited.pivot, start.pivot, 1e-4));
    assert!((orbited.distance - start.distance).abs() < 1e-4);
    assert!(orbited.yaw < start.yaw - 0.3, "{orbited:?}");
    // Saved on release.
    assert_eq!(saved_camera(dir.path()), Some(orbited));

    // Orbit with Alt and the left button, which never starts a gizmo drag.
    drag(
        &mut ed,
        &mut app,
        PointerButton::Primary,
        Modifiers::ALT,
        centre,
        egui::vec2(0.0, 60.0),
    );
    let tilted = camera(&ed);
    assert!(tilted.pitch < orbited.pitch - 0.2, "{tilted:?}");
    // The pointer jumped back to the centre as the button went down: a
    // motion before the press, not part of the orbit.
    assert_eq!(tilted.yaw, orbited.yaw);

    // Pan: Shift and the middle button.
    drag(
        &mut ed,
        &mut app,
        PointerButton::Middle,
        Modifiers::SHIFT,
        centre,
        egui::vec2(-80.0, 40.0),
    );
    let panned = camera(&ed);
    assert!(!close(panned.pivot, tilted.pivot, 0.1), "{panned:?}");
    assert_eq!((panned.yaw, panned.pitch), (tilted.yaw, tilted.pitch));

    // Zoom: two wheel notches in, towards the cursor.
    ed.frame(&mut app, &AllowAll, wheel(centre, 2.0)).unwrap();
    let zoomed = camera(&ed);
    assert!((zoomed.distance - panned.distance * 0.85 * 0.85).abs() < 1e-3);
    // Saved once the zoom has settled (half a second), not every frame.
    frames(&mut ed, &mut app, 5);
    assert_eq!(saved_camera(dir.path()), Some(panned));
    frames(&mut ed, &mut app, 30);
    assert_eq!(saved_camera(dir.path()), Some(zoomed));

    assert_eq!(app.state_hash(), hash, "the camera is not scene state");
    assert_eq!(app.history().log().len(), log);
    assert_eq!(scene_text(dir.path()), scene);
    // The app's own snapshot (which `screenshot` draws) keeps the game's
    // camera and gets no overlays.
    let game = app.snapshot().camera.world;
    assert!(close(game.w_axis.xyz(), Vec3::new(0.0, 5.0, 12.0), 1e-4));
    assert!(app.snapshot().debug_lines.is_empty());

    // Looking through the game's camera: the controls are off.
    ed.editor.set_view_settings(ViewSettings {
        game_camera: true,
        ..ViewSettings::default()
    });
    drag(
        &mut ed,
        &mut app,
        PointerButton::Middle,
        Modifiers::NONE,
        centre,
        egui::vec2(100.0, 0.0),
    );
    assert_eq!(camera(&ed), zoomed);
    drop(ed);

    // The next editor reads the camera and the view settings back.
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 1);
    let back = camera(&ed);
    assert!(close(back.eye(), zoomed.eye(), 1e-4) && close(back.pivot, zoomed.pivot, 1e-4));
    assert!(ed.editor.view_settings().game_camera);

    // A camera that is not finite is replaced by the starting one, where the
    // game's camera is, and never written.
    ed.editor.set_camera(EditorCamera {
        distance: f32::NAN,
        ..back
    });
    frames(&mut ed, &mut app, 1);
    assert!(close(camera(&ed).eye(), Vec3::new(0.0, 5.0, 12.0), 1e-4));
    assert!(saved_camera(dir.path()).is_some_and(|c| c.is_finite()));
}

/// While the right button is held, W, A, S, D, Q and E fly the camera and
/// do not pick the gizmo's mode; the wheel sets the speed.
#[test]
fn flying_moves_the_camera_and_keeps_the_keys_from_the_gizmo() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);
    let mut settings = *ed.editor.gizmo_settings();
    settings.mode = GizmoMode::Scale;
    ed.editor.set_gizmo_settings(settings);
    let start = camera(&ed);
    let centre = ed.editor.rects().get(Panel::Viewport).unwrap().center();

    ed.frame(
        &mut app,
        &AllowAll,
        button(centre, PointerButton::Secondary, true, Modifiers::NONE),
    )
    .unwrap();
    ed.frame(
        &mut app,
        &AllowAll,
        input(vec![key(Key::W, true, Modifiers::NONE)]),
    )
    .unwrap();
    frames(&mut ed, &mut app, 30);
    let flown = camera(&ed);
    let moved = flown.eye() - start.eye();
    assert!(moved.length() > 2.0, "{moved}");
    assert!(moved.normalize().dot(start.forward()) > 0.99, "{moved}");
    assert_eq!(ed.editor.gizmo_settings().mode, GizmoMode::Scale);

    // The wheel while flying sets the speed, not the distance.
    ed.frame(&mut app, &AllowAll, wheel(centre, 1.0)).unwrap();
    let faster = camera(&ed);
    assert!(
        (faster.speed - start.speed * 1.2).abs() < 1e-3,
        "{faster:?}"
    );

    ed.frame(
        &mut app,
        &AllowAll,
        input(vec![key(Key::W, false, Modifiers::NONE)]),
    )
    .unwrap();
    ed.frame(
        &mut app,
        &AllowAll,
        button(centre, PointerButton::Secondary, false, Modifiers::NONE),
    )
    .unwrap();
    // Not flying: W picks the move gizmo again.
    ed.frame(
        &mut app,
        &AllowAll,
        input(vec![key(Key::W, true, Modifiers::NONE)]),
    )
    .unwrap();
    assert_eq!(ed.editor.gizmo_settings().mode, GizmoMode::Translate);
}

/// F frames the selection's bounds, an entity without geometry as a metre
/// around its origin; Shift+F frames everything drawn. Framing leaves the
/// game's camera view.
#[test]
fn f_frames_the_selection_and_shift_f_everything() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    let out = ed.editor.run(
        &mut app,
        &AllowAll,
        "entity.spawn",
        json!({ "id": "far", "components": {
            "MeshRef": { "asset": "builtin:cube" },
            "Transform": { "translation": [40.0, 1.0, -30.0] } } }),
        false,
    );
    assert!(out.ok, "{:?}", out.diagnostics);
    ed.editor.select(Some("box"));
    ed.editor.set_view_settings(ViewSettings {
        game_camera: true,
        ..ViewSettings::default()
    });
    frames(&mut ed, &mut app, 2);
    let hash = app.state_hash();

    let f = |ed: &mut HeadlessEditor, app: &mut App, modifiers: Modifiers| {
        ed.frame(app, &AllowAll, input(vec![key(Key::F, true, modifiers)]))
            .unwrap();
        camera(ed)
    };
    let boxed = f(&mut ed, &mut app, Modifiers::NONE);
    assert!(
        close(boxed.pivot, Vec3::new(0.0, 4.0, 0.0), 1e-3),
        "{boxed:?}"
    );
    // The cube's bounding sphere (radius √3 / 2) fills the view.
    assert!(boxed.distance > 1.7 && boxed.distance < 2.2, "{boxed:?}");
    assert!(!ed.editor.view_settings().game_camera);
    // The box fills the middle of the view.
    let at = ed
        .editor
        .screen_point(&app, Vec3::new(0.0, 4.0, 0.0))
        .unwrap();
    let rect = ed.editor.rects().get(Panel::Viewport).unwrap();
    assert!((at - rect.center()).length() < 2.0, "{at:?} {rect:?}");

    let all = f(&mut ed, &mut app, Modifiers::SHIFT);
    assert!(
        close(all.pivot, Vec3::new(20.0, 2.5, -15.0), 1e-3),
        "{all:?}"
    );
    assert!(all.distance > 30.0, "{all:?}");

    ed.editor.select(Some("crate_1"));
    let crate_1 = f(&mut ed, &mut app, Modifiers::NONE);
    assert!(
        close(crate_1.pivot, Vec3::new(2.0, 0.5, 0.0), 1e-3),
        "{crate_1:?}"
    );
    assert!(crate_1.distance > 1.7 && crate_1.distance < 2.2);
    assert_eq!(app.state_hash(), hash);
}

/// The grid and the selection's outline are drawn into the editor's view
/// only, and each has its toggle; icons and the axis indicator answer
/// clicks: an icon selects its light, an axis end looks along the axis.
#[test]
fn the_overlays_draw_in_the_view_and_answer_clicks() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.select(Some("box"));
    frames(&mut ed, &mut app, 2);
    let rect = ed.editor.rects().get(Panel::Viewport).unwrap();
    let shown = ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    let differ = |a: &re_render::Image, b: &re_render::Image, r: egui::Rect| {
        let (x0, y0) = (r.left().max(0.0) as u32, r.top().max(0.0) as u32);
        let (x1, y1) = (r.right() as u32, r.bottom() as u32);
        (y0..y1)
            .flat_map(|y| (x0..x1).map(move |x| (x, y)))
            .filter(|&(x, y)| a.pixel(x, y) != b.pixel(x, y))
            .count()
    };

    // The grid lies on the ground, below the horizon.
    let ground = egui::Rect::from_min_max(
        rect.left_center() + egui::vec2(10.0, 40.0),
        rect.right_bottom() - egui::vec2(10.0, 10.0),
    );
    let mut view = *ed.editor.view_settings();
    view.grid = false;
    ed.editor.set_view_settings(view);
    let no_grid = ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    assert!(differ(&shown, &no_grid, ground) > 200, "no grid drawn");

    // The outline goes round the selected box.
    let at = ed
        .editor
        .screen_point(&app, Vec3::new(0.0, 4.0, 0.0))
        .unwrap();
    let around = egui::Rect::from_center_size(at, egui::vec2(160.0, 160.0));
    view.outline = false;
    ed.editor.set_view_settings(view);
    let no_outline = ed.frame(&mut app, &AllowAll, input(vec![])).unwrap();
    assert!(
        differ(&no_grid, &no_outline, around) > 20,
        "no outline drawn"
    );
    assert!(app.snapshot().debug_lines.is_empty());

    // The sun's icon, at the origin, selects it.
    let sun = ed.editor.screen_point(&app, Vec3::ZERO).unwrap();
    assert!(rect.contains(sun), "{sun:?}");
    ed.frame(&mut app, &AllowAll, click(sun)).unwrap();
    frames(&mut ed, &mut app, 2);
    assert_eq!(ed.editor.selected(), Some("sun"));

    // The axis indicator's X end, right of its centre while the camera
    // looks along -Z: a click looks along -X, from the +X side.
    assert!(close(camera(&ed).forward(), Vec3::NEG_Z, 1e-4));
    let x_end = rect.right_top() + egui::vec2(-42.0 + 30.0, 42.0);
    ed.frame(&mut app, &AllowAll, click(x_end)).unwrap();
    assert!(
        close(camera(&ed).forward(), Vec3::NEG_X, 1e-4),
        "{:?}",
        camera(&ed)
    );
    assert_eq!(
        ed.editor.selected(),
        Some("sun"),
        "not a click on the scene"
    );
}

/// "Align game camera with view" moves the game's active camera to the
/// editor camera's pose in one undo step.
#[test]
fn aligning_the_game_camera_is_one_undo_step() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 2);
    let mut cam = camera(&ed);
    cam.pivot = Vec3::new(3.0, 1.0, -2.0);
    cam.yaw = 0.7;
    cam.pitch = -0.4;
    cam.distance = 8.0;
    ed.editor.set_camera(cam);
    let out = ed
        .editor
        .align_game_camera(&mut app, &AllowAll)
        .expect("the scene has an active camera");
    assert!(out.ok, "{:?}", out.diagnostics);
    assert_eq!(
        app.history().undo_label(),
        Some("align camera with the view")
    );
    let Value::Vec3(t) = app
        .world()
        .get("camera", "Transform", "translation")
        .unwrap()
    else {
        panic!("a translation");
    };
    let Value::Quat(r) = app.world().get("camera", "Transform", "rotation").unwrap() else {
        panic!("a rotation");
    };
    assert!(close(t, cam.eye(), 1e-4), "{t} {}", cam.eye());
    assert!(r.angle_between(cam.rotation()) < 1e-3, "{r}");
    // The game's view now matches the editor's.
    frames(&mut ed, &mut app, 1);
    let game: Mat4 = app.snapshot().camera.world;
    assert!(close(game.w_axis.xyz(), cam.eye(), 1e-3));
    assert!(Quat::from_mat4(&game).angle_between(cam.rotation()) < 1e-3);

    let out = ed.editor.undo(&mut app, &AllowAll);
    assert!(out.ok, "{:?}", out.diagnostics);
    assert!(!app.history().can_undo(), "one step");
    let Value::Vec3(t) = app
        .world()
        .get("camera", "Transform", "translation")
        .unwrap()
    else {
        panic!("a translation");
    };
    assert_eq!(t, Vec3::new(0.0, 5.0, 12.0));

    // Looking through the game's camera there is nothing to align.
    ed.editor.set_view_settings(ViewSettings {
        game_camera: true,
        ..ViewSettings::default()
    });
    assert!(ed.editor.align_game_camera(&mut app, &AllowAll).is_none());
}

/// The selection is saved with the layout and restored by the next editor,
/// unless its entity no longer exists.
#[test]
fn the_selection_is_restored_when_its_entity_exists() {
    let dir = project();
    let mut app = open(dir.path());
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    ed.editor.select(Some("crate_1"));
    frames(&mut ed, &mut app, 1);
    drop(ed);
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 1);
    assert_eq!(ed.editor.selected(), Some("crate_1"));
    drop(ed);

    let path = layout::layout_path(dir.path());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"crate_1\""), "{text}");
    std::fs::write(&path, text.replace("\"crate_1\"", "\"gone\"")).unwrap();
    let mut ed = HeadlessEditor::new(&mut app, 1280, 720).expect("a headless GPU");
    frames(&mut ed, &mut app, 1);
    assert_eq!(ed.editor.selected(), None);
}
