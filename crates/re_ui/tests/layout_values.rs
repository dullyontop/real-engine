//! Layout values, exactly: small trees that each exercise a few of the
//! layout's rules, laid out and written out with every `f32` to the bit, and
//! compared with `tests/fixtures/layout/<case>.txt` (ADR-0058). Layout is f32
//! arithmetic with glam on libm and text is measured by egui over bundled
//! fonts, so the same values come out on every platform; a change too small
//! for an image golden's tolerance still shows here.

mod common;

use glam::Vec2;
use re_ui::{
    Arrange, Aspect, Axes, Axis, Corner, Dim, Dim2, Face, Family, Grid, List, Node, NodeId,
    Padding, Rgba, Root, Scroll, SizeLimits, Sort, TextStyle, UiTree,
};

const VIEW: Vec2 = Vec2::new(400.0, 300.0);

fn node(name: &str) -> Node {
    Node {
        name: name.into(),
        ..Node::default()
    }
}

fn text(s: &str, size: f32) -> TextStyle {
    TextStyle {
        text: s.into(),
        size,
        ..TextStyle::default()
    }
}

fn px(x: f32, y: f32) -> Dim2 {
    Dim2::px(x, y)
}

fn pad(px: f32) -> Option<Padding> {
    Some(Padding {
        left: Dim::px(px),
        top: Dim::px(px),
        right: Dim::px(px),
        bottom: Dim::px(px),
    })
}

fn screen(node: NodeId, size: Vec2) -> Root {
    Root {
        node,
        origin: Vec2::ZERO,
        abs_origin: Vec2::ZERO,
        size,
        global_z: false,
        clip: None,
        input: true,
    }
}

/// Lays `roots` out at `pixels_per_point` and compares every value with the
/// case's file.
fn check(case: &str, pixels_per_point: f32, tree: &UiTree, roots: &[Root]) {
    let out = common::in_pass(pixels_per_point, VIEW, |ctx| {
        let (items, visits) = common::lay_out(ctx, tree, roots);
        common::dump(tree, &items, &visits)
    });
    common::expect(case, &out);
}

#[test]
fn anchors_offsets_and_padding() {
    let mut t = UiTree::new();
    let half = t.add(Node {
        position: Dim2::scale(0.5, 0.5),
        anchor: Vec2::splat(0.5),
        size: Dim2::new(Dim::new(0.5, 10.0), Dim::new(0.25, 5.0)),
        ..node("half")
    });
    let corner = t.add(Node {
        position: Dim2::new(Dim::new(1.0, -10.0), Dim::px(20.0)),
        anchor: Vec2::new(1.0, 0.0),
        size: px(50.0, 40.0),
        ..node("corner")
    });
    let under = t.add(Node {
        size: px(5.0, 5.0),
        ..node("under hidden")
    });
    let hidden = t.add(Node {
        size: px(10.0, 10.0),
        visible: false,
        children: vec![under],
        ..node("hidden")
    });
    let fill = t.add(Node {
        size: Dim2::scale(1.0, 1.0),
        ..node("fill")
    });
    let inset = t.add(Node {
        position: Dim2::scale(1.0, 1.0),
        anchor: Vec2::ONE,
        size: px(10.0, 10.0),
        ..node("inset")
    });
    let padded = t.add(Node {
        position: px(20.0, 200.0),
        size: px(120.0, 80.0),
        padding: Some(Padding {
            left: Dim::new(0.1, 0.0),
            top: Dim::px(4.0),
            right: Dim::px(6.0),
            bottom: Dim::new(0.25, 0.0),
        }),
        children: vec![fill, inset],
        ..node("padded")
    });
    let inert = t.add(Node {
        position: px(300.0, 250.0),
        size: px(30.0, 30.0),
        interactable: false,
        ..node("inert")
    });
    let root = t.add(Node {
        children: vec![half, corner, hidden, padded, inert],
        ..node("screen")
    });
    // A second root placed away from the screen's corner, reporting
    // positions from a point above it (a top inset).
    let kid = t.add(Node {
        position: px(10.0, 10.0),
        size: Dim2::new(Dim::new(0.5, 0.0), Dim::px(20.0)),
        ..node("offset kid")
    });
    let offset = t.add(Node {
        children: vec![kid],
        ..node("offset root")
    });
    let roots = [
        screen(root, VIEW),
        Root {
            origin: Vec2::new(30.0, 76.0),
            abs_origin: Vec2::new(0.0, 36.0),
            input: false,
            ..screen(offset, Vec2::new(100.0, 50.0))
        },
    ];
    check("anchors_offsets_and_padding", 1.0, &t, &roots);
}

