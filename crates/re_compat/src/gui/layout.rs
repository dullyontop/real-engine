//! The UI layout: from the instance tree to screen rectangles.
//!
//! Each root (a `ScreenGui`, a `BillboardGui` placed by the camera, or a
//! `SurfaceGui` on its canvas) lays out its descendants top-down. An element's size is its `Size` against the
//! parent's content box (the parent's size less its `UIPadding`), limited by
//! its `UISizeConstraint` and `UIAspectRatioConstraint`, then grown by
//! `AutomaticSize` to fit its children and text. Its position is its
//! `Position` less `AnchorPoint` of its size, unless a `UIListLayout` or
//! `UIGridLayout` sibling places it. `UIScale` scales an element and its
//! descendants about the element's anchor point; `Rotation` turns the drawing
//! (and descendants) about the element's centre but not the layout, as in
//! the original. A `ScrollingFrame` places its children on its canvas.
//!
//! The result is a list of [`Item`]s in drawing order, and the values of
//! `AbsolutePosition`, `AbsoluteSize` and the other read-only properties,
//! written only when they change.

use glam::{Affine2, Quat, Vec2, Vec3};
use re_reflect::Value;
use re_scene::{Entity, GlobalTransform, World};

use super::props::{Props, Rgba, Scope, UDim2, vec2_value};
use super::text::{self, Laid, TextStyle};
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
        viewport.y * 0.5 / (self.fov_y.clamp(0.01, 3.1) * 0.5).tan()
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

/// What a drawn element is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Frame,
    Text,
    Image,
    TextBox,
    Scrolling,
    /// A `ViewportFrame`: its background, then the picture of its parts.
    Viewport,
}

/// One drawn element.
pub(crate) struct Item {
    pub e: Entity,
    pub kind: Kind,
    /// The element's rectangle in screen pixels, before rotation.
    pub min: Vec2,
    pub size: Vec2,
    /// Maps unrotated screen points to drawn ones.
    pub rot: Affine2,
    /// The total rotation in degrees.
    pub angle: f32,
    /// The clip rectangle (none while rotated).
    pub clip: Option<egui::Rect>,
    /// The `UIScale` factor that applies.
    pub scale: f32,
    /// The padding, for text placement: left, top, right, bottom (screen
    /// pixels).
    pub pad: [f32; 4],
    pub text: Option<(TextStyle, Laid, Vec2)>,
    /// A scrolling frame's canvas size in screen pixels.
    pub canvas: Option<Vec2>,
    /// Drawing order.
    pub key: (i64, u64),
    /// Whether it takes input.
    pub interactable: bool,
}

#[derive(Clone, Copy, Debug)]
struct Sc {
    k: f32,
    o: Vec2,
}

impl Sc {
    fn map(self, p: Vec2) -> Vec2 {
        self.o + p * self.k
    }

    /// This mapping after scaling local space by `s` about local point `at`.
    fn scaled_about(self, s: f32, at: Vec2) -> Sc {
        // p -> at + (p - at) * s, then self.
        Sc {
            k: self.k * s,
            o: self.map(at - at * s),
        }
    }
}

/// A placed root: what the frame lays out.
pub(crate) struct Root {
    pub e: Entity,
    /// The screen point of the root's top-left corner.
    pub origin: Vec2,
    /// The screen point that `AbsolutePosition` 0 is: the corner of GUI
    /// space, below the top bar, for screen UI and billboards alike (so a
    /// billboard's elements report where they are on the screen).
    pub abs_origin: Vec2,
    pub size: Vec2,
    pub global_z: bool,
    pub clip: Option<egui::Rect>,
    /// False when the root does not take input (a billboard that is not
    /// `Active`).
    pub input: bool,
}

pub(crate) struct Layout<'a> {
    pub s: &'a Scope<'a>,
    pub w: &'a World,
    pub ctx: &'a egui::Context,
    pub items: Vec<Item>,
    /// `(entity, property, value)` to write.
    pub writes: Vec<(Entity, &'static str, Value)>,
    seq: u64,
}

