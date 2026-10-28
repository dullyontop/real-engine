//! The frame's UI tree from the World's UI components (ADR-0058 part 7):
//! each entity with a `UiRoot` starts a root, and its children with a
//! `UiNode`, and theirs in turn, are the root's nodes, in child order (one
//! with a `UiRoot` of its own starts another root instead). A node under an
//! entity with neither is not drawn. A node's look is its components'
//! fields, then its style's over them, then the style's states the node is
//! in. Colours are linear in the scene and sRGB in the core.

use std::collections::HashMap;

use glam::{Vec2, Vec3};
use re_reflect::Color;
use re_render::egui;
use re_scene::hierarchy::children;
use re_scene::{
    Entity, GlobalTransform, Name, StableId, UiAlign, UiAutoSize, UiBar, UiBarDirection, UiButton,
    UiFont, UiImage, UiLayout, UiNode, UiPanel, UiRoot, UiScroll, UiSpace, UiText, UiTextInput,
    World,
};
use re_ui::theme::{self, Patch, State, Style};
use re_ui::{
    Arrange, Aspect, Axes, Bar, BarDirection, Corner, Dim, Dim2, Face, Family, Gradient, Grid,
    Image, List, Look, Node, NodeId, Padding, Rgba, Scroll, SizeLimits, Sort, Stroke, StrokeMode,
    TextStyle, Theme, UiTree,
};

/// Where a root is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Space {
    /// Over the whole view.
    Screen {
        /// The view's height in the root's units (0: a unit is a point).
        reference_height: f32,
    },
    /// A box in the world that faces the camera.
    World {
        /// The box's centre, relative to the render origin.
        at: Vec3,
        /// The box's size in the root's units.
        canvas: Vec2,
        /// The metres the box's height spans.
        height: f32,
        /// Hidden further than this from the camera (0: no limit).
        max_distance: f32,
    },
}

/// One root of the frame's tree.
#[derive(Clone, Debug, PartialEq)]
pub struct HostRoot {
    /// The entity with the `UiRoot`.
    pub entity: Entity,
    /// The node layout starts from: a container whose one child is the
    /// root entity's own node, filling it.
    pub node: NodeId,
    /// Where it is drawn.
    pub space: Space,
    /// Whether the pointer can reach its nodes.
    pub input: bool,
    /// Whether it takes focus and navigation while shown.
    pub focus: bool,
}

/// The frame's UI, built from the World.
#[derive(Clone, Debug, Default)]
pub struct Built {
    /// Every root's nodes.
    pub tree: UiTree,
    /// Screen roots in drawing order, then world roots.
    pub roots: Vec<HostRoot>,
    /// Each node's entity, by node index (`None` for a root's container).
    pub entities: Vec<Option<Entity>>,
}

impl Built {
    /// The entity of `node`.
    pub fn entity(&self, node: NodeId) -> Option<Entity> {
        self.entities.get(node.index()).copied().flatten()
    }
}

/// What building found to report: style names a theme lacks, and component
/// fields a style hides, as `(entity, component, field)`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Findings {
    /// Nodes naming a style their theme lacks, with the name.
    pub missing: Vec<(Entity, String)>,
    /// Component fields set on a node whose style sets them too.
    pub hidden: Vec<(Entity, &'static str, &'static str)>,
}

/// What the build reads besides the World.
pub struct Inputs<'a> {
    /// The project's theme, for roots with no `UiRoot.theme`.
    pub theme: &'a Theme,
    /// The themes roots name in `UiRoot.theme` that loaded; a root whose
    /// theme is not here uses the project's.
    pub root_themes: &'a HashMap<String, Theme>,
    /// The nodes in a state other than the default (from input routing).
    pub states: &'a HashMap<Entity, State>,
    /// The textures registered for `UiImage.image` paths.
    pub images: &'a HashMap<String, egui::TextureId>,
    /// The render origin, which world roots are placed relative to.
    pub origin: Vec3,
    /// Whether to look for fields a style hides (development runs).
    pub check_hidden: bool,
}

impl Inputs<'_> {
    /// The theme of a root whose `UiRoot.theme` is `name`.
    fn theme_of(&self, name: &str) -> &Theme {
        self.root_themes.get(name).unwrap_or(self.theme)
    }
}

