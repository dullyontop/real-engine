//! Translate, rotate and scale gizmos in the viewport (ADR-0044, and its
//! amendment for WP42, §4).
//!
//! The gizmo works on the selection: it moves the topmost selected entities
//! (one whose ancestor is also selected moves with that ancestor). It stands
//! at the active entity's origin, or at the centre of the selection's bounds
//! ([`GizmoPivot`]), and its local axes are the active entity's. A move adds
//! the drag's offset to each entity; a rotation or a scale acts about the
//! gizmo, and a scale multiplies each entity's own scale along its own axes.
//!
//! While a handle is dragged the entities move with it: the editor sends one
//! `scene.apply` of `set_field` ops on every moved entity's local
//! `Transform` (converted through its parent) whenever the drag changed, at
//! most every [`LIVE_INTERVAL`] seconds, and once more on release. Every one
//! carries the drag's own `coalesce` key, so the history merges them and the
//! whole drag is one undo step however many entities move; each lands in the
//! log like any edit. Escape puts them back (undoing the drag's step). A
//! generated entity's edit records an override, as every `set_field` on one
//! does (ADR-0017).
//!
//! The geometry, picking and drag maths are in the `math` module, without egui.

pub(crate) mod math;

use egui::{Color32, Pos2, Stroke};
use glam::{Affine3A, Quat, Vec2, Vec3};
use re_app::App;
use re_scene::hierarchy::parent;
use re_scene::{GlobalTransform, Transform, World};
use serde_json::{Value as Json, json};

pub use math::{Delta, GizmoMode, GizmoPivot, GizmoSettings, Handle, Orientation};
use math::{Drag, Frame, Screen};

use crate::camera::View;
use crate::editor::{Action, command};
use crate::selection::Selection;

/// The shortest time between two live edits of one drag, in seconds; every
/// edit rewrites the scene file.
pub(crate) const LIVE_INTERVAL: f64 = 0.05;

/// The gizmo's state: its settings and the drag in progress.
#[derive(Clone, Debug, Default)]
pub(crate) struct Gizmo {
    pub settings: GizmoSettings,
    pub drag: Option<Active>,
    /// The handle under the cursor in the last frame.
    pub hovered: Option<Handle>,
    /// Drags started so far; each drag's coalesce key is unique.
    pub drags: u64,
}

/// A drag in progress on the selection.
#[derive(Clone, Debug)]
pub(crate) struct Active {
    pub drag: Drag,
    /// The selection the drag started with; the drag ends if it changes.
    pub selection: Selection,
    /// The entities the drag moves, as they were when it started.
    pub targets: Vec<Target>,
    /// Where the gizmo stood when the drag started (the pivot, turned as the
    /// active entity).
    pub world: Affine3A,
    /// The axes the drag uses.
    pub orientation: Orientation,
    /// The change so far.
    pub delta: Delta,
    /// The key the drag's edits coalesce under.
    pub key: String,
    /// The change the last edit sent, and when (egui time), or `None`
    /// before the first.
    pub sent: Option<(Delta, f64)>,
}