fn props<'a>(l: &Layout<'a>, e: Entity) -> Props<'a> {
    l.s.props(e)
}

fn is_a(l: &Layout, e: Entity, class: &'static str) -> bool {
    l.s.is_a(e, class)
}

fn padding(l: &Layout, e: Entity, size: Vec2) -> [f32; 4] {
    match l.s.decor(e).padding {
        Some(p) => {
            let p = props(l, p);
            [
                p.udim("PaddingLeft").of(size.x),
                p.udim("PaddingTop").of(size.y),
                p.udim("PaddingRight").of(size.x),
                p.udim("PaddingBottom").of(size.y),
            ]
        }
        None => [0.0; 4],
    }
}

/// `size` fitted to the `UIAspectRatioConstraint` `c`.
fn aspect(l: &Layout, c: Entity, mut size: Vec2) -> Vec2 {
    let p = props(l, c);
    let ar = p.f32("AspectRatio", 1.0);
    if ar.is_finite() && ar > 0.0 {
        match p.item("AspectType", "FitWithinMaxSize").as_str() {
            "ScaleWithParentSize" => {
                if p.item("DominantAxis", "Width") == "Height" {
                    size.x = size.y * ar;
                } else {
                    size.y = size.x / ar;
                }
            }
            _ => {
                let w = size.x.min(size.y * ar);
                size = Vec2::new(w, w / ar);
            }
        }
    }
    size
}

fn axes(item: &str) -> (bool, bool) {
    match item {
        "X" => (true, false),
        "Y" => (false, true),
        "XY" => (true, true),
        _ => (false, false),
    }
}

fn rotate_about(center: Vec2, degrees: f32) -> Affine2 {
    Affine2::from_translation(center)
        * Affine2::from_angle(degrees.to_radians())
        * Affine2::from_translation(-center)
}

/// How a list or grid layout places its siblings.
struct Arrange {
    /// Placed offsets from the content origin, by child index.
    at: Vec<Option<Vec2>>,
    content: Vec2,
    /// A grid's cell size and its columns and rows.
    cells: Option<(Vec2, Vec2)>,
}

/// The children of `parent` that layouts and drawing see, with their
/// sizes. `None` sizes are for non-GuiObject children.
struct Kid {
    e: Entity,
    size: Vec2,
    visible: bool,
}

impl<'a> Layout<'a> {
    pub fn new(s: &'a Scope<'a>, ctx: &'a egui::Context) -> Self {
        Layout {
            s,
            w: s.w,
            ctx,
            items: Vec::new(),
            writes: Vec::new(),
            seq: 0,
        }
    }

    /// The factor of a `UIScale` under `e`, 1 without one.
    fn ui_scale(&self, e: Entity) -> f32 {
        self.s
            .decor(e)
            .scale
            .map(|u| props(self, u).f32("Scale", 1.0))
            .filter(|s| s.is_finite() && *s >= 0.0)
            .unwrap_or(1.0)
    }

    fn write(&mut self, e: Entity, name: &'static str, v: Value) {
        self.writes.push((e, name, v));
    }

    /// Lays out one root.
    pub fn root(&mut self, r: &Root) {
        let size = r.size;
        self.write(r.e, "AbsoluteSize", vec2_value(size));
        self.write(r.e, "AbsolutePosition", vec2_value(r.origin - r.abs_origin));
        let content = (Vec2::ZERO, size);
        self.children(
            r.e,
            content,
            // A UIScale under the root scales everything about its corner.
            Sc {
                k: self.ui_scale(r.e),
                o: r.origin,
            },
            Affine2::IDENTITY,
            0.0,
            r.clip,
            true,
            false,
            r,
        );
    }

    /// The size of a child before `AutomaticSize`, after its constraints.
    fn constrained(&self, e: Entity, size: Vec2) -> Vec2 {
        let size = match self.s.decor(e).aspect {
            Some(c) => aspect(self, c, size),
            None => size,
        };
        self.size_limits(e, size)
    }

