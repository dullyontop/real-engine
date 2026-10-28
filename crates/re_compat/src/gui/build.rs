//! The frame's UI tree: the Instance tree as the UI core's nodes.
//!
//! Each element (a `GuiObject`) becomes a [`re_ui::Node`], with the
//! modifiers among its children folded in: `UIPadding`, `UIScale`,
//! `UISizeConstraint`, `UIAspectRatioConstraint`, `UIListLayout` or
//! `UIGridLayout`, `UICorner`, `UIGradient` and the `UIStroke`s. A root (a
//! `ScreenGui`, `BillboardGui` or `SurfaceGui`) becomes a node for its
//! children and modifiers. The core lays the nodes out and paints them; the
//! step reports what it placed back as the read-only properties.

use std::collections::HashMap;

use glam::Vec2;
use re_reflect::Value;
use re_scene::Entity;
use re_ui::{
    Arrange, Aspect, Axes, Axis, Corner, Gradient, Grid, Image, List, Look, Node, NodeId, Padding,
    Rgba, Scroll, SizeLimits, Sort, Stroke, StrokeMode, TextStyle, UiTree, Visit,
};

use super::fonts;
use super::props::{FxMap, Scope, vec2_value};
use crate::instance::tree;

/// What a drawn element is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A root, or an element that is none of the others.
    Frame,
    Text,
    Image,
    TextBox,
    Scrolling,
    /// A `ViewportFrame`: its background, then the picture of its parts.
    Viewport,
}

fn kind_of(s: &Scope, e: Entity) -> Kind {
    if s.is_a(e, "TextBox") {
        Kind::TextBox
    } else if s.is_a(e, "TextLabel") || s.is_a(e, "TextButton") {
        Kind::Text
    } else if s.is_a(e, "ImageLabel") || s.is_a(e, "ImageButton") {
        Kind::Image
    } else if s.is_a(e, "ScrollingFrame") {
        Kind::Scrolling
    } else if s.is_a(e, "ViewportFrame") {
        Kind::Viewport
    } else {
        Kind::Frame
    }
}

fn axes(item: &str) -> Axes {
    match item {
        "X" => Axes::X,
        "Y" => Axes::Y,
        "XY" => Axes::BOTH,
        _ => Axes::NONE,
    }
}

/// The nodes of a frame and the instances behind them.
#[derive(Default)]
pub(crate) struct Built {
    pub tree: UiTree,
    /// Each node's entity, kind and list or grid layout, by node index.
    ents: Vec<Entity>,
    kinds: Vec<Kind>,
    layouts: Vec<Option<Entity>>,
    ids: FxMap<Entity, NodeId>,
}

impl Built {
    pub fn entity(&self, id: NodeId) -> Entity {
        self.ents[id.index()]
    }

    pub fn kind(&self, id: NodeId) -> Kind {
        self.kinds[id.index()]
    }

    /// The node of a drawn or laid-out entity.
    pub fn id(&self, e: Entity) -> Option<NodeId> {
        self.ids.get(&e).copied()
    }

    /// Adds root `e` and the elements under it; gives the root's node. The
    /// `images` are the textures the host drew `ViewportFrame`s into.
    pub fn root(
        &mut self,
        s: &Scope,
        e: Entity,
        images: &HashMap<Entity, egui::TextureId>,
    ) -> NodeId {
        let decor = s.decor(e);
        let node = Node {
            key: e.to_bits(),
            scale: decor.scale.map(|u| s.props(u).f32("Scale", 1.0)),
            arrange: decor.layout.map(|l| arrange(s, l)),
            ..Node::default()
        };
        let id = self.push(e, Kind::Frame, decor.layout, node);
        let by_name = matches!(self.tree.node(id).arrange, Some(a) if sorts_by_name(&a));
        let kids: Vec<NodeId> = decor
            .kids
            .iter()
            .map(|&c| self.element(s, c, by_name, images))
            .collect();
        self.tree.node_mut(id).children = kids;
        id
    }

    fn push(&mut self, e: Entity, kind: Kind, layout: Option<Entity>, node: Node) -> NodeId {
        let id = self.tree.add(node);
        self.ents.push(e);
        self.kinds.push(kind);
        self.layouts.push(layout);
        self.ids.insert(e, id);
        id
    }

