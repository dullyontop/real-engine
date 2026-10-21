//! Translate, rotate and scale gizmos in the viewport (ADR-0044).
//!
//! The gizmo works on the selected entity's world transform. While a handle
//! is dragged only the gizmo preview moves; when the drag ends the editor
//! sends one `scene.apply` transaction of `set_field` ops on the entity's
//! local `Transform` (converted through its parent), so a drag is one undo
//! step and lands in the log like any edit. A generated entity's edit
//! records an override, as every `set_field` on one does (ADR-0017).
//!
//! The geometry, picking and drag maths are in the `math` module, without egui.

pub(crate) mod math;

use egui::{Color32, Pos2, Sense, Stroke};
use glam::{Affine3A, Quat, Vec2, Vec3};
use re_app::App;
use re_scene::hierarchy::parent;
use re_scene::{GlobalTransform, Transform, World};
use serde_json::{Value as Json, json};

pub use math::{Delta, GizmoMode, GizmoSettings, Handle, Orientation};
use math::{Drag, Frame, Screen};

use crate::editor::{Action, command};

/// The gizmo's state: its settings and the drag in progress.
#[derive(Clone, Debug, Default)]
pub(crate) struct Gizmo {
    pub settings: GizmoSettings,
    pub drag: Option<Active>,
    /// The handle under the cursor in the last frame.
    pub hovered: Option<Handle>,
}

/// A drag in progress on one entity.
#[derive(Clone, Debug)]
pub(crate) struct Active {
    pub drag: Drag,
    pub id: String,
    /// The entity's world transform when the drag started.
    pub world: Affine3A,
    /// Its parent's world transform (identity for a root).
    pub parent: Affine3A,
    /// Its local `Transform` when the drag started: (scale, rotation, translation).
    pub local: (Vec3, Quat, Vec3),
    /// The axes the drag uses.
    pub orientation: Orientation,
    /// The change so far.
    pub delta: Delta,
}

const AXIS_COLOURS: [Color32; 3] = [
    Color32::from_rgb(230, 70, 70),
    Color32::from_rgb(90, 200, 80),
    Color32::from_rgb(70, 130, 240),
];
const VIEW_COLOUR: Color32 = Color32::from_rgb(220, 220, 220);
const ACTIVE_COLOUR: Color32 = Color32::from_rgb(255, 210, 60);

/// The world transforms of the entity `id` and of its parent, and its local
/// transform.
fn transforms(world: &World, id: &str) -> Option<(Affine3A, Affine3A, (Vec3, Quat, Vec3))> {
    let e = world.entity(id).ok()?;
    let local = world.ecs.get::<Transform>(e)?;
    let local = (local.scale, local.rotation, local.translation);
    let own = world.ecs.get::<GlobalTransform>(e)?.0;
    let parent = parent(&world.ecs, e)
        .and_then(|p| world.ecs.get::<GlobalTransform>(p))
        .map_or(Affine3A::IDENTITY, |g| g.0);
    Some((own, parent, local))
}

/// The screen of the viewport at `rect`, from the camera the game view was
/// last drawn with.
fn screen(app: &App, rect: egui::Rect) -> Option<Screen> {
    let cam = app.snapshot().camera;
    Screen::new(
        cam.world,
        cam.fov_y_radians,
        cam.near,
        cam.far,
        Vec2::new(rect.min.x, rect.min.y),
        Vec2::new(rect.width(), rect.height()),
    )
}

fn pos(v: Vec2) -> Pos2 {
    Pos2::new(v.x, v.y)
}

fn vec(p: Pos2) -> Vec2 {
    Vec2::new(p.x, p.y)
}

