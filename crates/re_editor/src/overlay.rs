//! Overlays only the editor sees (ADR-0044's amendment for WP42, §3): the
//! grid, the axis indicator, icons for lights and cameras, a selected
//! camera's frustum or light's reach, and the selection's outline.
//!
//! The grid and the selected entities' shapes are debug lines and the outline
//! a highlight, added to the editor's own copy of the render snapshot
//! ([`GameView::render`](crate::GameView::render)), so they are depth tested
//! like the scene; the axis indicator and the icons are egui shapes over the
//! viewport. None of them reaches `screenshot`, goldens or replays, which
//! the app draws from its own snapshot.

use std::collections::{HashMap, HashSet};

use egui::{Color32, Pos2, Stroke};
use glam::{Mat4, Quat, Vec2, Vec3, Vec4Swizzles};
use re_render::{Highlight, SceneExtras};
use re_scene::hierarchy::children;
use re_scene::{Camera, Entity, GlobalTransform, Light, LightKind, World};
use re_types::render::{DebugLine, RenderSnapshot};
use serde::{Deserialize, Serialize};

use crate::Editor;
use crate::gizmo::math::Screen;
use crate::selection::Selection;

/// What the view shows, and whether it looks through the game's camera:
/// editor state, saved in `layout.json` (`view`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewSettings {
    /// Draw from the game's active camera, as `real run` does; the camera
    /// controls are off.
    pub game_camera: bool,
    /// The grid on the ground (y = 0).
    pub grid: bool,
    /// The axis indicator in the view's corner.
    pub axes: bool,
    /// Icons for lights and cameras, and a selected one's frustum or reach.
    pub icons: bool,
    /// The selection's outline.
    pub outline: bool,
}

impl Default for ViewSettings {
    fn default() -> Self {
        ViewSettings {
            game_camera: false,
            grid: true,
            axes: true,
            icons: true,
            outline: true,
        }
    }
}

/// Grid lines across one direction, at most (the grid is coarser than its
/// spacing would make it past that).
const GRID_LINES: f32 = 400.0;
/// Pieces each grid line is cut into, so it can fade with distance.
const GRID_PIECES: usize = 16;
const GRID_MINOR: [f32; 4] = [0.45, 0.45, 0.45, 0.35];
const GRID_MAJOR: [f32; 4] = [0.6, 0.6, 0.6, 0.6];
const GRID_X_AXIS: [f32; 4] = [0.85, 0.2, 0.2, 0.9];
const GRID_Z_AXIS: [f32; 4] = [0.2, 0.35, 0.9, 0.9];
/// The colour of a selected camera's frustum and light's reach.
const SHAPE_COLOUR: [f32; 4] = [1.0, 0.75, 0.3, 0.9];
/// The selection's outline and fill, linear RGB and opacity.
pub(crate) const OUTLINE: [f32; 4] = [1.0, 0.45, 0.05, 1.0];
pub(crate) const FILL: [f32; 4] = [1.0, 0.45, 0.05, 0.08];
/// The most selected cameras and lights whose frustum or reach is drawn.
const MAX_REACHES: usize = 32;
/// How near the pointer must be to an icon or an axis end to pick it, in points.
const PICK_POINTS: f32 = 10.0;

/// Dresses the app's `snapshot` (relative to the render `origin`) as the
/// editor's view shows it: drawn from the editor camera unless the view looks
/// through the game's, with the grid and the selected camera's or light's
/// reach as debug lines. Adds the selection's outline to `extras`, the
/// frame's extras for [`Renderer::render_with`](re_render::Renderer::render_with)
/// (empty while native frames have none of their own). A camera's frustum is
/// drawn for a view of width over height `game_aspect`, the game's.
pub(crate) fn dress(
    editor: &Editor,
    world: &World,
    origin: Vec3,
    game_aspect: f32,
    snapshot: &mut RenderSnapshot,
    extras: &mut SceneExtras,
) {
    let view = &editor.view;
    if !view.game_camera
        && let Some(camera) = &editor.camera
    {
        snapshot.camera = camera.render_camera(origin);
    }
    let eye = snapshot.camera.world.w_axis.xyz() + origin;
    let mut lines = Vec::new();
    if view.grid {
        let gizmo = &editor.gizmo.settings;
        lines.extend(grid(eye, gizmo.snap.then_some(gizmo.translate_step)));
    }
    if view.icons {
        // The first few selected, so a box over a thousand lamps stays cheap.
        for id in editor.selection().ids().iter().take(MAX_REACHES) {
            lines.extend(reach(world, id, game_aspect));
        }
    }
    snapshot
        .debug_lines
        .extend(lines.into_iter().map(|l| DebugLine {
            a: l.a - origin,
            b: l.b - origin,
            color: l.color,
        }));
    if view.outline
        && let Some(highlight) = outline(world, editor.selection().ids())
    {
        extras.highlights.push(highlight);
    }
}

