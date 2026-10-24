//! The gizmo's geometry, picking and drag maths, on glam only (no egui, no
//! window), so every case is unit-tested.
//!
//! Screen positions are in egui points with y down, the same space the
//! viewport panel is drawn in.

use glam::{Affine3A, Mat4, Quat, Vec2, Vec3, Vec4, Vec4Swizzles};
use serde::{Deserialize, Serialize};

/// What the gizmo changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum GizmoMode {
    /// Arrows along the axes and squares for the planes between them.
    #[default]
    Translate,
    /// A ring around each axis and one facing the camera.
    Rotate,
    /// A box at the end of each axis and one at the centre (uniform).
    Scale,
}

/// Which axes the translate and rotate handles follow. Scale always uses the
/// entity's own axes: a scale along a world axis of a rotated entity would
/// shear it, which a [`Transform`](re_scene::Transform) cannot hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Orientation {
    /// The world's X, Y and Z.
    #[default]
    World,
    /// The selected entity's rotated axes.
    Local,
}

/// The gizmo's settings: editor UI state, saved with the layout.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GizmoSettings {
    /// Translate, rotate or scale.
    pub mode: GizmoMode,
    /// World or local axes.
    pub orientation: Orientation,
    /// Snap drags to the steps below.
    pub snap: bool,
    /// Translation step in metres.
    pub translate_step: f32,
    /// Rotation step in degrees.
    pub angle_step_deg: f32,
    /// Scale factor step.
    pub scale_step: f32,
}

impl Default for GizmoSettings {
    fn default() -> Self {
        GizmoSettings {
            mode: GizmoMode::Translate,
            orientation: Orientation::World,
            snap: false,
            translate_step: 0.5,
            angle_step_deg: 15.0,
            scale_step: 0.1,
        }
    }
}

/// How far (in points) the cursor may be from a handle and still pick it.
pub const PICK_RADIUS: f32 = 8.0;
/// The length of an axis handle on screen, in points.
pub const HANDLE_POINTS: f32 = 90.0;
/// Points on each ring's polyline.
pub const RING_SEGMENTS: usize = 64;

/// A camera as the viewport shows it: world to screen and back.
#[derive(Clone, Copy, Debug)]
pub struct Screen {
    view_proj: Mat4,
    inv_view_proj: Mat4,
    /// The viewport's top-left corner, in points.
    min: Vec2,
    /// The viewport's size, in points.
    size: Vec2,
    /// The camera's position.
    pub eye: Vec3,
}

impl Screen {
    /// A screen for a camera with camera-to-world transform `camera_world`, a
    /// vertical field of view and clip planes, drawn into the rectangle at
    /// `min` of `size` points. `None` when it cannot be inverted (a zero-size
    /// viewport, a degenerate camera).
    pub fn new(
        camera_world: Mat4,
        fov_y_radians: f32,
        near: f32,
        far: f32,
        min: Vec2,
        size: Vec2,
    ) -> Option<Screen> {
        if !(size.x > 0.0 && size.y > 0.0) {
            return None;
        }
        let proj =
            glam::camera::rh::proj::directx::perspective(fov_y_radians, size.x / size.y, near, far);
        let view_proj = proj * camera_world.inverse();
        if !view_proj.is_finite() || view_proj.determinant().abs() < 1e-12 {
            return None;
        }
        Some(Screen {
            view_proj,
            inv_view_proj: view_proj.inverse(),
            min,
            size,
            eye: camera_world.w_axis.xyz(),
        })
    }

    /// The point of the screen where `p` is drawn, or `None` when it is
    /// behind the camera.
    pub fn project(&self, p: Vec3) -> Option<Vec2> {
        let c = self.view_proj * p.extend(1.0);
        if c.w <= 1e-6 {
            return None;
        }
        let ndc = c.xy() / c.w;
        Some(self.min + Vec2::new((ndc.x + 1.0) * 0.5, (1.0 - ndc.y) * 0.5) * self.size)
    }

    /// The ray from the camera through screen point `s`: (origin, unit direction).
    pub fn ray(&self, s: Vec2) -> (Vec3, Vec3) {
        let uv = (s - self.min) / self.size;
        let ndc = Vec2::new(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0);
        let near = self.inv_view_proj.project_point3(ndc.extend(0.0));
        let far = self.inv_view_proj.project_point3(ndc.extend(1.0));
        (near, (far - near).normalize_or(Vec3::NEG_Z))
    }