/// Builds the frame's tree.
pub fn build(world: &mut World, inputs: &Inputs<'_>, findings: &mut Findings) -> Built {
    let mut roots: Vec<(Entity, UiRoot)> = world
        .ecs
        .query::<(Entity, &UiRoot)>()
        .iter(&world.ecs)
        .map(|(e, r)| (e, r.clone()))
        .collect();
    let world = &*world;
    let mut b = Builder {
        world,
        inputs,
        findings,
        built: Built::default(),
    };
    // Screen roots by order, then stable id; world roots are sorted by
    // distance when laid out.
    let id = |e: Entity| {
        world
            .ecs
            .get::<StableId>(e)
            .map_or_else(|| format!("~{:020}", e.to_bits()), |s| s.0.clone())
    };
    roots.sort_by_cached_key(|(e, r)| (r.space != UiSpace::Screen, r.order, id(*e)));
    for (e, r) in roots {
        b.root(e, &r);
    }
    b.built
}

struct Builder<'a, 'b> {
    world: &'a World,
    inputs: &'a Inputs<'b>,
    findings: &'a mut Findings,
    built: Built,
}

impl Builder<'_, '_> {
    fn push(&mut self, entity: Option<Entity>, node: Node) -> NodeId {
        self.built.entities.push(entity);
        self.built.tree.add(node)
    }

    fn root(&mut self, e: Entity, r: &UiRoot) {
        let ecs = &self.world.ecs;
        let own = ecs.get::<UiNode>(e).cloned().unwrap_or_default();
        if !own.visible {
            return;
        }
        let theme = self.inputs.theme_of(&r.theme);
        let mut node = self.node(e, &own, theme);
        // The root's own node fills the root.
        node.position = Dim2::ZERO;
        node.size = Dim2::scale(1.0, 1.0);
        node.anchor = Vec2::ZERO;
        let own_id = self.push(Some(e), node);
        let space = match r.space {
            UiSpace::Screen => Space::Screen {
                reference_height: r.reference_height,
            },
            UiSpace::World => {
                let at = ecs
                    .get::<GlobalTransform>(e)
                    .map_or(Vec3::ZERO, |g| Vec3::from(g.0.translation));
                Space::World {
                    at: at + r.offset - self.inputs.origin,
                    canvas: Vec2::new(own.width, own.height),
                    height: r.world_height,
                    max_distance: r.max_distance,
                }
            }
        };
        let container = self.push(
            None,
            Node {
                children: vec![own_id],
                ..Node::default()
            },
        );
        self.built.roots.push(HostRoot {
            entity: e,
            node: container,
            space,
            input: r.input,
            focus: r.focus,
        });
    }