    /// Adds element `e` and its descendants.
    fn element(
        &mut self,
        s: &Scope,
        e: Entity,
        named: bool,
        images: &HashMap<Entity, egui::TextureId>,
    ) -> NodeId {
        let decor = s.decor(e);
        let p = s.props(e);
        let kind = kind_of(s, e);
        let node = Node {
            key: e.to_bits(),
            name: if named {
                tree::name(s.w, e)
            } else {
                String::new()
            },
            children: Vec::new(),
            visible: p.bool("Visible", true),
            interactable: p.bool("Interactable", true),
            position: p.udim2("Position"),
            size: p.udim2("Size"),
            anchor: p.vec2("AnchorPoint", Vec2::ZERO),
            automatic_size: axes(&p.item("AutomaticSize", "None")),
            rotation: p.f32("Rotation", 0.0),
            z: p.int("ZIndex", 1),
            order: p.int("LayoutOrder", 0),
            clips: p.bool("ClipsDescendants", false),
            padding: decor.padding.map(|c| {
                let q = s.props(c);
                Padding {
                    left: q.udim("PaddingLeft"),
                    top: q.udim("PaddingTop"),
                    right: q.udim("PaddingRight"),
                    bottom: q.udim("PaddingBottom"),
                }
            }),
            scale: decor.scale.map(|u| s.props(u).f32("Scale", 1.0)),
            limits: decor.size.map(|c| {
                let q = s.props(c);
                SizeLimits {
                    min: q.vec2("MinSize", Vec2::ZERO),
                    max: q.vec2("MaxSize", Vec2::splat(f32::INFINITY)),
                }
            }),
            aspect: decor.aspect.map(|c| aspect(s, c)),
            arrange: decor.layout.map(|l| arrange(s, l)),
            scroll: (kind == Kind::Scrolling).then(|| Scroll {
                canvas: p.udim2("CanvasSize"),
                automatic: axes(&p.item("AutomaticCanvasSize", "None")),
                position: p.vec2("CanvasPosition", Vec2::ZERO),
                bar: p.f32("ScrollBarThickness", 12.0),
                bar_color: p
                    .color("ScrollBarImageColor3", Rgba::BLACK)
                    .with_transparency(p.f32("ScrollBarImageTransparency", 0.0)),
            }),
            text: matches!(kind, Kind::Text | Kind::TextBox).then(|| text_style(s, e, kind)),
            image: match kind {
                Kind::Image if !p.string("Image").is_empty() => Some(Image::Missing {
                    tint: image_tint(s, e),
                }),
                Kind::Viewport => images.get(&e).map(|&texture| Image::Texture {
                    texture,
                    tint: image_tint(s, e),
                }),
                _ => None,
            },
            look: look(s, e),
        };
        let id = self.push(e, kind, decor.layout, node);
        let by_name = matches!(self.tree.node(id).arrange, Some(a) if sorts_by_name(&a));
        let kids: Vec<NodeId> = decor
            .kids
            .iter()
            .map(|&c| self.element(s, c, by_name, images))
            .collect();
        self.tree.node_mut(id).children = kids;
        id
    }

    /// The read-only properties a frame's layout gives, as writes in the
    /// order the layout placed the elements.
    pub fn writes(&self, visits: &[Visit]) -> Vec<(Entity, &'static str, Value)> {
        let mut out = Vec::new();
        for v in visits {
            let e = self.entity(v.node);
            if v.root {
                out.push((e, "AbsoluteSize", vec2_value(v.size)));
                out.push((e, "AbsolutePosition", vec2_value(v.position)));
            } else {
                out.push((e, "AbsolutePosition", vec2_value(v.position)));
                out.push((e, "AbsoluteSize", vec2_value(v.size)));
                out.push((e, "AbsoluteRotation", Value::Float(v.rotation as f64)));
                if let Some((bounds, fits)) = v.text {
                    out.push((e, "TextBounds", vec2_value(bounds)));
                    out.push((e, "TextFits", Value::Bool(fits)));
                }
                if let Some(sv) = v.scroll {
                    if let Some(at) = sv.clamped {
                        out.push((e, "CanvasPosition", vec2_value(at)));
                    }
                    out.push((e, "AbsoluteCanvasSize", vec2_value(sv.canvas)));
                    out.push((e, "AbsoluteWindowSize", vec2_value(sv.window)));
                }
            }
            if let (Some(a), Some(l)) = (v.arranged, self.layouts[v.node.index()]) {
                out.push((l, "AbsoluteContentSize", vec2_value(a.content)));
                if let Some((cell, count)) = a.cells {
                    out.push((l, "AbsoluteCellSize", vec2_value(cell)));
                    out.push((l, "AbsoluteCellCount", vec2_value(count)));
                }
            }
        }
        out
    }
}

fn sorts_by_name(a: &Arrange) -> bool {
    match a {
        Arrange::List(l) => l.sort == Sort::Name,
        Arrange::Grid(g) => g.sort == Sort::Name,
    }
}

fn aspect(s: &Scope, c: Entity) -> Aspect {
    let p = s.props(c);
    let ratio = p.f32("AspectRatio", 1.0);
    match p.item("AspectType", "FitWithinMaxSize").as_str() {
        "ScaleWithParentSize" => Aspect::Follow {
            ratio,
            dominant: if p.item("DominantAxis", "Width") == "Height" {
                Axis::Y
            } else {
                Axis::X
            },
        },
        _ => Aspect::Fit { ratio },
    }
}

