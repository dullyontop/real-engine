//! Painting laid-out items as egui shapes: the draw list re_render's UI pass
//! paints.
//!
//! Backgrounds are rectangles (rounded by [`Look::corner`], outlined by the
//! border or a stroke), or a mesh when a gradient colours them. Text is the
//! laid-out galley, with its stroke drawn as offset copies under it. An
//! image is a placeholder or a host's texture, and a bar a fill over part of
//! the box. The shapes rotate with the node; a rotated node draws unclipped.
//! An item's opacity fades every shape it paints.

use egui::epaint::{
    ClippedShape, CornerRadius, Mesh, RectShape, Shape, Stroke, StrokeKind, TextShape, Vertex,
};
use egui::{Pos2, Rect};
use glam::{Affine2, Vec2};

use crate::layout::Item;
use crate::tree::{Gradient, Image, Look, NodeId, Rgba, StrokeMode, UiTree};

/// Multiplies a shape's alpha by `k` (0 to 1).
fn fade(shape: &mut Shape, k: f32) {
    match shape {
        Shape::Text(t) => t.opacity_factor *= k,
        Shape::Vec(shapes) => shapes.iter_mut().for_each(|s| fade(s, k)),
        other => egui::epaint::shape_transform::adjust_colors(other, move |c| {
            *c = c.gamma_multiply(k);
        }),
    }
}

/// What input found that changes the look.
#[derive(Clone, Copy, Debug, Default)]
pub struct Looks {
    pub hovered: Option<NodeId>,
    pub pressed: Option<NodeId>,
    /// Outlined: the gamepad's selection.
    pub selected: Option<NodeId>,
    /// Has a caret: the text field with the keyboard.
    pub focused: Option<NodeId>,
}

/// Paints items into a list of shapes.
pub struct Painter<'a> {
    tree: &'a UiTree,
    /// The clip of unclipped shapes.
    screen: Rect,
    shapes: Vec<ClippedShape>,
}

fn pos(p: Vec2) -> Pos2 {
    Pos2::new(p.x, p.y)
}

/// The value of a sequence at `t` (keys sorted by time).
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
    // A negative size is normal (an offset larger than the parent, or a tween
    // that overshoots zero), and `clamp` panics when its bounds cross.
    let r = r.clamp(0.0, (size.x.min(size.y) * 0.5).max(0.0));
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
            pts.push(c + Vec2::new(libm::cosf(a), libm::sinf(a)) * r);
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

/// The first stroke of `look` that goes where `mode` says, for a node with
/// text or without, as `(thickness, colour)` before the node's scale.
fn stroke_of(look: &Look, text_item: bool, mode: StrokeMode) -> Option<(f32, Rgba)> {
    look.strokes.iter().find_map(|s| {
        let wants = if s.mode == StrokeMode::Border || !text_item {
            StrokeMode::Border
        } else {
            StrokeMode::Contextual
        };
        (wants == mode).then_some((s.thickness, s.color))
    })
}

