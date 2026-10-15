//! What a frame can draw beyond a [`RenderSnapshot`](re_types::render::RenderSnapshot):
//! the environment (ambient, fog, sky, colour correction), spot lights,
//! camera-facing sprites, ribbons and highlights.
//!
//! [`SceneExtras`] is given to [`Renderer::render_with`](crate::Renderer::render_with)
//! next to the snapshot. It is renderer data, not a frozen contract: the
//! Roblox-style layer (`re_compat`) produces its own plain description of a
//! frame, and the host maps it to these types.
//!
//! Sprites and ribbons are expanded into camera-facing quads on the CPU, sorted
//! back to front and drawn in one call after the scene's blended items, depth
//! tested but not depth written. They are unlit; `emission` 1 makes them
//! additive. Highlights are drawn last, over the finished image.

use glam::Vec3;
use re_types::ids::EntityId;

/// The most spot lights one frame draws (the nearest to the camera).
pub const MAX_SPOT_LIGHTS: usize = 8;

/// The most highlights one frame draws; later ones are left out with an
/// `RE6018` warning.
pub const MAX_HIGHLIGHTS: usize = 31;

/// Everything a frame draws besides the snapshot. The default draws nothing
/// extra and keeps the renderer settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SceneExtras {
    /// Replaces the settings' ambient light for this frame.
    pub ambient: Option<Ambient>,
    /// Distance fog.
    pub fog: Option<Fog>,
    /// A sky gradient drawn behind the scene instead of the clear colour.
    pub sky: Option<Sky>,
    /// Colour correction of the scene (not of the UI).
    pub grading: Option<ColorGrading>,
    /// Spot lights; past [`MAX_SPOT_LIGHTS`] only the nearest are drawn.
    pub spot_lights: Vec<SpotLight>,
    /// Camera-facing quads (particles).
    pub sprites: Vec<Sprite>,
    /// Strips through points (beams and trails).
    pub ribbons: Vec<Ribbon>,
    /// Outlines and fills over sets of draw items.
    pub highlights: Vec<Highlight>,
}

impl SceneExtras {
    /// True when there is nothing to draw beyond the snapshot.
    pub fn is_empty(&self) -> bool {
        *self == SceneExtras::default()
    }
}

/// Hemisphere ambient light, linear RGB in the units of a directional light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ambient {
    /// From above.
    pub sky: [f32; 3],
    /// From below.
    pub ground: [f32; 3],
}

/// Linear distance fog: none before `start`, full `color` from `end` on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    /// Where the fog begins, in world units from the camera.
    pub start: f32,
    /// Where it is opaque.
    pub end: f32,
    /// Linear RGB. It is not lit: the fog looks this colour at any exposure.
    pub color: [f32; 3],
}

/// A vertical sky gradient, linear RGB, shown where nothing is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sky {
    /// Straight up.
    pub zenith: [f32; 3],
    /// At the horizon.
    pub horizon: [f32; 3],
    /// Straight down.
    pub ground: [f32; 3],
    /// A sun disc towards this direction (the way to the sun), with its colour.
    pub sun: Option<(Vec3, [f32; 3])>,
}

/// Colour correction applied to the lit scene, sprites and ribbons.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorGrading {
    /// Added to every channel.
    pub brightness: f32,
    /// 0 keeps the contrast; positive values stretch away from mid grey.
    pub contrast: f32,
    /// 0 keeps the saturation; -1 is grey.
    pub saturation: f32,
    /// Multiplies the result, linear RGB.
    pub tint: [f32; 3],
}

impl Default for ColorGrading {
    fn default() -> Self {
        ColorGrading {
            brightness: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            tint: [1.0; 3],
        }
    }
}

/// A cone of light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpotLight {
    /// World position.
    pub position: Vec3,
    /// The direction the light shines, normalised.
    pub direction: Vec3,
    /// Linear RGB colour.
    pub color: [f32; 3],
    /// Intensity in lumens, as for a point light.
    pub intensity: f32,
    /// Distance at which the light reaches zero.
    pub range: f32,
    /// Half the cone's angle, in radians; the light fades over the outer 20 %.
    pub half_angle: f32,
}

/// The fade of a sprite's quad.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpriteShape {
    /// A round spot that fades towards its rim.
    #[default]
    Soft,
    /// The whole quad, evenly.
    Square,
}

