//! The hierarchy: the entity tree, with buttons to add and delete entities.

use egui::RichText;
use re_scene::hierarchy::{children, parent};
use re_scene::{Entity, Generated, World};
use serde_json::json;

use crate::editor::{Action, Editor, command};

pub(super) fn hierarchy(
    ed: &mut Editor,
    ui: &mut egui::Ui,
    world: &World,
    actions: &mut Vec<Action>,
) {
    ui.horizontal(|ui| {
        ui.heading("Hierarchy");
        if ui
            .small_button("+")
            .on_hover_text("New entity (under the selected one)")
            .clicked()
        {
            let mut args = json!({ "name": "Entity" });
            if let Some(sel) = ed.selected.as_ref().filter(|s| world.entity(s).is_ok()) {
                args["parent"] = json!(sel);
            }
            actions.push(command("entity.spawn", args));
        }
        let can_delete = ed
            .selected
            .as_ref()
            .is_some_and(|s| world.entity(s).is_ok());
        if ui
            .add_enabled(can_delete, egui::Button::new("Delete").small())
            .on_hover_text("Delete the selected entity and its children")
            .clicked()
            && let Some(sel) = ed.selected.take()
        {
            actions.push(command("entity.delete", json!({ "id": sel })));
        }
    });
    ui.separator();
    let mut roots: Vec<(&str, Entity)> = world
        .ids()
        .iter()
        .filter(|(_, e)| parent(&world.ecs, *e).is_none())
        .collect();
    roots.sort();
    let mut clicked = None;
    let mut rows = Vec::new();
    egui::ScrollArea::vertical()
        .id_salt("re_editor.hierarchy")
        .auto_shrink(false)
        .show(ui, |ui| {
            if roots.is_empty() {
                ui.label(RichText::new("The scene is empty.").weak());
            }
            for (_, e) in roots {
                tree_node(
                    ui,
                    world,
                    e,
                    ed.selected.as_deref(),
                    &mut clicked,
                    &mut rows,
                );
            }
        });
    // Applied at the end of the next frame (see `draw`).
    for (id, rect) in rows {
        ed.pending
            .rects
            .insert(crate::editor::row_key(&id), vec![rect]);
    }
    if clicked.is_some() {
        ed.select_next = clicked;
        ui.ctx().request_repaint();
    }
}

fn tree_node(
    ui: &mut egui::Ui,
    world: &World,
    e: Entity,
    selected: Option<&str>,
    clicked: &mut Option<String>,
    rows: &mut Vec<(String, egui::Rect)>,
) {
    let Some(id) = world.id_of(e) else {
        return;
    };
    let name = world.name_of(e);
    let mut text = if name.is_empty() {
        RichText::new(id)
    } else {
        RichText::new(format!("{name} · {id}"))
    };
    if world.ecs.get::<Generated>(e).is_some() {
        text = text.italics();
    }
    let is_selected = selected == Some(id);
    let kids = children(&world.ecs, e);
    if kids.is_empty() {
        let r = ui.selectable_label(is_selected, text);
        rows.push((id.to_string(), r.rect));
        if r.clicked() {
            *clicked = Some(id.to_string());
        }
        return;
    }
    let state = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        ui.make_persistent_id(("re_editor.tree", id)),
        false,
    );
    state
        .show_header(ui, |ui| {
            let r = ui.selectable_label(is_selected, text);
            rows.push((id.to_string(), r.rect));
            if r.clicked() {
                *clicked = Some(id.to_string());
            }
        })
        .body(|ui| {
            for kid in kids {
                tree_node(ui, world, *kid, selected, clicked, rows);
            }
        });
}