/// The grid at y = 0 as seen from `eye`, in world space: minor lines every
/// power of ten metres that keeps about five metres of camera height per
/// line or more (1 m up to 50 m high, 10 m up to 500 m), or every `snap`
/// metres when snapping is on and the step is not too fine to see; major
/// lines every ten minor ones; out to a distance that grows with the
/// camera's height (at most [`GRID_LINES`] lines across), fading towards it.
/// The X axis is red, the Z axis blue. The grid is lifted a little over
/// y = 0, so it does not flicker on a ground drawn there.
pub(crate) fn grid(eye: Vec3, snap: Option<f32>) -> Vec<DebugLine> {
    let height = eye.y.abs().max(1.0);
    let auto = libm::powf(10.0, libm::floorf(libm::log10f(height / 5.0))).max(1.0);
    // The snap step, unless it is too fine to see from this high.
    let minor = snap
        .filter(|s| s.is_finite() && *s >= auto * 0.1)
        .unwrap_or(auto);
    let reach = (height * 20.0)
        .clamp(50.0, 20_000.0)
        .min(minor * GRID_LINES * 0.5);
    let major = minor * 10.0;
    let lift = (height * 1e-4).clamp(1e-3, 0.05);
    let foot = Vec2::new(eye.x, eye.z);
    let centre = (foot / major).round() * major;
    let first = |c: f32| libm::ceilf((c - reach) / minor) as i64;
    let last = |c: f32| libm::floorf((c + reach) / minor) as i64;
    let mut lines = Vec::new();
    // Lines along Z (constant x), then along X (constant z).
    for (axis, c) in [(0usize, centre.x), (1, centre.y)] {
        for i in first(c)..=last(c) {
            let at = i as f32 * minor;
            let colour = if i == 0 {
                if axis == 0 { GRID_Z_AXIS } else { GRID_X_AXIS }
            } else if i.rem_euclid(10) == 0 {
                GRID_MAJOR
            } else {
                GRID_MINOR
            };
            let across = if axis == 0 { centre.y } else { centre.x };
            for p in 0..GRID_PIECES {
                let t0 = across - reach + 2.0 * reach * p as f32 / GRID_PIECES as f32;
                let t1 = across - reach + 2.0 * reach * (p + 1) as f32 / GRID_PIECES as f32;
                let point = |t: f32| {
                    if axis == 0 {
                        Vec3::new(at, lift, t)
                    } else {
                        Vec3::new(t, lift, at)
                    }
                };
                let (a, b) = (point(t0), point(t1));
                let mid = (Vec2::new(a.x, a.z) + Vec2::new(b.x, b.z)) * 0.5;
                let fade = 1.0 - (mid.distance(foot) / reach).min(1.0);
                if fade <= 0.0 {
                    continue;
                }
                let mut color = colour;
                color[3] *= fade * fade;
                lines.push(DebugLine { a, b, color });
            }
        }
    }
    lines
}

/// What an icon stands for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum IconKind {
    Camera,
    Light(LightKind),
}

/// A light or a camera, where its icon goes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Icon {
    pub id: String,
    pub kind: IconKind,
    /// Its world position.
    pub at: Vec3,
}

