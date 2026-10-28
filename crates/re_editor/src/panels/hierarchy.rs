//! The hierarchy: the entity tree, with buttons to add and delete entities,
//! renaming in place and a context menu.
//!
//! A click selects one entity, Ctrl+click (Cmd+click on macOS) adds or
//! removes one, and Shift+click adds the rows from the active entity to the
//! clicked one, in the order they are shown; a click under the rows clears
//! the selection. F2 or a double-click renames an entity in its row (Enter
//! or a click elsewhere keeps the name, Escape drops it). A right click on a
//! row that is not selected selects it, and opens the menu for the
//! selection; under the rows, the menu adds or pastes at the scene's root.
//!
//! When another entity becomes the active one (a click in the view, a
//! paste, Ctrl+Up or Ctrl+Down), the rows above it unfold and the list
//! scrolls to it.
//!
//! Dragging rows moves entities ([`crate::reparent`]): onto a row puts them
//! under it, between two rows beside it, and under the rows at the root. A
//! selected row drags the selection.
//!
//! The search shows the entities whose name or id holds its text, or with
//! `c:` and a name, those with a component whose name holds it (ignoring
//! case), each with the rows above it. It is kept with the layout.

use std::collections::HashSet;

use egui::{Key, KeyboardShortcut, Modifiers, RichText};
use re_app::App;
use re_scene::hierarchy::{children, parent};
use re_scene::{Entity, Generated, World};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::clipboard::Place;
use crate::editor::{Action, Editor, command, menu_key, row_key, search_key};
use crate::reparent::{self, Drop};
use crate::selection::Selection;

/// The Hierarchy's entry in the layout's `panels`.
const PANEL: &str = "hierarchy";

/// The Hierarchy's view state, kept in the layout.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct View {
    /// The search text.
    search: String,
}

/// What a search shows.
enum Filter {
    /// Entities whose name or id holds this (lowercase).
    Text(String),
    /// Entities with a component whose name holds this (lowercase).
    Component(String),
}

impl Filter {
    fn new(search: &str) -> Option<Filter> {
        let search = search.trim();
        if search.is_empty() {
            return None;
        }
        let component = search
            .get(..2)
            .filter(|p| p.eq_ignore_ascii_case("c:"))
            .map(|_| search[2..].trim());
        match component {
            Some("") => None,
            Some(name) => Some(Filter::Component(name.to_lowercase())),
            None => Some(Filter::Text(search.to_lowercase())),
        }
    }

    fn matches(&self, world: &World, e: Entity, id: &str) -> bool {
        match self {
            Filter::Text(text) => {
                id.to_lowercase().contains(text) || world.name_of(e).to_lowercase().contains(text)
            }
            Filter::Component(name) => world
                .registry
                .components_of(&world.ecs, e)
                .iter()
                .any(|k| world.registry.schema(*k).name.to_lowercase().contains(name)),
        }
    }
}

/// The rows a search shows: the matches and the entities above them.
struct Found {
    matched: HashSet<Entity>,
    shown: HashSet<Entity>,
}

impl Found {
    fn new(world: &World, filter: &Filter) -> Found {
        let mut found = Found {
            matched: HashSet::new(),
            shown: HashSet::new(),
        };
        for (id, e) in world.ids().iter() {
            if !filter.matches(world, e, id) {
                continue;
            }
            found.matched.insert(e);
            let mut up = Some(e);
            while let Some(u) = up
                && found.shown.insert(u)
            {
                up = parent(&world.ecs, u);
            }
        }
        found
    }
}

/// The entities a row drag carries.
struct Dragged(Vec<String>);

/// Why a generated entity has no Rename (the `rename` op refuses it).
pub(crate) const GENERATED_NAME: &str =
    "a generated entity's name comes from its procedural source";

/// An entity being renamed in its row.
#[derive(Clone, Debug)]
pub(crate) struct Rename {
    pub(crate) id: String,
    /// The name typed so far.
    pub(crate) text: String,
    /// Whether the text field has asked for the keyboard (the first time it
    /// is drawn).
    focused: bool,
}

