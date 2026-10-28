//! Spline handles (ADR-0044 amendment §8): the control points of every
//! `path.spline` node in the graph of the active procedural source, or of
//! the source that generates the active entity, are handles over the
//! viewport, joined by the control polygon.
//!
//! A handle is dragged across the level it is at in the source's frame, so
//! the point's height stays as written, and the release sends one
//! `graph.set_param {source, node, param: "points"}` with the whole list:
//! one undo step per drag. Across, the point is kept to the millimetre.
//! Escape drops a drag. A handle under the pointer comes before the gizmo
//! (the viewport asks [`claims`] first, and leaves the gizmo unlit), so a
//! point at the source's origin can be moved; Alt (which orbits) never takes
//! a handle.
//!
//! A handle is drawn where its point is written. With a `field` connected,
//! the road runs that far above the terrain instead, so the handle can be
//! under or over the road it shapes.

use egui::{Color32, CursorIcon, Id, Key, Modifiers, Pos2, Stroke};
use glam::{Affine3A, Vec2, Vec3};
use re_app::App;
use re_proc::GraphStore;
use re_reflect::Value;
use re_scene::{Entity, Generated, GlobalTransform, Procedural, World};
use serde_json::{Map, Value as Json, json};

use super::{ProceduralSpot, record, set_param};
use crate::camera::View;
use crate::editor::{Action, Editor};
use crate::gizmo::math::Screen;

/// The node kind whose control points are handles.
const KIND: &str = "path.spline";
/// A handle's radius, in points.
const RADIUS: f32 = 5.0;
/// How near a press must be to a handle to take it, in points.
const REACH: f32 = 9.0;
/// How far along the pointer's ray a drag reaches, in metres: beyond it (at
/// a grazing angle to the level) the point stays where it last was.
const FAR: f32 = 10_000.0;

const LINE: Color32 = Color32::from_rgb(230, 180, 60);
const HANDLE: Color32 = Color32::from_rgb(250, 250, 250);
const HOVERED: Color32 = Color32::from_rgb(255, 210, 80);
const DRAGGED: Color32 = Color32::from_rgb(255, 140, 40);

/// One `path.spline` node's control points, as the source uses them.
struct Spline {
    node: String,
    /// The points as written: `[x, y, z]`, or an object with `position`.
    points: Vec<Json>,
    /// Their positions in the source's frame.
    at: Vec<Vec3>,
    /// The last point joins the first.
    closed: bool,
}

/// A handle being dragged, kept in egui's memory between frames.
#[derive(Clone, Debug)]
struct Drag {
    source: String,
    node: String,
    index: usize,
    /// Where the point was when the drag began, in the source's frame.
    from: Vec3,
    /// Where the pointer's ray met the point's level then, in the source's
    /// frame.
    grab: Vec3,
    /// Where the point is now, in the source's frame.
    to: Vec3,
}

fn drag_id() -> Id {
    Id::new("re_editor.spline_drag")
}

/// The active source's splines, as the handles are drawn from them.
struct Shown<'a> {
    screen: &'a Screen,
    /// The source's id.
    id: &'a str,
    /// The source's frame (its world transform).
    frame: Affine3A,
    overrides: Map<String, Json>,
    splines: Vec<Spline>,
}

/// The splines of the active selection's source, when it has any and the
/// view has an area.
fn shown<'a>(ed: &Editor, app: &'a App, view: &'a View) -> Option<Shown<'a>> {
    let world = app.world();
    let screen = view.screen.as_ref()?;
    let (source, id) = source_of(world, ed.selection.active()?)?;
    let frame = world.ecs.get::<GlobalTransform>(source)?.0;
    let overrides = overrides_of(world, id);
    let splines = splines(world, id, &overrides);
    (!splines.is_empty()).then_some(Shown {
        screen,
        id,
        frame,
        overrides,
        splines,
    })
}