    /// The node of entity `e` and its subtree.
    fn node(&mut self, e: Entity, n: &UiNode, theme: &Theme) -> Node {
        let ecs = &self.world.ecs;
        let style = self.style(e, n, theme);
        let button = ecs.get::<UiButton>(e);
        let state = State {
            disabled: button.is_some_and(|b| !b.enabled),
            ..self.inputs.states.get(&e).copied().unwrap_or_default()
        };
        let patch = style.as_ref().map(|s| s.at(state)).unwrap_or_default();
        if let Some(s) = &style
            && self.inputs.check_hidden
        {
            self.hidden(e, &s.look);
        }
        let panel = ecs.get::<UiPanel>(e);
        let look = if panel.is_some() || !patch.panel.is_empty() {
            let auto_color = button.is_some_and(|b| b.auto_color)
                && style.as_ref().is_none_or(|s| {
                    s.hover.panel.color.is_none() && s.pressed.panel.color.is_none()
                });
            look(
                &panel.cloned().unwrap_or_default(),
                &patch.panel,
                auto_color,
            )
        } else {
            Look::default()
        };
        let text = ecs.get::<UiText>(e).map(|t| {
            let input = ecs.get::<UiTextInput>(e);
            text_style(t, &patch.text, input)
        });
        let image = ecs.get::<UiImage>(e).and_then(|i| {
            let texture = *self.inputs.images.get(&i.image)?;
            Some(Image::Texture {
                texture,
                tint: Rgba::from_linear(color(None, i.tint)),
            })
        });
        let bar = ecs.get::<UiBar>(e).map(|b| Bar {
            value: b.value,
            direction: match patch.bar.direction {
                Some(d) => d,
                None => bar_direction(b.direction),
            },
            color: Rgba::from_linear(color(patch.bar.color, b.color)),
        });
        let scroll = ecs.get::<UiScroll>(e).map(|s| Scroll {
            canvas: Dim2::px(s.content_width.max(0.0), s.content_height.max(0.0)),
            automatic: Axes {
                x: s.content_width <= 0.0,
                y: s.content_height <= 0.0,
            },
            position: Vec2::new(s.scroll_x, s.scroll_y),
            bar: patch.scroll.bar.unwrap_or(s.bar),
            bar_color: Rgba::from_linear(color(patch.scroll.bar_color, s.bar_color)),
        });
        let kids: Vec<Entity> = children(ecs, e)
            .iter()
            .copied()
            .filter(|&c| ecs.get::<UiNode>(c).is_some() && ecs.get::<UiRoot>(c).is_none())
            .collect();
        let mut child_ids = Vec::with_capacity(kids.len());
        for c in kids {
            let cn = ecs.get::<UiNode>(c).cloned().unwrap_or_default();
            let node = self.node(c, &cn, theme);
            child_ids.push(self.push(Some(c), node));
        }
        Node {
            key: e.to_bits(),
            name: ecs.get::<Name>(e).map(|n| n.0.clone()).unwrap_or_default(),
            children: child_ids,
            visible: n.visible,
            interactable: true,
            position: Dim2::new(Dim::new(n.x_scale, n.x), Dim::new(n.y_scale, n.y)),
            size: Dim2::new(
                Dim::new(n.width_scale, n.width),
                Dim::new(n.height_scale, n.height),
            ),
            anchor: Vec2::new(n.anchor_x, n.anchor_y),
            automatic_size: match n.auto_size {
                UiAutoSize::None => Axes::NONE,
                UiAutoSize::Width => Axes::X,
                UiAutoSize::Height => Axes::Y,
                UiAutoSize::Both => Axes::BOTH,
            },
            rotation: n.rotation_deg,
            z: i64::from(n.z),
            order: i64::from(n.order),
            clips: n.clip,
            padding: [
                n.padding_left,
                n.padding_top,
                n.padding_right,
                n.padding_bottom,
            ]
            .iter()
            .any(|p| *p != 0.0)
            .then(|| Padding {
                left: Dim::px(n.padding_left),
                top: Dim::px(n.padding_top),
                right: Dim::px(n.padding_right),
                bottom: Dim::px(n.padding_bottom),
            }),
            scale: (n.scale != 1.0).then_some(n.scale),
            limits: [n.min_width, n.min_height, n.max_width, n.max_height]
                .iter()
                .any(|l| *l > 0.0)
                .then(|| {
                    let max = |m: f32| if m > 0.0 { m } else { f32::INFINITY };
                    SizeLimits {
                        min: Vec2::new(n.min_width.max(0.0), n.min_height.max(0.0)),
                        max: Vec2::new(max(n.max_width), max(n.max_height)),
                    }
                }),
            aspect: (n.aspect > 0.0).then_some(Aspect::Fit { ratio: n.aspect }),
            arrange: arrange(n),
            scroll,
            text,
            image,
            look,
            bar,
            opacity: n.opacity,
        }
    }

    /// The style a node names, reporting a name the theme lacks.
    fn style(&mut self, e: Entity, n: &UiNode, theme: &Theme) -> Option<Style> {
        if n.style.is_empty() {
            return None;
        }
        let style = theme.style(&n.style).cloned();
        if style.is_none() {
            self.findings.missing.push((e, n.style.clone()));
        }
        style
    }

    /// The fields of `e`'s components that differ from their defaults while
    /// `look` sets them.
    fn hidden(&mut self, e: Entity, look: &Patch) {
        let reg = &self.world.registry;
        for (component, field) in look.set() {
            let Ok(key) = reg.key(component) else {
                continue;
            };
            let schema = reg.schema(key);
            let Some(i) = schema.fields.iter().position(|f| f.name == field) else {
                continue;
            };
            if reg
                .get_field(&self.world.ecs, e, key, i)
                .is_some_and(|v| v != schema.fields[i].default)
            {
                self.findings.hidden.push((e, component, field));
            }
        }
    }
}

/// A style's colour, else the component's.
fn color(patch: Option<theme::Color>, own: Color) -> [f32; 4] {
    patch.map_or([own.r, own.g, own.b, own.a], |c| c.0)
}

fn align(a: UiAlign) -> f32 {
    match a {
        UiAlign::Start => 0.0,
        UiAlign::Center => 0.5,
        UiAlign::End => 1.0,
    }
}

fn theme_align(a: theme::Align) -> f32 {
    match a {
        theme::Align::Start => 0.0,
        theme::Align::Center => 0.5,
        theme::Align::End => 1.0,
    }
}

