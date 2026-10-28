//! The debugger panel (ADR-0045): breakpoints, the stopped script's stack
//! and variables, and the step buttons, over the same
//! [`Debugger`] handle a DAP client uses.
//!
//! The panel only reads the debugger's state and sends it commands; it never
//! takes its events, which belong to the DAP server when one runs.

use egui::RichText;
use re_script::debug::{Debugger, Resume, Stopped, Variable};

/// The panel's own state.
#[derive(Default)]
pub(crate) struct DebugPanel {
    pub debugger: Option<Debugger>,
    /// The `path:line` being typed into the breakpoint field.
    pub new_breakpoint: String,
    /// The frame whose variables are shown.
    pub frame: usize,
    /// The last error from the breakpoint field.
    pub error: Option<String>,
}

/// Reads `scripts/a.luau:12` as a file and a line.
pub(crate) fn parse_breakpoint(text: &str) -> Result<(String, u32), String> {
    let text = text.trim();
    let (path, line) = text
        .rsplit_once(':')
        .ok_or_else(|| format!("`{text}` is not path:line, such as scripts/game/main.luau:12"))?;
    let line: u32 = line
        .trim()
        .parse()
        .ok()
        .filter(|l| *l > 0)
        .ok_or_else(|| format!("`{line}` is not a line number"))?;
    let path = path.trim().replace('\\', "/");
    if path.is_empty() {
        return Err("the path is empty".into());
    }
    Ok((path, line))
}

/// Adds `line` to the breakpoints of `path`.
pub(crate) fn add_breakpoint(debugger: &Debugger, path: &str, line: u32) {
    let mut lines: Vec<u32> = debugger
        .breakpoints()
        .iter()
        .filter(|b| b.source == path)
        .map(|b| b.requested)
        .collect();
    if !lines.contains(&line) {
        lines.push(line);
        lines.sort_unstable();
        debugger.set_breakpoints(path, &lines);
    }
}

/// Removes the breakpoint requested on `line` of `path`.
pub(crate) fn remove_breakpoint(debugger: &Debugger, path: &str, line: u32) {
    let lines: Vec<u32> = debugger
        .breakpoints()
        .iter()
        .filter(|b| b.source == path && b.requested != line)
        .map(|b| b.requested)
        .collect();
    debugger.set_breakpoints(path, &lines);
}

pub(crate) fn ui(panel: &mut DebugPanel, ui: &mut egui::Ui) {
    let Some(debugger) = panel.debugger.clone() else {
        ui.label(RichText::new("No script debugger is attached.").weak());
        return;
    };
    let stopped = debugger.stopped();
    ui.horizontal(|ui| {
        let can = stopped.is_some();
        // No keys are bound to these, so the hover says what each does.
        for (label, how, what) in [
            (
                "Continue",
                Resume::Continue,
                "Run until the next breakpoint",
            ),
            (
                "Step over",
                Resume::StepOver,
                "Run to the next line, over calls",
            ),
            (
                "Step in",
                Resume::StepIn,
                "Run to the next line, into calls",
            ),
            (
                "Step out",
                Resume::StepOut,
                "Run until this function returns",
            ),
        ] {
            if ui
                .add_enabled(can, egui::Button::new(label))
                .on_hover_text(what)
                .clicked()
            {
                debugger.resume(how);
            }
        }
        if ui
            .add_enabled(!can, egui::Button::new("Pause"))
            .on_hover_text("Stop at the next line any script runs")
            .clicked()
        {
            debugger.pause();
        }
    });
    ui.separator();

    egui::ScrollArea::vertical()
        .id_salt("re_editor.debugger")
        .auto_shrink(false)
        .show(ui, |ui| {
            breakpoints(panel, ui, &debugger);
            ui.separator();
            match &stopped {
                Some(s) => {
                    let names = debugger.contexts();
                    let context = names
                        .iter()
                        .find(|c| c.id == s.context)
                        .map_or("?", |c| c.name.as_str());
                    ui.strong(format!("Stopped in {context}: {}", s.reason.dap_name()));
                    stack(panel, ui, s);
                }
                None => match debugger.last_unpaused() {
                    Some(s) => {
                        ui.strong("Last stop (not paused)").on_hover_text(
                            "The script reached a breakpoint where it could not pause (RE3070); this is its stack then",
                        );
                        stack(panel, ui, &s);
                    }
                    None => {
                        ui.label(RichText::new("Running.").weak());
                    }
                },
            }
        });
}