#[test]
fn aspect_ratios_and_size_limits() {
    let mut t = UiTree::new();
    let mut kids = Vec::new();
    let mut add = |n: Node| kids.push(t.add(n));
    add(Node {
        size: px(300.0, 300.0),
        aspect: Some(Aspect::Fit { ratio: 2.0 }),
        ..node("fit")
    });
    add(Node {
        size: px(100.0, 80.0),
        aspect: Some(Aspect::Follow {
            ratio: 0.5,
            dominant: Axis::Y,
        }),
        ..node("follow height")
    });
    add(Node {
        size: px(100.0, 80.0),
        aspect: Some(Aspect::Follow {
            ratio: 4.0,
            dominant: Axis::X,
        }),
        ..node("follow width")
    });
    add(Node {
        size: px(100.0, 80.0),
        aspect: Some(Aspect::Fit { ratio: 0.0 }),
        ..node("zero ratio")
    });
    add(Node {
        size: Dim2::scale(1.0, 1.0),
        limits: Some(SizeLimits {
            min: Vec2::ZERO,
            max: Vec2::new(120.0, 60.0),
        }),
        ..node("held under")
    });
    add(Node {
        size: px(10.0, 10.0),
        limits: Some(SizeLimits {
            min: Vec2::new(80.0, 80.0),
            max: Vec2::new(40.0, 40.0),
        }),
        ..node("least over most")
    });
    add(Node {
        size: px(200.0, 200.0),
        aspect: Some(Aspect::Fit { ratio: 1.5 }),
        limits: Some(SizeLimits {
            min: Vec2::ZERO,
            max: Vec2::new(150.0, 1000.0),
        }),
        ..node("ratio then limits")
    });
    let root = t.add(Node {
        children: kids,
        ..node("screen")
    });
    check(
        "aspect_ratios_and_size_limits",
        1.0,
        &t,
        &[screen(root, Vec2::new(300.0, 300.0))],
    );
}

#[test]
fn automatic_sizes_fit_text_and_children() {
    let mut t = UiTree::new();
    let label = t.add(Node {
        automatic_size: Axes::BOTH,
        text: Some(text("Automatic", 20.0)),
        ..node("label both")
    });
    let wrapped = t.add(Node {
        position: px(0.0, 40.0),
        size: px(120.0, 0.0),
        automatic_size: Axes::Y,
        text: Some(TextStyle {
            wrapped: true,
            ..text("A longer line that wraps inside its width.", 16.0)
        }),
        ..node("label wrapped")
    });
    let a = t.add(Node {
        position: px(30.0, 40.0),
        size: px(20.0, 10.0),
        ..node("a")
    });
    let holder = t.add(Node {
        position: px(200.0, 0.0),
        size: px(10.0, 10.0),
        automatic_size: Axes::BOTH,
        padding: pad(5.0),
        children: vec![a],
        ..node("holder")
    });
    let capped = t.add(Node {
        position: px(0.0, 160.0),
        automatic_size: Axes::X,
        size: px(0.0, 24.0),
        limits: Some(SizeLimits {
            min: Vec2::ZERO,
            max: Vec2::new(100.0, 50.0),
        }),
        text: Some(text("Wider than its largest size", 16.0)),
        ..node("capped")
    });
    let rows: Vec<NodeId> = (0..3)
        .map(|i| {
            t.add(Node {
                size: px(50.0, 20.0),
                order: i,
                ..node("row")
            })
        })
        .collect();
    let list = t.add(Node {
        position: px(300.0, 0.0),
        size: px(60.0, 0.0),
        automatic_size: Axes::Y,
        arrange: Some(Arrange::List(List {
            gap: Dim::px(4.0),
            ..List::default()
        })),
        children: rows,
        ..node("list")
    });
    let ghost = t.add(Node {
        position: px(70.0, 10.0),
        size: px(10.0, 10.0),
        visible: false,
        ..node("ghost")
    });
    let free = t.add(Node {
        position: px(200.0, 200.0),
        automatic_size: Axes::BOTH,
        children: vec![ghost],
        ..node("grows to a hidden child")
    });
    let root = t.add(Node {
        children: vec![label, wrapped, holder, capped, list, free],
        ..node("screen")
    });
    check(
        "automatic_sizes_fit_text_and_children",
        1.0,
        &t,
        &[screen(root, VIEW)],
    );
}

