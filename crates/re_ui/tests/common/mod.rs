#![allow(dead_code)]
//! Shared by the layout tests: a context with the bundled fonts, every value
//! a layout gives written out exactly, and the committed files those values
//! are compared with (`tests/fixtures/layout/`).

use std::fmt::Write as _;
use std::path::PathBuf;

use glam::{Affine2, Vec2};
use re_ui::{Item, Layout, NodeId, Root, UiTree, Visit};

/// Runs `f` in the second pass of a new context with the bundled fonts, at
/// `pixels_per_point` on a screen of `view` pixels; fonts take effect from
/// the second pass.
pub fn in_pass<R>(pixels_per_point: f32, view: Vec2, mut f: impl FnMut(&egui::Context) -> R) -> R {
    let ctx = egui::Context::default();
    re_ui::fonts::install(&ctx);
    let mut out = None;
    for n in 0..2 {
        let points = view / pixels_per_point;
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(points.x, points.y),
            )),
            max_texture_side: Some(8192),
            time: Some(f64::from(n) / 60.0),
            ..Default::default()
        };
        raw.viewports
            .entry(raw.viewport_id)
            .or_default()
            .native_pixels_per_point = Some(pixels_per_point);
        let mut output = ctx.run_ui(raw, |ui| out = Some(f(ui.ctx())));
        output.textures_delta.clear();
    }
    out.expect("the pass ran")
}

/// Lays `roots` of `tree` out in `ctx`: each root's items, then the visits.
pub fn lay_out(ctx: &egui::Context, tree: &UiTree, roots: &[Root]) -> (Vec<Vec<Item>>, Vec<Visit>) {
    let mut lay = Layout::new(tree, ctx);
    let items = roots.iter().map(|r| lay.root(r)).collect();
    (items, lay.take_visits())
}

fn f(x: f32) -> String {
    format!("{x:?}")
}

fn v(p: Vec2) -> String {
    format!("({}, {})", f(p.x), f(p.y))
}

fn ev(p: egui::Vec2) -> String {
    v(Vec2::new(p.x, p.y))
}

fn rect(r: egui::Rect) -> String {
    format!(
        "({}, {}, {}, {})",
        f(r.min.x),
        f(r.min.y),
        f(r.max.x),
        f(r.max.y)
    )
}

fn label(tree: &UiTree, id: NodeId) -> String {
    match tree.node(id).name.as_str() {
        "" => format!("#{}", id.0),
        name => format!("#{} {name}", id.0),
    }
}

/// Every value of the visits and items as text, each `f32` written so that
/// it reads back to the same bits.
pub fn dump(tree: &UiTree, roots: &[Vec<Item>], visits: &[Visit]) -> String {
    let mut s = String::new();
    for vi in visits {
        let mut line = format!(
            "visit {}{} at {} size {} rotation {}",
            label(tree, vi.node),
            if vi.root { " (root)" } else { "" },
            v(vi.position),
            v(vi.size),
            f(vi.rotation)
        );
        if let Some((bounds, fits)) = vi.text {
            let _ = write!(
                line,
                " text {} {}",
                v(bounds),
                if fits { "fits" } else { "overflows" }
            );
        }
        if let Some(sc) = vi.scroll {
            let _ = write!(line, " canvas {} window {}", v(sc.canvas), v(sc.window));
            if let Some(at) = sc.clamped {
                let _ = write!(line, " held at {}", v(at));
            }
        }
        if let Some(a) = vi.arranged {
            let _ = write!(line, " content {}", v(a.content));
            if let Some((cell, count)) = a.cells {
                let _ = write!(line, " cells {} by {}", v(cell), v(count));
            }
        }
        s += &line;
        s.push('\n');
    }
    for (i, items) in roots.iter().enumerate() {
        let _ = writeln!(s, "root {i}: {} drawn", items.len());
        for it in items {
            let mut line = format!(
                "item {} at {} size {} scale {} order ({}, {}){}",
                label(tree, it.node),
                v(it.min),
                v(it.size),
                f(it.scale),
                it.key.0,
                it.key.1,
                if it.interactable { " input" } else { "" }
            );
            if it.angle != 0.0 || it.rot != Affine2::IDENTITY {
                let m = it.rot.to_cols_array().map(f).join(", ");
                let _ = write!(line, " angle {} turn [{m}]", f(it.angle));
            }
            if let Some(c) = it.clip {
                let _ = write!(line, " clip {}", rect(c));
            }
            if it.pad != [0.0; 4] {
                let _ = write!(line, " padding [{}]", it.pad.map(f).join(", "));
            }
            if let Some(c) = it.canvas {
                let _ = write!(line, " canvas {}", v(c));
            }
            if it.opacity != 1.0 {
                let _ = write!(line, " opacity {}", f(it.opacity));
            }
            s += &line;
            s.push('\n');
            if let Some(t) = &it.text {
                let g = &t.galley;
                let _ = writeln!(
                    s,
                    "  text {} size {} in {} bounds {}{}",
                    if t.fits { "fits" } else { "overflows" },
                    f(t.size),
                    v(t.inner),
                    rect(g.rect),
                    if g.elided { " elided" } else { "" }
                );
                for r in &g.rows {
                    let _ = writeln!(
                        s,
                        "  row at {} size {} {:?}",
                        v(Vec2::new(r.pos.x, r.pos.y)),
                        ev(r.row.size),
                        r.row.text()
                    );
                }
            }
        }
    }
    s
}

/// Compares `actual` with `tests/fixtures/layout/<name>.txt`;
/// `RE_UPDATE_GOLDENS=1` writes the file instead. A difference leaves the
/// whole output in `out/re_ui/layout/` and names the first line that differs.
pub fn expect(name: &str, actual: &str) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root
        .join("tests/fixtures/layout")
        .join(format!("{name}.txt"));
    if std::env::var_os("RE_UPDATE_GOLDENS").is_some_and(|v| !v.is_empty()) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        println!("updated {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| {
            panic!(
                "{}: {e}: run with RE_UPDATE_GOLDENS=1 and review the file",
                path.display()
            )
        })
        .replace("\r\n", "\n");
    if expected == actual {
        return;
    }
    let out = root.join("../../out/re_ui/layout");
    std::fs::create_dir_all(&out).unwrap();
    let saved = out.join(format!("{name}.txt"));
    std::fs::write(&saved, actual).unwrap();
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    let n = (0..e.len().max(a.len()))
        .find(|&i| e.get(i) != a.get(i))
        .unwrap_or(e.len());
    panic!(
        "{name}: line {} differs from {}\n  expected: {}\n  actual:   {}\n(the whole output is in {})",
        n + 1,
        path.display(),
        e.get(n).unwrap_or(&"(end)"),
        a.get(n).unwrap_or(&"(end)"),
        saved.display()
    );
}
