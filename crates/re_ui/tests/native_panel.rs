//! A native panel built straight from `re_ui` nodes, with no compat instance
//! behind it: a backdrop with a gradient, a rounded and outlined panel held
//! between a smallest and a largest width, as tall as its content and
//! scaled down on a small screen, a title, wrapped body text, an image from
//! a texture, a column of buttons (one hovered), a bar, and a clipped
//! scrolling list. Its layout values are compared exactly with
//! `tests/fixtures/layout/native_panel_*.txt`, and it is painted through
//! re_render's UI pass at two resolutions and compared with the goldens in
//! `tests/fixtures/goldens/<backend>/`.

mod common;

use glam::Vec2;
use re_render::wgpu::util::DeviceExt;
use re_render::{Capture, Gpu, Renderer, UiFrame, wgpu};
use re_ui::{
    Arrange, Axes, Dim, Dim2, Face, Family, Gradient, Image, Item, Layout, List, Look, Looks, Node,
    NodeId, Padding, Painter, Rgba, Root, Scroll, SizeLimits, Stroke, StrokeMode, TextStyle,
    UiTree,
};

const INK: Rgba = Rgba([0.93, 0.94, 0.96, 1.0]);
const PANEL: Rgba = Rgba([0.11, 0.12, 0.15, 0.96]);
const ACCENT: Rgba = Rgba([0.98, 0.66, 0.18, 1.0]);

fn text(s: &str, face: Face, size: f32, align_x: f32) -> TextStyle {
    TextStyle {
        text: s.into(),
        face,
        size,
        color: INK,
        align: Vec2::new(align_x, 0.5),
        ..TextStyle::default()
    }
}

fn face(family: Family, weight: u16) -> Face {
    Face {
        family,
        weight,
        italic: false,
    }
}

fn filled(background: Rgba, corner: f32) -> Look {
    Look {
        background,
        border: 0.0,
        corner: Some(Dim::px(corner)),
        ..Look::default()
    }
}

/// The panel's nodes: the root, and the nodes a test looks at.
struct Panel {
    tree: UiTree,
    root: NodeId,
    panel: NodeId,
    body: NodeId,
    buttons: Vec<NodeId>,
    list: NodeId,
}

/// The panel's scale on a screen `height` pixels tall.
fn scale_for(height: f32) -> f32 {
    (height / 620.0).clamp(0.7, 1.0)
}

