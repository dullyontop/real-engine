//! The command palette (Ctrl+P): every command with its summary and fields,
//! run with JSON arguments.

use egui::RichText;
use re_app::App;
use re_core::Diagnostic;
use serde_json::{Value as Json, json};

use crate::Panel;
use crate::codes;
use crate::editor::{Action, Editor, Palette};

pub(super) fn palette(ed: &mut Editor, ctx: &egui::Context, app: &App, actions: &mut Vec<Action>) {
    if !ed.palette.open {
        return;
    }
    let mut open = true;
    let specs = app.commands().specs();
    let shown = egui::Window::new("Command palette")
        .id(egui::Id::new("re_editor.palette"))
        .open(&mut open)
        .collapsible(false)
        .default_width(460.0)
        .default_pos([280.0, 60.0])
        .show(ctx, |ui| {
            let p = &mut ed.palette;
            let filter = ui.add(
                egui::TextEdit::singleline(&mut p.filter)
                    .hint_text("Filter commands")
                    .desired_width(f32::INFINITY),
            );
            if p.focus_filter {
                filter.request_focus();
                p.focus_filter = false;
            }
            let needle = p.filter.to_lowercase();
            egui::ScrollArea::vertical()
                .id_salt("re_editor.palette.list")
                .max_height(180.0)
                .show(ui, |ui| {
                    for spec in specs.iter().filter(|s| {
                        needle.is_empty()
                            || s.name.to_lowercase().contains(&needle)
                            || s.summary.to_lowercase().contains(&needle)
                    }) {
                        let selected = p.selected.as_deref() == Some(spec.name.as_str());
                        let label = format!("{}  {}", spec.name, spec.summary);
                        if ui.selectable_label(selected, label).clicked() && !selected {
                            p.selected = Some(spec.name.clone());
                            p.args = "{}".to_string();
                        }
                    }
                });
            ui.separator();
            let Some(spec) = p
                .selected
                .as_deref()
                .and_then(|n| specs.iter().find(|s| s.name == n))
            else {
                ui.label(RichText::new("Pick a command.").weak());
                return;
            };
            ui.label(RichText::new(&spec.name).strong());
            ui.label(format!("{} ({} tier)", spec.summary, spec.tier.as_str()));
            let required: Vec<&str> = spec.input_schema["required"]
                .as_array()
                .map(|a| a.iter().filter_map(Json::as_str).collect())
                .unwrap_or_default();
            if let Some(props) = spec.input_schema["properties"].as_object() {
                for (name, prop) in props {
                    let star = if required.contains(&name.as_str()) {
                        "*"
                    } else {
                        ""
                    };
                    let doc = prop["description"].as_str().unwrap_or("");
                    ui.label(RichText::new(format!("{name}{star}: {doc}")).small());
                }
            }
            ui.add(
                egui::TextEdit::multiline(&mut p.args)
                    .code_editor()
                    .desired_rows(4)
                    .desired_width(f32::INFINITY),
            );
            ui.horizontal(|ui| {
                ui.checkbox(&mut p.dry_run, "dry run");
                let run = ui.button("Run").on_hover_text("Ctrl+Enter").clicked()
                    || ui.input_mut(|i| {
                        i.consume_shortcut(&egui::KeyboardShortcut::new(
                            egui::Modifiers::COMMAND,
                            egui::Key::Enter,
                        ))
                    });
                if run && let Some(action) = palette_action(p) {
                    actions.push(action);
                }
            });
        });
    if let Some(r) = shown {
        ed.rects.set(Panel::Palette, r.response.rect);
    }
    ed.palette.open &= open;
}

/// The command the palette would run: the selected command with its
/// arguments, or `RE8802` when they are not a JSON object.
pub(crate) fn palette_action(p: &Palette) -> Option<Action> {
    let name = p.selected.clone()?;
    let text = p.args.trim();
    let parsed = if text.is_empty() {
        Ok(json!({}))
    } else {
        serde_json::from_str::<Json>(text).map_err(|e| e.to_string())
    };
    match parsed {
        Ok(args @ Json::Object(_)) => Some(Action::Command {
            name,
            args,
            dry_run: p.dry_run,
            show_data: true,
        }),
        Ok(other) => Some(Action::Report(palette_error(
            &name,
            &format!("found {other}"),
        ))),
        Err(e) => Some(Action::Report(palette_error(&name, &e))),
    }
}

fn palette_error(name: &str, why: &str) -> Diagnostic {
    Diagnostic::error(
        codes::PALETTE_ARGS,
        format!("the arguments of `{name}` are not a JSON object: {why}"),
    )
    .with_hint(
        "type a JSON object such as {\"name\": \"Rock\"}; the palette lists the command's fields",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_arguments_must_be_an_object() {
        let mut p = Palette {
            selected: Some("undo".into()),
            args: "  ".into(),
            ..Palette::default()
        };
        assert!(matches!(palette_action(&p), Some(Action::Command { .. })));
        p.args = "3".into();
        match palette_action(&p) {
            Some(Action::Report(d)) => assert_eq!(d.code, codes::PALETTE_ARGS),
            other => panic!("{other:?}"),
        }
        p.selected = None;
        assert!(palette_action(&p).is_none());
    }
}