impl<'a> Painter<'a> {
    /// A painter of `tree`'s items for a target of `screen` (the clip of
    /// shapes that have none).
    pub fn new(tree: &'a UiTree, screen: Rect) -> Painter<'a> {
        Painter {
            tree,
            screen,
            shapes: Vec::new(),
        }
    }

    /// The shapes painted so far, back to front.
    pub fn finish(self) -> Vec<ClippedShape> {
        self.shapes
    }

    fn push(&mut self, clip: Option<Rect>, shape: Shape) {
        self.shapes.push(ClippedShape {
            clip_rect: clip.unwrap_or(self.screen),
            shape,
        });
    }

    /// Paints one item; one at no opacity paints nothing.
    pub fn item(&mut self, it: &Item, looks: &Looks) {
        if it.opacity <= 0.0 {
            return;
        }
        let start = self.shapes.len();
        self.shapes_of(it, looks);
        if it.opacity < 1.0 {
            for s in &mut self.shapes[start..] {
                fade(&mut s.shape, it.opacity);
            }
        }
    }

    fn shapes_of(&mut self, it: &Item, looks: &Looks) {
        let tree = self.tree;
        let n = tree.node(it.node);
        let look = &n.look;
        let corner = look.corner.map_or(0.0, |u| {
            (u.scale * it.size.x.min(it.size.y) + u.offset * it.scale).max(0.0)
        });
        let text_item = n.text.is_some();
        let stroke = |mode| stroke_of(look, text_item, mode).map(|(t, c)| (t * it.scale, c));
        let mut bg = look.background;
        if look.auto_color {
            let k = if looks.pressed == Some(it.node) {
                0.7
            } else if looks.hovered == Some(it.node) {
                0.85
            } else {
                1.0
            };
            bg = Rgba([bg.0[0] * k, bg.0[1] * k, bg.0[2] * k, bg.0[3]]);
        }
        let angle = it.angle.to_radians();
        let center = it.rot.transform_point2(it.min + it.size * 0.5);
        let rect = Rect::from_center_size(pos(center), egui::vec2(it.size.x, it.size.y));
        if bg.0[3] > 0.0 {
            match &look.gradient {
                Some(g) => self.gradient(it, g, bg, corner),
                None => self.push(
                    it.clip,
                    Shape::Rect(
                        RectShape::filled(rect, radius(corner), bg.egui()).with_angle(angle),
                    ),
                ),
            }
        }
        if let Some(bar) = n.bar
            && bar.color.0[3] > 0.0
        {
            let (min, size) = bar.fill(it.min, it.size);
            if size.x > 0.0 && size.y > 0.0 {
                let center = it.rot.transform_point2(min + size * 0.5);
                let fill = Rect::from_center_size(pos(center), egui::vec2(size.x, size.y));
                let r = corner.min(size.x * 0.5).min(size.y * 0.5);
                self.push(
                    it.clip,
                    Shape::Rect(
                        RectShape::filled(fill, radius(r), bar.color.egui()).with_angle(angle),
                    ),
                );
            }
        }
        // The border (a rounded corner hides it) or a stroke.
        let border = look.border * it.scale;
        let line = match stroke(StrokeMode::Border) {
            Some((t, c)) => Some((t, c)),
            None if corner == 0.0 && border > 0.0 && bg.0[3] > 0.0 => {
                Some((border, look.border_color))
            }
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
        match n.image {
            Some(Image::Missing { tint: c }) if c.0[3] > 0.0 => {
                self.push(
                    it.clip,
                    Shape::Rect(
                        RectShape::filled(
                            rect,
                            radius(corner),
                            (c * Rgba([0.6, 0.6, 0.6, 1.0])).egui(),
                        )
                        .with_angle(angle),
                    ),
                );
            }
            Some(Image::Texture { texture, tint: c }) if c.0[3] > 0.0 => {
                // The picture turns with the node.
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
            _ => {}
        }
        if let (Some(laid), Some(style)) = (&it.text, &n.text) {
            let gsize = Vec2::new(laid.galley.rect.width(), laid.galley.rect.height());
            let at = it.min + Vec2::new(it.pad[0], it.pad[1]) + (laid.inner - gsize) * style.align
                - Vec2::new(laid.galley.rect.min.x, laid.galley.rect.min.y);
            let origin = it.rot.transform_point2(at);
            let outline =
                stroke(StrokeMode::Contextual).or_else(|| style.stroke.map(|c| (it.scale, c)));
            if let Some((t, c)) = outline
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
            if looks.focused == Some(it.node) {
                // The caret, after the text (before a placeholder).
                let h = laid.galley.rect.height().max(laid.size);
                let x = if style.placeholder {
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
        if let Some(scroll) = &n.scroll {
            self.scroll_bar(it, scroll.position, scroll.bar, scroll.bar_color, angle);
        }
        if looks.selected == Some(it.node) {
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

    fn scroll_bar(&mut self, it: &Item, position: Vec2, thickness: f32, c: Rgba, angle: f32) {
        let canvas = it.canvas.unwrap_or(it.size);
        let window = it.size;
        let at = position * it.scale;
        let t = thickness * it.scale;
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

    /// A background coloured by a gradient: a fan of triangles from the
    /// centre whose vertices sample the colour and transparency keys.
    fn gradient(&mut self, it: &Item, g: &Gradient, bg: Rgba, corner: f32) {
        if !(it.size.x > 0.0 && it.size.y > 0.0 && it.size.is_finite()) {
            return;
        }
        let r = g.rotation.to_radians();
        let dir = Vec2::new(libm::cosf(r), libm::sinf(r));
        let center = it.min + it.size * 0.5;
        let color_at = |q: Vec2| {
            // The gradient runs along its rotated x axis across the node's
            // unit square, shifted by its offset.
            let u = (q - center) / it.size.max(Vec2::splat(1e-6));
            let t = u.dot(dir) + 0.5 - g.offset.dot(dir);
            let c = sample(&g.colors, t);
            let a = 1.0 - sample(&g.transparency, t)[0];
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

    /// A size that works out negative (an offset larger than the parent, or a
    /// tween that overshoots zero) or NaN is not an error: a rounded gradient
    /// background on it paints nothing.
    #[test]
    fn gradients_on_empty_and_nan_sizes_paint_nothing() {
        use crate::layout::{Layout, Root};
        use crate::tree::{Dim, Dim2, Node};

        let look = Look {
            background: Rgba::WHITE,
            corner: Some(Dim::px(8.0)),
            gradient: Some(Gradient {
                colors: vec![(0.0, [1.0; 3]), (1.0, [0.0; 3])],
                transparency: Vec::new(),
                offset: Vec2::ZERO,
                rotation: 0.0,
            }),
            ..Look::default()
        };
        let mut tree = UiTree::new();
        let sizes = [
            Dim2::px(20.0, 20.0),
            Dim2::new(Dim::new(1.0, -40.0), Dim::px(20.0)),
            Dim2::px(f32::NAN, 20.0),
        ];
        let children = sizes
            .map(|size| {
                tree.add(Node {
                    size,
                    look: look.clone(),
                    ..Node::default()
                })
            })
            .to_vec();
        let parent = tree.add(Node {
            size: Dim2::px(30.0, 30.0),
            children,
            ..Node::default()
        });
        let root = Root {
            node: tree.add(Node {
                children: vec![parent],
                ..Node::default()
            }),
            origin: Vec2::ZERO,
            abs_origin: Vec2::ZERO,
            size: Vec2::new(400.0, 300.0),
            global_z: false,
            clip: None,
            input: true,
        };
        let ctx = egui::Context::default();
        let mut items = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            items = Layout::new(&tree, ui.ctx()).root(&root);
        });
        output.textures_delta.clear();
        assert_eq!(items.len(), 4, "the parent and its three frames are drawn");
        let mut painter = Painter::new(&tree, Rect::EVERYTHING);
        for it in &items {
            painter.item(it, &Looks::default());
        }
        let meshes = painter
            .finish()
            .iter()
            .filter(|s| matches!(s.shape, Shape::Mesh(_)))
            .count();
        assert_eq!(meshes, 1, "only the 20 by 20 frame has a gradient to paint");
    }
}
