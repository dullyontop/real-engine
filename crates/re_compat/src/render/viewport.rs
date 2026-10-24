//! `ViewportFrame`s (ADR-0048 §2): the parts inside each one, seen through
//! its `CurrentCamera` and lit by its `Ambient` and one light, as a scene of
//! its own for the host to draw into the frame's texture.

use glam::Vec3;
use re_scene::Entity;
use re_types::render::{Camera, Light, LightKind};

use super::{FrameItem, Hub, RenderFrame, camera, lighting, parts};
use crate::gui::props::{Rgba, Scope};
use crate::instance::tree;

/// What a `ViewportFrame` shows: no shadows, particles, beams, sky or post
/// effects.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewportScene {
    /// The `ViewportFrame`.
    pub entity: Entity,
    /// Its `CurrentCamera`; without one nothing is drawn.
    pub camera: Option<Camera>,
    /// Its parts, with the meshes and materials of the frame's own
    /// [`RenderFrame::new_meshes`] and [`RenderFrame::new_materials`].
    pub items: Vec<FrameItem>,
    /// `Ambient`, linear RGB, from above and below alike.
    pub ambient: [f32; 3],
    /// `LightColor` along `LightDirection`.
    pub light: Light,
}

/// Adds the scene of every `ViewportFrame` in `frames`.
pub(super) fn draw(s: &Scope, hub: &mut Hub, frames: &[Entity], out: &mut RenderFrame) {
    for &vf in frames {
        if !tree::alive(s.w, vf) || tree::is_destroyed(s.w, vf) {
            continue;
        }
        let p = s.props(vf);
        let camera = p
            .entity("CurrentCamera")
            .filter(|&c| tree::alive(s.w, c) && s.is_a(c, "Camera"))
            .map(|c| camera::read(s, Some(c)).camera(hub.viewport));
        let mut items = Vec::new();
        if camera.is_some() {
            let mut parts_in: Vec<Entity> = tree::descendants(s.w, vf)
                .into_iter()
                .filter(|&e| s.is_a(e, "BasePart"))
                .collect();
            parts_in.sort_unstable();
            for e in parts_in {
                if let Some(i) = parts::info(s, e) {
                    let item = i.item(hub, out);
                    hub.use_material(out, item.material);
                    items.push(item);
                }
            }
        }
        let color = |name: &'static str, or: [f32; 3]| {
            lighting::linear(p.color(name, Rgba([or[0], or[1], or[2], 1.0])))
        };
        let dir = p.vec3("LightDirection");
        let dir = if dir.is_finite() && dir.length_squared() > 1e-12 {
            dir.normalize()
        } else {
            Vec3::NEG_ONE.normalize()
        };
        out.viewports.push(ViewportScene {
            entity: vf,
            camera,
            items,
            ambient: color("Ambient", [200.0 / 255.0; 3]),
            light: Light {
                kind: LightKind::Directional,
                direction: dir,
                color: color("LightColor", [140.0 / 255.0; 3]),
                intensity: 1.0,
                shadows: false,
                ..Light::default()
            },
        });
    }
}
