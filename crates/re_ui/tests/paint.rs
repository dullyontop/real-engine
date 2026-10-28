//! What the painter draws for a node's bar and opacity: the shapes, read
//! back from the painter.

mod common;

use egui::epaint::{Color32, Shape};
use glam::Vec2;
use re_ui::{
    Bar, BarDirection, Dim2, Item, Look, Looks, Node, NodeId, Painter, Rgba, Root, TextStyle,
    UiTree,
};

const VIEW: Vec2 = Vec2::new(200.0, 100.0);

fn screen(node: NodeId) -> Root {
    Root {
        node,
        origin: Vec2::ZERO,
        abs_origin: Vec2::ZERO,
        size: VIEW,
        global_z: false,
        clip: None,
        input: true,
    }
}

/// The items of `root` and the shapes they paint, back to front.
fn paint(tree: &UiTree, root: NodeId) -> (Vec<Item>, Vec<Shape>) {
    common::in_pass(1.0, VIEW, |ctx| {
        let (mut items, _) = common::lay_out(ctx, tree, &[screen(root)]);
        let items = items.remove(0);
        let mut painter = Painter::new(
            tree,
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(VIEW.x, VIEW.y)),
        );
        for it in &items {
            painter.item(it, &Looks::default());
        }
        (
            items,
            painter.finish().into_iter().map(|c| c.shape).collect(),
        )
    })
}

fn filled(color: Rgba) -> Look {
    Look {
        background: color,
        ..Look::default()
    }
}

fn rects(shapes: &[Shape]) -> Vec<(egui::Rect, Color32)> {
    shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Rect(r) => Some((r.rect, r.fill)),
            _ => None,
        })
        .collect()
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> egui::Rect {
    egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1))
}

/// A bar fills its share of the box from the edge its direction names, over
/// the background; a value outside 0 to 1 is clamped and not a number fills
/// nothing.
#[test]
fn bars_fill_from_their_edge() {
    let red = Rgba([1.0, 0.0, 0.0, 1.0]);
    let cases = [
        (
            BarDirection::Right,
            0.25,
            Some(rect(10.0, 20.0, 35.0, 60.0)),
        ),
        (
            BarDirection::Left,
            0.25,
            Some(rect(85.0, 20.0, 110.0, 60.0)),
        ),
        (BarDirection::Up, 0.25, Some(rect(10.0, 50.0, 110.0, 60.0))),
        (
            BarDirection::Down,
            0.25,
            Some(rect(10.0, 20.0, 110.0, 30.0)),
        ),
        (
            BarDirection::Right,
            2.0,
            Some(rect(10.0, 20.0, 110.0, 60.0)),
        ),
        (BarDirection::Right, -1.0, None),
        (BarDirection::Right, f32::NAN, None),
    ];
    for (direction, value, want) in cases {
        let mut t = UiTree::new();
        let bar = t.add(Node {
            position: Dim2::px(10.0, 20.0),
            size: Dim2::px(100.0, 40.0),
            look: filled(Rgba::BLACK),
            bar: Some(Bar {
                value,
                direction,
                color: red,
            }),
            ..Node::default()
        });
        let root = t.add(Node {
            children: vec![bar],
            ..Node::default()
        });
        let (_, shapes) = paint(&t, root);
        let mut got = rects(&shapes);
        assert_eq!(
            got.remove(0),
            (rect(10.0, 20.0, 110.0, 60.0), Rgba::BLACK.egui()),
            "the background first"
        );
        assert_eq!(
            got.first().copied(),
            want.map(|r| (r, red.egui())),
            "{direction:?} at {value}"
        );
    }
}

/// Opacity multiplies down the tree and fades every shape an item paints
/// (one at 0 paints nothing); it is clamped to 0 to 1, and not a number
/// counts as 1.
#[test]
fn opacity_multiplies_down_the_tree() {
    let mut t = UiTree::new();
    let white = filled(Rgba::WHITE);
    let label = t.add(Node {
        size: Dim2::px(80.0, 20.0),
        opacity: 0.5,
        text: Some(TextStyle {
            text: "faded".into(),
            size: 14.0,
            ..TextStyle::default()
        }),
        ..Node::default()
    });
    let odd = [2.0, -1.0, f32::NAN].map(|opacity| {
        t.add(Node {
            size: Dim2::px(10.0, 10.0),
            opacity,
            look: white.clone(),
            ..Node::default()
        })
    });
    let panel = t.add(Node {
        size: Dim2::px(100.0, 50.0),
        opacity: 0.5,
        look: white.clone(),
        children: [vec![label], odd.to_vec()].concat(),
        ..Node::default()
    });
    let root = t.add(Node {
        children: vec![panel],
        ..Node::default()
    });
    let (items, shapes) = paint(&t, root);
    let opacity: Vec<f32> = items.iter().map(|it| it.opacity).collect();
    // The panel, its label, then the three odd values under the panel's half.
    assert_eq!(opacity, [0.5, 0.25, 0.5, 0.0, 0.5]);
    let fills: Vec<Color32> = rects(&shapes).into_iter().map(|(_, c)| c).collect();
    let white = Rgba::WHITE.egui();
    assert_eq!(
        fills,
        [
            white.gamma_multiply(0.5),
            white.gamma_multiply(0.5),
            white.gamma_multiply(0.5)
        ]
    );
    let text: Vec<f32> = shapes
        .iter()
        .filter_map(|s| match s {
            Shape::Text(t) => Some(t.opacity_factor),
            _ => None,
        })
        .collect();
    assert_eq!(text, [0.25]);
}