impl Rename {
    pub(crate) fn new(id: &str, name: &str) -> Rename {
        Rename {
            id: id.to_string(),
            text: name.to_string(),
            focused: false,
        }
    }
}

pub(super) fn hierarchy(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    let world = app.world();
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
            ed.change_selection(Selection::new());
        }
    });
    ui.separator();
    let mut view: View = ed.panel_state(PANEL);
    ui.horizontal(|ui| {
        let clear = !view.search.is_empty();
        let width = ui.available_width() - if clear { 28.0 } else { 0.0 };
        let r = ui.add(
            egui::TextEdit::singleline(&mut view.search)
                .hint_text("Search: name, id or c:Component")
                .desired_width(width),
        );
        ed.pending.rects.insert(search_key(), vec![r.rect]);
        if clear
            && ui
                .small_button("×")
                .on_hover_text("Clear the search")
                .clicked()
        {
            view.search.clear();
        }
    });
    ed.set_panel_state(PANEL, &view);
    let found = Filter::new(&view.search).map(|f| Found::new(world, &f));
    let mut roots: Vec<(&str, Entity)> = world
        .ids()
        .iter()
        .filter(|(_, e)| parent(&world.ecs, *e).is_none())
        .collect();
    roots.sort();
    // A newly active entity is revealed.
    let active = ed.selection.active().map(str::to_string);
    let reveal = active.clone().filter(|_| active != ed.revealed);
    ed.revealed = active;
    let mut unfold = HashSet::new();
    if let Some(e) = reveal.as_ref().and_then(|id| world.entity(id).ok()) {
        let mut above = parent(&world.ecs, e);
        while let Some(p) = above {
            unfold.insert(p);
            above = parent(&world.ecs, p);
        }
    }
    let mut tree = Tree {
        selection: ed.selection.clone(),
        ed,
        app,
        actions,
        reveal,
        unfold,
        found,
        clicked: None,
        dropped: None,
        rows: Vec::new(),
    };
    egui::ScrollArea::vertical()
        .id_salt("re_editor.hierarchy")
        .auto_shrink(false)
        .show(ui, |ui| {
            if roots.is_empty() {
                ui.label(RichText::new("The scene is empty.").weak());
            } else if tree.found.as_ref().is_some_and(|f| f.shown.is_empty()) {
                ui.label(RichText::new("No entity matches the search.").weak());
            }
            for (_, e) in roots {
                tree.node(ui, e);
            }
            // The space under the rows.
            let size = egui::vec2(ui.available_width(), ui.available_height().max(24.0));
            let rest = ui.allocate_response(size, egui::Sense::click());
            if rest.clicked() {
                tree.ed.select_next = Some(Selection::new());
            }
            if rest.dnd_hover_payload::<Dragged>().is_some() {
                paint_drop(ui, rest.rect, &Drop::Root);
                if let Some(dragged) = rest.dnd_release_payload::<Dragged>() {
                    tree.dropped = Some((dragged.0.clone(), Drop::Root));
                }
            }
            rest.context_menu(|ui| tree.root_menu(ui));
        });
    let Tree {
        ed,
        actions,
        clicked,
        dropped,
        rows,
        ..
    } = tree;
    if let Some((ids, drop)) = dropped {
        let moves = reparent::plan(world, &ids, &drop);
        actions.extend(moves.refused.into_iter().map(Action::Report));
        if let Some(args) = moves.apply {
            actions.push(command("scene.apply", args));
        }
        for args in moves.proc_moves {
            actions.push(command("proc.move", args));
        }
    }
    // What a drag carries follows the pointer.
    if let Some(dragged) = egui::DragAndDrop::payload::<Dragged>(ui.ctx())
        && let Some(at) = ui.ctx().pointer_interact_pos()
    {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        let text = match dragged.0.as_slice() {
            [one] => one.clone(),
            ids => format!("{} entities", ids.len()),
        };
        egui::Area::new(egui::Id::new("re_editor.hierarchy.drag"))
            .order(egui::Order::Tooltip)
            .fixed_pos(at + egui::vec2(14.0, 10.0))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(text));
            });
    }
    if let Some(id) = clicked {
        let modifiers = ui.input(|i| i.modifiers);
        let order: Vec<&str> = rows.iter().map(|(id, _)| id.as_str()).collect();
        ed.select_next = Some(clicked_selection(&ed.selection, &id, modifiers, &order));
        ui.ctx().request_repaint();
    }
    // A rename whose row is no longer shown (its entity was deleted, or a
    // row above it folded) is dropped.
    if ed
        .renaming
        .as_ref()
        .is_some_and(|r| !rows.iter().any(|(id, _)| *id == r.id))
    {
        ed.renaming = None;
    }
    // Applied at the end of the next frame (see `draw`).
    for (id, rect) in rows {
        ed.pending.rects.insert(row_key(&id), vec![rect]);
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

/// The `scene.apply` of renaming entity `id` to `name` (one undo step).
fn rename_action(id: &str, name: &str) -> Action {
    command(
        "scene.apply",
        json!({
            "label": format!("rename {id}"),
            "ops": [{ "op": "rename", "id": id, "name": name }],
        }),
    )
}

/// The tree being drawn.
struct Tree<'a> {
    ed: &'a mut Editor,
    app: &'a App,
    actions: &'a mut Vec<Action>,
    /// The selection the rows show: as it was when the frame began.
    selection: Selection,
    /// The entity to scroll to, and the entities above it, to unfold.
    reveal: Option<String>,
    unfold: HashSet<Entity>,
    /// While searching, the rows shown.
    found: Option<Found>,
    /// What was dropped this frame, and where.
    dropped: Option<(Vec<String>, Drop)>,
    /// The row clicked this frame.
    clicked: Option<String>,
    /// Each row drawn, in order, and where.
    rows: Vec<(String, egui::Rect)>,
}