    /// World length that is drawn `points` long at the depth of `p`.
    pub fn world_size(&self, p: Vec3, points: f32) -> f32 {
        let (_, dir) = self.ray(self.project(p).unwrap_or(self.min + self.size * 0.5));
        // A direction across the view at that depth.
        let side = dir.any_orthonormal_vector();
        let a = self.project(p);
        let b = self.project(p + side);
        match (a, b) {
            (Some(a), Some(b)) if a.distance(b) > 1e-6 => points / a.distance(b),
            _ => 1.0,
        }
    }
}

/// Where the gizmo is: its origin, its three unit axes and the world length
/// of an axis handle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// The pivot (the selected entity's world position).
    pub origin: Vec3,
    /// The X, Y and Z axes, unit length.
    pub axes: [Vec3; 3],
    /// The world length of an axis handle.
    pub size: f32,
}

impl Frame {
    /// The frame for an entity at `world`, with `orientation`'s axes (scale
    /// mode always takes the entity's own) and handles [`HANDLE_POINTS`] long.
    pub fn new(
        screen: &Screen,
        world: &Affine3A,
        orientation: Orientation,
        mode: GizmoMode,
    ) -> Frame {
        let (_, rotation, origin) = world.to_scale_rotation_translation();
        let local = orientation == Orientation::Local || mode == GizmoMode::Scale;
        let rot = if local {
            rotation.normalize()
        } else {
            Quat::IDENTITY
        };
        Frame {
            origin,
            axes: [rot * Vec3::X, rot * Vec3::Y, rot * Vec3::Z],
            size: screen.world_size(origin, HANDLE_POINTS),
        }
    }

    /// The points of the ring around axis `axis` (`None`: the ring facing the
    /// camera, a little larger).
    pub fn ring(&self, screen: &Screen, axis: Option<usize>) -> Vec<Vec3> {
        let (n, r) = match axis {
            Some(i) => (self.axes[i], self.size),
            None => (
                (screen.eye - self.origin).normalize_or(Vec3::Z),
                self.size * 1.2,
            ),
        };
        let u = n.any_orthonormal_vector();
        let v = n.cross(u);
        (0..=RING_SEGMENTS)
            .map(|k| {
                let a = k as f32 / RING_SEGMENTS as f32 * std::f32::consts::TAU;
                self.origin + (u * libm::cosf(a) + v * libm::sinf(a)) * r
            })
            .collect()
    }

    /// The corners of the square for the plane whose normal is axis `normal`.
    pub fn plane_quad(&self, normal: usize) -> [Vec3; 4] {
        let (a, b) = (self.axes[(normal + 1) % 3], self.axes[(normal + 2) % 3]);
        let (lo, hi) = (0.25 * self.size, 0.5 * self.size);
        [
            self.origin + a * lo + b * lo,
            self.origin + a * hi + b * lo,
            self.origin + a * hi + b * hi,
            self.origin + a * lo + b * hi,
        ]
    }
}

/// One part of the gizmo the cursor can grab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// The arrow (translate), ring (rotate) or end box (scale) of axis 0, 1 or 2.
    Axis(usize),
    /// The square of the plane whose normal is axis 0, 1 or 2 (translate).
    Plane(usize),
    /// The ring facing the camera (rotate) or the centre box (uniform scale).
    View,
}

fn segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() > 0.0 {
        ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    p.distance(a + ab * t)
}

fn polyline_distance(screen: &Screen, p: Vec2, points: &[Vec3]) -> f32 {
    let projected: Vec<Option<Vec2>> = points.iter().map(|q| screen.project(*q)).collect();
    projected
        .windows(2)
        .filter_map(|w| Some(segment_distance(p, w[0]?, w[1]?)))
        .fold(f32::INFINITY, f32::min)
}

fn inside_quad(p: Vec2, q: [Vec2; 4]) -> bool {
    // Same sign of every edge's cross product (the quad is convex on screen).
    let mut sign = 0.0f32;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let c = (b - a).perp_dot(p - a);
        if c.abs() < 1e-6 {
            continue;
        }
        if sign == 0.0 {
            sign = c.signum();
        } else if c.signum() != sign {
            return false;
        }
    }
    sign != 0.0
}