impl Shown<'_> {
    /// Where the point at `local` (in the source's frame) is drawn.
    fn project(&self, local: Vec3) -> Option<Pos2> {
        let at = self.screen.project(self.frame.transform_point3(local))?;
        Some(Pos2::new(at.x, at.y))
    }

    /// The handle in reach of `p`, the nearest: (spline, point).
    fn under(&self, p: Pos2) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize, f32)> = None;
        for (si, s) in self.splines.iter().enumerate() {
            for (pi, local) in s.at.iter().enumerate() {
                let Some(at) = self.project(*local) else {
                    continue;
                };
                let dist = at.distance(p);
                if dist <= REACH && best.is_none_or(|(_, _, b)| dist < b) {
                    best = Some((si, pi, dist));
                }
            }
        }
        best.map(|(si, pi, _)| (si, pi))
    }

    /// Where the pointer at `p` meets the level `y` of the source's frame,
    /// in that frame.
    fn level(&self, p: Pos2, y: f32) -> Option<Vec3> {
        let (o, d) = self.screen.ray(Vec2::new(p.x, p.y));
        let inv = self.frame.inverse();
        let (lo, ld) = (inv.transform_point3(o), inv.transform_vector3(d));
        if ld.y.abs() < 1e-6 {
            return None;
        }
        let t = (y - lo.y) / ld.y;
        let hit = lo + ld * t;
        let reach = (self.frame.transform_point3(hit) - o).length();
        (t > 0.0 && reach <= FAR).then_some(hit)
    }
}

/// True when the spline handles take the pointer before the gizmo: while
/// one is dragged, or (with no gizmo drag running) while the pointer is in
/// reach of one. The viewport then keeps the gizmo and its own clicks out.
pub(super) fn claims(ed: &Editor, ui: &egui::Ui, app: &App, view: &View) -> bool {
    let Some(shown) = shown(ed, app, view) else {
        return false;
    };
    if ui
        .data(|d| d.get_temp::<Drag>(drag_id()))
        .is_some_and(|d| d.source == shown.id)
    {
        return true;
    }
    if ed.gizmo.drag.is_some() || ed.nav.active() {
        return false;
    }
    ui.input(|i| i.pointer.latest_pos())
        .filter(|p| view.rect.contains(*p))
        .is_some_and(|p| shown.under(p).is_some())
}