    fn size_limits(&self, e: Entity, size: Vec2) -> Vec2 {
        match self.s.decor(e).size {
            Some(c) => {
                let p = props(self, c);
                let min = p.vec2("MinSize", Vec2::ZERO);
                let max = p.vec2("MaxSize", Vec2::splat(f32::INFINITY));
                size.max(min).min(max.max(min))
            }
            None => size,
        }
    }

    /// A grid's cell size in `area`, fitted to a constraint parented to the
    /// layout.
    fn cell_size(&self, grid: Entity, area: Vec2) -> Vec2 {
        let cell = props(self, grid).udim2("CellSize").of(area);
        match self.s.decor(grid).aspect {
            Some(c) => aspect(self, c, cell),
            None => cell,
        }
    }

    fn text_style(&self, e: Entity, scale: f32) -> TextStyle {
        let p = props(self, e);
        let (family, weight, italic) = p.font_face();
        let placeholder = is_a(self, e, "TextBox") && p.string("Text").is_empty();
        let (txt, color) = if placeholder {
            (
                p.string("PlaceholderText"),
                p.color("PlaceholderColor3", Rgba([0.7, 0.7, 0.7, 1.0])),
            )
        } else {
            (p.string("Text"), p.color("TextColor3", Rgba::WHITE))
        };
        TextStyle {
            text: txt,
            family,
            weight,
            italic,
            size: p.f32("TextSize", 14.0) * scale,
            color: color.with_transparency(p.f32("TextTransparency", 0.0)),
            scaled: p.bool("TextScaled", false),
            wrapped: p.bool("TextWrapped", false) || p.bool("TextScaled", false),
            truncate: p.item("TextTruncate", "None") != "None",
            rich: p.bool("RichText", false),
            line_height: p.f32("LineHeight", 1.0),
            x_align: match p.item("TextXAlignment", "Center").as_str() {
                "Left" => 0.0,
                "Right" => 1.0,
                _ => 0.5,
            },
        }
    }