/// One entity a drag moves, as it was when the drag started.
#[derive(Clone, Debug)]
pub(crate) struct Target {
    pub id: String,
    /// Its world transform.
    pub world: Affine3A,
    /// Its parent's world transform (identity for a root).
    pub parent: Affine3A,
    /// Its local `Transform`: (scale, rotation, translation).
    pub local: (Vec3, Quat, Vec3),
    /// The fields the drag has set so far (translation, rotation, scale);
    /// once set, a field is sent with every later edit, so a handle dragged
    /// back to its start puts that field back too.
    pub fields: [bool; 3],
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

/// The entities the gizmo moves for `selection` (the topmost selected ones
/// with a `Transform`) and where it stands: at the active entity's origin,
/// or the centre of the selection's bounds, turned as the active entity (or
/// the first one moved, when the active entity has no transform). `None`
/// when nothing selected has a transform.
fn gizmo_world(
    app: &App,
    selection: &Selection,
    settings: &GizmoSettings,
) -> Option<(Affine3A, Vec<Target>)> {
    let world = app.world();
    let targets: Vec<Target> = selection
        .topmost(world)
        .into_iter()
        .filter_map(|id| {
            let (world, parent, local) = transforms(world, &id)?;
            Some(Target {
                id,
                world,
                parent,
                local,
                fields: [false; 3],
            })
        })
        .collect();
    let first = targets.first()?.world;
    let active = selection
        .active()
        .and_then(|id| transforms(world, id))
        .map_or(first, |(w, _, _)| w);
    let (_, rotation, origin) = active.to_scale_rotation_translation();
    let pivot = match settings.pivot {
        GizmoPivot::Active => origin,
        GizmoPivot::Centre => {
            let ids: Vec<String> = targets.iter().map(|t| t.id.clone()).collect();
            crate::overlay::bounds(app, Some(&ids)).map_or_else(
                || {
                    targets
                        .iter()
                        .map(|t| Vec3::from(t.world.translation))
                        .sum::<Vec3>()
                        / targets.len() as f32
                },
                |b| b.center(),
            )
        }
    };
    Some((
        Affine3A::from_rotation_translation(rotation.normalize(), pivot),
        targets,
    ))
}

fn pos(v: Vec2) -> Pos2 {
    Pos2::new(v.x, v.y)
}

fn vec(p: Pos2) -> Vec2 {
    Vec2::new(p.x, p.y)
}

impl Gizmo {
    /// Draws the gizmo for `selection` over the viewport `view` and handles
    /// its drags (`response` is the viewport's), pushing the edits that move
    /// the entities. While `navigating` (the pointer moves the camera), no
    /// drag starts and no handle is lit.
    #[expect(
        clippy::too_many_arguments,
        reason = "the viewport and its response, the app, the selection and the edits it pushes"
    )]
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        response: &egui::Response,
        view: &View,
        app: &App,
        selection: &Selection,
        navigating: bool,
        actions: &mut Vec<Action>,
    ) {
        self.hovered = None;
        if self
            .drag
            .as_ref()
            .is_some_and(|a| a.selection != *selection)
        {
            self.drag = None; // the selection changed under the drag
        }
        let Some(screen) = view.screen else {
            self.drag = None;
            return;
        };
        let Some((world, targets)) = gizmo_world(app, selection, &self.settings) else {
            self.drag = None;
            return;
        };
        let cursor = ui.input(|i| i.pointer.interact_pos()).map(vec);

        // A drag in progress: follow the cursor and move the entities,
        // finish on release, put them back on Escape.
        if let Some(active) = &mut self.drag {
            if let Some(c) = cursor {
                active.delta = active.drag.delta(&screen, c, &self.settings);
            }
            let now = ui.input(|i| i.time);
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                let active = self.drag.take().expect("a drag");
                actions.extend(cancel(&active, app));
            } else if response.drag_stopped() || !ui.input(|i| i.pointer.primary_down()) {
                let mut active = self.drag.take().expect("a drag");
                actions.extend(edit(&mut active, now));
            } else if active
                .sent
                .is_none_or(|(_, at)| now - at >= LIVE_INTERVAL || now < at)
            {
                actions.extend(edit(active, now));
            }
        } else if let Some(c) = cursor.filter(|_| !navigating) {
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
                self.drags += 1;
                let first = &targets[0].id;
                self.drag = Some(Active {
                    delta: drag.delta(&screen, c, &self.settings),
                    drag,
                    selection: selection.clone(),
                    key: format!("gizmo:{first}:{}", self.drags),
                    targets,
                    world,
                    orientation: self.settings.orientation,
                    sent: None,
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
        draw(ui.painter_at(view.rect), &screen, &frame, mode, lit);
    }
}

/// A point over the viewport `view` where the gizmo on `selection` picks
/// `handle`, in egui points, or `None` when that handle is not shown
/// (nothing to move, behind the camera, or not a handle of this mode).
pub(crate) fn handle_point(
    app: &App,
    view: &View,
    settings: &GizmoSettings,
    selection: &Selection,
    handle: Handle,
) -> Option<Pos2> {
    let screen = view.screen?;
    let rect = view.rect;
    let (world, _) = gizmo_world(app, selection, settings)?;
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

/// The `scene.apply` that brings the entities to where the drag has them
/// now, under the drag's coalesce key, or `None` when nothing changed since
/// the last one (or, before the first, since the drag started).
pub(crate) fn edit(active: &mut Active, now: f64) -> Option<Action> {
    if active.sent.is_some_and(|(d, _)| d == active.delta)
        || (active.sent.is_none() && active.delta.is_identity())
    {
        return None;
    }
    let pivot = active.drag.frame.origin;
    let axes = active.drag.frame.axes;
    let mut ops = Vec::new();
    for target in &mut active.targets {
        let (s0, r0, t0) = target.local;
        let at = Vec3::from(target.world.translation);
        let (s, r, t) =
            math::apply_about(target.local, &target.parent, at, &active.delta, pivot, axes);
        let changed = [
            t.distance(t0) > 1e-6,
            !math::same_rotation(r, r0),
            s.distance(s0) > 1e-6,
        ];
        for (set, changed) in target.fields.iter_mut().zip(changed) {
            *set |= changed;
        }
        ops.extend(transform_ops(&target.id, target.fields, (s, r, t)));
    }
    if ops.is_empty() {
        return None;
    }
    active.sent = Some((active.delta, now));
    let verb = match active.drag.mode {
        GizmoMode::Translate => "move",
        GizmoMode::Rotate => "rotate",
        GizmoMode::Scale => "scale",
    };
    Some(command(
        "scene.apply",
        json!({
            "label": format!("{verb} {}", names(&active.targets)),
            "ops": ops,
            "coalesce": active.key,
        }),
    ))
}

/// What the edit labels name: the one entity's id, or how many.
fn names(targets: &[Target]) -> String {
    match targets {
        [one] => one.id.clone(),
        _ => format!("{} entities", targets.len()),
    }
}

/// What puts the entities back where a cancelled drag started: an undo of
/// the drag's step while it is the latest, otherwise an edit setting the
/// fields back (another writer's edit came between). `None` when the drag
/// sent nothing.
fn cancel(active: &Active, app: &App) -> Option<Action> {
    active.sent?;
    if app.history().undo_key() == Some(active.key.as_str()) {
        return Some(command("undo", json!({})));
    }
    let ops: Vec<Json> = active
        .targets
        .iter()
        .flat_map(|t| transform_ops(&t.id, t.fields, t.local))
        .collect();
    Some(command(
        "scene.apply",
        json!({
            "label": format!("cancel the drag of {}", names(&active.targets)),
            "ops": ops,
        }),
    ))
}

/// `set_field` ops setting the `fields` (translation, rotation, scale) of
/// entity `id`'s `Transform` to `local` (scale, rotation, translation).
fn transform_ops(id: &str, fields: [bool; 3], (s, r, t): (Vec3, Quat, Vec3)) -> Vec<Json> {
    let v3 = |v: Vec3| json!([v.x, v.y, v.z]);
    let values = [
        ("translation", v3(t)),
        ("rotation", json!(r.to_array())),
        ("scale", v3(s)),
    ];
    values
        .into_iter()
        .zip(fields)
        .filter(|(_, set)| *set)
        .map(|((name, value), _)| {
            json!({
                "op": "set_field",
                "id": id,
                "component": "Transform",
                "field": name,
                "value": value,
            })
        })
        .collect()
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

/// The toolbar's gizmo controls: mode, orientation, pivot and snapping. Keys
/// W, E and R pick the mode when no text field has the keyboard.
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
            "Gizmo axes: the world's or the active entity's own (scale always uses the entity's)",
        )
        .clicked()
    {
        settings.orientation = if local {
            Orientation::World
        } else {
            Orientation::Local
        };
    }
    let centre = settings.pivot == GizmoPivot::Centre;
    if ui
        .selectable_label(centre, if centre { "Centre" } else { "Pivot" })
        .on_hover_text(
            "Where the gizmo stands for several entities: the active one's origin (Pivot) or the centre of their bounds (Centre)",
        )
        .clicked()
    {
        settings.pivot = if centre {
            GizmoPivot::Active
        } else {
            GizmoPivot::Centre
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