#[test]
fn lists_sort_align_and_skip_hidden_children() {
    let mut t = UiTree::new();
    let mut column = Vec::new();
    for (name, order, size, visible) in [
        ("third", 3, px(100.0, 30.0), true),
        ("first", 1, px(60.0, 20.0), true),
        ("second", 2, px(80.0, 40.0), true),
        ("hidden", 0, px(90.0, 90.0), false),
    ] {
        column.push(t.add(Node {
            size,
            order,
            visible,
            ..node(name)
        }));
    }
    let column = t.add(Node {
        size: px(180.0, 200.0),
        arrange: Some(Arrange::List(List {
            horizontal: false,
            align: Vec2::new(0.5, 1.0),
            sort: Sort::Order,
            gap: Dim::new(0.05, 0.0),
        })),
        children: column,
        ..node("column")
    });
    let mut row = Vec::new();
    for (name, size) in [
        ("b", px(30.0, 30.0)),
        ("c", Dim2::new(Dim::new(0.1, 0.0), Dim::new(0.5, 0.0))),
        ("a", px(50.0, 10.0)),
    ] {
        row.push(t.add(Node { size, ..node(name) }));
    }
    let row = t.add(Node {
        position: px(190.0, 0.0),
        size: px(200.0, 60.0),
        padding: pad(4.0),
        arrange: Some(Arrange::List(List {
            horizontal: true,
            align: Vec2::new(1.0, 0.5),
            sort: Sort::Name,
            gap: Dim::px(8.0),
        })),
        children: row,
        ..node("row by name")
    });
    let inner: Vec<NodeId> = (0..2)
        .map(|i| {
            t.add(Node {
                size: px(20.0, 20.0),
                order: i,
                ..node("cell")
            })
        })
        .collect();
    let nested = t.add(Node {
        size: px(0.0, 20.0),
        automatic_size: Axes::X,
        order: 1,
        arrange: Some(Arrange::List(List {
            horizontal: true,
            gap: Dim::px(2.0),
            ..List::default()
        })),
        children: inner,
        ..node("nested")
    });
    let tail = t.add(Node {
        size: px(30.0, 20.0),
        order: 2,
        ..node("tail")
    });
    let outer = t.add(Node {
        position: px(190.0, 100.0),
        size: px(200.0, 20.0),
        arrange: Some(Arrange::List(List {
            horizontal: true,
            ..List::default()
        })),
        children: vec![tail, nested],
        ..node("outer")
    });
    let root = t.add(Node {
        children: vec![column, row, outer],
        ..node("screen")
    });
    check(
        "lists_sort_align_and_skip_hidden_children",
        1.0,
        &t,
        &[screen(root, VIEW)],
    );
}

