//! The viewport: the game view's texture, with the gizmo over it.

use egui::RichText;
use re_app::App;

use crate::editor::{Action, Editor};

pub(super) fn viewport(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    let rect = ui.max_rect();
    let ppp = ui.ctx().pixels_per_point();
    let px = [
        (rect.width() * ppp).round().max(1.0) as u32,
        (rect.height() * ppp).round().max(1.0) as u32,
    ];
    ed.viewport_px = Some(px);
    match ed.viewport_texture {
        Some((texture, _)) => {
            let image =
                egui::Image::from_texture(egui::load::SizedTexture::new(texture, rect.size()));
            ui.put(rect, image);
        }
        None => {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new("No game view (no GPU)").weak());
            });
        }
    }
    let selected = ed.selected.clone();
    ed.gizmo.ui(ui, rect, app, selected.as_deref(), actions);
}