/// The lights and cameras of `world` with ids, in id order.
pub(crate) fn icons(world: &World) -> Vec<Icon> {
    let mut out: Vec<Icon> = world
        .ids()
        .iter()
        .filter_map(|(id, e)| {
            let kind = if world.ecs.get::<Camera>(e).is_some() {
                IconKind::Camera
            } else {
                IconKind::Light(world.ecs.get::<Light>(e)?.kind)
            };
            let at = world
                .ecs
                .get::<GlobalTransform>(e)
                .map_or(Vec3::ZERO, |g| Vec3::from(g.0.translation));
            Some(Icon {
                id: id.to_string(),
                kind,
                at,
            })
        })
        .collect();
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// Draws the icons on `painter` and returns the one under `pointer`, if any
/// (the nearest to it).
pub(crate) fn draw_icons(
    painter: &egui::Painter,
    screen: &Screen,
    icons: &[Icon],
    selection: &Selection,
    pointer: Option<Pos2>,
) -> Option<String> {
    let mut hovered: Option<(f32, &str)> = None;
    let mut shown = Vec::new();
    for icon in icons {
        let Some(p) = screen.project(icon.at) else {
            continue;
        };
        let p = Pos2::new(p.x, p.y);
        if !painter.clip_rect().contains(p) {
            continue;
        }
        if let Some(c) = pointer {
            let d = c.distance(p);
            if d <= PICK_POINTS && hovered.is_none_or(|(best, _)| d < best) {
                hovered = Some((d, &icon.id));
            }
        }
        shown.push((icon, p));
    }
    for (icon, p) in shown {
        let lit = hovered.is_some_and(|(_, id)| id == icon.id);
        let colour = if selection.contains(&icon.id) {
            Color32::from_rgb(255, 150, 40)
        } else if lit {
            Color32::WHITE
        } else {
            Color32::from_rgb(220, 220, 200)
        };
        draw_icon(painter, p, icon.kind, colour);
    }
    hovered.map(|(_, id)| id.to_string())
}

fn draw_icon(painter: &egui::Painter, p: Pos2, kind: IconKind, colour: Color32) {
    let shadow = Color32::from_black_alpha(140);
    painter.circle_filled(p, 9.0, shadow);
    let stroke = Stroke::new(1.5, colour);
    match kind {
        IconKind::Camera => {
            let body =
                egui::Rect::from_center_size(p + egui::vec2(-1.5, 0.0), egui::vec2(9.0, 7.0));
            painter.rect_stroke(body, 1.0, stroke, egui::StrokeKind::Middle);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    p + egui::vec2(3.0, 0.0),
                    p + egui::vec2(7.0, -3.5),
                    p + egui::vec2(7.0, 3.5),
                ],
                colour,
                Stroke::NONE,
            ));
        }
        IconKind::Light(kind) => {
            let r = if kind == LightKind::Directional {
                3.5
            } else {
                3.0
            };
            painter.circle_filled(p, r, colour);
            let rays = if kind == LightKind::Directional { 8 } else { 6 };
            for i in 0..rays {
                let a = i as f32 * std::f32::consts::TAU / rays as f32;
                let d = egui::vec2(libm::cosf(a), libm::sinf(a));
                painter.line_segment([p + d * (r + 1.5), p + d * 7.0], stroke);
            }
        }
    }
}

/// The debug lines that show what the selected entity `id` reaches, when it
/// is a camera (its frustum, in a view of width over height `aspect`, out to
/// 100 m at most) or a light (a point light's range as three circles, a
/// directional light's direction as an arrow).
pub(crate) fn reach(world: &World, id: &str, aspect: f32) -> Vec<DebugLine> {
    let Ok(e) = world.entity(id) else {
        return Vec::new();
    };
    let Some(g) = world.ecs.get::<GlobalTransform>(e).map(|g| g.0) else {
        return Vec::new();
    };
    let (_, rotation, at) = g.to_scale_rotation_translation();
    let line = |a: Vec3, b: Vec3| DebugLine {
        a,
        b,
        color: SHAPE_COLOUR,
    };
    let mut out = Vec::new();
    if let Some(c) = world.ecs.get::<Camera>(e) {
        let m = Mat4::from_rotation_translation(rotation, at);
        let half_y = libm::tanf(c.fov_y_deg.to_radians() * 0.5);
        let corners = |d: f32| {
            let (x, y) = (half_y * aspect * d, half_y * d);
            [(-x, -y), (x, -y), (x, y), (-x, y)]
                .map(|(x, y)| m.transform_point3(Vec3::new(x, y, -d)))
        };
        let near = corners(c.near.max(0.01));
        let far = corners(c.far.clamp(c.near.max(0.01), 100.0));
        for i in 0..4 {
            let j = (i + 1) % 4;
            out.push(line(near[i], near[j]));
            out.push(line(far[i], far[j]));
            out.push(line(near[i], far[i]));
        }
    } else if let Some(l) = world.ecs.get::<Light>(e) {
        match l.kind {
            LightKind::Point => {
                let r = l.range.max(0.01);
                for (u, v) in [(Vec3::X, Vec3::Y), (Vec3::X, Vec3::Z), (Vec3::Y, Vec3::Z)] {
                    let point = |i: usize| {
                        let a = i as f32 * std::f32::consts::TAU / 48.0;
                        at + (u * libm::cosf(a) + v * libm::sinf(a)) * r
                    };
                    for i in 0..48 {
                        out.push(line(point(i), point(i + 1)));
                    }
                }
            }
            LightKind::Directional => {
                let dir = (rotation * Vec3::NEG_Z).normalize_or(Vec3::NEG_Y);
                let side = dir.any_orthonormal_vector();
                let up = dir.cross(side);
                for offset in [Vec3::ZERO, side * 0.4, -side * 0.4, up * 0.4, -up * 0.4] {
                    out.push(line(at + offset, at + offset + dir * 2.0));
                }
                let tip = at + dir * 2.0;
                for s in [side, -side, up, -up] {
                    out.push(line(tip, tip - dir * 0.4 + s * 0.2));
                }
            }
        }
    }
    out
}