/// One camera-facing quad.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    /// The centre, in world space.
    pub position: Vec3,
    /// The side, in world units.
    pub size: f32,
    /// Rotation about the view axis, in radians.
    pub rotation: f32,
    /// Linear RGB and opacity.
    pub color: [f32; 4],
    /// 0 blends over what is behind; 1 adds to it.
    pub emission: f32,
    /// How the quad fades.
    pub shape: SpriteShape,
}

/// One point of a [`Ribbon`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RibbonPoint {
    /// World position.
    pub position: Vec3,
    /// The strip's width here, in world units.
    pub width: f32,
    /// Linear RGB and opacity here.
    pub color: [f32; 4],
}

/// A strip through two or more points.
#[derive(Clone, Debug, PartialEq)]
pub struct Ribbon {
    /// The points, in order.
    pub points: Vec<RibbonPoint>,
    /// `None` turns the strip to face the camera; `Some(n)` keeps it
    /// perpendicular to `n` (a beam that does not face the camera).
    pub normal: Option<Vec3>,
    /// 0 blends over what is behind; 1 adds to it.
    pub emission: f32,
}

/// An outline and fill over the draw items of some entities.
#[derive(Clone, Debug, PartialEq)]
pub struct Highlight {
    /// The entities whose draw items are highlighted.
    pub entities: Vec<EntityId>,
    /// Linear RGB and opacity of the fill.
    pub fill: [f32; 4],
    /// Linear RGB and opacity of the outline.
    pub outline: [f32; 4],
    /// Shown through other geometry; otherwise only where the items are seen.
    pub always_on_top: bool,
}

/// One vertex of the effects pass: `struct EffectVertex` in effects.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
pub(crate) struct EffectVertex {
    pub position: [f32; 3],
    pub uv: [f32; 2],
    pub color: [f32; 4],
    /// x: emission, y: 1 for a soft round fade.
    pub params: [f32; 2],
}

/// One quad of the effects pass, with its distance for sorting.
struct Quad {
    depth: f32,
    order: usize,
    vertices: [EffectVertex; 4],
}

fn finite(v: Vec3) -> bool {
    v.is_finite()
}

fn finite4(c: [f32; 4]) -> bool {
    c.iter().all(|v| v.is_finite())
}

/// Expands sprites and ribbons into quads facing a camera at `eye` with the
/// given right and up axes, sorted back to front. Returns the vertices (four
/// per quad) and the number of quads. Non-finite input is left out.
pub(crate) fn effect_quads(
    sprites: &[Sprite],
    ribbons: &[Ribbon],
    eye: Vec3,
    right: Vec3,
    up: Vec3,
) -> Vec<EffectVertex> {
    let mut quads: Vec<Quad> = Vec::new();
    for s in sprites {
        if !finite(s.position)
            || !s.size.is_finite()
            || s.size <= 0.0
            || !s.rotation.is_finite()
            || !finite4(s.color)
            || s.color[3] <= 0.0
        {
            continue;
        }
        let (sin, cos) = s.rotation.sin_cos();
        let half = s.size * 0.5;
        let r = (right * cos + up * sin) * half;
        let u = (up * cos - right * sin) * half;
        let soft = f32::from(u8::from(s.shape == SpriteShape::Soft));
        let v = |p: Vec3, uv: [f32; 2]| EffectVertex {
            position: p.to_array(),
            uv,
            color: s.color,
            params: [s.emission.clamp(0.0, 1.0), soft],
        };
        let c = s.position;
        quads.push(Quad {
            depth: c.distance_squared(eye),
            order: quads.len(),
            vertices: [
                v(c - r - u, [0.0, 1.0]),
                v(c + r - u, [1.0, 1.0]),
                v(c + r + u, [1.0, 0.0]),
                v(c - r + u, [0.0, 0.0]),
            ],
        });
    }
    for ribbon in ribbons {
        let points: Vec<&RibbonPoint> = ribbon
            .points
            .iter()
            .filter(|p| finite(p.position) && p.width.is_finite() && finite4(p.color))
            .collect();
        if points.len() < 2 {
            continue;
        }
        // Each point's two edge positions.
        let total: f32 = points
            .windows(2)
            .map(|w| w[0].position.distance(w[1].position))
            .sum();
        let mut along = 0.0;
        let mut edges = Vec::with_capacity(points.len());
        for (i, p) in points.iter().enumerate() {
            let prev = points[i.saturating_sub(1)].position;
            let next = points[(i + 1).min(points.len() - 1)].position;
            let tangent = (next - prev).normalize_or_zero();
            let facing = ribbon
                .normal
                .filter(|n| finite(*n) && n.length_squared() > 1e-12)
                .unwrap_or(eye - p.position);
            let mut side = tangent.cross(facing).normalize_or_zero();
            if side == Vec3::ZERO {
                side = right;
            }
            if i > 0 {
                along += points[i - 1].position.distance(p.position);
            }
            let u = if total > 0.0 { along / total } else { 0.0 };
            let half = side * (p.width.max(0.0) * 0.5);
            edges.push((p.position - half, p.position + half, u, p.color));
        }
        let emission = ribbon.emission.clamp(0.0, 1.0);
        for (i, w) in edges.windows(2).enumerate() {
            let (a0, a1, ua, ca) = w[0];
            let (b0, b1, ub, cb) = w[1];
            if ca[3] <= 0.0 && cb[3] <= 0.0 {
                continue;
            }
            let v = |p: Vec3, uv: [f32; 2], color: [f32; 4]| EffectVertex {
                position: p.to_array(),
                uv,
                color,
                params: [emission, 0.0],
            };
            let middle = (points[i].position + points[i + 1].position) * 0.5;
            quads.push(Quad {
                depth: middle.distance_squared(eye),
                order: quads.len(),
                vertices: [
                    v(a0, [ua, 1.0], ca),
                    v(b0, [ub, 1.0], cb),
                    v(b1, [ub, 0.0], cb),
                    v(a1, [ua, 0.0], ca),
                ],
            });
        }
    }
    // Back to front; equal distances keep their input order (deterministic).
    quads.sort_by(|a, b| b.depth.total_cmp(&a.depth).then(a.order.cmp(&b.order)));
    quads.into_iter().flat_map(|q| q.vertices).collect()
}

