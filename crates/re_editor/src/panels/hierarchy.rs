//! The hierarchy: the entity tree, with buttons to add and delete entities.
//!
//! A click selects one entity, Ctrl+click (Cmd+click on macOS) adds or
//! removes one, and Shift+click adds the rows from the active entity to the
//! clicked one, in the order they are shown.

use egui::RichText;
use re_scene::hierarchy::{children, parent};
use re_scene::{Entity, Generated, World};
use serde_json::json;

use crate::editor::{Action, Editor, command};
use crate::selection::Selection;

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
            .on_hover_text("New entity (under the active one)")
            .clicked()
        {
            let mut args = json!({ "name": "Entity" });
            if let Some(sel) = ed.selection.active().filter(|s| world.entity(s).is_ok()) {
                args["parent"] = json!(sel);
            }
            actions.push(command("entity.spawn", args));
        }
        let delete = ed.delete_action(world);
        if ui
            .add_enabled(delete.is_some(), egui::Button::new("Delete").small())
            .on_hover_text("Delete the selected entities and their children (one undo step)")
            .clicked()
        {
            actions.extend(delete);
            ed.selection.clear();
        }
    });
    ui.separator();
    let mut roots: Vec<(&str, Entity)> = world
        .ids()
        .iter()
        .filter(|(_, e)| parent(&world.ecs, *e).is_none())
        .collect();
    roots.sort();
    let mut clicked: Option<String> = None;
    let mut rows = Vec::new();
    egui::ScrollArea::vertical()
        .id_salt("re_editor.hierarchy")
        .auto_shrink(false)
        .show(ui, |ui| {
            if roots.is_empty() {
                ui.label(RichText::new("The scene is empty.").weak());
            }
            for (_, e) in roots {
                tree_node(ui, world, e, &ed.selection, &mut clicked, &mut rows);
            }
        });
    if let Some(id) = clicked {
        let modifiers = ui.input(|i| i.modifiers);
        let order: Vec<&str> = rows.iter().map(|(id, _)| id.as_str()).collect();
        ed.select_next = Some(clicked_selection(&ed.selection, &id, modifiers, &order));
        ui.ctx().request_repaint();
    }
    // Applied at the end of the next frame (see `draw`).
    for (id, rect) in rows {
        ed.pending
            .rects
            .insert(crate::editor::row_key(&id), vec![rect]);
    }
}

/// The selection after a click on the row of `id`, with `modifiers` held,
/// in a tree showing the rows `order`: as in the viewport
/// ([`Selection::clicked`]), except that Shift adds the rows from the active
/// one to the clicked one.
fn clicked_selection(
    selection: &Selection,
    id: &str,
    modifiers: egui::Modifiers,
    order: &[&str],
) -> Selection {
    if !modifiers.shift || modifiers.command {
        return selection.clicked(id, modifiers);
    }
    let mut next = selection.clone();
    let at = |id: &str| order.iter().position(|r| *r == id);
    if let (Some(a), Some(b)) = (selection.active().and_then(at), at(id)) {
        for row in &order[a.min(b)..=a.max(b)] {
            next.add(row);
        }
    }
    next.add(id);
    next
}

fn tree_node(
    ui: &mut egui::Ui,
    world: &World,
    e: Entity,
    selection: &Selection,
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
    let is_selected = selection.contains(id);
    if selection.len() > 1 && selection.active() == Some(id) {
        text = text.strong();
    }
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
                tree_node(ui, world, *kid, selection, clicked, rows);
            }
        });
}

#[cfg(test)]
mod tests {
    use egui::Modifiers;

    use super::*;

    #[test]
    fn shift_click_adds_the_rows_from_the_active_one() {
        let order = ["a", "b", "c", "d", "e"];
        let next = clicked_selection(&Selection::one("b"), "d", Modifiers::SHIFT, &order);
        assert_eq!(next.ids(), ["b", "c", "d"]);
        assert_eq!(next.active(), Some("d"));
        // Upwards from the active one, keeping what is selected.
        let next = clicked_selection(&next, "a", Modifiers::SHIFT, &order);
        assert_eq!(next.ids(), ["b", "c", "d", "a"]);
        assert_eq!(next.active(), Some("a"));
        // With nothing selected, or the active one's row not shown, only the
        // clicked one is added.
        let next = clicked_selection(&Selection::new(), "c", Modifiers::SHIFT, &order);
        assert_eq!(next, Selection::one("c"));
        let next = clicked_selection(&Selection::one("x"), "c", Modifiers::SHIFT, &order);
        assert_eq!(next.ids(), ["x", "c"]);
        // Ctrl+Shift toggles, as Ctrl does.
        let next = clicked_selection(
            &Selection::one("b"),
            "b",
            Modifiers::COMMAND | Modifiers::SHIFT,
            &order,
        );
        assert!(next.is_empty());
    }
}