impl Gizmo {
    /// Draws the gizmo for the entity `selected` over the viewport at `rect`
    /// and handles its drags. A finished drag pushes one `scene.apply`.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        app: &App,
        selected: Option<&str>,
        actions: &mut Vec<Action>,
    ) {
        let response = ui.interact(
            rect,
            ui.id().with("re_editor.gizmo"),
            Sense::click_and_drag(),
        );
        self.hovered = None;
        let (Some(id), Some(screen)) = (selected, screen(app, rect)) else {
            self.drag = None;
            return;
        };
        let Some((world, parent, local)) = transforms(app.world(), id) else {
            self.drag = None;
            return;
        };
        if self.drag.as_ref().is_some_and(|a| a.id != id) {
            self.drag = None; // the selection changed under the drag
        }
        let cursor = ui.input(|i| i.pointer.interact_pos()).map(vec);

        // A drag in progress: follow the cursor, commit on release, cancel on Escape.
        if let Some(active) = &mut self.drag {
            if let Some(c) = cursor {
                active.delta = active.drag.delta(&screen, c, &self.settings);
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.drag = None;
            } else if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
                let active = self.drag.take().expect("a drag");
                if let Some(a) = commit(&active) {
                    actions.push(a);
                }
            }
        } else if let Some(c) = cursor {
            let frame = Frame::new(
                &screen,
                &world,
                self.settings.orientation,
                self.settings.mode,
            );
            self.hovered = math::pick(&screen, &frame, self.settings.mode, c);
            if response.drag_started_by(egui::PointerButton::Primary)
                && let Some(start) = ui.input(|i| i.pointer.press_origin()).map(vec)
                && let Some(handle) = math::pick(&screen, &frame, self.settings.mode, start)
            {
                let drag = Drag {
                    handle,
                    mode: self.settings.mode,
                    frame,
                    start,
                };
                self.drag = Some(Active {
                    delta: drag.delta(&screen, c, &self.settings),
                    drag,
                    id: id.to_string(),
                    world,
                    parent,
                    local,
                    orientation: self.settings.orientation,
                });
            }
        }

        // Draw at the previewed transform.
        // A drag keeps the mode and axes it started with.
        let (shown, active, orientation, mode) = match &self.drag {
            Some(a) => (
                math::apply(&a.world, &a.delta, a.drag.frame.origin),
                Some(a.drag.handle),
                a.orientation,
                a.drag.mode,
            ),
            None => (world, None, self.settings.orientation, self.settings.mode),
        };
        let frame = Frame::new(&screen, &shown, orientation, mode);
        let lit = active.or(self.hovered);
        draw(ui.painter_at(rect), &screen, &frame, mode, lit);
    }
}

/// A point over the viewport at `rect` where the gizmo on entity `id` picks
/// `handle`, in egui points, or `None` when that handle is not shown (no
/// such entity, behind the camera, or not a handle of this mode).
pub(crate) fn handle_point(
    app: &App,
    rect: egui::Rect,
    settings: &GizmoSettings,
    id: &str,
    handle: Handle,
) -> Option<Pos2> {
    let screen = screen(app, rect)?;
    let (world, _, _) = transforms(app.world(), id)?;
    let frame = Frame::new(&screen, &world, settings.orientation, settings.mode);
    let mut candidates = vec![frame.origin];
    for i in 0..3 {
        for t in [0.5, 0.8, 1.0] {
            candidates.push(frame.origin + frame.axes[i] * frame.size * t);
        }
        let quad = frame.plane_quad(i);
        candidates.push(quad.iter().sum::<Vec3>() / 4.0);
        candidates.extend(frame.ring(&screen, Some(i)));
    }
    candidates.extend(frame.ring(&screen, None));
    candidates
        .into_iter()
        .filter_map(|p| screen.project(p))
        .filter(|p| rect.contains(pos(*p)))
        .find(|p| math::pick(&screen, &frame, settings.mode, *p) == Some(handle))
        .map(pos)
}

/// The `scene.apply` that makes a finished drag permanent, or `None` when it
/// changed nothing.
pub(crate) fn commit(active: &Active) -> Option<Action> {
    if active.delta.is_identity() {
        return None;
    }
    let (s0, r0, t0) = active.local;
    let (s, r, t) = math::apply_local(active.local, &active.parent, &active.delta);
    let mut ops = Vec::new();
    let mut field = |name: &str, value: Json| {
        ops.push(json!({
            "op": "set_field",
            "id": active.id,
            "component": "Transform",
            "field": name,
            "value": value,
        }));
    };
    let v3 = |v: Vec3| json!([v.x, v.y, v.z]);
    if t.distance(t0) > 1e-6 {
        field("translation", v3(t));
    }
    if !math::same_rotation(r, r0) {
        field("rotation", json!(r.to_array()));
    }
    if s.distance(s0) > 1e-6 {
        field("scale", v3(s));
    }
    if ops.is_empty() {
        return None;
    }
    let verb = match active.drag.mode {
        GizmoMode::Translate => "move",
        GizmoMode::Rotate => "rotate",
        GizmoMode::Scale => "scale",
    };
    Some(command(
        "scene.apply",
        json!({ "label": format!("{verb} {}", active.id), "ops": ops }),
    ))
}

