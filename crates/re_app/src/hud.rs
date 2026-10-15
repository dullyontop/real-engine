//! The heads-up display: `HudText` components drawn with egui over the frame.

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
                        for line in here {
                            ui.label(
                                egui::RichText::new(&line.text)
                                    .size(line.size)
                                    .color(egui::Color32::WHITE),
                            );
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