/// The handle under the cursor, if any: the nearest within [`PICK_RADIUS`],
/// with plane squares and the centre box winning over the axes they sit on.
pub fn pick(screen: &Screen, frame: &Frame, mode: GizmoMode, cursor: Vec2) -> Option<Handle> {
    let o = screen.project(frame.origin)?;
    let tip = |i: usize| screen.project(frame.origin + frame.axes[i] * frame.size);
    match mode {
        GizmoMode::Translate => {
            for n in 0..3 {
                let quad = frame.plane_quad(n).map(|c| screen.project(c));
                if let [Some(a), Some(b), Some(c), Some(d)] = quad
                    && inside_quad(cursor, [a, b, c, d])
                {
                    return Some(Handle::Plane(n));
                }
            }
            (0..3)
                .filter_map(|i| Some((i, segment_distance(cursor, o, tip(i)?))))
                .filter(|(_, d)| *d <= PICK_RADIUS)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| Handle::Axis(i))
        }
        GizmoMode::Rotate => {
            let mut best: Option<(Handle, f32)> = None;
            for (h, axis) in [
                (Handle::Axis(0), Some(0)),
                (Handle::Axis(1), Some(1)),
                (Handle::Axis(2), Some(2)),
                (Handle::View, None),
            ] {
                let d = polyline_distance(screen, cursor, &frame.ring(screen, axis));
                if d <= PICK_RADIUS && best.is_none_or(|(_, b)| d < b) {
                    best = Some((h, d));
                }
            }
            best.map(|(h, _)| h)
        }
        GizmoMode::Scale => {
            if cursor.distance(o) <= PICK_RADIUS * 1.5 {
                return Some(Handle::View);
            }
            (0..3)
                .filter_map(|i| Some((i, cursor.distance(tip(i)?))))
                .filter(|(_, d)| *d <= PICK_RADIUS * 1.5)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| Handle::Axis(i))
        }
    }
}

/// A change made by a drag, relative to where it started.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Delta {
    /// Move by this world offset.
    Translate(Vec3),
    /// Rotate by this world rotation about the frame's origin.
    Rotate(Quat),
    /// Multiply the scale along the entity's own axes.
    Scale(Vec3),
}

impl Delta {
    /// No change.
    pub fn is_identity(&self) -> bool {
        match *self {
            Delta::Translate(d) => d == Vec3::ZERO,
            Delta::Rotate(q) => q.angle_between(Quat::IDENTITY) == 0.0,
            Delta::Scale(s) => s == Vec3::ONE,
        }
    }
}

/// A drag in progress: the handle and where it started.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drag {
    /// The handle grabbed.
    pub handle: Handle,
    /// The mode it was grabbed in.
    pub mode: GizmoMode,
    /// The gizmo as it was when the drag started.
    pub frame: Frame,
    /// Where the cursor was.
    pub start: Vec2,
}

fn closest_on_line(ray: (Vec3, Vec3), origin: Vec3, axis: Vec3) -> Option<f32> {
    // The parameter t of the point origin + axis * t nearest to the ray.
    let (ro, rd) = ray;
    let w = origin - ro;
    let b = axis.dot(rd);
    let denom = 1.0 - b * b;
    if denom.abs() < 1e-6 {
        return None; // the axis points at the camera
    }
    Some((b * rd.dot(w) - axis.dot(w)) / denom)
}

fn hit_plane(ray: (Vec3, Vec3), origin: Vec3, normal: Vec3) -> Option<Vec3> {
    let (ro, rd) = ray;
    let denom = normal.dot(rd);
    if denom.abs() < 1e-6 {
        return None;
    }
    let t = normal.dot(origin - ro) / denom;
    (t > 0.0).then(|| ro + rd * t)
}

fn snap(v: f32, step: f32) -> f32 {
    if step > 0.0 {
        (v / step).round() * step
    } else {
        v
    }
}

