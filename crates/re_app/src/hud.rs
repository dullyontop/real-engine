//! The heads-up display: `HudText` components drawn with egui over the frame,
//! each line in its own colour. A line breaks only at its `\n`s: egui would
//! wrap it to the width its panel had the frame before, so text that grows
//! (a speed going from 9 to 10) would wrap and stay wrapped.

use re_render::egui;
use re_scene::{HudAnchor, HudText, World, entity_id};

/// One line of HUD text.
#[derive(Clone, Debug, PartialEq)]
pub struct HudLine {
    /// Where on the screen.
    pub anchor: HudAnchor,
    /// The text.
    pub text: String,
    /// Font size in logical pixels.
    pub size: f32,
    /// Linear RGBA colour.
    pub color: [f32; 4],
}

/// The world's HUD lines, in entity order (so lines sharing an anchor stack in
/// a stable order).
pub fn collect(world: &mut World) -> Vec<HudLine> {
    let mut lines: Vec<_> = world
        .ecs
        .query::<(re_scene::ecs::entity::Entity, &HudText)>()
        .iter(&world.ecs)
        .filter(|(_, h)| !h.text.is_empty())
        .map(|(e, h)| {
            (
                entity_id(e),
                HudLine {
                    anchor: h.anchor,
                    text: h.text.clone(),
                    size: h.size.clamp(4.0, 200.0),
                    color: [h.color.r, h.color.g, h.color.b, h.color.a].map(|c| {
                        if c.is_finite() {
                            c.clamp(0.0, 1.0)
                        } else {
                            1.0
                        }
                    }),
                },
            )
        })
        .collect();
    lines.sort_by_key(|(id, _)| *id);
    lines.into_iter().map(|(_, l)| l).collect()
}

const ANCHORS: [HudAnchor; 7] = [
    HudAnchor::TopLeft,
    HudAnchor::Top,
    HudAnchor::TopRight,
    HudAnchor::Center,
    HudAnchor::BottomLeft,
    HudAnchor::Bottom,
    HudAnchor::BottomRight,
];

fn align(anchor: HudAnchor) -> (egui::Align2, egui::Vec2) {
    const M: f32 = 16.0;
    match anchor {
        HudAnchor::TopLeft => (egui::Align2::LEFT_TOP, egui::vec2(M, M)),
        HudAnchor::Top => (egui::Align2::CENTER_TOP, egui::vec2(0.0, M)),
        HudAnchor::TopRight => (egui::Align2::RIGHT_TOP, egui::vec2(-M, M)),
        HudAnchor::Center => (egui::Align2::CENTER_CENTER, egui::Vec2::ZERO),
        HudAnchor::BottomLeft => (egui::Align2::LEFT_BOTTOM, egui::vec2(M, -M)),
        HudAnchor::Bottom => (egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -M)),
        HudAnchor::BottomRight => (egui::Align2::RIGHT_BOTTOM, egui::vec2(-M, -M)),
    }
}

/// Draws `lines` into the egui context: one panel per anchor that has text.
pub fn draw(ctx: &egui::Context, lines: &[HudLine]) {
    for anchor in ANCHORS {
        let here: Vec<&HudLine> = lines.iter().filter(|l| l.anchor == anchor).collect();
        if here.is_empty() {
            continue;
        }
        let (a, offset) = align(anchor);
        egui::Area::new(egui::Id::new(("re_app hud", anchor as u8)))
            .anchor(a, offset)
            .interactable(false)
            .fade_in(false)
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(egui::Color32::from_black_alpha(150))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                        for line in here {
                            let [r, g, b, a] = line.color;
                            ui.label(egui::RichText::new(&line.text).size(line.size).color(
                                egui::Color32::from(egui::Rgba::from_rgba_unmultiplied(r, g, b, a)),
                            ));
                        }
                    });
            });
    }
}

/// egui's input for a pass over a target of `size_px` physical pixels.
pub fn raw_input(size_px: [u32; 2], pixels_per_point: f32, time: f64) -> egui::RawInput {
    let points = egui::vec2(size_px[0] as f32, size_px[1] as f32) / pixels_per_point;
    let mut input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, points)),
        time: Some(time),
        ..egui::RawInput::default()
    };
    if let Some(v) = input.viewports.get_mut(&egui::ViewportId::ROOT) {
        v.native_pixels_per_point = Some(pixels_per_point);
    }
    input
}

/// Runs egui for one HUD frame of `size_px` physical pixels at
/// `pixels_per_point`, giving the output to paint.
pub fn run(
    ctx: &egui::Context,
    lines: &[HudLine],
    size_px: [u32; 2],
    pixels_per_point: f32,
    time: f64,
) -> egui::FullOutput {
    let input = raw_input(size_px, pixels_per_point, time);
    // egui lays a new area out invisibly on its first pass, so a frame captured
    // on its own would have no HUD. Run twice and paint the second pass, keeping
    // the first pass's texture updates (the font atlas) in order.
    let first = ctx.run_ui(input.clone(), |ui| draw(ui.ctx(), lines));
    let mut second = ctx.run_ui(input, |ui| draw(ui.ctx(), lines));
    let mut delta = first.textures_delta;
    delta.append(std::mem::take(&mut second.textures_delta));
    second.textures_delta = delta;
    second
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(anchor: HudAnchor, text: &str) -> HudLine {
        HudLine {
            anchor,
            text: text.into(),
            size: 22.0,
            color: [1.0; 4],
        }
    }

    /// The rows of each text drawn, in drawing order.
    fn rows(mut output: egui::FullOutput) -> Vec<(String, usize)> {
        // The font atlas uploads go nowhere here.
        output.textures_delta.clear();
        output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(t) => Some((t.galley.text().to_string(), t.galley.rows.len())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn lines_that_grow_do_not_wrap() {
        let ctx = egui::Context::default();
        for anchor in ANCHORS {
            for text in [
                "9 km/h  D1",
                "10 km/h  D1",
                "+268  Delivered to 7 Market Avenue (handled with care)",
            ] {
                let out = run(&ctx, &[line(anchor, text)], [1280, 720], 1.0, 0.0);
                assert_eq!(rows(out), [(text.to_string(), 1)], "{anchor:?}");
            }
            let out = run(&ctx, &[line(anchor, "two\nlines")], [1280, 720], 1.0, 0.0);
            assert_eq!(rows(out), [("two\nlines".to_string(), 2)], "{anchor:?}");
        }
    }
}
