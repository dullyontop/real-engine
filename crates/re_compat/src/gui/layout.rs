//! Where billboards go: a `BillboardGui` placed on the screen by the camera
//! looking at its adornee. The UI core lays out what is under it
//! ([`re_ui::Layout`]).

use glam::{Vec2, Vec3};
use re_scene::{Entity, GlobalTransform};

use super::props::Scope;
use crate::instance::tree;

/// The camera billboards are placed by: the UI core's, which places native
/// world roots too (ADR-0058).
pub use re_ui::project::Camera as GuiCamera;

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