/// The outline of the selected entities `ids` and everything under them in
/// the hierarchy (a terrain source's chunks are drawn as the source), as one
/// highlight, or `None` when none of them exists.
pub(crate) fn outline(world: &World, ids: &[String]) -> Option<Highlight> {
    let mut stack: Vec<Entity> = ids.iter().filter_map(|id| world.entity(id).ok()).collect();
    if stack.is_empty() {
        return None;
    }
    let mut seen = HashSet::new();
    let mut entities = Vec::new();
    while let Some(e) = stack.pop() {
        let id = re_scene::entity_id(e);
        if seen.insert(id) {
            entities.push(id);
            stack.extend(children(&world.ecs, e));
        }
    }
    Some(Highlight {
        entities,
        fill: FILL,
        outline: OUTLINE,
        always_on_top: false,
    })
}

const AXIS_COLOURS: [Color32; 3] = [
    Color32::from_rgb(230, 70, 70),
    Color32::from_rgb(90, 200, 80),
    Color32::from_rgb(70, 130, 240),
];

/// The axis indicator's centre and radius in `rect`: the top-right corner.
fn indicator(rect: egui::Rect) -> (Pos2, f32) {
    (rect.right_top() + egui::vec2(-42.0, 42.0), 30.0)
}

/// Where the ends of the axes are drawn, for a camera turned by `rotation`:
/// (axis, end, depth), the farthest first.
fn axis_ends(rect: egui::Rect, rotation: Quat) -> Vec<(Vec3, Pos2, f32)> {
    let (centre, radius) = indicator(rect);
    let (right, up, forward) = (
        rotation * Vec3::X,
        rotation * Vec3::Y,
        rotation * Vec3::NEG_Z,
    );
    let mut ends: Vec<(Vec3, Pos2, f32)> = [Vec3::X, Vec3::Y, Vec3::Z]
        .into_iter()
        .flat_map(|a| [a, -a])
        .map(|a| {
            let p = centre + egui::vec2(a.dot(right), -a.dot(up)) * radius;
            (a, p, a.dot(forward))
        })
        .collect();
    ends.sort_by(|a, b| b.2.total_cmp(&a.2));
    ends
}

/// The axis whose end, in the indicator in the top-right corner of `rect`
/// turned by `rotation`, is under `pointer` (signed: −X for the end opposite
/// X), if any; of two ends drawn on top of each other, the one on top.
pub(crate) fn axis_under(rect: egui::Rect, rotation: Quat, pointer: Option<Pos2>) -> Option<Vec3> {
    let c = pointer?;
    axis_ends(rect, rotation)
        .into_iter()
        .rev()
        .map(|(a, p, _)| (a, c.distance(p)))
        .filter(|(_, d)| *d <= PICK_POINTS)
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(a, _)| a)
}

/// Draws the axis indicator in the top-right corner of `rect`, turned as a
/// camera turned by `rotation` sees the axes, with the end of `lit` lit.
pub(crate) fn draw_axes(
    painter: &egui::Painter,
    rect: egui::Rect,
    rotation: Quat,
    lit: Option<Vec3>,
) {
    let (centre, radius) = indicator(rect);
    painter.circle_filled(centre, radius + 12.0, Color32::from_black_alpha(60));
    for (axis, p, _) in axis_ends(rect, rotation) {
        let i = axis.abs().max_position();
        let positive = axis.max_element() > 0.0;
        let colour = if lit == Some(axis) {
            Color32::WHITE
        } else {
            AXIS_COLOURS[i]
        };
        if positive {
            painter.line_segment([centre, p], Stroke::new(2.0, colour));
            painter.circle_filled(p, 8.0, colour);
            painter.text(
                p,
                egui::Align2::CENTER_CENTER,
                ["X", "Y", "Z"][i],
                egui::FontId::proportional(10.0),
                Color32::BLACK,
            );
        } else {
            painter.circle_stroke(p, 6.0, Stroke::new(1.5, colour.gamma_multiply(0.7)));
        }
    }
}

