//! The toolbar: the project's name, undo and redo, the command palette, the
//! gizmo's settings and the status line.

use egui::RichText;
use re_app::App;
use serde_json::json;

use crate::editor::{Editor, command};
use crate::gizmo;

pub(super) fn toolbar(ed: &mut Editor, ui: &mut egui::Ui, app: &App) {
    ui.horizontal(|ui| {
        ui.strong(&app.config().project.name);
        ui.separator();
        let history = app.history();
        let undo = ui
            .add_enabled(history.can_undo(), egui::Button::new("Undo"))
            .on_hover_text(match history.undo_label() {
                Some(label) => format!("Undo {label} (Ctrl+Z)"),
                None => "Nothing to undo".to_string(),
            });
        if undo.clicked() {
            ed.later.push(command("undo", json!({})));
            ui.ctx().request_repaint();
        }
        let redo = ui
            .add_enabled(history.can_redo(), egui::Button::new("Redo"))
            .on_hover_text(match history.redo_label() {
                Some(label) => format!("Redo {label} (Ctrl+Shift+Z)"),
                None => "Nothing to redo".to_string(),
            });
        if redo.clicked() {
            ed.later.push(command("redo", json!({})));
            ui.ctx().request_repaint();
        }
        if ui
            .button("Commands…")
            .on_hover_text("Command palette (Ctrl+P)")
            .clicked()
        {
            let open = !ed.palette.open;
            ed.set_palette_open(open);
        }
        ui.separator();
        gizmo::toolbar(&mut ed.gizmo.settings, ui);
        ui.separator();
        ui.label(RichText::new(&ed.status).weak());
    });
}
