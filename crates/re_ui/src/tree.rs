//! The tree a host gives the UI core: plain nodes, with no entity, class or
//! script behind them. A host builds one a frame from its own model (compat
//! from the Instance tree, native UI from components) and lays its roots
//! out with [`Layout`](crate::Layout).

use glam::Vec2;

use crate::text::TextStyle;

/// A length: a fraction of the parent's length plus pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dim {
    pub scale: f32,
    pub offset: f32,
}

impl Dim {
    pub const ZERO: Dim = Dim::new(0.0, 0.0);

    pub const fn new(scale: f32, offset: f32) -> Dim {
        Dim { scale, offset }
    }

    /// `offset` pixels.
    pub const fn px(offset: f32) -> Dim {
        Dim::new(0.0, offset)
    }

    /// The length against a parent `length`.
    pub fn of(self, length: f32) -> f32 {
        self.scale * length + self.offset
    }
}

/// A size or position: a [`Dim`] on each axis.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dim2 {
    pub x: Dim,
    pub y: Dim,
}

impl Dim2 {
    pub const ZERO: Dim2 = Dim2::new(Dim::ZERO, Dim::ZERO);

    pub const fn new(x: Dim, y: Dim) -> Dim2 {
        Dim2 { x, y }
    }

    /// `x` by `y` pixels.
    pub const fn px(x: f32, y: f32) -> Dim2 {
        Dim2::new(Dim::px(x), Dim::px(y))
    }

    /// `x` and `y` of the parent's size.
    pub const fn scale(x: f32, y: f32) -> Dim2 {
        Dim2::new(Dim::new(x, 0.0), Dim::new(y, 0.0))
    }

    /// The point or size against a parent `size`.
    pub fn of(self, size: Vec2) -> Vec2 {
        Vec2::new(self.x.of(size.x), self.y.of(size.y))
    }
}

/// A colour with alpha: sRGB channels and alpha from 0 to 1, not
/// premultiplied, as egui paints them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rgba(pub [f32; 4]);

impl Rgba {
    pub const WHITE: Rgba = Rgba([1.0, 1.0, 1.0, 1.0]);
    pub const BLACK: Rgba = Rgba([0.0, 0.0, 0.0, 1.0]);
    pub const TRANSPARENT: Rgba = Rgba([0.0, 0.0, 0.0, 0.0]);

    /// An opaque colour from sRGB channels.
    pub const fn rgb(r: f32, g: f32, b: f32) -> Rgba {
        Rgba([r, g, b, 1.0])
    }

    /// The colour with its alpha scaled by `1 - t`.
    pub fn with_transparency(self, t: f32) -> Rgba {
        let mut c = self.0;
        c[3] *= (1.0 - t).clamp(0.0, 1.0);
        Rgba(c)
    }

    pub fn egui(self) -> egui::Color32 {
        let c = self.0.map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8);
        egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
    }
}

/// Channel by channel product.
impl std::ops::Mul for Rgba {
    type Output = Rgba;

    fn mul(self, o: Rgba) -> Rgba {
        let (a, b) = (self.0, o.0);
        Rgba([a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]])
    }
}

/// Which axes of a box grow to fit its content.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Axes {
    pub x: bool,
    pub y: bool,
}

impl Axes {
    pub const NONE: Axes = Axes { x: false, y: false };
    pub const X: Axes = Axes { x: true, y: false };
    pub const Y: Axes = Axes { x: false, y: true };
    pub const BOTH: Axes = Axes { x: true, y: true };

    pub fn any(self) -> bool {
        self.x || self.y
    }
}

/// Space inside a node's edges that its children keep out of.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Padding {
    pub left: Dim,
    pub top: Dim,
    pub right: Dim,
    pub bottom: Dim,
}

impl Padding {
    /// Left, top, right and bottom in pixels for a node of `size`.
    pub fn of(&self, size: Vec2) -> [f32; 4] {
        [
            self.left.of(size.x),
            self.top.of(size.y),
            self.right.of(size.x),
            self.bottom.of(size.y),
        ]
    }
}