#[test]
fn grids_fill_lines_from_each_corner() {
    let mut t = UiTree::new();
    let cells = |t: &mut UiTree, n: usize| -> Vec<NodeId> {
        (0..n)
            .map(|i| {
                t.add(Node {
                    order: i as i64,
                    ..node(&format!("cell {i}"))
                })
            })
            .collect()
    };
    let kids = cells(&mut t, 7);
    let rows = t.add(Node {
        size: px(200.0, 120.0),
        arrange: Some(Arrange::Grid(Grid {
            horizontal: true,
            align: Vec2::splat(0.5),
            cell: px(40.0, 30.0),
            gap: px(5.0, 5.0),
            max_cells: 3,
            start: Corner::BottomRight,
            ..Grid::default()
        })),
        children: kids,
        ..node("rows from the bottom right")
    });
    let kids = cells(&mut t, 5);
    let columns = t.add(Node {
        position: px(200.0, 0.0),
        size: px(200.0, 150.0),
        arrange: Some(Arrange::Grid(Grid {
            horizontal: false,
            cell: Dim2::scale(0.25, 0.2),
            gap: px(2.0, 2.0),
            start: Corner::TopRight,
            cell_aspect: Some(Aspect::Fit { ratio: 1.0 }),
            ..Grid::default()
        })),
        children: kids,
        ..node("columns from the top right")
    });
    let mut named = Vec::new();
    for name in ["d", "b", "a", "c"] {
        named.push(t.add(node(name)));
    }
    let by_name = t.add(Node {
        position: px(0.0, 160.0),
        size: px(100.0, 100.0),
        arrange: Some(Arrange::Grid(Grid {
            horizontal: true,
            sort: Sort::Name,
            cell: px(30.0, 30.0),
            start: Corner::BottomLeft,
            ..Grid::default()
        })),
        children: named,
        ..node("by name from the bottom left")
    });
    let root = t.add(Node {
        children: vec![rows, columns, by_name],
        ..node("screen")
    });
    check(
        "grids_fill_lines_from_each_corner",
        1.0,
        &t,
        &[screen(root, VIEW)],
    );
}

#[test]
fn scale_rotation_and_drawing_order() {
    let mut t = UiTree::new();
    let kid = t.add(Node {
        position: px(10.0, 10.0),
        size: px(80.0, 20.0),
        text: Some(text("Scaled", 14.0)),
        ..node("scaled kid")
    });
    let scaled = t.add(Node {
        position: Dim2::scale(0.5, 0.5),
        anchor: Vec2::splat(0.5),
        size: px(200.0, 100.0),
        scale: Some(0.5),
        children: vec![kid],
        ..node("scaled")
    });
    let turned_kid = t.add(Node {
        size: px(20.0, 20.0),
        rotation: 15.0,
        ..node("turned kid")
    });
    let turned = t.add(Node {
        position: px(50.0, 200.0),
        size: px(80.0, 40.0),
        rotation: 30.0,
        clips: true,
        children: vec![turned_kid],
        ..node("turned")
    });
    let full_turn = t.add(Node {
        position: px(300.0, 200.0),
        size: px(20.0, 20.0),
        rotation: 360.0,
        clips: true,
        ..node("full turn")
    });
    let mut stack = Vec::new();
    for (name, z) in [("z3", 3), ("z1", 1), ("z2", 2), ("z1 again", 1)] {
        stack.push(t.add(Node {
            position: px(0.0, 0.0),
            size: px(10.0, 10.0),
            z,
            ..node(name)
        }));
    }
    let stack = t.add(Node {
        position: px(5.0, 5.0),
        size: px(40.0, 40.0),
        children: stack,
        ..node("siblings by z")
    });
    let ignored = t.add(Node {
        position: px(350.0, 250.0),
        size: px(10.0, 10.0),
        scale: Some(f32::NAN),
        ..node("not a scale")
    });
    let root = t.add(Node {
        children: vec![scaled, turned, full_turn, stack, ignored],
        ..node("screen")
    });

    // A root whose nodes draw in z order across the whole root.
    let low_kid = t.add(Node {
        size: px(5.0, 5.0),
        z: 1,
        ..node("low kid")
    });
    let low = t.add(Node {
        size: px(20.0, 20.0),
        z: 5,
        children: vec![low_kid],
        ..node("low")
    });
    let high = t.add(Node {
        position: px(10.0, 0.0),
        size: px(20.0, 20.0),
        z: 2,
        ..node("high")
    });
    let global = t.add(Node {
        children: vec![low, high],
        ..node("global z root")
    });

    // A root scaled by two about its corner.
    let big_kid = t.add(Node {
        position: px(5.0, 5.0),
        size: px(10.0, 10.0),
        ..node("big kid")
    });
    let big = t.add(Node {
        scale: Some(2.0),
        children: vec![big_kid],
        ..node("scaled root")
    });
    let roots = [
        screen(root, VIEW),
        Root {
            global_z: true,
            ..screen(global, Vec2::new(50.0, 50.0))
        },
        Root {
            origin: Vec2::new(300.0, 10.0),
            abs_origin: Vec2::new(300.0, 10.0),
            ..screen(big, Vec2::new(40.0, 40.0))
        },
    ];
    check("scale_rotation_and_drawing_order", 1.0, &t, &roots);
}

