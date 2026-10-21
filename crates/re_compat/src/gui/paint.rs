//! Painting laid-out UI as egui shapes.
//!
//! Backgrounds are rectangles (rounded by `UICorner`, outlined by the border
//! or a `UIStroke`), or a mesh when a `UIGradient` colours them. Text is the
//! laid-out galley, with its stroke drawn as offset copies under it. A
//! `ViewportFrame` shows the texture the host drew its parts into. The
//! shapes rotate with `Rotation`; a rotated element draws unclipped.

use egui::epaint::{
    ClippedShape, CornerRadius, Mesh, RectShape, Shape, Stroke, StrokeKind, TextShape, Vertex,
};
use std::collections::HashMap;

use egui::{Pos2, Rect};
use glam::{Affine2, Vec2};

use super::layout::{Item, Kind};
use super::props::{Props, Rgba, Scope};
use super::text;
use re_scene::Entity;

/// What the input pass found that changes the look.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Looks {
    pub hovered: Option<Entity>,
    pub pressed: Option<Entity>,
    pub selected: Option<Entity>,
    pub focused: Option<Entity>,
}

pub(crate) struct Painter<'a> {
    pub s: &'a Scope<'a>,
    pub screen: Rect,
    pub shapes: Vec<ClippedShape>,
    /// The texture each `ViewportFrame` was drawn into.
    pub images: &'a HashMap<Entity, egui::TextureId>,
}

fn pos(p: Vec2) -> Pos2 {
    Pos2::new(p.x, p.y)
}

/// The value of a sequence at `t` (keypoints sorted by time).
fn sample(keys: &[(f32, [f32; 3])], t: f32) -> [f32; 3] {
    let Some(first) = keys.first() else {
        return [1.0; 3];
    };
    if t <= first.0 {
        return first.1;
    }
    for pair in keys.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if t <= b.0 {
            let span = (b.0 - a.0).max(1e-6);
            let k = (t - a.0) / span;
            return std::array::from_fn(|i| a.1[i] + (b.1[i] - a.1[i]) * k);
        }
    }
    keys.last().map_or([1.0; 3], |k| k.1)
}

/// The outline of a rounded rectangle, clockwise, with straight edges split
/// so per-vertex colours follow a gradient.
fn outline(min: Vec2, size: Vec2, r: f32) -> Vec<Vec2> {
    let r = r.clamp(0.0, size.x.min(size.y) * 0.5);
    let max = min + size;
    let mut pts = Vec::new();
    let edge = |a: Vec2, b: Vec2, pts: &mut Vec<Vec2>| {
        const SPLIT: usize = 16;
        for i in 0..SPLIT {
            pts.push(a.lerp(b, i as f32 / SPLIT as f32));
        }
    };
    let arc = |c: Vec2, from: f32, pts: &mut Vec<Vec2>| {
        if r <= 0.0 {
            pts.push(c);
            return;
        }
        const SEG: usize = 8;
        for i in 0..=SEG {
            let a = (from + 90.0 * i as f32 / SEG as f32).to_radians();
            pts.push(c + Vec2::new(a.cos(), a.sin()) * r);
        }
    };
    // Screen y points down, so angles go clockwise from +x.
    arc(Vec2::new(min.x + r, min.y + r), 180.0, &mut pts);
    edge(
        Vec2::new(min.x + r, min.y),
        Vec2::new(max.x - r, min.y),
        &mut pts,
    );
    arc(Vec2::new(max.x - r, min.y + r), 270.0, &mut pts);
    edge(
        Vec2::new(max.x, min.y + r),
        Vec2::new(max.x, max.y - r),
        &mut pts,
    );
    arc(Vec2::new(max.x - r, max.y - r), 0.0, &mut pts);
    edge(
        Vec2::new(max.x - r, max.y),
        Vec2::new(min.x + r, max.y),
        &mut pts,
    );
    arc(Vec2::new(min.x + r, max.y - r), 90.0, &mut pts);
    edge(
        Vec2::new(min.x, max.y - r),
        Vec2::new(min.x, min.y + r),
        &mut pts,
    );
    pts.dedup_by(|a, b| a.distance_squared(*b) < 1e-6);
    pts
}