/// The smallest and largest size a node takes, in pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizeLimits {
    pub min: Vec2,
    pub max: Vec2,
}

/// An axis.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Axis {
    #[default]
    X,
    Y,
}

/// A width-to-height ratio a node keeps. A ratio that is not a positive
/// number does nothing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Aspect {
    /// The largest box of the ratio inside the size.
    Fit { ratio: f32 },
    /// The `dominant` axis keeps its length and the other follows it.
    Follow { ratio: f32, dominant: Axis },
}

impl Aspect {
    /// `size` fitted to the ratio.
    pub fn apply(self, mut size: Vec2) -> Vec2 {
        let ar = match self {
            Aspect::Fit { ratio } | Aspect::Follow { ratio, .. } => ratio,
        };
        if ar.is_finite() && ar > 0.0 {
            match self {
                Aspect::Follow {
                    dominant: Axis::Y, ..
                } => size.x = size.y * ar,
                Aspect::Follow { .. } => size.y = size.x / ar,
                Aspect::Fit { .. } => {
                    let w = size.x.min(size.y * ar);
                    size = Vec2::new(w, w / ar);
                }
            }
        }
        size
    }
}

/// How an arrangement orders the children it places.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    /// By [`Node::order`], then child order.
    #[default]
    Order,
    /// By [`Node::name`], then child order.
    Name,
}

/// The corner a grid starts filling from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Corner {
    #[default]
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// Children placed one after another along a direction.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct List {
    pub horizontal: bool,
    /// Where the run sits in the content box: 0 at the start, 0.5 in the
    /// middle and 1 at the end, horizontally then vertically.
    pub align: Vec2,
    pub sort: Sort,
    /// The gap between children, against the content box's length.
    pub gap: Dim,
}

/// Children placed in equal cells, line by line.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Grid {
    /// Lines fill horizontally (rows) or vertically (columns).
    pub horizontal: bool,
    pub align: Vec2,
    pub sort: Sort,
    /// Each cell's size, which replaces the children's own.
    pub cell: Dim2,
    pub gap: Dim2,
    /// At most this many cells a line; 0 for as many as fit.
    pub max_cells: i64,
    pub start: Corner,
    /// A ratio the cell size keeps.
    pub cell_aspect: Option<Aspect>,
}

/// How a node places its children instead of their own positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Arrange {
    List(List),
    Grid(Grid),
}

/// A node that scrolls its children over a canvas larger than itself.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scroll {
    /// The canvas size; it is never smaller than the node.
    pub canvas: Dim2,
    /// The canvas grows to fit the children on these axes.
    pub automatic: Axes,
    /// The canvas point at the node's top-left corner, as the host has it;
    /// layout holds it to the canvas.
    pub position: Vec2,
    /// The scroll bars' thickness in pixels; 0 hides them.
    pub bar: f32,
    pub bar_color: Rgba,
}

/// Where a stroke goes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StrokeMode {
    /// Around the text of a node with text, else around its box.
    #[default]
    Contextual,
    /// Around the box.
    Border,
}

/// An outline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stroke {
    pub mode: StrokeMode,
    pub thickness: f32,
    pub color: Rgba,
}

/// A background coloured along a direction: the background colour times the
/// colour keys, its alpha times one less the transparency keys.
#[derive(Clone, Debug, PartialEq)]
pub struct Gradient {
    /// `(t, rgb)` keys, sorted by `t` from 0 to 1.
    pub colors: Vec<(f32, [f32; 3])>,
    /// `(t, transparency)` keys (the value in each channel), sorted by `t`.
    pub transparency: Vec<(f32, [f32; 3])>,
    /// Shifts the gradient across the node, in node sizes.
    pub offset: Vec2,
    /// Turns the gradient's direction from +x, in degrees.
    pub rotation: f32,
}