#[test]
fn clipping_and_scrolling() {
    let mut t = UiTree::new();
    let spill = t.add(Node {
        position: px(80.0, 30.0),
        size: px(50.0, 50.0),
        ..node("spill")
    });
    let inner_spill = t.add(Node {
        position: px(-20.0, -20.0),
        size: px(60.0, 60.0),
        ..node("inner spill")
    });
    let inner = t.add(Node {
        position: px(60.0, 0.0),
        size: px(60.0, 40.0),
        clips: true,
        children: vec![inner_spill],
        ..node("inner clipper")
    });
    let clipper = t.add(Node {
        position: px(10.0, 10.0),
        size: px(100.0, 50.0),
        clips: true,
        children: vec![spill, inner],
        ..node("clipper")
    });
    let lines: Vec<NodeId> = (0..4)
        .map(|i| {
            t.add(Node {
                position: Dim2::new(Dim::ZERO, Dim::new(0.25 * i as f32, 0.0)),
                size: Dim2::new(Dim::new(1.0, 0.0), Dim::new(0.25, 0.0)),
                ..node("line")
            })
        })
        .collect();
    let scroller = t.add(Node {
        position: px(150.0, 10.0),
        size: px(100.0, 80.0),
        scroll: Some(Scroll {
            canvas: Dim2::new(Dim::new(1.0, 0.0), Dim::new(3.0, 0.0)),
            automatic: Axes::NONE,
            position: Vec2::new(0.0, 1000.0),
            bar: 6.0,
            bar_color: Rgba::BLACK,
        }),
        children: lines,
        ..node("scrolled past its end")
    });
    let tiles: Vec<NodeId> = (0..6)
        .map(|i| {
            t.add(Node {
                size: px(50.0, 40.0),
                order: i,
                ..node("tile")
            })
        })
        .collect();
    let strip = t.add(Node {
        position: px(10.0, 120.0),
        size: px(160.0, 60.0),
        padding: pad(4.0),
        arrange: Some(Arrange::List(List {
            horizontal: true,
            gap: Dim::px(2.0),
            ..List::default()
        })),
        scroll: Some(Scroll {
            canvas: Dim2::ZERO,
            automatic: Axes::X,
            position: Vec2::new(-5.0, 0.0),
            bar: 0.0,
            bar_color: Rgba::BLACK,
        }),
        children: tiles,
        ..node("strip that grows")
    });
    let root = t.add(Node {
        children: vec![clipper, scroller, strip],
        ..node("screen")
    });
    let edge = t.add(Node {
        position: px(-10.0, -10.0),
        size: px(40.0, 40.0),
        ..node("past the clip")
    });
    let clipped = t.add(Node {
        children: vec![edge],
        ..node("clipped root")
    });
    let roots = [
        screen(root, VIEW),
        Root {
            origin: Vec2::new(300.0, 200.0),
            abs_origin: Vec2::new(300.0, 200.0),
            clip: Some(egui::Rect::from_min_size(
                egui::pos2(300.0, 200.0),
                egui::vec2(60.0, 60.0),
            )),
            ..screen(clipped, Vec2::new(60.0, 60.0))
        },
    ];
    check("clipping_and_scrolling", 1.0, &t, &roots);
}

