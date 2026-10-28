//! The editor camera (ADR-0044's amendment for WP42, §2): the camera the
//! viewport draws the scene from, owned by the editor.
//!
//! It is editor state, never part of the scene: moving it sends no command,
//! changes no `state_hash` and saves nothing but the editor's layout
//! (`.real/editor/layout.json`, `camera`). It orbits a pivot, so its pose is
//! the pivot, the angles it looks from (yaw about +Y, then pitch) and its
//! distance from the pivot. Until the layout holds one, it starts where the
//! game's camera is ([`EditorCamera::from_pose`]).
//!
//! Controls over the viewport ([`navigate`]):
//! * orbit about the pivot: the middle button, or Alt and the left button;
//! * pan: Shift and the middle button;
//! * zoom towards the point under the cursor: the wheel;
//! * fly while the right button is held: W, A, S and D, Q and E down and
//!   up, Shift for four times the speed; the wheel changes the speed;
//! * F frames the selection, Shift+F everything (`Editor::frame_selection`).
//!
//! The maths are on glam only, so every control is unit-tested without egui.

use std::f32::consts::{FRAC_PI_2, TAU};

use glam::{Mat4, Quat, Vec2, Vec3, Vec4Swizzles};
use re_types::mesh::Aabb;
use serde::{Deserialize, Serialize};

use crate::gizmo::math::Screen;

/// Radians the camera turns per point the pointer moves.
const TURN_PER_POINT: f32 = 0.005;
/// The steepest the camera looks up or down: 89°.
const MAX_PITCH: f32 = 89.0 * (FRAC_PI_2 / 90.0);
/// The nearest the camera comes to its pivot, in metres; zooming in further
/// moves the pivot forward.
pub const MIN_DISTANCE: f32 = 0.05;
/// The farthest the camera goes from its pivot, in metres.
pub const MAX_DISTANCE: f32 = 100_000.0;
/// The distance one wheel notch zooms to, as a fraction of the current one.
const ZOOM_PER_NOTCH: f32 = 0.85;
/// The speed one wheel notch sets while flying, as a multiple of the current
/// one.
const SPEED_PER_NOTCH: f32 = 1.2;
/// The slowest and fastest fly speeds, in metres per second.
pub const SPEEDS: (f32, f32) = (0.1, 2000.0);
/// Points of a wheel event that count as one notch.
const POINTS_PER_NOTCH: f32 = 50.0;
/// How far ahead a pose that looks above the horizon puts its pivot, in metres.
const DEFAULT_PIVOT_DISTANCE: f32 = 10.0;

/// The editor camera: a pivot, the angles the camera looks at it from, its
/// distance, field of view and fly speed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorCamera {
    /// The point the camera orbits and looks at, in world space.
    pub pivot: Vec3,
    /// The turn about +Y, in radians: 0 looks along −Z, a positive turn
    /// looks further left.
    pub yaw: f32,
    /// The tilt, in radians: positive looks up, within ±89°.
    pub pitch: f32,
    /// The distance from the camera to the pivot, in metres.
    pub distance: f32,
    /// The vertical field of view, in degrees.
    pub fov_y_deg: f32,
    /// How fast the camera flies, in metres per second.
    pub speed: f32,
}

impl Default for EditorCamera {
    /// At (0, 10, 20) looking at the origin, where the game is drawn from
    /// when it has no active camera.
    fn default() -> Self {
        let eye = Vec3::new(0.0, 10.0, 20.0);
        let mut camera = EditorCamera {
            pivot: Vec3::ZERO,
            yaw: 0.0,
            pitch: 0.0,
            distance: eye.length(),
            fov_y_deg: 60.0,
            speed: 10.0,
        };
        camera.set_forward(-eye);
        camera
    }
}

impl EditorCamera {
    /// A camera at the pose of a camera-to-world transform, with a vertical
    /// field of view in radians. Its pivot is where it looks at the ground
    /// (y = 0) up to 1 km ahead, else 10 m ahead.
    pub fn from_pose(world: Mat4, fov_y_radians: f32) -> EditorCamera {
        let eye = world.w_axis.xyz();
        let forward = (-world.z_axis.xyz()).normalize_or(Vec3::NEG_Z);
        let ground = (forward.y < -1e-3)
            .then(|| -eye.y / forward.y)
            .filter(|t| (0.5..=1000.0).contains(t));
        let distance = ground.unwrap_or(DEFAULT_PIVOT_DISTANCE);
        let mut camera = EditorCamera {
            pivot: eye + forward * distance,
            distance,
            fov_y_deg: fov_y_radians.to_degrees().clamp(1.0, 179.0),
            ..EditorCamera::default()
        };
        camera.set_forward(forward);
        camera
    }