/// How a node's box looks.
#[derive(Clone, Debug, PartialEq)]
pub struct Look {
    /// The fill; a fully transparent one draws nothing.
    pub background: Rgba,
    /// The width of the plain border in pixels, drawn around a filled,
    /// square-cornered box with no border stroke.
    pub border: f32,
    pub border_color: Rgba,
    /// Rounds the corners (and hides the plain border).
    pub corner: Option<Dim>,
    /// Outlines; the first of each mode is drawn.
    pub strokes: Vec<Stroke>,
    pub gradient: Option<Gradient>,
    /// Darkens the fill while hovered and pressed (buttons).
    pub auto_color: bool,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            background: Rgba::TRANSPARENT,
            border: 0.0,
            border_color: Rgba::BLACK,
            corner: None,
            strokes: Vec::new(),
            gradient: None,
            auto_color: false,
        }
    }
}

/// A picture over a node's box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Image {
    /// An image that is not loaded: a flat placeholder in its tint.
    Missing { tint: Rgba },
    /// A texture the host registered with egui, stretched over the node and
    /// turned with it.
    Texture {
        texture: egui::TextureId,
        tint: Rgba,
    },
}

/// A node of a [`UiTree`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeId(pub u32);

impl NodeId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One box of UI.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    /// The host's own id for the node (an entity's bits).
    pub key: u64,
    /// For arrangements that sort by name.
    pub name: String,
    /// In child order.
    pub children: Vec<NodeId>,
    pub visible: bool,
    /// Whether it can take input while drawn.
    pub interactable: bool,
    /// The position of its anchor point against the parent's content box.
    pub position: Dim2,
    pub size: Dim2,
    /// The point of the node, in node sizes, that `position` places.
    pub anchor: Vec2,
    /// Grows to fit its children and text.
    pub automatic_size: Axes,
    /// Turns the drawing (and the children's) about its centre, in degrees;
    /// layout is not turned.
    pub rotation: f32,
    /// Drawing order among siblings, or across the root when the root says
    /// so.
    pub z: i64,
    /// Sort key for arrangements that sort by order.
    pub order: i64,
    /// Clips the children to its box.
    pub clips: bool,
    pub padding: Option<Padding>,
    /// Scales the node and its children about its anchor point; a scale that
    /// is not a number of 0 or more is 1.
    pub scale: Option<f32>,
    pub limits: Option<SizeLimits>,
    pub aspect: Option<Aspect>,
    pub arrange: Option<Arrange>,
    pub scroll: Option<Scroll>,
    pub text: Option<TextStyle>,
    pub image: Option<Image>,
    pub look: Look,
}

impl Default for Node {
    fn default() -> Self {
        Node {
            key: 0,
            name: String::new(),
            children: Vec::new(),
            visible: true,
            interactable: true,
            position: Dim2::ZERO,
            size: Dim2::ZERO,
            anchor: Vec2::ZERO,
            automatic_size: Axes::NONE,
            rotation: 0.0,
            z: 1,
            order: 0,
            clips: false,
            padding: None,
            scale: None,
            limits: None,
            aspect: None,
            arrange: None,
            scroll: None,
            text: None,
            image: None,
            look: Look::default(),
        }
    }
}

/// The nodes of one frame. Ids index the nodes in the order they were
/// added.
#[derive(Clone, Debug, Default)]
pub struct UiTree {
    nodes: Vec<Node>,
}

impl UiTree {
    pub fn new() -> UiTree {
        UiTree::default()
    }

    /// Adds a node and gives its id; its children are added before or after
    /// and listed in [`Node::children`].
    pub fn add(&mut self, node: Node) -> NodeId {
        let id = NodeId(self.nodes.len() as u32);
        self.nodes.push(node);
        id
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id.index()]
    }

    pub fn node_mut(&mut self, id: NodeId) -> &mut Node {
        &mut self.nodes[id.index()]
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Removes every node, keeping the memory.
    pub fn clear(&mut self) {
        self.nodes.clear();
    }

    /// The scale a node's [`Node::scale`] gives.
    pub(crate) fn scale_of(&self, id: NodeId) -> f32 {
        self.node(id)
            .scale
            .filter(|s| s.is_finite() && *s >= 0.0)
            .unwrap_or(1.0)
    }
}
