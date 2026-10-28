//! Layout: from a [`UiTree`]'s roots to screen rectangles.
//!
//! Each root (a screen, a billboard placed by a camera, or a surface's
//! canvas) lays out its descendants top-down. A node's size is its
//! [`Node::size`] against the parent's content box (the parent's size less
//! its padding), limited by its [`Node::aspect`] and [`Node::limits`], then
//! grown by [`Node::automatic_size`] to fit its children and text. Its
//! position is its [`Node::position`] less [`Node::anchor`] of its size,
//! unless the parent's [`Arrange`] places it. [`Node::scale`] scales a node
//! and its descendants about the node's anchor point; [`Node::rotation`]
//! turns the drawing (and descendants) about the node's centre but not the
//! layout. A node with [`Node::scroll`] places its children on its canvas.
//!
//! The result is a list of [`Item`]s in drawing order, and a [`Visit`] per
//! node with the values a host reports back, in the order the nodes were
//! placed.

use std::sync::Arc;

use egui::Galley;
use glam::{Affine2, Vec2};

use crate::text::{self, Laid, Sizing};
use crate::tree::{Arrange, Corner, Grid, NodeId, Sort, UiTree};

/// A placed root: what a frame lays out.
#[derive(Clone, Copy, Debug)]
pub struct Root {
    pub node: NodeId,
    /// The screen point of the root's top-left corner.
    pub origin: Vec2,
    /// The screen point that reported positions are measured from.
    pub abs_origin: Vec2,
    pub size: Vec2,
    /// Children draw in [`Node::z`](crate::Node::z) order across the whole
    /// root, not among their siblings.
    pub global_z: bool,
    pub clip: Option<egui::Rect>,
    /// False when nothing under the root takes input.
    pub input: bool,
}

/// A drawn node's laid-out text.
#[derive(Clone, Debug)]
pub struct LaidText {
    pub galley: Arc<Galley>,
    /// False when the text did not fit its box.
    pub fits: bool,
    /// The size in pixels, the node's scale applied.
    pub size: f32,
    /// The box the text was laid out in: the node less its padding.
    pub inner: Vec2,
}

/// One drawn node.
#[derive(Clone, Debug)]
pub struct Item {
    pub node: NodeId,
    /// The node's rectangle in screen pixels, before rotation.
    pub min: Vec2,
    pub size: Vec2,
    /// Maps unrotated screen points to drawn ones.
    pub rot: Affine2,
    /// The total rotation in degrees.
    pub angle: f32,
    /// The clip rectangle (none while rotated).
    pub clip: Option<egui::Rect>,
    /// The scale that applies.
    pub scale: f32,
    /// The padding: left, top, right, bottom (screen pixels).
    pub pad: [f32; 4],
    pub text: Option<LaidText>,
    /// A scrolling node's canvas size in screen pixels.
    pub canvas: Option<Vec2>,
    /// Drawing order.
    pub key: (i64, u64),
    /// Whether it takes input.
    pub interactable: bool,
}

impl Item {
    /// True if screen point `p` is inside the drawn item.
    pub fn contains(&self, p: Vec2) -> bool {
        if self.clip.is_some_and(|c| !c.contains(egui::pos2(p.x, p.y))) {
            return false;
        }
        let q = self.rot.inverse().transform_point2(p);
        q.cmpge(self.min).all() && q.cmple(self.min + self.size).all()
    }

    /// The drawn centre.
    pub fn center(&self) -> Vec2 {
        self.rot.transform_point2(self.min + self.size * 0.5)
    }
}

/// What a frame found for one node, drawn or not, for the host to report.
#[derive(Clone, Debug, PartialEq)]
pub struct Visit {
    pub node: NodeId,
    /// A root of the frame (its values are its placement).
    pub root: bool,
    /// Its scaled top-left corner, from the root's
    /// [`abs_origin`](Root::abs_origin).
    pub position: Vec2,
    /// Its scaled size.
    pub size: Vec2,
    /// Its total rotation in degrees.
    pub rotation: f32,
    /// Its text's size before scaling, and whether the text fit.
    pub text: Option<(Vec2, bool)>,
    pub scroll: Option<ScrollVisit>,
    /// What its arrangement placed.
    pub arranged: Option<Arranged>,
}

/// A scrolling node's canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollVisit {
    /// The position held to the canvas, when the host's was outside it.
    pub clamped: Option<Vec2>,
    /// The canvas size, scaled.
    pub canvas: Vec2,
    /// The node's scaled size.
    pub window: Vec2,
}