/// The bounds of every draw item of the entities `ids` (all items when `None`)
/// in world space, from the meshes' bounds the renderer keeps; an entity
/// without one counts as a metre-wide box around its origin.
pub(crate) fn bounds(app: &re_app::App, ids: Option<&[String]>) -> Option<re_types::mesh::Aabb> {
    let world = app.world();
    let origin = app.render_origin();
    let snapshot = app.snapshot();
    let renderer = app.graphics().map(|g| g.renderer());
    // The entities asked for, and everything under them.
    let wanted: Option<HashMap<re_types::EntityId, Entity>> = ids.map(|ids| {
        let mut set = HashMap::new();
        let mut stack: Vec<Entity> = ids.iter().filter_map(|id| world.entity(id).ok()).collect();
        while let Some(e) = stack.pop() {
            set.insert(re_scene::entity_id(e), e);
            stack.extend(children(&world.ecs, e));
        }
        set
    });
    let wants = |e: re_types::EntityId| wanted.as_ref().is_none_or(|w| w.contains_key(&e));
    let mut points: Vec<Vec3> = Vec::new();
    let mut drawn = HashSet::new();
    for item in snapshot.items.iter().filter(|i| wants(i.entity)) {
        if let Some(b) = renderer.and_then(|r| r.mesh_bounds(item.mesh)) {
            points.extend(corners(&b).map(|c| item.world.transform_point3(c) + origin));
            drawn.insert(item.entity);
        }
    }
    for item in snapshot.skinned.iter().filter(|i| wants(i.entity)) {
        points.extend(corners(&item.bounds).map(|c| c + origin));
        drawn.insert(item.entity);
    }
    if let Some(wanted) = &wanted {
        // Entities with no geometry drawn: a metre around their origin.
        for (_, e) in wanted.iter().filter(|(id, _)| !drawn.contains(*id)) {
            if let Some(g) = world.ecs.get::<GlobalTransform>(*e) {
                let at = Vec3::from(g.0.translation);
                points.push(at - Vec3::splat(0.5));
                points.push(at + Vec3::splat(0.5));
            }
        }
    }
    re_types::mesh::Aabb::from_points(points.into_iter().filter(|p| p.is_finite()))
}

fn corners(b: &re_types::mesh::Aabb) -> [Vec3; 8] {
    let (lo, hi) = (b.min, b.max);
    [
        Vec3::new(lo.x, lo.y, lo.z),
        Vec3::new(hi.x, lo.y, lo.z),
        Vec3::new(lo.x, hi.y, lo.z),
        Vec3::new(hi.x, hi.y, lo.z),
        Vec3::new(lo.x, lo.y, hi.z),
        Vec3::new(hi.x, lo.y, hi.z),
        Vec3::new(lo.x, hi.y, hi.z),
        Vec3::new(hi.x, hi.y, hi.z),
    ]
}

/// The game's camera as `real run` draws from it: the first active
/// `Camera` in entity order (its rotation and translation), else (0, 10,
/// 20) looking at the origin; with its vertical field of view in radians.
pub(crate) fn game_camera(world: &World) -> Option<(Mat4, f32)> {
    let mut cams: Vec<(Entity, f32)> = world
        .ids()
        .iter()
        .filter_map(|(_, e)| {
            let c = world.ecs.get::<Camera>(e)?;
            c.active.then_some((e, c.fov_y_deg.to_radians()))
        })
        .collect();
    cams.sort_by_key(|(e, _)| e.to_bits());
    let (e, fov) = cams.first().copied()?;
    let g = world.ecs.get::<GlobalTransform>(e)?.0;
    let (_, r, t) = g.to_scale_rotation_translation();
    Some((Mat4::from_rotation_translation(r, t), fov))
}