    /// Places a list or grid layout's siblings.
    fn arrange(&self, layout: Entity, kids: &mut [Kid], content: Vec2) -> Arrange {
        let p = props(self, layout);
        let horizontal = p.item("FillDirection", "Horizontal") == "Horizontal";
        let ha = match p.item("HorizontalAlignment", "Left").as_str() {
            "Center" => 0.5,
            "Right" => 1.0,
            _ => 0.0,
        };
        let va = match p.item("VerticalAlignment", "Top").as_str() {
            "Center" => 0.5,
            "Bottom" => 1.0,
            _ => 0.0,
        };
        let by_name = p.item("SortOrder", "LayoutOrder") == "Name";
        let mut order: Vec<usize> = (0..kids.len()).filter(|&i| kids[i].visible).collect();
        if by_name {
            order.sort_by_cached_key(|&i| tree::name(self.w, kids[i].e));
        } else {
            order.sort_by_cached_key(|&i| props(self, kids[i].e).int("LayoutOrder", 0));
        }
        let mut at = vec![None; kids.len()];
        if is_a(self, layout, "UIGridLayout") {
            let cell = self.cell_size(layout, content);
            let pad = p.udim2("CellPadding").of(content);
            let main_len = if horizontal { content.x } else { content.y };
            let (cm, pm) = if horizontal {
                (cell.x, pad.x)
            } else {
                (cell.y, pad.y)
            };
            let fit = if cm + pm > 0.0 {
                ((main_len + pm) / (cm + pm)).floor().max(1.0) as usize
            } else {
                1
            };
            let max = p.int("FillDirectionMaxCells", 0);
            let per = if max > 0 { fit.min(max as usize) } else { fit };
            let n = order.len();
            let cols = per.min(n.max(1));
            let lines = n.div_ceil(per.max(1));
            let (nx, ny) = if horizontal {
                (cols, lines)
            } else {
                (lines, cols)
            };
            let block = Vec2::new(
                nx as f32 * cell.x + nx.saturating_sub(1) as f32 * pad.x,
                ny as f32 * cell.y + ny.saturating_sub(1) as f32 * pad.y,
            );
            let corner = p.item("StartCorner", "TopLeft");
            let start = (content - block) * Vec2::new(ha, va);
            for (slot, &i) in order.iter().enumerate() {
                let (mut cx, mut cy) = if horizontal {
                    (slot % per, slot / per)
                } else {
                    (slot / per, slot % per)
                };
                if corner.ends_with("Right") {
                    cx = nx.saturating_sub(1) - cx;
                }
                if corner.starts_with("Bottom") {
                    cy = ny.saturating_sub(1) - cy;
                }
                kids[i].size = cell;
                at[i] = Some(start + Vec2::new(cx as f32, cy as f32) * (cell + pad));
            }
            return Arrange {
                at,
                content: block,
                cells: Some((cell, Vec2::new(nx as f32, ny as f32))),
            };
        }
        let gap = p
            .udim("Padding")
            .of(if horizontal { content.x } else { content.y });
        let main = |s: Vec2| if horizontal { s.x } else { s.y };
        let cross = |s: Vec2| if horizontal { s.y } else { s.x };
        let total: f32 = order.iter().map(|&i| main(kids[i].size)).sum::<f32>()
            + gap * order.len().saturating_sub(1) as f32;
        let widest = order
            .iter()
            .map(|&i| cross(kids[i].size))
            .fold(0.0f32, f32::max);
        let (ma, ca) = if horizontal { (ha, va) } else { (va, ha) };
        let mut cursor = (main(content) - total) * ma;
        for &i in &order {
            let s = kids[i].size;
            let c = (cross(content) - cross(s)) * ca;
            at[i] = Some(if horizontal {
                Vec2::new(cursor, c)
            } else {
                Vec2::new(c, cursor)
            });
            cursor += main(s) + gap;
        }
        let size = if horizontal {
            Vec2::new(total, widest)
        } else {
            Vec2::new(widest, total)
        };
        Arrange {
            at,
            content: size,
            cells: None,
        }
    }