fn panel(icon: Option<egui::TextureId>, view: Vec2) -> Panel {
    let mut t = UiTree::new();
    let add = |t: &mut UiTree, n: Node| t.add(n);

    let backdrop = add(
        &mut t,
        Node {
            name: "backdrop".into(),
            size: Dim2::scale(1.0, 1.0),
            look: Look {
                background: Rgba::WHITE,
                border: 0.0,
                gradient: Some(Gradient {
                    colors: vec![(0.0, [0.16, 0.28, 0.42]), (1.0, [0.05, 0.07, 0.12])],
                    transparency: vec![(0.0, [0.0; 3]), (1.0, [0.0; 3])],
                    offset: Vec2::ZERO,
                    rotation: 90.0,
                }),
                ..Look::default()
            },
            ..Node::default()
        },
    );

    let title = add(
        &mut t,
        Node {
            name: "title".into(),
            size: Dim2::new(Dim::new(1.0, 0.0), Dim::px(40.0)),
            order: 1,
            text: Some(TextStyle {
                color: ACCENT,
                ..text("Paused", face(Family::Montserrat, 900), 30.0, 0.0)
            }),
            ..Node::default()
        },
    );
    let body = add(
        &mut t,
        Node {
            name: "body".into(),
            size: Dim2::new(Dim::new(1.0, 0.0), Dim::ZERO),
            automatic_size: Axes::Y,
            order: 2,
            text: Some(TextStyle {
                wrapped: true,
                align: Vec2::new(0.0, 0.0),
                ..text(
                    "Drawn by the shared UI core, with no compat instance behind it.",
                    face(Family::SourceSans3, 400),
                    17.0,
                    0.0,
                )
            }),
            ..Node::default()
        },
    );

    // An icon and a caption side by side.
    let icon_node = add(
        &mut t,
        Node {
            name: "icon".into(),
            size: Dim2::px(48.0, 48.0),
            order: 1,
            image: Some(match icon {
                Some(texture) => Image::Texture {
                    texture,
                    tint: Rgba::WHITE,
                },
                None => Image::Missing { tint: ACCENT },
            }),
            look: filled(Rgba::TRANSPARENT, 0.0),
            ..Node::default()
        },
    );
    let caption = add(
        &mut t,
        Node {
            name: "caption".into(),
            size: Dim2::new(Dim::new(1.0, -60.0), Dim::px(48.0)),
            order: 2,
            text: Some(text(
                "Deliveries 7 / 12",
                face(Family::RobotoMono, 700),
                18.0,
                0.0,
            )),
            ..Node::default()
        },
    );
    let row = add(
        &mut t,
        Node {
            name: "icon row".into(),
            size: Dim2::new(Dim::new(1.0, 0.0), Dim::px(48.0)),
            order: 3,
            arrange: Some(Arrange::List(List {
                horizontal: true,
                align: Vec2::new(0.0, 0.5),
                gap: Dim::px(12.0),
                ..List::default()
            })),
            children: vec![icon_node, caption],
            ..Node::default()
        },
    );

    // A bar: a track and its fill.
    let fill = add(
        &mut t,
        Node {
            name: "bar fill".into(),
            size: Dim2::new(Dim::new(0.58, 0.0), Dim::new(1.0, 0.0)),
            look: filled(ACCENT, 6.0),
            ..Node::default()
        },
    );
    let bar = add(
        &mut t,
        Node {
            name: "bar".into(),
            size: Dim2::new(Dim::new(1.0, 0.0), Dim::px(12.0)),
            order: 4,
            look: filled(Rgba([1.0, 1.0, 1.0, 0.12]), 6.0),
            children: vec![fill],
            ..Node::default()
        },
    );

    let mut buttons = Vec::new();
    for (i, label) in ["Resume", "Settings", "Quit"].into_iter().enumerate() {
        let b = add(
            &mut t,
            Node {
                name: label.to_lowercase(),
                size: Dim2::new(Dim::new(1.0, 0.0), Dim::px(40.0)),
                order: 10 + i as i64,
                text: Some(text(label, face(Family::Montserrat, 700), 18.0, 0.5)),
                look: Look {
                    auto_color: true,
                    strokes: vec![Stroke {
                        mode: StrokeMode::Border,
                        thickness: 1.0,
                        color: Rgba([1.0, 1.0, 1.0, 0.25]),
                    }],
                    ..filled(Rgba([0.22, 0.25, 0.32, 1.0]), 8.0)
                },
                ..Node::default()
            },
        );
        buttons.push(b);
    }

    // A clipped list that scrolls: eight rows on a canvas that grows to fit
    // them, scrolled down by one row.
    let rows: Vec<NodeId> = (0..8)
        .map(|i| {
            add(
                &mut t,
                Node {
                    name: format!("stop {}", i + 1),
                    size: Dim2::new(Dim::new(1.0, -12.0), Dim::px(22.0)),
                    order: i,
                    text: Some(text(
                        &format!("Stop {:>2}  ·  {} m", i + 1, 120 * (i + 1)),
                        face(Family::RobotoMono, 400),
                        14.0,
                        0.0,
                    )),
                    look: filled(
                        if i % 2 == 0 {
                            Rgba([1.0, 1.0, 1.0, 0.06])
                        } else {
                            Rgba::TRANSPARENT
                        },
                        0.0,
                    ),
                    ..Node::default()
                },
            )
        })
        .collect();
    let list = add(
        &mut t,
        Node {
            name: "stops".into(),
            size: Dim2::new(Dim::new(1.0, 0.0), Dim::px(70.0)),
            order: 20,
            clips: true,
            arrange: Some(Arrange::List(List {
                horizontal: false,
                ..List::default()
            })),
            scroll: Some(Scroll {
                canvas: Dim2::ZERO,
                automatic: Axes::Y,
                position: Vec2::new(0.0, 22.0),
                bar: 6.0,
                bar_color: Rgba([1.0, 1.0, 1.0, 0.5]),
            }),
            look: filled(Rgba([0.0, 0.0, 0.0, 0.25]), 0.0),
            children: rows,
            ..Node::default()
        },
    );

    let mut kids = vec![title, body, row, bar];
    kids.extend(&buttons);
    kids.push(list);
    let panel = add(
        &mut t,
        Node {
            name: "panel".into(),
            position: Dim2::scale(0.5, 0.5),
            anchor: Vec2::splat(0.5),
            size: Dim2::scale(0.6, 0.0),
            automatic_size: Axes::Y,
            scale: Some(scale_for(view.y)),
            limits: Some(SizeLimits {
                min: Vec2::new(360.0, 0.0),
                max: Vec2::new(460.0, f32::INFINITY),
            }),
            padding: Some(Padding {
                left: Dim::px(18.0),
                top: Dim::px(16.0),
                right: Dim::px(18.0),
                bottom: Dim::px(16.0),
            }),
            arrange: Some(Arrange::List(List {
                horizontal: false,
                gap: Dim::px(10.0),
                ..List::default()
            })),
            look: Look {
                strokes: vec![Stroke {
                    mode: StrokeMode::Border,
                    thickness: 2.0,
                    color: Rgba([1.0, 1.0, 1.0, 0.18]),
                }],
                ..filled(PANEL, 14.0)
            },
            children: kids,
            ..Node::default()
        },
    );
    let root = add(
        &mut t,
        Node {
            name: "screen".into(),
            children: vec![backdrop, panel],
            ..Node::default()
        },
    );
    Panel {
        tree: t,
        root,
        panel,
        body,
        buttons,
        list,
    }
}

