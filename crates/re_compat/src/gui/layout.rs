//! Where billboards go: a `BillboardGui` placed on the screen by the camera
//! looking at its adornee. The UI core lays out what is under it
//! ([`re_ui::Layout`]).

use glam::{Quat, Vec2, Vec3};
use re_scene::{Entity, GlobalTransform};

use super::props::Scope;
use crate::instance::tree;

/// The camera billboards are placed by.
#[derive(Clone, Copy, Debug)]
pub struct GuiCamera {
    /// Where the camera is, in studs.
    pub position: Vec3,
    /// Its orientation; it looks along its -Z axis.
    pub rotation: Quat,
    /// The vertical field of view in radians.
    pub fov_y: f32,
}

impl GuiCamera {
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

    /// Pixels per stud at one stud in front of the camera.
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

/// The world position a billboard is drawn at, before its offsets.
pub(crate) fn adornee_position(s: &Scope, bb: Entity) -> Option<Vec3> {
    let w = s.w;
    let p = s.props(bb);
    let at = p.entity("Adornee").or_else(|| tree::parent(w, bb))?;
    // The pose scripts and physics wrote this frame, else the propagated
    // transform.
    let pos = |e: Entity| {
        crate::physics::world_pose(w, e).map(|p| p.pos).or_else(|| {
            w.ecs
                .get::<GlobalTransform>(e)
                .map(|g| Vec3::from(g.0.translation))
        })
    };
    pos(at).or_else(|| {
        // A model: the first descendant with a transform.
        tree::descendants(w, at).into_iter().find_map(pos)
    })
}

/// A billboard's screen rectangle and depth.
pub(crate) fn billboard_rect(
    s: &Scope,
    bb: Entity,
    cam: &GuiCamera,
    viewport: Vec2,
) -> Option<(Vec2, Vec2, f32)> {
    let p = s.props(bb);
    let base = adornee_position(s, bb)?;
    let local = p.vec3("StudsOffset");
    let at = base + p.vec3("StudsOffsetWorldSpace") + cam.rotation * local;
    let (center, depth) = cam.project(at, viewport)?;
    if (at - cam.position).length() > p.f32("MaxDistance", f32::INFINITY) {
        return None;
    }
    let size = p.udim2("Size");
    let per_stud = cam.focal(viewport) / depth;
    let px = Vec2::new(
        size.x.scale * per_stud + size.x.offset,
        size.y.scale * per_stud + size.y.offset,
    );
    let shift = p.vec2("SizeOffset", Vec2::ZERO) * px;
    Some((center - px * 0.5 + Vec2::new(shift.x, -shift.y), px, depth))
}