fn breakpoints(panel: &mut DebugPanel, ui: &mut egui::Ui, debugger: &Debugger) {
    ui.strong("Breakpoints");
    for bp in debugger.breakpoints() {
        ui.horizontal(|ui| {
            if ui.small_button("✕").on_hover_text("Remove").clicked() {
                remove_breakpoint(debugger, &bp.source, bp.requested);
            }
            let text = format!("{}:{}", bp.source, bp.requested);
            match bp.line {
                Some(l) if l != bp.requested => ui.label(format!("{text} (stops on {l})")),
                Some(_) => ui.label(text),
                None => ui.label(RichText::new(text).weak()).on_hover_text(
                    "Not placed: no loaded chunk of this file has code at or after this line",
                ),
            };
        });
    }
    ui.horizontal(|ui| {
        let field = ui.add(
            egui::TextEdit::singleline(&mut panel.new_breakpoint)
                .hint_text("scripts/game/main.luau:12")
                .desired_width(220.0),
        );
        let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if ui.button("Add").clicked() || enter {
            match parse_breakpoint(&panel.new_breakpoint) {
                Ok((path, line)) => {
                    add_breakpoint(debugger, &path, line);
                    panel.new_breakpoint.clear();
                    panel.error = None;
                }
                Err(e) => panel.error = Some(e),
            }
        }
    });
    if let Some(e) = &panel.error {
        ui.colored_label(egui::Color32::from_rgb(235, 90, 80), e);
    }
}

fn stack(panel: &mut DebugPanel, ui: &mut egui::Ui, s: &Stopped) {
    if panel.frame >= s.frames.len() {
        panel.frame = 0;
    }
    for (i, f) in s.frames.iter().enumerate() {
        let at = match (&f.source, f.line) {
            (Some(src), Some(line)) => format!("{}  {src}:{line}", f.name),
            _ => format!("{}  [Rust]", f.name),
        };
        if ui.selectable_label(panel.frame == i, at).clicked() {
            panel.frame = i;
        }
    }
    let Some(frame) = s.frames.get(panel.frame) else {
        return;
    };
    for scope in &frame.scopes {
        egui::CollapsingHeader::new(&scope.name)
            .id_salt(("re_editor.debugger.scope", panel.frame, &scope.name))
            .default_open(true)
            .show(ui, |ui| variables(ui, s, s.list(scope.variables), 0));
    }
}

fn variables(ui: &mut egui::Ui, s: &Stopped, list: &[Variable], depth: usize) {
    for v in list {
        match v.children {
            Some(c) if depth < 8 => {
                egui::CollapsingHeader::new(format!("{} = {}", v.name, v.value))
                    .id_salt(("re_editor.debugger.var", c, depth, &v.name))
                    .show(ui, |ui| variables(ui, s, s.list(c), depth + 1));
            }
            _ => {
                ui.label(RichText::new(format!("{} = {}", v.name, v.value)).monospace())
                    .on_hover_text(&v.type_name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoint_text_is_path_colon_line() {
        assert_eq!(
            parse_breakpoint(" scripts/a.luau:12 "),
            Ok(("scripts/a.luau".into(), 12))
        );
        assert_eq!(
            parse_breakpoint(r"scripts\b.luau:3"),
            Ok(("scripts/b.luau".into(), 3))
        );
        assert!(parse_breakpoint("scripts/a.luau").is_err());
        assert!(parse_breakpoint("scripts/a.luau:0").is_err());
        assert!(parse_breakpoint(":4").is_err());
    }

    #[test]
    fn breakpoints_are_added_once_and_removed() {
        let d = Debugger::new();
        add_breakpoint(&d, "scripts/a.luau", 5);
        add_breakpoint(&d, "scripts/a.luau", 2);
        add_breakpoint(&d, "scripts/a.luau", 5);
        let lines: Vec<u32> = d.breakpoints().iter().map(|b| b.requested).collect();
        assert_eq!(lines, vec![2, 5]);
        remove_breakpoint(&d, "scripts/a.luau", 2);
        let lines: Vec<u32> = d.breakpoints().iter().map(|b| b.requested).collect();
        assert_eq!(lines, vec![5]);
    }
}