impl Tree<'_> {
    fn node(&mut self, ui: &mut egui::Ui, e: Entity) {
        let world = self.app.world();
        let Some(id) = world.id_of(e) else {
            return;
        };
        let kids = children(&world.ecs, e);
        match self.found.as_ref().map(|f| f.shown.contains(&e)) {
            Some(false) => return,
            Some(true) => {
                // While searching, every row on the way to a match is open.
                self.row(ui, e, id);
                ui.indent(("re_editor.found", id), |ui| {
                    for kid in kids {
                        self.node(ui, *kid);
                    }
                });
                return;
            }
            None => {}
        }
        if kids.is_empty() {
            self.row(ui, e, id);
            return;
        }
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            ui.make_persistent_id(("re_editor.tree", id)),
            false,
        );
        if self.unfold.contains(&e) {
            state.set_open(true);
        }
        state.show_header(ui, |ui| self.row(ui, e, id)).body(|ui| {
            for kid in kids {
                self.node(ui, *kid);
            }
        });
    }

    /// One row: the entity's name and id, or the text field renaming it.
    fn row(&mut self, ui: &mut egui::Ui, e: Entity, id: &str) {
        let world = self.app.world();
        if let Some(rename) = self.ed.renaming.as_mut().filter(|r| r.id == id) {
            let mut output = egui::TextEdit::singleline(&mut rename.text)
                .hint_text(id)
                .desired_width(ui.available_width().min(240.0))
                .show(ui);
            let r = output.response.response;
            if !rename.focused {
                // The whole name is selected, so what is typed replaces it.
                r.request_focus();
                let all = egui::text::CCursorRange::select_all(&output.galley);
                output.state.cursor.set_char_range(Some(all));
                output.state.store(ui.ctx(), r.id);
                rename.focused = true;
            }
            self.rows.push((id.to_string(), r.rect));
            if r.lost_focus() {
                let cancelled = ui.input(|i| i.key_pressed(Key::Escape));
                let text = std::mem::take(&mut rename.text);
                self.ed.renaming = None;
                if !cancelled && text != world.name_of(e) {
                    self.actions.push(rename_action(id, &text));
                }
            }
            return;
        }
        let name = world.name_of(e);
        let mut text = if name.is_empty() {
            RichText::new(id)
        } else {
            RichText::new(format!("{name} · {id}"))
        };
        let generated = world.ecs.get::<Generated>(e).is_some();
        if generated {
            text = text.italics();
        }
        if self.selection.len() > 1 && self.selection.active() == Some(id) {
            text = text.strong();
        }
        if self.found.as_ref().is_some_and(|f| !f.matched.contains(&e)) {
            text = text.weak();
        }
        let selected = self.selection.contains(id);
        let mut r =
            ui.add(egui::Button::selectable(selected, text).sense(egui::Sense::click_and_drag()));
        let note = world.note_of(e);
        if !note.is_empty() {
            r = r.on_hover_text(note);
        }
        self.rows.push((id.to_string(), r.rect));
        if self.reveal.as_deref() == Some(id) {
            r.scroll_to_me(None);
        }
        if r.double_clicked() {
            self.ed.start_rename(world, id);
        } else if r.clicked() {
            self.clicked = Some(id.to_string());
        }
        if r.secondary_clicked() && !selected {
            self.ed.select_next = Some(Selection::one(id));
        }
        // A selected row drags the selection's topmost entities.
        if r.drag_started() {
            let ids = if selected {
                self.selection.topmost(world)
            } else {
                vec![id.to_string()]
            };
            egui::DragAndDrop::set_payload(ui.ctx(), Dragged(ids));
        }
        if let Some(dragged) = r.dnd_hover_payload::<Dragged>()
            && !dragged.0.iter().any(|d| d == id)
            && let Some(at) = ui.ctx().pointer_interact_pos()
        {
            let height = r.rect.height().max(1.0);
            let drop = match (at.y - r.rect.top()) / height {
                y if y < 0.25 => Drop::Before(id.to_string()),
                y if y > 0.75 => Drop::After(id.to_string()),
                _ => Drop::Into(id.to_string()),
            };
            paint_drop(ui, r.rect, &drop);
            if let Some(dragged) = r.dnd_release_payload::<Dragged>() {
                self.dropped = Some((dragged.0.clone(), drop));
            }
        }
        r.context_menu(|ui| self.menu(ui, e, id, generated));
    }

    /// The menu of a row, for the selection (which the row is part of).
    fn menu(&mut self, ui: &mut egui::Ui, e: Entity, id: &str, generated: bool) {
        let app = self.app;
        let world = app.world();
        let ed = &mut *self.ed;
        let actions = &mut *self.actions;
        if item(
            ui,
            ed,
            "Rename",
            shortcut(Modifiers::NONE, Key::F2),
            !generated,
        )
        .on_disabled_hover_text(format!("Not here: {GENERATED_NAME}"))
        .clicked()
        {
            ed.start_rename(world, id);
            ui.close();
        }
        ui.separator();
        if item(
            ui,
            ed,
            "Duplicate",
            shortcut(Modifiers::COMMAND, Key::D),
            true,
        )
        .clicked()
        {
            if let Some(clip) = ed.copy_clip(world) {
                actions.push(Action::Paste {
                    clip,
                    place: Place::AfterOriginals,
                });
            }
            ui.close();
        }
        if item(ui, ed, "Copy", shortcut(Modifiers::COMMAND, Key::C), true).clicked() {
            if let Some(text) = ed.copy_selection(world) {
                ui.ctx().copy_text(text);
            }
            ui.close();
        }
        if item(ui, ed, "Cut", shortcut(Modifiers::COMMAND, Key::X), true).clicked() {
            if let Some((text, delete)) = ed.cut(world) {
                ui.ctx().copy_text(text);
                actions.push(delete);
                ed.change_selection(Selection::new());
            }
            ui.close();
        }
        if item(ui, ed, "Paste", shortcut(Modifiers::COMMAND, Key::V), true).clicked() {
            ed.paste_request = Some(Place::Beside);
            ui.close();
        }
        if item(ui, ed, "Paste as child", None, true).clicked() {
            ed.paste_request = Some(Place::Under);
            ui.close();
        }
        if item(
            ui,
            ed,
            "Delete",
            shortcut(Modifiers::NONE, Key::Delete),
            true,
        )
        .clicked()
        {
            actions.extend(ed.delete_action(world));
            ed.change_selection(Selection::new());
            ui.close();
        }
        ui.separator();
        if item(ui, ed, "Focus", shortcut(Modifiers::NONE, Key::F), true).clicked() {
            ed.frame_selection(app, false);
            ui.close();
        }
        let has_children = !children(&world.ecs, e).is_empty();
        let select_children = shortcut(Modifiers::COMMAND, Key::ArrowDown);
        if item(ui, ed, "Select children", select_children, has_children).clicked() {
            ed.select_next = Some(ed.selection.children(world));
            ui.close();
        }
        super::procedural::entity_menu(ed, ui, app, id, actions);
    }

    /// The menu under the rows: a new entity, or a paste, at the root.
    fn root_menu(&mut self, ui: &mut egui::Ui) {
        let ed = &mut *self.ed;
        if item(ui, ed, "New entity", None, true).clicked() {
            self.actions
                .push(command("entity.spawn", json!({ "name": "Entity" })));
            ui.close();
        }
        if item(ui, ed, "Paste", shortcut(Modifiers::COMMAND, Key::V), true).clicked() {
            ed.paste_request = Some(Place::Root);
            ui.close();
        }
    }
}

