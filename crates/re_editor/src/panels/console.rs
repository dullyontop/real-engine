//! The console: diagnostics, command results and the transaction log.

use egui::{Color32, RichText};

use crate::editor::{Editor, LineKind};

pub(super) fn console(ed: &mut Editor, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.heading("Console");
        if ui.small_button("Clear").clicked() {
            ed.console.clear();
        }
    });
    egui::ScrollArea::vertical()
        .id_salt("re_editor.console")
        .auto_shrink(false)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            for line in &ed.console {
                let text = RichText::new(&line.text).monospace();
                let text = match line.kind {
                    LineKind::Info => text,
                    LineKind::Warning => text.color(Color32::from_rgb(230, 180, 60)),
                    LineKind::Error => text.color(Color32::from_rgb(235, 90, 80)),
                };
                ui.label(text);
            }
        });
}