/// A `UIListLayout` or `UIGridLayout`.
fn arrange(s: &Scope, layout: Entity) -> Arrange {
    let p = s.props(layout);
    let horizontal = p.item("FillDirection", "Horizontal") == "Horizontal";
    let align = Vec2::new(
        match p.item("HorizontalAlignment", "Left").as_str() {
            "Center" => 0.5,
            "Right" => 1.0,
            _ => 0.0,
        },
        match p.item("VerticalAlignment", "Top").as_str() {
            "Center" => 0.5,
            "Bottom" => 1.0,
            _ => 0.0,
        },
    );
    let sort = if p.item("SortOrder", "LayoutOrder") == "Name" {
        Sort::Name
    } else {
        Sort::Order
    };
    if s.is_a(layout, "UIGridLayout") {
        let corner = p.item("StartCorner", "TopLeft");
        Arrange::Grid(Grid {
            horizontal,
            align,
            sort,
            cell: p.udim2("CellSize"),
            gap: p.udim2("CellPadding"),
            max_cells: p.int("FillDirectionMaxCells", 0),
            start: match (corner.starts_with("Bottom"), corner.ends_with("Right")) {
                (false, false) => Corner::TopLeft,
                (false, true) => Corner::TopRight,
                (true, false) => Corner::BottomLeft,
                (true, true) => Corner::BottomRight,
            },
            cell_aspect: s.decor(layout).aspect.map(|c| aspect(s, c)),
        })
    } else {
        Arrange::List(List {
            horizontal,
            align,
            sort,
            gap: p.udim("Padding"),
        })
    }
}

fn text_style(s: &Scope, e: Entity, kind: Kind) -> TextStyle {
    let p = s.props(e);
    let (family, weight, italic) = p.font_face();
    let placeholder = kind == Kind::TextBox && p.string("Text").is_empty();
    let (text, color) = if placeholder {
        (
            p.string("PlaceholderText"),
            p.color("PlaceholderColor3", Rgba([0.7, 0.7, 0.7, 1.0])),
        )
    } else {
        (p.string("Text"), p.color("TextColor3", Rgba::WHITE))
    };
    let stroke = p.f32("TextStrokeTransparency", 1.0);
    TextStyle {
        text,
        face: fonts::face(&family, &weight, italic),
        size: p.f32("TextSize", 14.0),
        color: color.with_transparency(p.f32("TextTransparency", 0.0)),
        scaled: p.bool("TextScaled", false),
        wrapped: p.bool("TextWrapped", false) || p.bool("TextScaled", false),
        truncate: p.item("TextTruncate", "None") != "None",
        rich: p.bool("RichText", false),
        line_height: p.f32("LineHeight", 1.0),
        align: Vec2::new(
            match p.item("TextXAlignment", "Center").as_str() {
                "Left" => 0.0,
                "Right" => 1.0,
                _ => 0.5,
            },
            match p.item("TextYAlignment", "Center").as_str() {
                "Top" => 0.0,
                "Bottom" => 1.0,
                _ => 0.5,
            },
        ),
        placeholder,
        stroke: (stroke < 1.0).then(|| {
            p.color("TextStrokeColor3", Rgba::BLACK)
                .with_transparency(stroke)
        }),
    }
}

fn image_tint(s: &Scope, e: Entity) -> Rgba {
    let p = s.props(e);
    p.color("ImageColor3", Rgba::WHITE)
        .with_transparency(p.f32("ImageTransparency", 0.0))
}

fn look(s: &Scope, e: Entity) -> Look {
    let decor = s.decor(e);
    let p = s.props(e);
    let transparency = p.f32("BackgroundTransparency", 0.0);
    Look {
        background: p
            .color("BackgroundColor3", Rgba::WHITE)
            .with_transparency(transparency),
        border: p.f32("BorderSizePixel", 1.0),
        border_color: p
            .color("BorderColor3", Rgba([0.1, 0.1, 0.1, 1.0]))
            .with_transparency(transparency),
        corner: decor.corner.map(|c| s.props(c).udim("CornerRadius")),
        strokes: decor
            .strokes
            .iter()
            .filter_map(|&c| {
                let q = s.props(c);
                q.bool("Enabled", true).then(|| Stroke {
                    mode: if q.item("ApplyStrokeMode", "Contextual") == "Border" {
                        StrokeMode::Border
                    } else {
                        StrokeMode::Contextual
                    },
                    thickness: q.f32("Thickness", 1.0),
                    color: q
                        .color("Color", Rgba::BLACK)
                        .with_transparency(q.f32("Transparency", 0.0)),
                })
            })
            .collect(),
        gradient: decor
            .gradient
            .filter(|&g| s.props(g).bool("Enabled", true))
            .map(|g| {
                let q = s.props(g);
                Gradient {
                    colors: q.sequence("Color"),
                    transparency: q.sequence("Transparency"),
                    offset: q.vec2("Offset", Vec2::ZERO),
                    rotation: q.f32("Rotation", 0.0),
                }
            }),
        auto_color: s.is_a(e, "GuiButton") && p.bool("AutoButtonColor", true),
    }
}