/// Draws the handles of the active source's splines over `view`, and moves
/// one with the pointer. True while the pointer is on a handle or drags one,
/// so the viewport does not also act on it.
pub(super) fn overlay(
    ed: &mut Editor,
    ui: &mut egui::Ui,
    app: &App,
    view: &View,
    actions: &mut Vec<Action>,
) -> bool {
    let mut drag: Option<Drag> = ui.data(|d| d.get_temp(drag_id()));
    let Some(shown) = shown(ed, app, view) else {
        ui.data_mut(|d| d.remove::<Drag>(drag_id()));
        return false;
    };
    let id = shown.id;
    // A drag whose point is gone (the selection or the graph changed) is
    // dropped.
    if drag.as_ref().is_some_and(|d| {
        d.source != id
            || !shown
                .splines
                .iter()
                .any(|s| s.node == d.node && d.index < s.at.len())
    }) {
        drag = None;
    }

    // The gizmo comes first unless the handles claimed the pointer
    // ([`claims`]), which leaves it unlit.
    let busy = ed.nav.active() || ed.gizmo.drag.is_some() || ed.gizmo.hovered.is_some();
    let (pointer, pressed, down) = ui.input(|i| {
        (
            i.pointer.latest_pos(),
            i.pointer.primary_pressed() && !i.modifiers.alt,
            i.pointer.primary_down(),
        )
    });

    // The drag goes on while the button is held; Escape drops it.
    let mut taken = false;
    if let Some(d) = drag.as_mut() {
        taken = true;
        if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            drag = None;
        } else if down {
            if let Some(hit) = pointer.and_then(|p| shown.level(p, d.from.y)) {
                d.to = d.from + (hit - d.grab);
                d.to.y = d.from.y;
            }
        } else {
            finish(d, &shown, app, actions);
            drag = None;
        }
    }

    let hovered = pointer
        .filter(|p| view.rect.contains(*p) && drag.is_none() && !busy)
        .and_then(|p| shown.under(p));
    if let (Some((si, pi)), true, Some(p)) = (hovered, pressed, pointer) {
        let s = &shown.splines[si];
        let from = s.at[pi];
        if let Some(grab) = shown.level(p, from.y) {
            drag = Some(Drag {
                source: id.to_string(),
                node: s.node.clone(),
                index: pi,
                from,
                grab,
                to: from,
            });
        }
    }
    taken |= hovered.is_some() || drag.is_some();

    // Where each handle is drawn: a dragged one where it is being moved.
    let place = |s: &Spline, pi: usize| match &drag {
        Some(d) if d.node == s.node && d.index == pi => d.to,
        _ => s.at[pi],
    };
    let painter = ui.painter_at(view.rect);
    for s in &shown.splines {
        let mut line: Vec<Pos2> = (0..s.at.len())
            .filter_map(|pi| shown.project(place(s, pi)))
            .collect();
        // Only a spline drawn whole is joined up.
        if line.len() == s.at.len() {
            if s.closed && line.len() > 2 {
                line.push(line[0]);
            }
            painter.add(egui::Shape::line(line, Stroke::new(1.5, LINE)));
        }
    }
    for (si, s) in shown.splines.iter().enumerate() {
        for pi in 0..s.at.len() {
            let local = place(s, pi);
            let Some(at) = shown.project(local) else {
                continue;
            };
            let dragged = drag
                .as_ref()
                .is_some_and(|d| d.node == s.node && d.index == pi);
            let fill = if dragged {
                DRAGGED
            } else if hovered == Some((si, pi)) {
                HOVERED
            } else {
                HANDLE
            };
            painter.circle_filled(at, RADIUS, fill);
            painter.circle_stroke(at, RADIUS, Stroke::new(1.5, Color32::BLACK));
            let spot = ProceduralSpot::SplinePoint(id, &s.node, pi);
            let rect = egui::Rect::from_center_size(at, egui::Vec2::splat(2.0 * RADIUS));
            record(&mut ed.pending, spot, rect);
            if dragged {
                let text = format!(
                    "{}[{pi}]  {:.3}, {:.3}, {:.3}",
                    s.node, local.x, local.y, local.z
                );
                painter.text(
                    at + egui::vec2(RADIUS + 4.0, -RADIUS - 4.0),
                    egui::Align2::LEFT_BOTTOM,
                    text,
                    egui::FontId::monospace(12.0),
                    Color32::WHITE,
                );
            }
        }
    }
    if drag.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    } else if hovered.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }
    ui.data_mut(|d| {
        if let Some(drag) = drag {
            d.insert_temp(drag_id(), drag);
        } else {
            d.remove::<Drag>(drag_id());
        }
    });
    taken
}

/// The release of drag `d`: when the point moved, one `graph.set_param`
/// (or the `GraphSource.params` write that stands for it) with the whole
/// list of points.
fn finish(d: &Drag, shown: &Shown<'_>, app: &App, actions: &mut Vec<Action>) {
    let Some(s) = shown.splines.iter().find(|s| s.node == d.node) else {
        return;
    };
    let mut points = s.points.clone();
    let Some(point) = points.get_mut(d.index) else {
        return;
    };
    let moved = moved(point, d.to);
    if moved == *point {
        return;
    }
    *point = moved;
    let has_set_param = app.commands().spec("graph.set_param").is_some();
    actions.push(set_param(
        &shown.overrides,
        shown.id,
        &d.node,
        "points",
        Json::Array(points),
        has_set_param,
    ));
}

/// The procedural source of the entity `id` (itself, or the source that
/// generates it) and the source's id, when it has a graph.
fn source_of<'w>(world: &'w World, id: &str) -> Option<(Entity, &'w str)> {
    let e = world.entity(id).ok()?;
    let source = world.ecs.get::<Generated>(e).map_or(e, |g| g.source);
    world.ecs.get::<Procedural>(source)?;
    let registry = &world.registry;
    let graph = registry.try_key("GraphSource")?;
    registry
        .has(&world.ecs, source, graph)
        .then(|| world.id_of(source))
        .flatten()
        .map(|id| (source, id))
}

