//! `SurfaceGui`s (ADR-0048 §2): UI laid out on a canvas and drawn on a face
//! of a part as a textured quad, and the host's record of which surface
//! textures need painting again.

use std::collections::HashMap;

use egui::epaint::ClippedShape;
use glam::{Mat4, Quat, Vec2, Vec3};
use re_scene::Entity;
use re_types::render::QuadDepth;

use super::props::Scope;
use crate::instance::tree;

/// The largest side of a surface's texture, in pixels; a larger canvas is
/// scaled down to fit.
pub const MAX_TEXTURE: f32 = 2048.0;

/// What the host draws for one `SurfaceGui`: its shapes painted into a
/// texture of `texture_px` pixels, shown on a face of a part.
#[derive(Clone, Debug)]
pub struct SurfaceDraw {
    /// The `SurfaceGui`.
    pub entity: Entity,
    /// The part it is drawn on.
    pub part: Entity,
    /// The canvas size in layout pixels (what `AbsoluteSize` reports).
    pub canvas: Vec2,
    /// The texture size in pixels.
    pub texture_px: [u32; 2],
    /// Texture pixels per layout pixel (below 1 when the canvas was scaled
    /// down): the `pixels_per_point` to tessellate the shapes with.
    pub pixels_per_point: f32,
    /// The shapes, back to front, in canvas pixels.
    pub shapes: Vec<ClippedShape>,
    /// Quad-to-world transform: the face's centre, with +X along the
    /// canvas's right, +Y along its up and +Z out of the face.
    pub world: Mat4,
    /// The face's width and height in studs.
    pub size: Vec2,
    /// Depth-tested a little off the face, or over everything.
    pub depth: QuadDepth,
    /// `LightInfluence`, clamped to 0–1.
    pub light_influence: f32,
    /// `Brightness`.
    pub brightness: f32,
}

/// Where a surface sits on its part.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Placement {
    pub part: Entity,
    pos: Vec3,
    rot: Quat,
    half: Vec3,
    /// The face's normal, right and up axes in the part's frame.
    axes: (Vec3, Vec3, Vec3),
    /// The face's width and height in studs.
    pub size: Vec2,
}

/// The normal, right and up axes of a `NormalId` face in a part's frame:
/// the UI is upright seen from outside the face, and on the top and bottom
/// faces its top points to the part's front. Right × up is the normal.
fn face_axes(face: &str) -> (Vec3, Vec3, Vec3) {
    match face {
        "Back" => (Vec3::Z, Vec3::X, Vec3::Y),
        "Right" => (Vec3::X, -Vec3::Z, Vec3::Y),
        "Left" => (-Vec3::X, Vec3::Z, Vec3::Y),
        "Top" => (Vec3::Y, Vec3::X, -Vec3::Z),
        "Bottom" => (-Vec3::Y, -Vec3::X, -Vec3::Z),
        _ => (-Vec3::Z, -Vec3::X, Vec3::Y),
    }
}

impl Placement {
    /// The surface's part (its `Adornee`, else its parent) and face, `None`
    /// when it has no part to sit on.
    pub(crate) fn of(s: &Scope, e: Entity) -> Option<Placement> {
        let p = s.props(e);
        let part = p
            .entity("Adornee")
            .or_else(|| tree::parent(s.w, e))
            .filter(|&x| tree::alive(s.w, x) && s.is_a(x, "BasePart"))?;
        let pose = crate::physics::world_pose(s.w, part)?;
        let size = s.props(part).vec3("Size").abs();
        if !(size.is_finite() && pose.pos.is_finite() && pose.rot.is_finite()) {
            return None;
        }
        let axes = face_axes(&p.item("Face", "Front"));
        Some(Placement {
            part,
            pos: pose.pos,
            rot: pose.rot.normalize(),
            half: size * 0.5,
            axes,
            size: Vec2::new(axes.1.abs().dot(size), axes.2.abs().dot(size)),
        })
    }

    /// The face's centre in the world.
    pub(crate) fn center(&self) -> Vec3 {
        self.pos + self.rot * (self.axes.0 * self.half)
    }

    /// Quad-to-world: the face's centre and axes.
    pub(crate) fn world(&self) -> Mat4 {
        let (n, r, u) = self.axes;
        Mat4::from_cols(
            (self.rot * r).extend(0.0),
            (self.rot * u).extend(0.0),
            (self.rot * n).extend(0.0),
            self.center().extend(1.0),
        )
    }

    /// Where a ray from `origin` along unit `dir` first enters the part,
    /// when it enters through this face: the distance and the point on the
    /// face from its top-left corner (0) to its bottom-right (1).
    pub(crate) fn hit(&self, origin: Vec3, dir: Vec3) -> Option<(f32, Vec2)> {
        let inv = self.rot.inverse();
        let o = inv * (origin - self.pos);
        let d = inv * dir;
        let mut near = f32::NEG_INFINITY;
        let mut far = f32::INFINITY;
        let mut entry = Vec3::ZERO;
        for i in 0..3 {
            let h = self.half[i];
            if d[i].abs() < 1e-9 {
                if o[i].abs() > h {
                    return None;
                }
                continue;
            }
            let t1 = (-h - o[i]) / d[i];
            let t2 = (h - o[i]) / d[i];
            let (a, b, side) = if t1 < t2 {
                (t1, t2, -1.0)
            } else {
                (t2, t1, 1.0)
            };
            if a > near {
                near = a;
                entry = Vec3::ZERO;
                entry[i] = side;
            }
            far = far.min(b);
        }
        if near > far || near < 0.0 || entry != self.axes.0 {
            return None;
        }
        let q = o + d * near;
        let (_, r, u) = self.axes;
        let at = Vec2::new(
            q.dot(r) / self.size.x.max(1e-6) + 0.5,
            0.5 - q.dot(u) / self.size.y.max(1e-6),
        );
        Some((near, at.clamp(Vec2::ZERO, Vec2::ONE)))
    }
}