/// Lays the panel out at `view` and paints it; the second button is
/// hovered.
fn frame(
    ctx: &egui::Context,
    p: &Panel,
    view: Vec2,
) -> (Vec<Item>, Vec<egui::epaint::ClippedShape>) {
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(view.x, view.y));
    let mut lay = Layout::new(&p.tree, ctx);
    let items = lay.root(&Root {
        node: p.root,
        origin: Vec2::ZERO,
        abs_origin: Vec2::ZERO,
        size: view,
        global_z: false,
        clip: None,
        input: true,
    });
    let looks = Looks {
        hovered: Some(p.buttons[1]),
        ..Looks::default()
    };
    let mut painter = Painter::new(&p.tree, screen);
    for it in &items {
        painter.item(it, &looks);
    }
    (items, painter.finish())
}

fn item(items: &[Item], id: NodeId) -> &Item {
    items.iter().find(|it| it.node == id).expect("drawn")
}

/// A 16 by 16 texture: a two-colour checker under a diagonal fade.
fn icon_texture(gpu: &Gpu) -> wgpu::Texture {
    let n = 16u32;
    let mut px = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let on = (x / 4 + y / 4) % 2 == 0;
            let k = (x + y) as f32 / (2 * n - 2) as f32;
            let (a, b) = ([250.0, 170.0, 45.0], [40.0, 160.0, 210.0]);
            let c: [f32; 3] = if on { a } else { b };
            px.extend(c.map(|v| (v * (1.0 - 0.4 * k)) as u8));
            px.push(255);
        }
    }
    gpu.device.create_texture_with_data(
        &gpu.queue,
        &wgpu::TextureDescriptor {
            label: Some("native panel icon"),
            size: wgpu::Extent3d {
                width: n,
                height: n,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
        wgpu::util::TextureDataOrder::LayerMajor,
        &px,
    )
}

#[test]
fn native_panel_lays_out_without_a_gpu() {
    let ctx = egui::Context::default();
    re_ui::fonts::install(&ctx);
    for (view, width) in [
        (Vec2::new(480.0, 320.0), 360.0),
        (Vec2::new(1280.0, 720.0), 460.0),
    ] {
        let p = panel(None, view);
        let k = scale_for(view.y);
        // Fonts take effect from the second pass.
        let mut laid = None;
        for _ in 0..2 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(view.x, view.y),
                )),
                ..Default::default()
            };
            let mut output = ctx.run_ui(raw, |ui| laid = Some(frame(ui.ctx(), &p, view)));
            output.textures_delta.clear();
        }
        let (items, shapes) = laid.unwrap();
        let near = |a: f32, b: f32| (a - b).abs() < 0.01;
        let panel = item(&items, p.panel);
        assert!(
            near(panel.size.x, width * k),
            "{view}: held by its limits, then scaled: {}",
            panel.size
        );
        assert!(
            ((panel.min + panel.size * 0.5) - view * 0.5)
                .abs()
                .max_element()
                < 0.01,
            "{view}: centred"
        );
        assert!(panel.size.y <= view.y, "{view}: fits: {}", panel.size);
        // The buttons stack under each other, 40 pixels tall and 10 apart
        // before the scale.
        let tops: Vec<f32> = p.buttons.iter().map(|&b| item(&items, b).min.y).collect();
        assert!(near(tops[1] - tops[0], 50.0 * k), "{tops:?}");
        assert!(near(tops[2] - tops[1], 50.0 * k), "{tops:?}");
        // The body wraps inside the panel's padding, and the panel grows to
        // hold everything.
        let body = item(&items, p.body);
        let rows = body.text.as_ref().unwrap().galley.rows.len();
        assert!(rows >= 2, "{view}: {rows} rows");
        assert!(body.size.x <= (width - 36.0) * k + 0.01);
        let list = item(&items, p.list);
        assert!(
            near(
                list.min.y + list.size.y + 16.0 * k,
                panel.min.y + panel.size.y
            ),
            "{view}: as tall as its content"
        );
        assert!(shapes.len() > 20);
    }
}