/// The indices of `quads` quads: two triangles each.
pub(crate) fn quad_indices(quads: usize) -> Vec<u32> {
    (0..quads as u32)
        .flat_map(|q| {
            let b = q * 4;
            [b, b + 1, b + 2, b, b + 2, b + 3]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sprite(x: f32) -> Sprite {
        Sprite {
            position: Vec3::new(x, 0.0, 0.0),
            size: 2.0,
            rotation: 0.0,
            color: [1.0; 4],
            emission: 0.0,
            shape: SpriteShape::Soft,
        }
    }

    #[test]
    fn sprites_face_the_camera_and_sort_back_to_front() {
        let eye = Vec3::new(0.0, 0.0, 10.0);
        let v = effect_quads(&[sprite(0.0), sprite(30.0)], &[], eye, Vec3::X, Vec3::Y);
        assert_eq!(v.len(), 8);
        // The far sprite (x = 30) comes first.
        assert!(v[0].position[0] > 20.0);
        // Quads lie in the camera's right/up plane, two units wide.
        let near = &v[4..];
        assert_eq!(near[0].position, [-1.0, -1.0, 0.0]);
        assert_eq!(near[2].position, [1.0, 1.0, 0.0]);
    }

    #[test]
    fn ribbons_have_one_quad_per_segment_and_skip_bad_points() {
        let p = |x: f32| RibbonPoint {
            position: Vec3::new(x, 0.0, 0.0),
            width: 1.0,
            color: [1.0; 4],
        };
        let mut bad = p(5.0);
        bad.position.y = f32::NAN;
        let ribbon = Ribbon {
            points: vec![p(0.0), bad, p(1.0), p(2.0)],
            normal: None,
            emission: 1.0,
        };
        let v = effect_quads(&[], &[ribbon], Vec3::new(0.0, 0.0, 5.0), Vec3::X, Vec3::Y);
        assert_eq!(v.len(), 8);
        // Facing the camera on +Z, the strip spreads along Y.
        let ys: Vec<f32> = v.iter().map(|v| v.position[1]).collect();
        assert!(ys.iter().all(|y| (y.abs() - 0.5).abs() < 1e-5));
        assert_eq!(quad_indices(2), vec![0, 1, 2, 0, 2, 3, 4, 5, 6, 4, 6, 7]);
    }

    #[test]
    fn invisible_and_broken_sprites_are_skipped() {
        let mut clear = sprite(0.0);
        clear.color[3] = 0.0;
        let mut nan = sprite(0.0);
        nan.size = f32::NAN;
        assert!(effect_quads(&[clear, nan], &[], Vec3::Z, Vec3::X, Vec3::Y).is_empty());
    }
}