/// The id of the entity [`game_camera`] picks.
pub(crate) fn game_camera_id(world: &World) -> Option<String> {
    let mut cams: Vec<(Entity, &str)> = world
        .ids()
        .iter()
        .filter(|(_, e)| world.ecs.get::<Camera>(*e).is_some_and(|c| c.active))
        .map(|(id, e)| (e, id))
        .collect();
    cams.sort_by_key(|(e, _)| e.to_bits());
    cams.first().map(|(_, id)| id.to_string())
}

/// The rotation of a camera-to-world transform, without its scale.
pub(crate) fn rotation_of(world: &Mat4) -> Quat {
    Quat::from_mat4(&Mat4::from_cols(
        world.x_axis.xyz().normalize_or(Vec3::X).extend(0.0),
        world.y_axis.xyz().normalize_or(Vec3::Y).extend(0.0),
        world.z_axis.xyz().normalize_or(Vec3::Z).extend(0.0),
        glam::Vec4::W,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_is_metres_near_the_ground_and_coarser_high_up() {
        let near = grid(Vec3::new(0.3, 5.0, 0.2), None);
        assert!(!near.is_empty());
        // Every line lies on the ground, lifted a little.
        assert!(
            near.iter()
                .all(|l| l.a.y > 0.0 && l.a.y < 0.01 && l.a.y == l.b.y)
        );
        let xs: std::collections::BTreeSet<i64> = near
            .iter()
            .filter(|l| l.a.x == l.b.x)
            .map(|l| (l.a.x * 1000.0).round() as i64)
            .collect();
        let step = xs.iter().zip(xs.iter().skip(1)).map(|(a, b)| b - a).min();
        assert_eq!(step, Some(1000), "a line every metre");
        // The axes are coloured.
        assert!(near.iter().any(|l| l.color[..3] == GRID_X_AXIS[..3]));
        assert!(near.iter().any(|l| l.color[..3] == GRID_Z_AXIS[..3]));
        // It fades: nothing at full strength far from the camera's foot.
        assert!(near.iter().all(|l| l.color[3] <= 0.9));

        let high = grid(Vec3::new(0.0, 400.0, 0.0), None);
        let xs: std::collections::BTreeSet<i64> = high
            .iter()
            .filter(|l| l.a.x == l.b.x)
            .map(|l| l.a.x.round() as i64)
            .collect();
        let step = xs.iter().zip(xs.iter().skip(1)).map(|(a, b)| b - a).min();
        assert_eq!(step, Some(10), "a line every 10 m from 400 m up");
        // Snapping sets the minor step.
        let snapped = grid(Vec3::new(0.0, 5.0, 0.0), Some(0.5));
        assert!(
            snapped
                .iter()
                .any(|l| l.a.x == l.b.x && (l.a.x - 0.5).abs() < 1e-6)
        );
        // However fine, the count stays bounded.
        let fine = grid(Vec3::new(0.0, 900.0, 0.0), Some(0.01));
        assert!(fine.len() <= (GRID_LINES as usize + 2) * 2 * GRID_PIECES);
    }

    #[test]
    fn the_axis_indicator_turns_with_the_camera_and_picks_ends() {
        let rect = egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(800.0, 600.0));
        let ends = axis_ends(rect, Quat::IDENTITY);
        let (centre, _) = indicator(rect);
        let x = ends.iter().find(|e| e.0 == Vec3::X).unwrap().1;
        let y = ends.iter().find(|e| e.0 == Vec3::Y).unwrap().1;
        assert!(x.x > centre.x && (x.y - centre.y).abs() < 1e-3);
        assert!(y.y < centre.y);
        // The camera looks along -Z: +Z points at it and is drawn last.
        assert_eq!(ends.last().unwrap().0, Vec3::Z);
        assert_eq!(axis_under(rect, Quat::IDENTITY, Some(x)), Some(Vec3::X));
        // Looking along -Z, both Z ends are drawn at the centre: +Z is on top.
        assert_eq!(
            axis_under(rect, Quat::IDENTITY, Some(centre)),
            Some(Vec3::Z)
        );
        let between = centre + (x - centre) * 0.5;
        assert_eq!(axis_under(rect, Quat::IDENTITY, Some(between)), None);
        assert_eq!(axis_under(rect, Quat::IDENTITY, None), None);
        // Turned half way round Y, the camera looks along +Z: -X is drawn
        // where X was.
        let turned = Quat::from_rotation_y(std::f32::consts::PI);
        assert_eq!(axis_under(rect, turned, Some(x)), Some(Vec3::NEG_X));
    }
}