    /// Lays out the children of `parent` in the content box `content`
    /// (local pixels). Returns the extent of what was placed, from the
    /// content origin, for `AutomaticSize`. `dry` measures only.
    #[allow(clippy::too_many_arguments)]
    fn children(
        &mut self,
        parent: Entity,
        content: (Vec2, Vec2),
        sc: Sc,
        rot: Affine2,
        angle: f32,
        clip: Option<egui::Rect>,
        shown: bool,
        dry: bool,
        root: &Root,
    ) -> Vec2 {
        let (origin, area) = content;
        let mut kids: Vec<Kid> = Vec::new();
        let decor = self.s.decor(parent);
        let layout = decor.layout;
        for &c in &decor.kids {
            let p = props(self, c);
            let base = p.udim2("Size").of(area);
            kids.push(Kid {
                e: c,
                size: base,
                visible: p.bool("Visible", true),
            });
        }
        // Grid cells replace sizes before constraints and AutomaticSize.
        let grid = layout.filter(|&l| is_a(self, l, "UIGridLayout"));
        if let Some(g) = grid {
            let cell = self.cell_size(g, area);
            for k in &mut kids {
                k.size = cell;
            }
        }
        for k in &mut kids {
            k.size = self.constrained(k.e, k.size);
            let (ax, ay) = axes(&props(self, k.e).item("AutomaticSize", "None"));
            if ax || ay {
                let fit = self.measure(k.e, k.size, sc.k, root, (ax, ay));
                if ax {
                    k.size.x = k.size.x.max(fit.x);
                }
                if ay {
                    k.size.y = k.size.y.max(fit.y);
                }
                k.size = self.size_limits(k.e, k.size);
            }
        }
        let arranged = layout.map(|l| (l, self.arrange(l, &mut kids, area)));
        if let Some((l, a)) = &arranged
            && !dry
        {
            self.write(*l, "AbsoluteContentSize", vec2_value(a.content));
            if let Some((cell, count)) = a.cells {
                self.write(*l, "AbsoluteCellSize", vec2_value(cell));
                self.write(*l, "AbsoluteCellCount", vec2_value(count));
            }
        }
        let mut extent = arranged.as_ref().map_or(Vec2::ZERO, |(_, a)| a.content);
        // Drawing order among siblings.
        let mut idx: Vec<usize> = (0..kids.len()).collect();
        if !root.global_z {
            idx.sort_by_cached_key(|&i| props(self, kids[i].e).int("ZIndex", 1));
        }
        for i in idx {
            let k = &kids[i];
            let p = props(self, k.e);
            let size = k.size;
            let anchor = p.vec2("AnchorPoint", Vec2::ZERO);
            let pos = match arranged.as_ref().and_then(|(_, a)| a.at[i]) {
                Some(at) => origin + at,
                None => origin + p.udim2("Position").of(area) - anchor * size,
            };
            if k.visible || arranged.is_none() {
                extent = extent.max(pos + size - origin);
            }
            if dry {
                continue;
            }
            // UIScale scales the element about its anchor point.
            let s = self.ui_scale(k.e);
            let sc_k = sc.scaled_about(s, pos + anchor * size);
            let min = sc_k.map(pos);
            let ssize = size * sc_k.k;
            let r = p.f32("Rotation", 0.0);
            let rot_k = if r != 0.0 {
                rot * rotate_about(min + ssize * 0.5, r)
            } else {
                rot
            };
            let angle_k = angle + r;
            let rotated = angle_k.rem_euclid(360.0) != 0.0;
            self.write(k.e, "AbsolutePosition", vec2_value(min - root.abs_origin));
            self.write(k.e, "AbsoluteSize", vec2_value(ssize));
            self.write(k.e, "AbsoluteRotation", Value::Float(angle_k as f64));
            let visible = shown && k.visible;
            let pad = padding(self, k.e, size);
            let spad = pad.map(|x| x * sc_k.k);
            let kind = if is_a(self, k.e, "TextBox") {
                Kind::TextBox
            } else if is_a(self, k.e, "TextLabel") || is_a(self, k.e, "TextButton") {
                Kind::Text
            } else if is_a(self, k.e, "ImageLabel") || is_a(self, k.e, "ImageButton") {
                Kind::Image
            } else if is_a(self, k.e, "ScrollingFrame") {
                Kind::Scrolling
            } else if is_a(self, k.e, "ViewportFrame") {
                Kind::Viewport
            } else {
                Kind::Frame
            };
            let text = if matches!(kind, Kind::Text | Kind::TextBox) {
                let style = self.text_style(k.e, sc_k.k);
                let inner =
                    (ssize - Vec2::new(spad[0] + spad[2], spad[1] + spad[3])).max(Vec2::ZERO);
                let laid = text::lay_out(self.ctx, &style, [Some(inner.x), Some(inner.y)]);
                let b = text::bounds(&laid);
                self.write(k.e, "TextBounds", vec2_value(b / sc_k.k.max(1e-6)));
                self.write(k.e, "TextFits", Value::Bool(laid.fits));
                Some((style, laid, inner))
            } else {
                None
            };
            let screen =
                egui::Rect::from_min_size(egui::pos2(min.x, min.y), egui::vec2(ssize.x, ssize.y));
            let clips = p.bool("ClipsDescendants", false) || kind == Kind::Scrolling;
            let child_clip = if clips && !rotated {
                Some(clip.map_or(screen, |c| c.intersect(screen)))
            } else {
                clip
            };
            self.seq += 1;
            let z = if root.global_z { p.int("ZIndex", 1) } else { 0 };
            let interactable = visible && root.input && p.bool("Interactable", true);
            let slot = visible.then_some(self.items.len());
            if visible {
                self.items.push(Item {
                    e: k.e,
                    kind,
                    min,
                    size: ssize,
                    rot: rot_k,
                    angle: angle_k,
                    clip: if rotated { None } else { clip },
                    scale: sc_k.k,
                    pad: spad,
                    text,
                    canvas: None,
                    key: (z, self.seq),
                    interactable,
                });
            }
            // The content box of the children.
            let inner_origin = pos + Vec2::new(pad[0], pad[1]);
            let inner = (size - Vec2::new(pad[0] + pad[2], pad[1] + pad[3])).max(Vec2::ZERO);
            if kind == Kind::Scrolling {
                // The canvas is never smaller than the window.
                let mut canvas = p.udim2("CanvasSize").of(size).max(size);
                let (ax, ay) = axes(&p.item("AutomaticCanvasSize", "None"));
                if ax || ay {
                    let fit = self.measure_children(k.e, (inner_origin, inner), sc_k, root);
                    if ax {
                        canvas.x = canvas.x.max(fit.x + pad[0] + pad[2]);
                    }
                    if ay {
                        canvas.y = canvas.y.max(fit.y + pad[1] + pad[3]);
                    }
                }
                let max_scroll = (canvas - size).max(Vec2::ZERO);
                let at = p.vec2("CanvasPosition", Vec2::ZERO);
                let clamped = at.clamp(Vec2::ZERO, max_scroll);
                if clamped != at {
                    self.write(k.e, "CanvasPosition", vec2_value(clamped));
                }
                self.write(k.e, "AbsoluteCanvasSize", vec2_value(canvas * sc_k.k));
                if let Some(i) = slot {
                    self.items[i].canvas = Some(canvas * sc_k.k);
                }
                self.write(k.e, "AbsoluteWindowSize", vec2_value(ssize));
                let corigin = pos - clamped + Vec2::new(pad[0], pad[1]);
                let carea = (canvas - Vec2::new(pad[0] + pad[2], pad[1] + pad[3])).max(Vec2::ZERO);
                self.children(
                    k.e,
                    (corigin, carea),
                    sc_k,
                    rot_k,
                    angle_k,
                    child_clip,
                    visible,
                    false,
                    root,
                );
            } else {
                self.children(
                    k.e,
                    (inner_origin, inner),
                    sc_k,
                    rot_k,
                    angle_k,
                    child_clip,
                    visible,
                    false,
                    root,
                );
            }
        }
        extent
    }