fn bar_direction(d: UiBarDirection) -> BarDirection {
    match d {
        UiBarDirection::Right => BarDirection::Right,
        UiBarDirection::Left => BarDirection::Left,
        UiBarDirection::Up => BarDirection::Up,
        UiBarDirection::Down => BarDirection::Down,
    }
}

fn family(f: UiFont) -> Family {
    match f {
        UiFont::Sans => Family::SourceSans3,
        UiFont::Display => Family::Montserrat,
        UiFont::Mono => Family::RobotoMono,
    }
}

fn theme_family(f: theme::Font) -> Family {
    match f {
        theme::Font::Sans => Family::SourceSans3,
        theme::Font::Display => Family::Montserrat,
        theme::Font::Mono => Family::RobotoMono,
    }
}

fn arrange(n: &UiNode) -> Option<Arrange> {
    let align = Vec2::new(align(n.align_x), align(n.align_y));
    match n.layout {
        UiLayout::None => None,
        UiLayout::Row | UiLayout::Column => Some(Arrange::List(List {
            horizontal: n.layout == UiLayout::Row,
            align,
            sort: Sort::Order,
            gap: Dim::px(n.gap),
        })),
        UiLayout::Grid => Some(Arrange::Grid(Grid {
            horizontal: true,
            align,
            sort: Sort::Order,
            cell: Dim2::px(n.cell_width, n.cell_height),
            gap: Dim2::px(n.gap, n.gap),
            max_cells: i64::from(n.max_cells.max(0)),
            start: Corner::TopLeft,
            cell_aspect: None,
        })),
    }
}

/// A panel's look: its fields with the style's over them.
fn look(p: &UiPanel, s: &theme::PanelPatch, auto_color: bool) -> Look {
    let fill = color(s.color, p.color);
    let corner = s.corner.unwrap_or(p.corner);
    let outline = s.outline.unwrap_or(p.outline);
    let outline_color = color(s.outline_color, p.outline_color);
    let far = color(s.gradient, p.gradient);
    let mut look = Look {
        background: Rgba::from_linear(fill),
        corner: (corner > 0.0).then(|| Dim::px(corner)),
        auto_color,
        ..Look::default()
    };
    if outline > 0.0 && outline_color[3] > 0.0 {
        look.strokes.push(Stroke {
            mode: StrokeMode::Border,
            thickness: outline,
            color: Rgba::from_linear(outline_color),
        });
    }
    if far[3] > 0.0 {
        // The core's gradient multiplies the fill by its keys: a white fill,
        // and the two colours as the keys.
        let (a, b) = (Rgba::from_linear(fill).0, Rgba::from_linear(far).0);
        look.background = Rgba::WHITE;
        look.gradient = Some(Gradient {
            colors: vec![(0.0, [a[0], a[1], a[2]]), (1.0, [b[0], b[1], b[2]])],
            transparency: vec![(0.0, [1.0 - a[3]; 3]), (1.0, [1.0 - b[3]; 3])],
            offset: Vec2::ZERO,
            rotation: s.gradient_angle_deg.unwrap_or(p.gradient_angle_deg),
        });
    }
    look
}

/// A text's style: its fields with the style's over them. A text input with
/// no text shows its placeholder, faded.
fn text_style(t: &UiText, s: &theme::TextPatch, input: Option<&UiTextInput>) -> TextStyle {
    let placeholder = input.filter(|i| t.text.is_empty() && !i.placeholder.is_empty());
    let mut c = color(s.color, t.color);
    if placeholder.is_some() {
        c[3] *= 0.5;
    }
    let outline = color(s.outline_color, t.outline_color);
    TextStyle {
        text: placeholder.map_or_else(|| t.text.clone(), |i| i.placeholder.clone()),
        face: Face {
            family: s.font.map_or_else(|| family(t.font), theme_family),
            weight: s.weight.unwrap_or(t.weight).clamp(1, 1000) as u16,
            italic: false,
        },
        size: s.size.unwrap_or(t.size),
        color: Rgba::from_linear(c),
        scaled: s.fit.unwrap_or(t.fit),
        wrapped: s.wrap.unwrap_or(t.wrap),
        truncate: s.truncate.unwrap_or(t.truncate),
        rich: t.rich && placeholder.is_none(),
        line_height: s.line_height.unwrap_or(t.line_height),
        align: Vec2::new(
            s.align_x.map_or_else(|| align(t.align_x), theme_align),
            s.align_y.map_or_else(|| align(t.align_y), theme_align),
        ),
        placeholder: false,
        stroke: (outline[3] > 0.0).then(|| Rgba::from_linear(outline)),
    }
}