fn text_cases(t: &mut UiTree) -> NodeId {
    let face = |family, weight, italic| Face {
        family,
        weight,
        italic,
    };
    let mut kids = Vec::new();
    let mut at = 0.0;
    let mut add = |t: &mut UiTree, name: &str, size: Dim2, style: TextStyle| {
        let id = t.add(Node {
            position: px(0.0, at),
            size,
            text: Some(style),
            ..node(name)
        });
        at += 44.0;
        kids.push(id);
    };
    add(
        t,
        "scaled to fit",
        px(200.0, 40.0),
        TextStyle {
            scaled: true,
            ..text("Fits the box", 14.0)
        },
    );
    add(
        t,
        "wrapped",
        px(100.0, 40.0),
        TextStyle {
            wrapped: true,
            align: Vec2::ZERO,
            ..text("Wraps at the box's width, and runs out of it.", 14.0)
        },
    );
    add(
        t,
        "truncated",
        px(80.0, 20.0),
        TextStyle {
            truncate: true,
            ..text("A long line that does not fit", 14.0)
        },
    );
    add(
        t,
        "rich",
        px(300.0, 20.0),
        TextStyle {
            rich: true,
            ..text(
                "<b>Bold</b> and <font color=\"#ff0000\">red</font> &amp; plain",
                14.0,
            )
        },
    );
    add(
        t,
        "line height",
        px(120.0, 40.0),
        TextStyle {
            line_height: 1.5,
            ..text("One\nTwo", 12.0)
        },
    );
    add(
        t,
        "bottom right",
        px(120.0, 40.0),
        TextStyle {
            align: Vec2::ONE,
            face: face(Family::Montserrat, 500, false),
            ..text("Corner", 14.0)
        },
    );
    add(
        t,
        "bold sans",
        px(120.0, 20.0),
        TextStyle {
            face: face(Family::SourceSans3, 700, false),
            ..text("Source Sans 3", 14.0)
        },
    );
    add(
        t,
        "mono, nearest weight",
        px(160.0, 20.0),
        TextStyle {
            face: face(Family::RobotoMono, 600, true),
            ..text("Roboto Mono 0123", 14.0)
        },
    );
    let scaled = t.add(Node {
        position: px(250.0, 0.0),
        size: px(60.0, 20.0),
        scale: Some(2.0),
        text: Some(text("Twice", 12.0)),
        ..node("scaled node")
    });
    kids.push(scaled);
    t.add(Node {
        children: kids,
        ..node("screen")
    })
}

#[test]
fn text_sizes_wraps_truncates_and_runs() {
    let mut t = UiTree::new();
    let root = text_cases(&mut t);
    check(
        "text_sizes_wraps_truncates_and_runs",
        1.0,
        &t,
        &[screen(root, VIEW)],
    );
}

/// The same text at a pixels per point of 1.5, where egui rounds glyphs to
/// physical pixels.
#[test]
fn text_at_one_and_a_half_pixels_per_point() {
    let mut t = UiTree::new();
    let root = text_cases(&mut t);
    check(
        "text_at_one_and_a_half_pixels_per_point",
        1.5,
        &t,
        &[screen(root, VIEW)],
    );
}

/// Laying the same tree out twice gives the same values.
#[test]
fn layout_is_repeatable() {
    let mut t = UiTree::new();
    let root = text_cases(&mut t);
    let roots = [screen(root, VIEW)];
    let once = common::in_pass(1.0, VIEW, |ctx| {
        let (a, va) = common::lay_out(ctx, &t, &roots);
        let (b, vb) = common::lay_out(ctx, &t, &roots);
        (common::dump(&t, &a, &va), common::dump(&t, &b, &vb))
    });
    assert_eq!(once.0, once.1);
}