/// The canvas of a surface in layout pixels, and the texture pixels per
/// layout pixel (below 1 when the canvas is larger than [`MAX_TEXTURE`]).
pub(crate) fn canvas(s: &Scope, e: Entity, at: &Placement) -> (Vec2, f32) {
    let p = s.props(e);
    let c = if p.item("SizingMode", "FixedSize") == "PixelsPerStud" {
        at.size * p.f32("PixelsPerStud", 50.0).max(0.0)
    } else {
        p.vec2("CanvasSize", Vec2::new(800.0, 600.0))
    };
    let c = if c.is_finite() {
        c.max(Vec2::ONE)
    } else {
        Vec2::ONE
    };
    let big = c.max_element();
    (
        c,
        if big > MAX_TEXTURE {
            MAX_TEXTURE / big
        } else {
            1.0
        },
    )
}

/// The texture size of a canvas at `k` texture pixels per layout pixel.
pub(crate) fn texture_px(canvas: Vec2, k: f32) -> [u32; 2] {
    let px = (canvas * k)
        .ceil()
        .clamp(Vec2::ONE, Vec2::splat(MAX_TEXTURE));
    [px.x as u32, px.y as u32]
}

/// How the quad meets the scene: `AlwaysOnTop` draws it over everything;
/// otherwise it is lifted a little off the face, more for a higher
/// `ZOffset`.
pub(crate) fn depth(s: &Scope, e: Entity) -> QuadDepth {
    let p = s.props(e);
    if p.bool("AlwaysOnTop", false) {
        QuadDepth::OnTop
    } else {
        let z = p.f32("ZOffset", 0.0);
        let z = if z.is_finite() { z } else { 0.0 };
        QuadDepth::Tested {
            bias: (0.02 + 0.01 * z).clamp(0.0, 1.0),
        }
    }
}

/// Which surfaces the host paints this frame, and the ones it can forget.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SurfacePlan {
    /// Indices into the frame's surfaces to paint again.
    pub redraw: Vec<usize>,
    /// Surfaces drawn before that the frame no longer has: free their
    /// textures.
    pub gone: Vec<Entity>,
}

struct Painted {
    px: [u32; 2],
    shapes: Vec<ClippedShape>,
    frame: u64,
}

/// The host's record of what each surface texture holds, so a surface is
/// painted again only when its picture changed (its shapes or its texture
/// size), and at most a budget of them a frame (`[compat.gui]
/// surface_redraws_per_frame`); the rest keep last frame's texture.
#[derive(Default)]
pub struct SurfaceRedraws {
    painted: HashMap<Entity, Painted>,
    frame: u64,
}

impl SurfaceRedraws {
    /// The plan for this frame's `surfaces`: the ones never painted first,
    /// then the changed ones that have waited longest, at most `budget` of
    /// them. Each one planned counts as painted.
    pub fn plan(&mut self, surfaces: &[SurfaceDraw], budget: usize) -> SurfacePlan {
        self.frame += 1;
        let gone: Vec<Entity> = self
            .painted
            .keys()
            .filter(|e| !surfaces.iter().any(|s| s.entity == **e))
            .copied()
            .collect();
        for e in &gone {
            self.painted.remove(e);
        }
        let mut due: Vec<(u64, usize)> = surfaces
            .iter()
            .enumerate()
            .filter_map(|(i, s)| match self.painted.get(&s.entity) {
                None => Some((0, i)),
                Some(p) if p.px != s.texture_px || p.shapes != s.shapes => Some((p.frame, i)),
                Some(_) => None,
            })
            .collect();
        due.sort_unstable();
        due.truncate(budget);
        let mut redraw: Vec<usize> = due.into_iter().map(|(_, i)| i).collect();
        redraw.sort_unstable();
        for &i in &redraw {
            let s = &surfaces[i];
            self.painted.insert(
                s.entity,
                Painted {
                    px: s.texture_px,
                    shapes: s.shapes.clone(),
                    frame: self.frame,
                },
            );
        }
        let mut gone = gone;
        gone.sort_unstable();
        SurfacePlan { redraw, gone }
    }

    /// Marks a surface as needing paint even if its shapes are the same (a
    /// viewport on it drew again).
    pub fn invalidate(&mut self, e: Entity) {
        if let Some(p) = self.painted.get_mut(&e) {
            p.shapes.clear();
            p.px = [0, 0];
        }
    }

    /// Forgets every texture (the host lost them): each surface is painted
    /// again.
    pub fn clear(&mut self) {
        self.painted.clear();
    }
}