/// What an arrangement placed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Arranged {
    /// The size of what it placed.
    pub content: Vec2,
    /// A grid's cell size, and its columns and rows.
    pub cells: Option<(Vec2, Vec2)>,
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

fn rotate_about(center: Vec2, degrees: f32) -> Affine2 {
    Affine2::from_translation(center)
        * Affine2::from_angle(degrees.to_radians())
        * Affine2::from_translation(-center)
}

/// How a list or grid places its children.
struct Placed {
    /// Offsets from the content origin, by child index.
    at: Vec<Option<Vec2>>,
    content: Vec2,
    cells: Option<(Vec2, Vec2)>,
}

/// A child as the layout sees it, with its size.
struct Kid {
    id: NodeId,
    size: Vec2,
    visible: bool,
}

/// Lays roots out; one per frame.
pub struct Layout<'a> {
    tree: &'a UiTree,
    ctx: &'a egui::Context,
    items: Vec<Item>,
    visits: Vec<Visit>,
    seq: u64,
}

impl<'a> Layout<'a> {
    /// A layout of `tree`'s roots, with text laid out in `ctx`'s fonts.
    pub fn new(tree: &'a UiTree, ctx: &'a egui::Context) -> Layout<'a> {
        Layout {
            tree,
            ctx,
            items: Vec::new(),
            visits: Vec::new(),
            seq: 0,
        }
    }

    /// Lays out one root and gives its drawn items in drawing order.
    pub fn root(&mut self, r: &Root) -> Vec<Item> {
        let size = r.size;
        let at = self.visits.len();
        self.visits.push(Visit {
            node: r.node,
            root: true,
            position: r.origin - r.abs_origin,
            size,
            rotation: 0.0,
            text: None,
            scroll: None,
            arranged: None,
        });
        self.children(
            r.node,
            Some(at),
            (Vec2::ZERO, size),
            // A scale on the root scales everything about its corner.
            Sc {
                k: self.tree.scale_of(r.node),
                o: r.origin,
            },
            Affine2::IDENTITY,
            0.0,
            r.clip,
            true,
            r,
        );
        let mut items = std::mem::take(&mut self.items);
        items.sort_by_key(|it| it.key);
        items
    }

    /// The visits of every root laid out so far, in order.
    pub fn take_visits(&mut self) -> Vec<Visit> {
        std::mem::take(&mut self.visits)
    }

    /// The size of a child before its automatic size, after its limits.
    fn constrained(&self, id: NodeId, size: Vec2) -> Vec2 {
        let n = self.tree.node(id);
        let size = match n.aspect {
            Some(a) => a.apply(size),
            None => size,
        };
        self.size_limits(id, size)
    }

    fn size_limits(&self, id: NodeId, size: Vec2) -> Vec2 {
        match self.tree.node(id).limits {
            Some(l) => size.max(l.min).min(l.max.max(l.min)),
            None => size,
        }
    }

    /// A grid's cell size in `area`.
    fn cell_size(g: &Grid, area: Vec2) -> Vec2 {
        let cell = g.cell.of(area);
        match g.cell_aspect {
            Some(a) => a.apply(cell),
            None => cell,
        }
    }

    fn padding(&self, id: NodeId, size: Vec2) -> [f32; 4] {
        self.tree.node(id).padding.map_or([0.0; 4], |p| p.of(size))
    }

    /// Places a list's or grid's children.
    fn arrange(&self, arrange: &Arrange, kids: &mut [Kid], content: Vec2) -> Placed {
        let (horizontal, align, sort) = match arrange {
            Arrange::List(l) => (l.horizontal, l.align, l.sort),
            Arrange::Grid(g) => (g.horizontal, g.align, g.sort),
        };
        let (ha, va) = (align.x, align.y);
        let mut order: Vec<usize> = (0..kids.len()).filter(|&i| kids[i].visible).collect();
        match sort {
            Sort::Name => order.sort_by_cached_key(|&i| self.tree.node(kids[i].id).name.clone()),
            Sort::Order => order.sort_by_cached_key(|&i| self.tree.node(kids[i].id).order),
        }
        let mut at = vec![None; kids.len()];
        let gap = match arrange {
            Arrange::Grid(g) => {
                let cell = Self::cell_size(g, content);
                let pad = g.gap.of(content);
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
                let max = g.max_cells;
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
                let start = (content - block) * Vec2::new(ha, va);
                for (slot, &i) in order.iter().enumerate() {
                    let (mut cx, mut cy) = if horizontal {
                        (slot % per, slot / per)
                    } else {
                        (slot / per, slot % per)
                    };
                    if matches!(g.start, Corner::TopRight | Corner::BottomRight) {
                        cx = nx.saturating_sub(1) - cx;
                    }
                    if matches!(g.start, Corner::BottomLeft | Corner::BottomRight) {
                        cy = ny.saturating_sub(1) - cy;
                    }
                    kids[i].size = cell;
                    at[i] = Some(start + Vec2::new(cx as f32, cy as f32) * (cell + pad));
                }
                return Placed {
                    at,
                    content: block,
                    cells: Some((cell, Vec2::new(nx as f32, ny as f32))),
                };
            }
            Arrange::List(l) => l.gap.of(if horizontal { content.x } else { content.y }),
        };
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
        Placed {
            at,
            content: size,
            cells: None,
        }
    }

    /// Lays out the children of `parent` in the content box `content`
    /// (local pixels), recording the parent's arrangement in its visit
    /// `visit`. Returns the extent of what was placed, from the content
    /// origin, for automatic sizes. Without a visit it measures only.
    #[allow(clippy::too_many_arguments)]
    fn children(
        &mut self,
        parent: NodeId,
        visit: Option<usize>,
        content: (Vec2, Vec2),
        sc: Sc,
        rot: Affine2,
        angle: f32,
        clip: Option<egui::Rect>,
        shown: bool,
        root: &Root,
    ) -> Vec2 {
        let dry = visit.is_none();
        let (origin, area) = content;
        let tree = self.tree;
        let p = tree.node(parent);
        let mut kids: Vec<Kid> = p
            .children
            .iter()
            .map(|&c| {
                let n = tree.node(c);
                Kid {
                    id: c,
                    size: n.size.of(area),
                    visible: n.visible,
                }
            })
            .collect();
        // Grid cells replace sizes before limits and automatic sizes.
        if let Some(Arrange::Grid(g)) = &p.arrange {
            let cell = Self::cell_size(g, area);
            for k in &mut kids {
                k.size = cell;
            }
        }
        for k in &mut kids {
            k.size = self.constrained(k.id, k.size);
            let auto = tree.node(k.id).automatic_size;
            if auto.any() {
                let fit = self.measure(k.id, k.size, sc.k, root, (auto.x, auto.y));
                if auto.x {
                    k.size.x = k.size.x.max(fit.x);
                }
                if auto.y {
                    k.size.y = k.size.y.max(fit.y);
                }
                k.size = self.size_limits(k.id, k.size);
            }
        }
        let arranged = p.arrange.as_ref().map(|a| self.arrange(a, &mut kids, area));
        if let (Some(a), Some(v)) = (&arranged, visit) {
            self.visits[v].arranged = Some(Arranged {
                content: a.content,
                cells: a.cells,
            });
        }
        let mut extent = arranged.as_ref().map_or(Vec2::ZERO, |a| a.content);
        // Drawing order among siblings.
        let mut idx: Vec<usize> = (0..kids.len()).collect();
        if !root.global_z {
            idx.sort_by_cached_key(|&i| tree.node(kids[i].id).z);
        }
        for i in idx {
            let k = &kids[i];
            let n = tree.node(k.id);
            let size = k.size;
            let anchor = n.anchor;
            let pos = match arranged.as_ref().and_then(|a| a.at[i]) {
                Some(at) => origin + at,
                None => origin + n.position.of(area) - anchor * size,
            };
            if k.visible || arranged.is_none() {
                extent = extent.max(pos + size - origin);
            }
            if dry {
                continue;
            }
            // The scale scales the node about its anchor point.
            let s = tree.scale_of(k.id);
            let sc_k = sc.scaled_about(s, pos + anchor * size);
            let min = sc_k.map(pos);
            let ssize = size * sc_k.k;
            let r = n.rotation;
            let rot_k = if r != 0.0 {
                rot * rotate_about(min + ssize * 0.5, r)
            } else {
                rot
            };
            let angle_k = angle + r;
            let rotated = angle_k.rem_euclid(360.0) != 0.0;
            let me = self.visits.len();
            self.visits.push(Visit {
                node: k.id,
                root: false,
                position: min - root.abs_origin,
                size: ssize,
                rotation: angle_k,
                text: None,
                scroll: None,
                arranged: None,
            });
            let visible = shown && k.visible;
            let pad = self.padding(k.id, size);
            let spad = pad.map(|x| x * sc_k.k);
            let text = n.text.as_ref().map(|style| {
                let sizing = Sizing::of(style, sc_k.k);
                let inner =
                    (ssize - Vec2::new(spad[0] + spad[2], spad[1] + spad[3])).max(Vec2::ZERO);
                let laid = text::lay_out(self.ctx, style, sizing, [Some(inner.x), Some(inner.y)]);
                let b = text::bounds(&laid);
                self.visits[me].text = Some((b / sc_k.k.max(1e-6), laid.fits));
                LaidText {
                    galley: laid.galley,
                    fits: laid.fits,
                    size: sizing.size,
                    inner,
                }
            });
            let screen =
                egui::Rect::from_min_size(egui::pos2(min.x, min.y), egui::vec2(ssize.x, ssize.y));
            let clips = n.clips || n.scroll.is_some();
            let child_clip = if clips && !rotated {
                Some(clip.map_or(screen, |c| c.intersect(screen)))
            } else {
                clip
            };
            self.seq += 1;
            let z = if root.global_z { n.z } else { 0 };
            let interactable = visible && root.input && n.interactable;
            let slot = visible.then_some(self.items.len());
            if visible {
                self.items.push(Item {
                    node: k.id,
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
            if let Some(scroll) = &n.scroll {
                // The canvas is never smaller than the window.
                let mut canvas = scroll.canvas.of(size).max(size);
                let auto = scroll.automatic;
                if auto.any() {
                    let fit = self.measure_children(k.id, (inner_origin, inner), sc_k, root);
                    if auto.x {
                        canvas.x = canvas.x.max(fit.x + pad[0] + pad[2]);
                    }
                    if auto.y {
                        canvas.y = canvas.y.max(fit.y + pad[1] + pad[3]);
                    }
                }
                let max_scroll = (canvas - size).max(Vec2::ZERO);
                let at = scroll.position;
                let clamped = at.clamp(Vec2::ZERO, max_scroll);
                self.visits[me].scroll = Some(ScrollVisit {
                    clamped: (clamped != at).then_some(clamped),
                    canvas: canvas * sc_k.k,
                    window: ssize,
                });
                if let Some(i) = slot {
                    self.items[i].canvas = Some(canvas * sc_k.k);
                }
                let corigin = pos - clamped + Vec2::new(pad[0], pad[1]);
                let carea = (canvas - Vec2::new(pad[0] + pad[2], pad[1] + pad[3])).max(Vec2::ZERO);
                self.children(
                    k.id,
                    Some(me),
                    (corigin, carea),
                    sc_k,
                    rot_k,
                    angle_k,
                    child_clip,
                    visible,
                    root,
                );
            } else {
                self.children(
                    k.id,
                    Some(me),
                    (inner_origin, inner),
                    sc_k,
                    rot_k,
                    angle_k,
                    child_clip,
                    visible,
                    root,
                );
            }
        }
        extent
    }

    fn measure_children(&mut self, id: NodeId, content: (Vec2, Vec2), sc: Sc, root: &Root) -> Vec2 {
        self.children(
            id,
            None,
            content,
            sc,
            Affine2::IDENTITY,
            0.0,
            None,
            false,
            root,
        )
    }

    /// The size an automatic size grows `id` to: its children's extent and
    /// its text, plus its padding (local pixels).
    fn measure(&mut self, id: NodeId, size: Vec2, k: f32, root: &Root, auto: (bool, bool)) -> Vec2 {
        let pad = self.padding(id, size);
        let inner = (size - Vec2::new(pad[0] + pad[2], pad[1] + pad[3])).max(Vec2::ZERO);
        let mut fit = self.measure_children(id, (Vec2::ZERO, inner), Sc { k, o: Vec2::ZERO }, root);
        if let Some(style) = &self.tree.node(id).text {
            let sizing = Sizing {
                size: style.size * 1.0,
                scaled: false,
                truncate: false,
            };
            let bounds = [(!auto.0).then_some(inner.x), (!auto.1).then_some(inner.y)];
            let laid: Laid = text::lay_out(self.ctx, style, sizing, bounds);
            fit = fit.max(text::bounds(&laid));
        }
        fit + Vec2::new(pad[0] + pad[2], pad[1] + pad[3])
    }
}