/// The source's parameter overrides (`GraphSource.params`): empty when it
/// has none, or when they are not a JSON object (the Procedural panel says
/// so).
fn overrides_of(world: &World, id: &str) -> Map<String, Json> {
    match world.get(id, "GraphSource", "params") {
        Ok(Value::Str(s)) if !s.trim().is_empty() => match serde_json::from_str(&s) {
            Ok(Json::Object(m)) => m,
            _ => Map::new(),
        },
        _ => Map::new(),
    }
}

/// The `path.spline` nodes of the source's graph, with the points the
/// source uses: its override, else the graph's. A node whose points are not
/// all readable is left out.
fn splines(world: &World, id: &str, overrides: &Map<String, Json>) -> Vec<Spline> {
    let Some(Ok(doc)) = world
        .ecs
        .get_resource::<GraphStore>()
        .and_then(|store| store.current(world, id))
    else {
        return Vec::new();
    };
    doc.nodes
        .iter()
        .filter(|n| n.kind == KIND)
        .filter_map(|n| {
            let own = overrides.get(&n.id).and_then(Json::as_object);
            let param = |name: &str| own.and_then(|o| o.get(name)).or_else(|| n.params.get(name));
            let points = param("points")?.as_array()?.clone();
            let at = points.iter().map(position).collect::<Option<Vec<_>>>()?;
            Some(Spline {
                node: n.id.clone(),
                points,
                at,
                closed: param("closed").and_then(Json::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

/// A control point's position: `[x, y, z]`, or an object's `position`.
fn position(point: &Json) -> Option<Vec3> {
    let xyz = match point {
        Json::Array(_) => point,
        Json::Object(m) => m.get("position")?,
        _ => return None,
    };
    match xyz.as_array()?.as_slice() {
        [x, y, z] => Some(Vec3::new(
            x.as_f64()? as f32,
            y.as_f64()? as f32,
            z.as_f64()? as f32,
        )),
        _ => None,
    }
}

/// Control point `point`, as written, moved across to `to`: `x` and `z` to
/// the millimetre, its height (and in an object, its width and bank) as
/// written.
fn moved(point: &Json, to: Vec3) -> Json {
    let mm = |v: f32| (f64::from(v) * 1000.0).round() / 1000.0;
    let across = |xyz: &Json| match xyz.as_array().map(Vec::as_slice) {
        Some([_, y, _]) => json!([mm(to.x), y, mm(to.z)]),
        _ => xyz.clone(),
    };
    match point {
        Json::Array(_) => across(point),
        Json::Object(m) => {
            let mut m = m.clone();
            if let Some(xyz) = m.get_mut("position") {
                *xyz = across(xyz);
            }
            Json::Object(m)
        }
        _ => point.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_are_read_as_written() {
        assert_eq!(
            position(&json!([1, 2.5, -3])),
            Some(Vec3::new(1.0, 2.5, -3.0))
        );
        assert_eq!(
            position(&json!({ "position": [4, 0, 1], "width": 9 })),
            Some(Vec3::new(4.0, 0.0, 1.0))
        );
        assert_eq!(position(&json!([1, 2])), None);
        assert_eq!(position(&json!({ "width": 9 })), None);
        assert_eq!(position(&json!("x")), None);
    }

    #[test]
    fn a_moved_point_keeps_its_form_and_height() {
        let to = Vec3::new(3.100_000_4, 99.0, -0.123_456);
        assert_eq!(moved(&json!([1, 2.5, 0]), to), json!([3.1, 2.5, -0.123]));
        assert_eq!(
            moved(&json!({ "position": [1, 7, 0], "bank": 4 }), to),
            json!({ "position": [3.1, 7, -0.123], "bank": 4 })
        );
        assert_eq!(moved(&json!("x"), to), json!("x"));
    }
}