/// Shows where a drop would go: a line between rows, or a frame around the
/// row it would go under.
fn paint_drop(ui: &egui::Ui, rect: egui::Rect, drop: &Drop) {
    let stroke = egui::Stroke::new(2.0, ui.visuals().selection.bg_fill);
    let painter = ui.painter();
    match drop {
        Drop::Before(_) | Drop::Root => {
            painter.hline(rect.x_range(), rect.top(), stroke);
        }
        Drop::After(_) => {
            painter.hline(rect.x_range(), rect.bottom(), stroke);
        }
        Drop::Into(_) => {
            painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
        }
    }
}

fn shortcut(modifiers: Modifiers, key: Key) -> Option<KeyboardShortcut> {
    Some(KeyboardShortcut::new(modifiers, key))
}

/// A menu item with its shortcut, recorded for [`Editor::menu_item`]. The
/// procedural entries of a row's menu (`procedural::entity_menu`) use it too.
pub(super) fn item(
    ui: &mut egui::Ui,
    ed: &mut Editor,
    label: &str,
    shortcut: Option<KeyboardShortcut>,
    enabled: bool,
) -> egui::Response {
    let mut button = egui::Button::new(label);
    if let Some(s) = shortcut {
        button = button.shortcut_text(ui.ctx().format_shortcut(&s));
    }
    let r = ui.add_enabled(enabled, button);
    ed.pending.rects.insert(menu_key(label), vec![r.rect]);
    r
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

    #[test]
    fn a_search_reads_text_or_a_component_name() {
        let read = |search: &str| match Filter::new(search) {
            None => "none".to_string(),
            Some(Filter::Text(t)) => format!("text {t}"),
            Some(Filter::Component(c)) => format!("component {c}"),
        };
        assert_eq!(read("  "), "none");
        assert_eq!(read(" Crate "), "text crate");
        assert_eq!(read("c: PointLight"), "component pointlight");
        assert_eq!(read("C:mesh"), "component mesh");
        // A prefix with no name yet shows every row.
        assert_eq!(read("c: "), "none");
        assert_eq!(read("cab"), "text cab");
    }
}