/// Every value the panel's layout gives, at both resolutions, exactly.
#[test]
fn native_panel_layout_values() {
    for view in [Vec2::new(480.0, 320.0), Vec2::new(1280.0, 720.0)] {
        let p = panel(None, view);
        let roots = [Root {
            node: p.root,
            origin: Vec2::ZERO,
            abs_origin: Vec2::ZERO,
            size: view,
            global_z: false,
            clip: None,
            input: true,
        }];
        let out = common::in_pass(1.0, view, |ctx| {
            let (items, visits) = common::lay_out(ctx, &p.tree, &roots);
            common::dump(&p.tree, &items, &visits)
        });
        common::expect(&format!("native_panel_{}x{}", view.x, view.y), &out);
    }
}

#[test]
fn native_panel_paints_at_two_resolutions() {
    let backends = re_render::backends_from_env().expect("RE_BACKENDS");
    let gpu = match Gpu::headless(backends) {
        Ok(g) => g,
        Err(d) if std::env::var_os("CI").is_none() => {
            println!("skipped: no adapter: {}", d.render_human());
            return;
        }
        Err(d) => panic!("{}", d.render_human()),
    };
    let set = match gpu.backend() {
        wgpu::Backend::Vulkan => "vulkan",
        wgpu::Backend::Gl => "gl",
        other => {
            println!("no image set for {other:?}");
            return;
        }
    };
    let mut renderer = Renderer::new(&gpu, Capture::FORMAT);
    let icon = icon_texture(&gpu);
    let icon_id = renderer.register_ui_texture(&icon.create_view(&Default::default()));
    for (w, h) in [(480u32, 320u32), (1280, 720)] {
        let view = Vec2::new(w as f32, h as f32);
        let p = panel(Some(icon_id), view);
        let ctx = egui::Context::default();
        re_ui::fonts::install(&ctx);
        let capture = Capture::new(&gpu.device, w, h).unwrap();
        // Pass 0 installs the fonts (and is painted elsewhere, for its font
        // atlas); pass 1 is the one kept.
        let first = Capture::new(&gpu.device, w, h).unwrap();
        for n in 0..2 {
            let mut raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(view.x, view.y),
                )),
                max_texture_side: Some(8192),
                time: Some(f64::from(n) / 60.0),
                ..Default::default()
            };
            raw.viewports
                .entry(raw.viewport_id)
                .or_default()
                .native_pixels_per_point = Some(1.0);
            let mut output = ctx.run_ui(raw, |ui| {
                let (_, shapes) = frame(ui.ctx(), &p, view);
                let painter = ui.ctx().layer_painter(egui::LayerId::background());
                for s in shapes {
                    painter.with_clip_rect(s.clip_rect).add(s.shape);
                }
            });
            let ui = UiFrame::from_output(&ctx, &mut output, [w, h]);
            let target = if n == 0 { &first } else { &capture };
            renderer
                .paint_ui(&gpu.device, &gpu.queue, target.view(), &ui)
                .unwrap_or_else(|d| panic!("{}", d.render_human()));
        }
        assert_eq!(gpu.errors().count(), 0, "{:?}", gpu.errors().take());
        let image = capture.read(&gpu.device, &gpu.queue).unwrap();
        assert!(image.stats([0; 4]).distinct_colors > 100);
        golden(&image, set, &format!("native_panel_{w}x{h}"));
    }
}

/// Saves `image` into `out/re_ui/<set>/` and compares it with
/// `tests/fixtures/goldens/<set>/<name>.png`; `RE_UPDATE_GOLDENS=1` rewrites
/// the golden instead.
fn golden(image: &re_render::Image, set: &str, name: &str) {
    use re_render::imgdiff::{self, Tolerance};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = root.join("../../out/re_ui").join(set);
    image.save_png(&out.join(format!("{name}.png"))).unwrap();
    let golden = root
        .join("tests/fixtures/goldens")
        .join(set)
        .join(format!("{name}.png"));
    if std::env::var_os("RE_UPDATE_GOLDENS").is_some_and(|v| !v.is_empty()) {
        image.save_png(&golden).unwrap();
        println!("updated {}", golden.display());
        return;
    }
    let expected = re_render::Image::load_png(&golden).unwrap_or_else(|d| {
        panic!(
            "{}missing golden: run with RE_UPDATE_GOLDENS=1 and review the image",
            d.render_human()
        )
    });
    let report = imgdiff::compare(image, &expected, Tolerance::default()).unwrap();
    println!("{name} ({set}): {}", report.summary());
    if !report.matches() {
        let diff = imgdiff::diff_image(image, &expected, Tolerance::default()).unwrap();
        diff.save_png(&out.join(format!("{name}.diff.png")))
            .unwrap();
        panic!(
            "{name} does not match its {set} golden: {} (actual and diff in {})",
            report.summary(),
            out.display()
        );
    }
}