impl Drag {
    /// The change from the drag's start to the cursor at `cursor`. Snapping
    /// rounds the change, measured along the gizmo's axes, to the settings'
    /// steps.
    pub fn delta(&self, screen: &Screen, cursor: Vec2, settings: &GizmoSettings) -> Delta {
        let f = &self.frame;
        match (self.mode, self.handle) {
            (GizmoMode::Translate, Handle::Axis(i)) => {
                let axis = f.axes[i];
                let t = |s| closest_on_line(screen.ray(s), f.origin, axis);
                let mut d = match (t(self.start), t(cursor)) {
                    (Some(a), Some(b)) => b - a,
                    _ => 0.0,
                };
                if settings.snap {
                    d = snap(d, settings.translate_step);
                }
                Delta::Translate(axis * d)
            }
            (GizmoMode::Translate, Handle::Plane(n)) => {
                let hit = |s| hit_plane(screen.ray(s), f.origin, f.axes[n]);
                let mut d = match (hit(self.start), hit(cursor)) {
                    (Some(a), Some(b)) => b - a,
                    _ => Vec3::ZERO,
                };
                if settings.snap {
                    // Snap the two in-plane components along the gizmo's axes.
                    let mut out = Vec3::ZERO;
                    for k in [(n + 1) % 3, (n + 2) % 3] {
                        out += f.axes[k] * snap(d.dot(f.axes[k]), settings.translate_step);
                    }
                    d = out;
                }
                Delta::Translate(d)
            }
            (GizmoMode::Rotate, h) => {
                let normal = match h {
                    Handle::Axis(i) => f.axes[i],
                    _ => (screen.eye - f.origin).normalize_or(Vec3::Z),
                };
                let Some(o) = screen.project(f.origin) else {
                    return Delta::Rotate(Quat::IDENTITY);
                };
                let (v0, v1) = (self.start - o, cursor - o);
                if v0.length() < 1.0 || v1.length() < 1.0 {
                    return Delta::Rotate(Quat::IDENTITY);
                }
                // Counter-clockwise on screen (y is down) is a positive turn
                // about an axis pointing at the camera.
                let ccw = -libm::atan2f(v0.perp_dot(v1), v0.dot(v1));
                let facing = normal.dot(screen.eye - f.origin);
                let mut angle = if facing >= 0.0 { ccw } else { -ccw };
                if settings.snap {
                    angle = snap(angle, settings.angle_step_deg.to_radians());
                }
                Delta::Rotate(Quat::from_axis_angle(normal, angle))
            }
            (GizmoMode::Scale, h) => {
                let Some(o) = screen.project(f.origin) else {
                    return Delta::Scale(Vec3::ONE);
                };
                let factor = |along: Vec2, len: f32| {
                    let t = (cursor - self.start).dot(along) / len.max(1.0);
                    let mut s = 1.0 + t;
                    if settings.snap {
                        s = snap(s, settings.scale_step);
                    }
                    s.max(0.001)
                };
                match h {
                    Handle::Axis(i) => {
                        let Some(tip) = screen.project(f.origin + f.axes[i] * f.size) else {
                            return Delta::Scale(Vec3::ONE);
                        };
                        let along = tip - o;
                        let s = factor(along.normalize_or_zero(), along.length());
                        let mut v = Vec3::ONE;
                        v[i] = s;
                        Delta::Scale(v)
                    }
                    _ => {
                        // Right or up grows, left or down shrinks.
                        let s = factor(Vec2::new(1.0, -1.0).normalize(), HANDLE_POINTS);
                        Delta::Scale(Vec3::splat(s))
                    }
                }
            }
            (GizmoMode::Translate, Handle::View) => Delta::Translate(Vec3::ZERO),
        }
    }
}

/// `world` changed by `delta`, with `pivot` the centre of rotation.
pub fn apply(world: &Affine3A, delta: &Delta, pivot: Vec3) -> Affine3A {
    let (s, r, t) = world.to_scale_rotation_translation();
    let (s, r, t) = match *delta {
        Delta::Translate(d) => (s, r, t + d),
        Delta::Rotate(q) => (s, (q * r).normalize(), pivot + q * (t - pivot)),
        Delta::Scale(f) => (s * f, r, t),
    };
    Affine3A::from_scale_rotation_translation(s, r, t)
}