    /// True when every number in it is finite (a camera read from a broken
    /// pose is replaced, never saved).
    pub fn is_finite(&self) -> bool {
        self.pivot.is_finite()
            && [
                self.yaw,
                self.pitch,
                self.distance,
                self.fov_y_deg,
                self.speed,
            ]
            .iter()
            .all(|v| v.is_finite())
    }

    /// The camera with each number brought into its range: the pitch within
    /// ±89°, the distance, the field of view and the speed within their
    /// limits (a hand-edited layout may hold anything). A number that is not
    /// finite stays so.
    pub fn clamped(self) -> EditorCamera {
        EditorCamera {
            pitch: self.pitch.clamp(-MAX_PITCH, MAX_PITCH),
            distance: self.distance.clamp(MIN_DISTANCE, MAX_DISTANCE),
            fov_y_deg: self.fov_y_deg.clamp(1.0, 179.0),
            speed: self.speed.clamp(SPEEDS.0, SPEEDS.1),
            ..self
        }
    }

    /// The camera's rotation: the yaw about +Y, then the pitch.
    pub fn rotation(&self) -> Quat {
        Quat::from_rotation_y(self.yaw) * Quat::from_rotation_x(self.pitch)
    }

    /// The way the camera looks (its −Z), normalised.
    pub fn forward(&self) -> Vec3 {
        self.rotation() * Vec3::NEG_Z
    }

    /// The camera's position.
    pub fn eye(&self) -> Vec3 {
        self.pivot - self.forward() * self.distance
    }

    /// The camera-to-world transform, in world space.
    pub fn world(&self) -> Mat4 {
        Mat4::from_rotation_translation(self.rotation(), self.eye())
    }

    /// The near and far clip distances for the view, from the distance to
    /// the pivot: close up, a near plane of a few centimetres; far away, a
    /// far plane that takes in a streamed world.
    pub fn clip(&self) -> (f32, f32) {
        let near = (self.distance * 0.01).clamp(0.02, 2.0);
        let far = (self.distance * 400.0).clamp(2000.0, 50_000.0);
        (near, far)
    }

    /// The camera the renderer draws from, relative to the render `origin`.
    pub fn render_camera(&self, origin: Vec3) -> re_types::render::Camera {
        let (near, far) = self.clip();
        re_types::render::Camera {
            world: Mat4::from_rotation_translation(self.rotation(), self.eye() - origin),
            fov_y_radians: self.fov_y_deg.to_radians(),
            near,
            far,
        }
    }

