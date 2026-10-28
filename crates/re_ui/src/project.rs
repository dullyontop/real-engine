//! Placing UI in the world: the camera that projects a world root (a native
//! `UiRoot` in world space, or compat's `BillboardGui`) onto the screen.
//! World units are metres for native scenes and studs for compat.

use glam::{Quat, Vec2, Vec3};

/// The camera world roots are placed by.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    /// Where the camera is, in world units.
    pub position: Vec3,
    /// Its orientation; it looks along its -Z axis.
    pub rotation: Quat,
    /// The vertical field of view in radians.
    pub fov_y: f32,
}

impl Camera {
    /// The screen point of `p` in a `viewport`-pixel view and its distance in
    /// front of the camera, `None` behind it.
    pub fn project(&self, p: Vec3, viewport: Vec2) -> Option<(Vec2, f32)> {
        let v = self.rotation.inverse() * (p - self.position);
        let depth = -v.z;
        if depth <= 0.05 {
            return None;
        }
        let f = self.focal(viewport);
        Some((
            Vec2::new(
                viewport.x * 0.5 + f * v.x / depth,
                viewport.y * 0.5 - f * v.y / depth,
            ),
            depth,
        ))
    }

    /// Pixels per world unit at one unit in front of the camera.
    pub fn focal(&self, viewport: Vec2) -> f32 {
        viewport.y * 0.5 / libm::tanf(self.fov_y.clamp(0.01, 3.1) * 0.5)
    }

    /// The unit direction through screen point `q` of a `viewport`-pixel
    /// view.
    pub fn ray(&self, q: Vec2, viewport: Vec2) -> Vec3 {
        let f = self.focal(viewport);
        let local = Vec3::new(
            (q.x - viewport.x * 0.5) / f,
            -(q.y - viewport.y * 0.5) / f,
            -1.0,
        );
        (self.rotation * local).normalize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projects_and_casts_rays_back() {
        let cam = Camera {
            position: Vec3::new(1.0, 2.0, 3.0),
            rotation: Quat::from_rotation_y(0.3) * Quat::from_rotation_x(-0.2),
            fov_y: 70f32.to_radians(),
        };
        let view = Vec2::new(800.0, 600.0);
        let ahead = cam.position + cam.rotation * Vec3::new(0.0, 0.0, -10.0);
        let (p, depth) = cam.project(ahead, view).expect("in front");
        assert!((p - view * 0.5).length() < 1e-3, "{p}");
        assert!((depth - 10.0).abs() < 1e-4, "{depth}");
        // A ray through a screen point projects back onto it.
        let q = Vec2::new(100.0, 450.0);
        let (back, _) = cam
            .project(cam.position + cam.ray(q, view) * 7.0, view)
            .expect("in front");
        assert!((back - q).length() < 1e-2, "{back}");
        // Behind the camera, or too close to it, is off the screen.
        assert_eq!(
            cam.project(cam.position + cam.rotation * Vec3::Z, view),
            None
        );
        assert_eq!(
            cam.project(
                cam.position + cam.rotation * Vec3::new(0.0, 0.0, -0.04),
                view
            ),
            None
        );
        // One unit at one unit away spans the focal length.
        let half = libm::tanf(35f32.to_radians());
        assert!((cam.focal(view) - 300.0 / half).abs() < 1e-3);
    }
}