/// The entity's local transform `(scale, rotation, translation)` changed by
/// `delta`, a change made in world space about the entity's own origin,
/// under a parent at `parent` (identity for a root).
///
/// The change is carried into the parent's space rather than read back from
/// a world matrix, so a parent with non-uniform scale does not leak shear
/// into the rotation or scale of a plain move.
pub fn apply_local(
    local: (Vec3, Quat, Vec3),
    parent: &Affine3A,
    delta: &Delta,
) -> (Vec3, Quat, Vec3) {
    let (s, r, t) = local;
    match *delta {
        Delta::Translate(d) => (s, r, t + parent.inverse().transform_vector3(d)),
        Delta::Rotate(q) => {
            let (_, pr, _) = parent.to_scale_rotation_translation();
            let pr = pr.normalize();
            (s, (pr.inverse() * q * pr * r).normalize(), t)
        }
        Delta::Scale(f) => (s * f, r, t),
    }
}

/// Whether two unit quaternions are the same rotation, within about 0.001°.
/// (`Quat::angle_between` uses an approximate `acos` that reads up to 0.05°
/// for two equal rotations.)
pub fn same_rotation(a: Quat, b: Quat) -> bool {
    let d = Vec4::from(a) - Vec4::from(b);
    let s = Vec4::from(a) + Vec4::from(b);
    d.length_squared().min(s.length_squared()) < 1e-10
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A camera at (0, 0, 10) looking at the origin, in an 800 × 600 viewport.
    fn screen() -> Screen {
        Screen::new(
            Mat4::from_translation(Vec3::new(0.0, 0.0, 10.0)),
            60f32.to_radians(),
            0.1,
            1000.0,
            Vec2::new(100.0, 50.0),
            Vec2::new(800.0, 600.0),
        )
        .unwrap()
    }

    fn frame(s: &Screen) -> Frame {
        Frame::new(
            s,
            &Affine3A::IDENTITY,
            Orientation::World,
            GizmoMode::Translate,
        )
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        a.distance(b) < 1e-3
    }

    #[test]
    fn projection_round_trips() {
        let s = screen();
        let centre = s.project(Vec3::ZERO).unwrap();
        assert!(centre.distance(Vec2::new(500.0, 350.0)) < 1e-3, "{centre}");
        let (o, d) = s.ray(centre);
        assert!(close(o + d * (o.z / -d.z), Vec3::ZERO), "{o} {d}");
        // Up in the world is up on screen (smaller y).
        assert!(s.project(Vec3::Y).unwrap().y < centre.y);
        assert!(
            s.project(Vec3::new(0.0, 0.0, 20.0)).is_none(),
            "behind the camera"
        );
    }

    #[test]
    fn handles_are_a_constant_size_on_screen() {
        let s = screen();
        let f = frame(&s);
        let o = s.project(f.origin).unwrap();
        let tip = s.project(f.origin + f.axes[0] * f.size).unwrap();
        assert!(
            (o.distance(tip) - HANDLE_POINTS).abs() < 0.5,
            "{}",
            o.distance(tip)
        );
    }

    #[test]
    fn picking_finds_the_handle_under_the_cursor() {
        let s = screen();
        let f = frame(&s);
        let o = s.project(Vec3::ZERO).unwrap();
        let along = |axis: Vec3, k: f32| s.project(axis * f.size * k).unwrap();
        let t = GizmoMode::Translate;
        assert_eq!(pick(&s, &f, t, along(Vec3::X, 0.9)), Some(Handle::Axis(0)));
        assert_eq!(pick(&s, &f, t, along(Vec3::Y, 0.9)), Some(Handle::Axis(1)));
        assert_eq!(
            pick(&s, &f, t, along(Vec3::X + Vec3::Y, 0.375)),
            Some(Handle::Plane(2))
        );
        assert_eq!(pick(&s, &f, t, o + Vec2::new(-60.0, 60.0)), None);
        // Rotate: a point on the Z ring (it faces the camera) and on no other.
        let r = GizmoMode::Rotate;
        let p = s.project((Vec3::X + Vec3::Y).normalize() * f.size).unwrap();
        assert_eq!(pick(&s, &f, r, p), Some(Handle::Axis(2)));
        let view = s
            .project((Vec3::X - Vec3::Y).normalize() * f.size * 1.2)
            .unwrap();
        assert_eq!(pick(&s, &f, r, view), Some(Handle::View));
        // Scale: the centre and the tips.
        let sc = GizmoMode::Scale;
        assert_eq!(pick(&s, &f, sc, o), Some(Handle::View));
        assert_eq!(pick(&s, &f, sc, along(Vec3::Y, 1.0)), Some(Handle::Axis(1)));
    }

    #[test]
    fn translate_along_an_axis_follows_the_cursor() {
        let s = screen();
        let f = frame(&s);
        let start = s.project(Vec3::X * f.size * 0.8).unwrap();
        let drag = Drag {
            handle: Handle::Axis(0),
            mode: GizmoMode::Translate,
            frame: f,
            start,
        };
        // The cursor where world (2, 0, 0) is drawn, then 30 points off the
        // axis: the part across the axis barely counts (perspective).
        let settings = GizmoSettings::default();
        let to = s.project(Vec3::new(f.size * 0.8 + 2.0, 0.0, 0.0)).unwrap();
        for (off, tolerance) in [(0.0, 1e-3), (30.0, 0.05)] {
            match drag.delta(&s, to + Vec2::new(0.0, off), &settings) {
                Delta::Translate(d) => {
                    assert!(
                        d.distance(Vec3::new(2.0, 0.0, 0.0)) < tolerance,
                        "{off}: {d}"
                    )
                }
                other => panic!("{other:?}"),
            }
        }
        let snapped = GizmoSettings {
            snap: true,
            translate_step: 0.5,
            ..settings
        };
        let to = s.project(Vec3::new(f.size * 0.8 + 1.3, 0.0, 0.0)).unwrap();
        assert_eq!(
            drag.delta(&s, to, &snapped),
            Delta::Translate(Vec3::new(1.5, 0.0, 0.0))
        );
    }

    #[test]
    fn translate_in_a_plane_moves_in_both_axes() {
        let s = screen();
        let f = frame(&s);
        let start = s.project((Vec3::X + Vec3::Y) * f.size * 0.375).unwrap();
        let drag = Drag {
            handle: Handle::Plane(2),
            mode: GizmoMode::Translate,
            frame: f,
            start,
        };
        let to = s
            .project((Vec3::X + Vec3::Y) * f.size * 0.375 + Vec3::new(1.0, -2.0, 0.0))
            .unwrap();
        match drag.delta(&s, to, &GizmoSettings::default()) {
            Delta::Translate(d) => assert!(close(d, Vec3::new(1.0, -2.0, 0.0)), "{d}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rotate_follows_the_angle_around_the_centre() {
        let s = screen();
        let f = frame(&s);
        let o = s.project(Vec3::ZERO).unwrap();
        // From the right of the centre to above it: a quarter turn
        // counter-clockwise on screen, +90° about +Z (which faces the camera).
        let drag = Drag {
            handle: Handle::Axis(2),
            mode: GizmoMode::Rotate,
            frame: f,
            start: o + Vec2::new(90.0, 0.0),
        };
        let to = o + Vec2::new(0.0, -90.0);
        match drag.delta(&s, to, &GizmoSettings::default()) {
            Delta::Rotate(q) => {
                assert!(close(q * Vec3::X, Vec3::Y), "{q}");
            }
            other => panic!("{other:?}"),
        }
        // Snapped to 15°: 50° becomes 45°.
        let a = 50f32.to_radians();
        let to = o + Vec2::new(libm::cosf(a), -libm::sinf(a)) * 90.0;
        let snapped = GizmoSettings {
            snap: true,
            ..GizmoSettings::default()
        };
        match drag.delta(&s, to, &snapped) {
            Delta::Rotate(q) => assert!((q.to_axis_angle().1 - 45f32.to_radians()).abs() < 1e-4),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn scale_along_an_axis_and_uniformly() {
        let s = screen();
        let f = Frame::new(
            &s,
            &Affine3A::IDENTITY,
            Orientation::World,
            GizmoMode::Scale,
        );
        let tip = s.project(Vec3::X * f.size).unwrap();
        let o = s.project(Vec3::ZERO).unwrap();
        let drag = Drag {
            handle: Handle::Axis(0),
            mode: GizmoMode::Scale,
            frame: f,
            start: tip,
        };
        // Dragging the tip one handle length further doubles X.
        let to = tip + (tip - o);
        match drag.delta(&s, to, &GizmoSettings::default()) {
            Delta::Scale(v) => assert!(close(v, Vec3::new(2.0, 1.0, 1.0)), "{v}"),
            other => panic!("{other:?}"),
        }
        let uni = Drag {
            handle: Handle::View,
            mode: GizmoMode::Scale,
            frame: f,
            start: o,
        };
        match uni.delta(
            &s,
            o + Vec2::new(-1000.0, 1000.0),
            &GizmoSettings::default(),
        ) {
            Delta::Scale(v) => assert!(
                v.x == 0.001 && v.y == v.x && v.z == v.x,
                "shrinks, never below zero: {v}"
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn local_orientation_follows_the_entity() {
        let s = screen();
        let q = Quat::from_rotation_z(90f32.to_radians());
        let world = Affine3A::from_rotation_translation(q, Vec3::new(1.0, 2.0, 0.0));
        let local = Frame::new(&s, &world, Orientation::Local, GizmoMode::Translate);
        assert!(close(local.axes[0], Vec3::Y));
        assert!(close(local.origin, Vec3::new(1.0, 2.0, 0.0)));
        let global = Frame::new(&s, &world, Orientation::World, GizmoMode::Translate);
        assert!(close(global.axes[0], Vec3::X));
        // Scale always takes the entity's axes.
        let scale = Frame::new(&s, &world, Orientation::World, GizmoMode::Scale);
        assert!(close(scale.axes[0], Vec3::Y));
    }

    #[test]
    fn deltas_apply_about_the_pivot_and_convert_to_local() {
        let world = Affine3A::from_translation(Vec3::new(2.0, 0.0, 0.0));
        let q = Quat::from_rotation_y(90f32.to_radians());
        let turned = apply(&world, &Delta::Rotate(q), Vec3::ZERO);
        let (_, r, t) = turned.to_scale_rotation_translation();
        assert!(close(t, Vec3::new(0.0, 0.0, -2.0)), "{t}");
        assert!(same_rotation(r, q), "{r} {q}");
        let scaled = apply(&world, &Delta::Scale(Vec3::new(2.0, 1.0, 1.0)), Vec3::ZERO);
        assert!(close(
            scaled.to_scale_rotation_translation().0,
            Vec3::new(2.0, 1.0, 1.0)
        ));
        // Under a parent moved by (0, 5, 0) and scaled by 2: a world move of
        // (4, 0, 0) is a local move of (2, 0, 0).
        let parent = Affine3A::from_scale_rotation_translation(
            Vec3::splat(2.0),
            Quat::IDENTITY,
            Vec3::new(0.0, 5.0, 0.0),
        );
        let local = (Vec3::ONE, Quat::IDENTITY, Vec3::ZERO);
        let (s, r, t) = apply_local(local, &parent, &Delta::Translate(Vec3::new(4.0, 0.0, 0.0)));
        assert!(
            close(t, Vec3::new(2.0, 0.0, 0.0)) && close(s, Vec3::ONE),
            "{t} {s}"
        );
        assert!(same_rotation(r, Quat::IDENTITY), "{r}");
        // A non-uniformly scaled parent: moving a rotated child keeps its
        // rotation and scale exactly.
        let parent = Affine3A::from_scale(Vec3::new(2.0, 1.0, 1.0));
        let q45 = Quat::from_rotation_z(45f32.to_radians());
        let local = (Vec3::ONE, q45, Vec3::ZERO);
        let (s, r, t) = apply_local(local, &parent, &Delta::Translate(Vec3::Z));
        assert_eq!((s, r), (Vec3::ONE, q45));
        assert!(close(t, Vec3::Z), "{t}");
        // A world rotation under a rotated parent turns about the same world axis.
        let parent = Affine3A::from_rotation_y(90f32.to_radians());
        let (_, r, _) = apply_local(
            (Vec3::ONE, Quat::IDENTITY, Vec3::ZERO),
            &parent,
            &Delta::Rotate(q45),
        );
        let world = Quat::from_rotation_y(90f32.to_radians()) * r;
        assert!(
            same_rotation(world, q45 * Quat::from_rotation_y(90f32.to_radians())),
            "{world}"
        );
        assert!(Delta::Translate(Vec3::ZERO).is_identity());
    }

    #[test]
    fn a_degenerate_viewport_has_no_screen() {
        let cam = Mat4::from_translation(Vec3::Z);
        assert!(Screen::new(cam, 1.0, 0.1, 100.0, Vec2::ZERO, Vec2::new(0.0, 10.0)).is_none());
        assert!(Screen::new(Mat4::ZERO, 1.0, 0.1, 100.0, Vec2::ZERO, Vec2::ONE).is_none());
    }
}