    /// Points the camera along `forward`, keeping its yaw when it looks
    /// straight up or down.
    fn set_forward(&mut self, forward: Vec3) {
        let f = forward.normalize_or(Vec3::NEG_Z);
        let flat = Vec2::new(f.x, f.z);
        if flat.length_squared() > 1e-8 {
            self.yaw = libm::atan2f(-f.x, -f.z);
        }
        self.pitch = libm::asinf(f.y.clamp(-1.0, 1.0)).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Turns the camera about its pivot by a pointer move of `delta` points:
    /// a move right turns the scene right, a move down looks from higher up.
    pub fn orbit(&mut self, delta: Vec2) {
        // Kept within ±180°, so a saved camera stays readable.
        self.yaw = libm::remainderf(self.yaw - delta.x * TURN_PER_POINT, TAU);
        self.pitch = (self.pitch - delta.y * TURN_PER_POINT).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Turns the camera where it stands by a pointer move of `delta` points
    /// (looking around while flying); the pivot moves with the view.
    pub fn look(&mut self, delta: Vec2) {
        let eye = self.eye();
        self.orbit(delta);
        self.pivot = eye + self.forward() * self.distance;
    }

    /// Moves the camera across the view so that the scene at the pivot's
    /// depth follows a pointer move of `delta` points, in a view
    /// `view_height` points high.
    pub fn pan(&mut self, delta: Vec2, view_height: f32) {
        if view_height <= 0.0 {
            return;
        }
        let per_point =
            2.0 * self.distance * libm::tanf(self.fov_y_deg.to_radians() * 0.5) / view_height;
        let rotation = self.rotation();
        let right = rotation * Vec3::X;
        let up = rotation * Vec3::Y;
        self.pivot += (-right * delta.x + up * delta.y) * per_point;
    }

    /// Zooms by `notches` wheel notches (positive: in) towards `towards`, a
    /// point at the pivot's depth that stays where it is on the screen (the
    /// point under the cursor), or towards the pivot. Past the nearest
    /// distance, the camera moves forward instead.
    pub fn zoom(&mut self, notches: f32, towards: Option<Vec3>) {
        if notches == 0.0 || !notches.is_finite() {
            return;
        }
        let wanted = self.distance * libm::powf(ZOOM_PER_NOTCH, notches);
        let distance = wanted.clamp(MIN_DISTANCE, MAX_DISTANCE);
        let scale = distance / self.distance;
        if let Some(p) = towards {
            self.pivot = p + (self.pivot - p) * scale;
        }
        if wanted < MIN_DISTANCE {
            // As near as it gets: the rest of the zoom moves forward.
            self.pivot += self.forward() * (MIN_DISTANCE - wanted);
        }
        self.distance = distance;
    }

    /// The point on the ray through the cursor (`origin`, unit `dir`) at the
    /// pivot's depth, or `None` when the ray looks away from the view.
    pub fn at_pivot_depth(&self, origin: Vec3, dir: Vec3) -> Option<Vec3> {
        let along = dir.dot(self.forward());
        if along <= 1e-4 {
            return None;
        }
        let eye = self.eye();
        let depth = self.distance - (origin - eye).dot(self.forward());
        Some(origin + dir * (depth / along))
    }

    /// Flies `dt` seconds along `motion`: x right, y up (the world's), z
    /// forward, each from −1 to 1; four times as fast when `fast`.
    pub fn fly(&mut self, motion: Vec3, dt: f32, fast: bool) {
        if motion == Vec3::ZERO || dt <= 0.0 {
            return;
        }
        let rotation = self.rotation();
        let dir = rotation * Vec3::X * motion.x + Vec3::Y * motion.y + self.forward() * motion.z;
        let speed = if fast { self.speed * 4.0 } else { self.speed };
        self.pivot += dir * speed * dt;
    }

    /// Changes the fly speed by `notches` wheel notches (positive: faster).
    pub fn change_speed(&mut self, notches: f32) {
        if notches.is_finite() {
            self.speed =
                (self.speed * libm::powf(SPEED_PER_NOTCH, notches)).clamp(SPEEDS.0, SPEEDS.1);
        }
    }

    /// Frames `bounds` in a view of width over height `aspect`: the pivot at
    /// its centre, near enough that its bounding sphere fills the view. The
    /// angles stay as they are.
    pub fn frame(&mut self, bounds: Aabb, aspect: f32) {
        let radius = bounds.half_extents().length().max(0.25);
        let half_y = self.fov_y_deg.to_radians() * 0.5;
        let half_x = libm::atanf(libm::tanf(half_y) * aspect.max(1e-3));
        let half = half_y.min(half_x);
        self.pivot = bounds.center();
        self.distance = (radius / libm::sinf(half) * 1.1).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Looks at the pivot from the side `axis` points to (from +X: along
    /// −X), keeping the distance.
    pub fn look_from(&mut self, axis: Vec3) {
        self.set_forward(-axis);
    }
}

/// The viewport as it is drawn this frame: where, and from which camera.
/// The gizmo, the overlays and the procedural tools' viewport hook
/// (`panels::procedural::viewport_overlay`) all work from it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct View {
    /// The image's rectangle, in egui points.
    pub rect: egui::Rect,
    /// The camera the scene is drawn from, in world space (not relative to
    /// the render origin).
    pub camera: re_types::render::Camera,
    /// World to the image's points and back; `None` when the image has no
    /// area.
    pub screen: Option<Screen>,
}

impl View {
    /// The view of `camera` drawn into `rect`.
    pub fn new(rect: egui::Rect, camera: re_types::render::Camera) -> View {
        let screen = Screen::new(
            camera.world,
            camera.fov_y_radians,
            camera.near,
            camera.far,
            Vec2::new(rect.min.x, rect.min.y),
            Vec2::new(rect.width(), rect.height()),
        );
        View {
            rect,
            camera,
            screen,
        }
    }
}

/// What the pointer is doing to the editor camera.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Navigate {
    /// Turning about the pivot.
    Orbit,
    /// Moving across the view.
    Pan,
    /// Looking around and flying with the keys.
    Fly,
}

impl Navigate {
    /// The button that keeps the navigation going.
    fn button(self, alt_orbit: bool) -> egui::PointerButton {
        match self {
            Navigate::Orbit if alt_orbit => egui::PointerButton::Primary,
            Navigate::Orbit | Navigate::Pan => egui::PointerButton::Middle,
            Navigate::Fly => egui::PointerButton::Secondary,
        }
    }
}

/// The navigation in progress, kept between frames.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Navigation {
    pub mode: Option<Navigate>,
    /// The orbit was started with Alt and the left button.
    alt_orbit: bool,
}

impl Navigation {
    /// True while the right button flies the camera, when W, A, S, D, Q and
    /// E move it rather than pick the gizmo's mode.
    pub fn flying(&self) -> bool {
        self.mode == Some(Navigate::Fly)
    }