    fn measure_children(&mut self, e: Entity, content: (Vec2, Vec2), sc: Sc, root: &Root) -> Vec2 {
        self.children(
            e,
            content,
            sc,
            Affine2::IDENTITY,
            0.0,
            None,
            false,
            true,
            root,
        )
    }

    /// The size `AutomaticSize` grows `e` to: its children's extent and its
    /// text, plus its padding (local pixels).
    fn measure(&mut self, e: Entity, size: Vec2, k: f32, root: &Root, auto: (bool, bool)) -> Vec2 {
        let pad = padding(self, e, size);
        let inner = (size - Vec2::new(pad[0] + pad[2], pad[1] + pad[3])).max(Vec2::ZERO);
        let mut fit = self.measure_children(e, (Vec2::ZERO, inner), Sc { k, o: Vec2::ZERO }, root);
        if is_a(self, e, "TextLabel") || is_a(self, e, "TextButton") || is_a(self, e, "TextBox") {
            let mut style = self.text_style(e, 1.0);
            style.scaled = false;
            style.truncate = false;
            let bounds = [(!auto.0).then_some(inner.x), (!auto.1).then_some(inner.y)];
            let laid = text::lay_out(self.ctx, &style, bounds);
            fit = fit.max(text::bounds(&laid));
        }
        fit + Vec2::new(pad[0] + pad[2], pad[1] + pad[3])
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
    let size: UDim2 = p.udim2("Size");
    let per_stud = cam.focal(viewport) / depth;
    let px = Vec2::new(
        size.x.scale * per_stud + size.x.offset,
        size.y.scale * per_stud + size.y.offset,
    );
    let shift = p.vec2("SizeOffset", Vec2::ZERO) * px;
    Some((center - px * 0.5 + Vec2::new(shift.x, -shift.y), px, depth))
}