fn draw(
    painter: egui::Painter,
    screen: &Screen,
    frame: &Frame,
    mode: GizmoMode,
    lit: Option<Handle>,
) {
    let Some(o) = screen.project(frame.origin) else {
        return;
    };
    let colour = |h: Handle, base: Color32| if lit == Some(h) { ACTIVE_COLOUR } else { base };
    let line = |a: Vec2, b: Vec2, c: Color32, w: f32| {
        painter.line_segment([pos(a), pos(b)], Stroke::new(w, c))
    };
    match mode {
        GizmoMode::Translate => {
            for (n, &base) in AXIS_COLOURS.iter().enumerate() {
                let quad: Option<Vec<Pos2>> = frame
                    .plane_quad(n)
                    .iter()
                    .map(|c| screen.project(*c).map(pos))
                    .collect();
                if let Some(quad) = quad {
                    let c = colour(Handle::Plane(n), base);
                    painter.add(egui::Shape::convex_polygon(
                        quad,
                        c.gamma_multiply(0.35),
                        Stroke::new(1.0, c),
                    ));
                }
            }
            for (i, &base) in AXIS_COLOURS.iter().enumerate() {
                let Some(tip) = screen.project(frame.origin + frame.axes[i] * frame.size) else {
                    continue;
                };
                let c = colour(Handle::Axis(i), base);
                line(o, tip, c, 3.0);
                // The arrow head.
                let dir = (tip - o).normalize_or_zero();
                let side = dir.perp() * 5.0;
                painter.add(egui::Shape::convex_polygon(
                    vec![pos(tip + dir * 12.0), pos(tip + side), pos(tip - side)],
                    c,
                    Stroke::NONE,
                ));
            }
        }
        GizmoMode::Rotate => {
            for (h, axis, base) in [
                (Handle::Axis(0), Some(0), AXIS_COLOURS[0]),
                (Handle::Axis(1), Some(1), AXIS_COLOURS[1]),
                (Handle::Axis(2), Some(2), AXIS_COLOURS[2]),
                (Handle::View, None, VIEW_COLOUR),
            ] {
                let points: Option<Vec<Pos2>> = frame
                    .ring(screen, axis)
                    .iter()
                    .map(|p| screen.project(*p).map(pos))
                    .collect();
                if let Some(points) = points {
                    painter.add(egui::Shape::line(points, Stroke::new(2.5, colour(h, base))));
                }
            }
        }
        GizmoMode::Scale => {
            for (i, &base) in AXIS_COLOURS.iter().enumerate() {
                let Some(tip) = screen.project(frame.origin + frame.axes[i] * frame.size) else {
                    continue;
                };
                let c = colour(Handle::Axis(i), base);
                line(o, tip, c, 3.0);
                painter.rect_filled(
                    egui::Rect::from_center_size(pos(tip), egui::vec2(10.0, 10.0)),
                    1.0,
                    c,
                );
            }
            painter.rect_filled(
                egui::Rect::from_center_size(pos(o), egui::vec2(12.0, 12.0)),
                1.0,
                colour(Handle::View, VIEW_COLOUR),
            );
        }
    }
}

/// The toolbar's gizmo controls: mode, orientation and snapping. Keys W, E
/// and R pick the mode when no text field has the keyboard.
pub(crate) fn toolbar(settings: &mut GizmoSettings, ui: &mut egui::Ui) {
    for (mode, label, key) in [
        (GizmoMode::Translate, "Move", "W"),
        (GizmoMode::Rotate, "Rotate", "E"),
        (GizmoMode::Scale, "Scale", "R"),
    ] {
        if ui
            .selectable_label(settings.mode == mode, label)
            .on_hover_text(format!("{label} gizmo ({key})"))
            .clicked()
        {
            settings.mode = mode;
        }
    }
    let local = settings.orientation == Orientation::Local;
    if ui
        .selectable_label(local, if local { "Local" } else { "World" })
        .on_hover_text(
            "Gizmo axes: the world's or the entity's own (scale always uses the entity's)",
        )
        .clicked()
    {
        settings.orientation = if local {
            Orientation::World
        } else {
            Orientation::Local
        };
    }
    ui.checkbox(&mut settings.snap, "Snap")
        .on_hover_text(format!(
            "Snap drags to {} m, {}° and ×{}",
            settings.translate_step, settings.angle_step_deg, settings.scale_step
        ));
}

/// W, E and R pick the gizmo mode.
pub(crate) fn shortcuts(settings: &mut GizmoSettings, ctx: &egui::Context) {
    if ctx.egui_wants_keyboard_input() {
        return;
    }
    for (key, mode) in [
        (egui::Key::W, GizmoMode::Translate),
        (egui::Key::E, GizmoMode::Rotate),
        (egui::Key::R, GizmoMode::Scale),
    ] {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, key)) {
            settings.mode = mode;
        }
    }
}