    /// True while the pointer moves the camera.
    pub fn active(&self) -> bool {
        self.mode.is_some()
    }
}

/// Moves `camera` with the pointer and keys over the viewport at `rect`
/// (`response` is the viewport's): a press over the view starts orbiting,
/// panning or flying, which goes on until its button is released, and the
/// wheel over the view zooms towards the cursor (or sets the fly speed).
/// Returns true when it changed the camera.
pub(crate) fn navigate(
    camera: &mut EditorCamera,
    nav: &mut Navigation,
    ui: &egui::Ui,
    response: &egui::Response,
    screen: Option<&Screen>,
) -> bool {
    use egui::{Key, PointerButton};
    let before = *camera;
    let (pressed, modifiers, delta, dt) = ui.input(|i| {
        let pressed = [
            PointerButton::Primary,
            PointerButton::Middle,
            PointerButton::Secondary,
        ]
        .into_iter()
        .find(|b| i.pointer.button_pressed(*b));
        (
            pressed,
            i.modifiers,
            i.pointer.delta(),
            i.stable_dt.min(0.1),
        )
    });
    if let Some(mode) = nav.mode {
        if !ui.input(|i| i.pointer.button_down(mode.button(nav.alt_orbit))) {
            nav.mode = None;
        } else {
            let delta = Vec2::new(delta.x, delta.y);
            match mode {
                Navigate::Orbit => camera.orbit(delta),
                Navigate::Pan => camera.pan(delta, response.rect.height()),
                Navigate::Fly => {
                    camera.look(delta);
                    let key = |k: Key| {
                        if ui.input(|i| i.key_down(k)) {
                            1.0
                        } else {
                            0.0
                        }
                    };
                    let motion = Vec3::new(
                        key(Key::D) - key(Key::A),
                        key(Key::E) - key(Key::Q),
                        key(Key::W) - key(Key::S),
                    );
                    camera.fly(motion, dt, modifiers.shift);
                }
            }
        }
    } else if response.hovered() {
        // A press starts a navigation; the pointer's motion in that frame
        // came before it.
        nav.mode = match pressed {
            Some(PointerButton::Middle) if modifiers.shift => Some(Navigate::Pan),
            Some(PointerButton::Middle) => Some(Navigate::Orbit),
            Some(PointerButton::Primary) if modifiers.alt => Some(Navigate::Orbit),
            Some(PointerButton::Secondary) => Some(Navigate::Fly),
            _ => None,
        };
        nav.alt_orbit = pressed == Some(PointerButton::Primary);
    }
    let notches = wheel_notches(ui);
    if notches != 0.0 && (response.hovered() || nav.active()) {
        if nav.flying() {
            camera.change_speed(notches);
        } else {
            let towards = ui
                .input(|i| i.pointer.hover_pos())
                .zip(screen)
                .and_then(|(p, s)| {
                    let (origin, dir) = s.ray(Vec2::new(p.x, p.y));
                    camera.at_pivot_depth(origin, dir)
                });
            camera.zoom(notches, towards);
        }
    }
    *camera != before
}

/// The wheel notches of this frame's wheel events (positive: away from the
/// person, which zooms in), unsmoothed so the zoom answers at once.
fn wheel_notches(ui: &egui::Ui) -> f32 {
    ui.input(|i| {
        i.events
            .iter()
            .map(|e| match e {
                egui::Event::MouseWheel { unit, delta, .. } => {
                    let d = delta.y + delta.x;
                    match unit {
                        egui::MouseWheelUnit::Point => d / POINTS_PER_NOTCH,
                        egui::MouseWheelUnit::Line => d,
                        egui::MouseWheelUnit::Page => d * 10.0,
                    }
                }
                _ => 0.0,
            })
            .sum()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        a.distance(b) < 1e-3
    }

    #[test]
    fn the_default_looks_at_the_origin_from_where_the_game_would() {
        let c = EditorCamera::default();
        assert!(close(c.eye(), Vec3::new(0.0, 10.0, 20.0)), "{}", c.eye());
        assert!(close(c.pivot, Vec3::ZERO));
        assert!(c.forward().y < 0.0 && c.forward().z < 0.0);
    }

    #[test]
    fn a_pose_round_trips_and_puts_the_pivot_on_the_ground() {
        // At (3, 4, 5), looking down and towards -Z and +X.
        let rot = Quat::from_rotation_y(-0.6) * Quat::from_rotation_x(-0.5);
        let world = Mat4::from_rotation_translation(rot, Vec3::new(3.0, 4.0, 5.0));
        let c = EditorCamera::from_pose(world, 0.9);
        assert!(close(c.eye(), Vec3::new(3.0, 4.0, 5.0)), "{}", c.eye());
        assert!(close(c.forward(), rot * Vec3::NEG_Z));
        assert!(c.pivot.y.abs() < 1e-3, "{}", c.pivot);
        assert!((c.fov_y_deg - 0.9f32.to_degrees()).abs() < 1e-3);
        let m = c.world();
        assert!(close(m.w_axis.xyz(), Vec3::new(3.0, 4.0, 5.0)));
        // Looking above the horizon: the pivot is 10 m ahead.
        let up = Mat4::from_rotation_translation(Quat::from_rotation_x(0.3), Vec3::ONE);
        let c = EditorCamera::from_pose(up, 1.0);
        assert!((c.distance - 10.0).abs() < 1e-4);
        assert!(close(c.eye(), Vec3::ONE));
    }

    #[test]
    fn orbiting_keeps_the_pivot_and_the_distance() {
        let mut c = EditorCamera::default();
        let pivot = c.pivot;
        c.orbit(Vec2::new(120.0, -40.0));
        assert!(close(c.pivot, pivot));
        assert!((c.eye().distance(pivot) - c.distance).abs() < 1e-3);
        // A move right turns the scene right: the camera goes left (−X).
        assert!(c.eye().x < 0.0, "{}", c.eye());
        // The pitch stops short of straight down.
        c.orbit(Vec2::new(0.0, 10_000.0));
        assert!(c.pitch >= -MAX_PITCH - 1e-6 && c.forward().y > -1.0);
        // Many turns later, the yaw is still within half a turn.
        let forward = c.forward();
        c.orbit(Vec2::new(10.0 * TAU / TURN_PER_POINT, 0.0));
        assert!(c.yaw.abs() <= std::f32::consts::PI, "{}", c.yaw);
        assert!(close(c.forward(), forward));
    }

    #[test]
    fn a_camera_is_brought_into_range() {
        let c = EditorCamera {
            pitch: 3.0,
            distance: -1.0,
            fov_y_deg: 0.0,
            speed: 1e9,
            ..EditorCamera::default()
        }
        .clamped();
        assert_eq!(
            (c.pitch, c.distance, c.fov_y_deg, c.speed),
            (MAX_PITCH, MIN_DISTANCE, 1.0, SPEEDS.1)
        );
        assert_eq!(EditorCamera::default().clamped(), EditorCamera::default());
        let nan = EditorCamera {
            distance: f32::NAN,
            ..EditorCamera::default()
        };
        assert!(!nan.clamped().is_finite());
    }

    #[test]
    fn looking_around_keeps_the_eye() {
        let mut c = EditorCamera::default();
        let eye = c.eye();
        c.look(Vec2::new(50.0, 30.0));
        assert!(close(c.eye(), eye), "{} {}", c.eye(), eye);
        assert_ne!(c.forward(), EditorCamera::default().forward());
    }

    #[test]
    fn panning_moves_the_scene_with_the_pointer() {
        let mut c = EditorCamera::default();
        let screen = |c: &EditorCamera| {
            Screen::new(
                c.world(),
                c.fov_y_deg.to_radians(),
                0.1,
                1000.0,
                Vec2::ZERO,
                Vec2::new(800.0, 600.0),
            )
            .unwrap()
        };
        let before = screen(&c).project(Vec3::ZERO).unwrap();
        c.pan(Vec2::new(40.0, -25.0), 600.0);
        let after = screen(&c).project(Vec3::ZERO).unwrap();
        assert!(
            (after - before - Vec2::new(40.0, -25.0)).length() < 0.05,
            "{before} -> {after}"
        );
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor() {
        let mut c = EditorCamera::default();
        let s = Screen::new(
            c.world(),
            c.fov_y_deg.to_radians(),
            0.1,
            1000.0,
            Vec2::ZERO,
            Vec2::new(800.0, 600.0),
        )
        .unwrap();
        let cursor = Vec2::new(600.0, 200.0);
        let (o, d) = s.ray(cursor);
        let p = c.at_pivot_depth(o, d).unwrap();
        let before = c.distance;
        c.zoom(2.0, Some(p));
        assert!((c.distance - before * 0.85 * 0.85).abs() < 1e-3);
        let s = Screen::new(
            c.world(),
            c.fov_y_deg.to_radians(),
            0.1,
            1000.0,
            Vec2::ZERO,
            Vec2::new(800.0, 600.0),
        )
        .unwrap();
        let back = s.project(p).unwrap();
        assert!((back - cursor).length() < 0.05, "{back}");
        // Out again, the same way.
        c.zoom(-2.0, Some(p));
        assert!((c.distance - before).abs() < 1e-3);
    }

    #[test]
    fn zooming_past_the_nearest_distance_moves_forward() {
        let mut c = EditorCamera {
            distance: 0.06,
            ..EditorCamera::default()
        };
        let eye = c.eye();
        c.zoom(5.0, None);
        assert_eq!(c.distance, MIN_DISTANCE);
        assert!((c.eye() - eye).dot(c.forward()) > 0.0);
        let far = EditorCamera {
            distance: MAX_DISTANCE,
            ..EditorCamera::default()
        };
        let mut c = far;
        c.zoom(-3.0, None);
        assert_eq!(c.distance, MAX_DISTANCE);
    }

    #[test]
    fn flying_moves_along_the_view_and_the_speed_changes_by_notches() {
        let mut c = EditorCamera::default();
        let (eye, f) = (c.eye(), c.forward());
        c.fly(Vec3::new(0.0, 0.0, 1.0), 0.5, false);
        assert!(close(c.eye(), eye + f * 5.0), "{}", c.eye());
        c.fly(Vec3::new(0.0, 1.0, 0.0), 0.1, true);
        assert!(close(c.eye(), eye + f * 5.0 + Vec3::Y * 4.0));
        c.change_speed(1.0);
        assert!((c.speed - 12.0).abs() < 1e-4);
        c.change_speed(1000.0);
        assert_eq!(c.speed, SPEEDS.1);
    }

    #[test]
    fn framing_fills_the_view_with_the_bounds() {
        let mut c = EditorCamera::default();
        let b = Aabb::new(Vec3::new(9.0, 0.0, -1.0), Vec3::new(11.0, 2.0, 1.0));
        c.frame(b, 16.0 / 9.0);
        assert!(close(c.pivot, Vec3::new(10.0, 1.0, 0.0)));
        // The bounding sphere (radius √3) fits the vertical field of view.
        let half = c.fov_y_deg.to_radians() * 0.5;
        let fits = 3f32.sqrt() / libm::sinf(half);
        assert!(
            c.distance > fits && c.distance < fits * 1.2,
            "{}",
            c.distance
        );
        // A tall, narrow view fits the horizontal field of view instead.
        let mut narrow = c;
        narrow.frame(b, 0.25);
        assert!(narrow.distance > c.distance);
    }

    #[test]
    fn looking_from_an_axis_faces_back_along_it() {
        let mut c = EditorCamera::default();
        c.look_from(Vec3::X);
        assert!(close(c.forward(), Vec3::NEG_X), "{}", c.forward());
        let yaw = c.yaw;
        c.look_from(Vec3::Y);
        assert_eq!(c.yaw, yaw, "straight down keeps the yaw");
        assert!(c.forward().y < -0.99);
        c.look_from(Vec3::NEG_Z);
        assert!(close(c.forward(), Vec3::Z));
    }
}