impl<'a> Painter<'a> {
    fn push(&mut self, clip: Option<Rect>, shape: Shape) {
        self.shapes.push(ClippedShape {
            clip_rect: clip.unwrap_or(self.screen),
            shape,
        });
    }

    fn props(&self, e: Entity) -> Props<'a> {
        self.s.props(e)
    }

    /// Paints one element.
    pub fn item(&mut self, it: &Item, looks: &Looks) {
        let sc = self.s;
        let decor = sc.decor(it.e);
        let p = self.props(it.e);
        let corner = decor
            .corner
            .map(|c| self.props(c).udim("CornerRadius"))
            .map_or(0.0, |u| {
                (u.scale * it.size.x.min(it.size.y) + u.offset * it.scale).max(0.0)
            });
        let stroke_of = |mode: &str| {
            decor.strokes.iter().find_map(|&c| {
                let s = sc.props(c);
                if !s.bool("Enabled", true) {
                    return None;
                }
                let applies = s.item("ApplyStrokeMode", "Contextual");
                let text_item = matches!(it.kind, Kind::Text | Kind::TextBox);
                let wants = if applies == "Border" || !text_item {
                    "Border"
                } else {
                    "Text"
                };
                (wants == mode).then(|| {
                    (
                        s.f32("Thickness", 1.0) * it.scale,
                        s.color("Color", Rgba([0.0, 0.0, 0.0, 1.0]))
                            .with_transparency(s.f32("Transparency", 0.0)),
                    )
                })
            })
        };
        let mut bg = p
            .color("BackgroundColor3", Rgba::WHITE)
            .with_transparency(p.f32("BackgroundTransparency", 0.0));
        let is_button = sc.is_a(it.e, "GuiButton");
        if is_button && p.bool("AutoButtonColor", true) {
            let k = if looks.pressed == Some(it.e) {
                0.7
            } else if looks.hovered == Some(it.e) {
                0.85
            } else {
                1.0
            };
            bg = Rgba([bg.0[0] * k, bg.0[1] * k, bg.0[2] * k, bg.0[3]]);
        }
        let angle = it.angle.to_radians();
        let center = it.rot.transform_point2(it.min + it.size * 0.5);
        let rect = Rect::from_center_size(pos(center), egui::vec2(it.size.x, it.size.y));
        let gradient = decor
            .gradient
            .filter(|&g| self.props(g).bool("Enabled", true));
        if bg.0[3] > 0.0 {
            match gradient {
                Some(g) => self.gradient(it, g, bg, corner),
                None => self.push(
                    it.clip,
                    Shape::Rect(
                        RectShape::filled(rect, radius(corner), bg.egui()).with_angle(angle),
                    ),
                ),
            }
        }
        // The border (UICorner hides it, as in the original) or a stroke.
        let border = p.f32("BorderSizePixel", 1.0) * it.scale;
        let line = match stroke_of("Border") {
            Some((t, c)) => Some((t, c)),
            None if corner == 0.0 && border > 0.0 && bg.0[3] > 0.0 => Some((
                border,
                p.color("BorderColor3", Rgba([0.1, 0.1, 0.1, 1.0]))
                    .with_transparency(p.f32("BackgroundTransparency", 0.0)),
            )),
            None => None,
        };
        if let Some((t, c)) = line
            && t > 0.0
            && c.0[3] > 0.0
        {
            self.push(
                it.clip,
                Shape::Rect(
                    RectShape::stroke(
                        rect,
                        radius(corner),
                        Stroke::new(t, c.egui()),
                        StrokeKind::Outside,
                    )
                    .with_angle(angle),
                ),
            );
        }
        if it.kind == Kind::Image && !p.string("Image").is_empty() {
            let c = p
                .color("ImageColor3", Rgba::WHITE)
                .with_transparency(p.f32("ImageTransparency", 0.0));
            if c.0[3] > 0.0 {
                self.push(
                    it.clip,
                    Shape::Rect(
                        RectShape::filled(
                            rect,
                            radius(corner),
                            c.mul(Rgba([0.6, 0.6, 0.6, 1.0])).egui(),
                        )
                        .with_angle(angle),
                    ),
                );
            }
        }
        if it.kind == Kind::Viewport
            && let Some(&texture) = self.images.get(&it.e)
        {
            let c = p
                .color("ImageColor3", Rgba::WHITE)
                .with_transparency(p.f32("ImageTransparency", 0.0));
            if c.0[3] > 0.0 {
                // The picture turns with the frame.
                let mut mesh = Mesh::with_texture(texture);
                let corners = [Vec2::ZERO, Vec2::X, Vec2::ONE, Vec2::Y];
                for uv in corners {
                    let at = it.rot.transform_point2(it.min + it.size * uv);
                    mesh.vertices.push(Vertex {
                        pos: pos(at),
                        uv: Pos2::new(uv.x, uv.y),
                        color: c.egui(),
                    });
                }
                mesh.indices.extend([0, 1, 2, 0, 2, 3]);
                self.push(it.clip, Shape::Mesh(mesh.into()));
            }
        }
        if let Some((style, laid, inner)) = &it.text {
            let gsize = text::bounds(laid);
            let y_align = match p.item("TextYAlignment", "Center").as_str() {
                "Top" => 0.0,
                "Bottom" => 1.0,
                _ => 0.5,
            };
            let at = it.min
                + Vec2::new(it.pad[0], it.pad[1])
                + (*inner - gsize) * Vec2::new(style.x_align, y_align)
                - Vec2::new(laid.galley.rect.min.x, laid.galley.rect.min.y);
            let origin = it.rot.transform_point2(at);
            let stroke = stroke_of("Text").or_else(|| {
                let t = p.f32("TextStrokeTransparency", 1.0);
                (t < 1.0).then(|| {
                    (
                        it.scale,
                        p.color("TextStrokeColor3", Rgba([0.0, 0.0, 0.0, 1.0]))
                            .with_transparency(t),
                    )
                })
            });
            if let Some((t, c)) = stroke
                && t > 0.0
                && c.0[3] > 0.0
            {
                for (dx, dy) in [
                    (-1.0, -1.0),
                    (0.0, -1.0),
                    (1.0, -1.0),
                    (-1.0, 0.0),
                    (1.0, 0.0),
                    (-1.0, 1.0),
                    (0.0, 1.0),
                    (1.0, 1.0),
                ] {
                    let off = Affine2::from_angle(angle).transform_vector2(Vec2::new(dx, dy) * t);
                    let mut s = TextShape::new(pos(origin + off), laid.galley.clone(), c.egui())
                        .with_angle(angle);
                    s.override_text_color = Some(c.egui());
                    self.push(it.clip, Shape::Text(s));
                }
            }
            self.push(
                it.clip,
                Shape::Text(
                    TextShape::new(pos(origin), laid.galley.clone(), style.color.egui())
                        .with_angle(angle),
                ),
            );
            if looks.focused == Some(it.e) {
                // The caret, after the text.
                let h = laid.galley.rect.height().max(style.size);
                let x = if p.string("Text").is_empty() {
                    at.x + laid.galley.rect.min.x
                } else {
                    at.x + laid.galley.rect.max.x
                };
                let top = it
                    .rot
                    .transform_point2(Vec2::new(x + 1.0, at.y + laid.galley.rect.min.y));
                let bottom = it
                    .rot
                    .transform_point2(Vec2::new(x + 1.0, at.y + laid.galley.rect.min.y + h));
                self.push(
                    it.clip,
                    Shape::line_segment(
                        [pos(top), pos(bottom)],
                        Stroke::new(1.0, style.color.egui()),
                    ),
                );
            }
        }
        if it.kind == Kind::Scrolling {
            self.scroll_bar(it, &p, angle);
        }
        if looks.selected == Some(it.e) {
            self.push(
                None,
                Shape::Rect(
                    RectShape::stroke(
                        rect,
                        radius(corner),
                        Stroke::new(2.0, egui::Color32::WHITE),
                        StrokeKind::Outside,
                    )
                    .with_angle(angle),
                ),
            );
        }
    }

    fn scroll_bar(&mut self, it: &Item, p: &Props, angle: f32) {
        let canvas = it.canvas.unwrap_or(it.size);
        let window = it.size;
        let at = p.vec2("CanvasPosition", Vec2::ZERO) * it.scale;
        let t = p.f32("ScrollBarThickness", 12.0) * it.scale;
        let c = p
            .color("ScrollBarImageColor3", Rgba([0.0, 0.0, 0.0, 1.0]))
            .with_transparency(p.f32("ScrollBarImageTransparency", 0.0));
        if t <= 0.0 || c.0[3] <= 0.0 {
            return;
        }
        if canvas.y > window.y + 0.5 {
            let len = window.y * window.y / canvas.y;
            let top = at.y / (canvas.y - window.y) * (window.y - len);
            let min = it.min + Vec2::new(window.x - t, top);
            self.bar(it, min, Vec2::new(t, len), c, angle);
        }
        if canvas.x > window.x + 0.5 {
            let len = window.x * window.x / canvas.x;
            let left = at.x / (canvas.x - window.x) * (window.x - len);
            let min = it.min + Vec2::new(left, window.y - t);
            self.bar(it, min, Vec2::new(len, t), c, angle);
        }
    }

    fn bar(&mut self, it: &Item, min: Vec2, size: Vec2, c: Rgba, angle: f32) {
        let center = it.rot.transform_point2(min + size * 0.5);
        let rect = Rect::from_center_size(pos(center), egui::vec2(size.x, size.y));
        self.push(
            it.clip,
            Shape::Rect(RectShape::filled(rect, CornerRadius::ZERO, c.egui()).with_angle(angle)),
        );
    }

    /// A background coloured by a `UIGradient`: a fan of triangles from the
    /// centre whose vertices sample the colour and transparency sequences.
    fn gradient(&mut self, it: &Item, g: Entity, bg: Rgba, corner: f32) {
        let gp = self.props(g);
        let colors = gp.sequence("Color");
        let alphas = gp.sequence("Transparency");
        let offset = gp.vec2("Offset", Vec2::ZERO);
        let r = gp.f32("Rotation", 0.0).to_radians();
        let dir = Vec2::new(r.cos(), r.sin());
        let center = it.min + it.size * 0.5;
        let color_at = |q: Vec2| {
            // The gradient runs along its rotated x axis across the element's
            // unit square, shifted by Offset.
            let u = (q - center) / it.size.max(Vec2::splat(1e-6));
            let t = u.dot(dir) + 0.5 - offset.dot(dir);
            let c = sample(&colors, t);
            let a = 1.0 - sample(&alphas, t)[0];
            Rgba([
                bg.0[0] * c[0],
                bg.0[1] * c[1],
                bg.0[2] * c[2],
                bg.0[3] * a.clamp(0.0, 1.0),
            ])
            .egui()
        };
        let pts = outline(it.min, it.size, corner);
        let mut mesh = Mesh::default();
        let vertex = |q: Vec2| Vertex {
            pos: pos(it.rot.transform_point2(q)),
            uv: egui::epaint::WHITE_UV,
            color: color_at(q),
        };
        mesh.vertices.push(vertex(center));
        for &q in &pts {
            mesh.vertices.push(vertex(q));
        }
        let n = pts.len() as u32;
        for i in 0..n {
            mesh.indices.extend_from_slice(&[0, 1 + i, 1 + (i + 1) % n]);
        }
        self.push(it.clip, Shape::mesh(mesh));
    }
}

fn radius(r: f32) -> CornerRadius {
    CornerRadius::same(r.round().clamp(0.0, 255.0) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_sample_between_keys() {
        let keys = [
            (0.0, [1.0, 0.0, 0.0]),
            (0.5, [0.0, 1.0, 0.0]),
            (1.0, [0.0, 0.0, 1.0]),
        ];
        assert_eq!(sample(&keys, -1.0), [1.0, 0.0, 0.0]);
        assert_eq!(sample(&keys, 0.25), [0.5, 0.5, 0.0]);
        assert_eq!(sample(&keys, 2.0), [0.0, 0.0, 1.0]);
        let o = outline(Vec2::ZERO, Vec2::new(100.0, 50.0), 10.0);
        assert!(
            o.iter()
                .all(|p| p.x >= -1e-3 && p.x <= 100.001 && p.y >= -1e-3 && p.y <= 50.001)
        );
    }
}
