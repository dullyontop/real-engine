//! The panels: toolbar, hierarchy, inspector, graph parameters, console,
//! command palette and the viewport. They read the [`App`] and push
//! [`Action`]s; [`Editor::ui`](crate::Editor::ui) applies them afterwards.

use std::collections::BTreeSet;

use egui::{Color32, RichText};
use glam::{EulerRot, Quat, Vec3};
use re_app::App;
use re_core::Diagnostic;
use re_reflect::{Color, FieldType, Value};
use re_scene::hierarchy::{children, parent};
use re_scene::{Entity, Generated, World};
use serde_json::{Map, Value as Json, json};

use crate::Panel;
use crate::codes;
use crate::editor::{Action, Editor, LineKind, Palette, Pending, command};

/// Draws every panel into the root `ui`.
pub(crate) fn draw(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    let r = egui::Panel::top("re_editor.toolbar").show(ui, |ui| toolbar(ed, ui, app, actions));
    ed.rects.set(Panel::Toolbar, r.response.rect);

    let r = egui::Panel::bottom("re_editor.console")
        .resizable(true)
        .default_size(160.0)
        .show(ui, |ui| console(ed, ui));
    ed.rects.set(Panel::Console, r.response.rect);

    let r = egui::Panel::left("re_editor.hierarchy")
        .resizable(true)
        .default_size(240.0)
        .show(ui, |ui| hierarchy(ed, ui, app.world(), actions));
    ed.rects.set(Panel::Hierarchy, r.response.rect);

    egui::Panel::right("re_editor.right")
        .resizable(true)
        .default_size(340.0)
        .show(ui, |ui| {
            let width = ui.available_width();
            let height = ui.available_height();
            let r = ui.allocate_ui(egui::vec2(width, height * 0.6), |ui| {
                ui.set_min_size(ui.max_rect().size());
                egui::ScrollArea::vertical()
                    .id_salt("re_editor.inspector")
                    .auto_shrink(false)
                    .show(ui, |ui| inspector(ed, ui, app.world(), actions));
            });
            ed.rects.set(Panel::Inspector, r.response.rect);
            ui.separator();
            let r = ui.allocate_ui(ui.available_size(), |ui| {
                ui.set_min_size(ui.max_rect().size());
                egui::ScrollArea::vertical()
                    .id_salt("re_editor.graph")
                    .auto_shrink(false)
                    .show(ui, |ui| graph(ed, ui, app, actions));
            });
            ed.rects.set(Panel::Graph, r.response.rect);
        });

    let r = egui::CentralPanel::no_frame().show(ui, |ui| viewport(ed, ui));
    ed.rects.set(Panel::Viewport, r.response.rect);

    palette(ed, ui.ctx(), app, actions);
}

// ---- toolbar ------------------------------------------------------------

fn toolbar(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
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
            actions.push(command("undo", json!({})));
        }
        let redo = ui
            .add_enabled(history.can_redo(), egui::Button::new("Redo"))
            .on_hover_text(match history.redo_label() {
                Some(label) => format!("Redo {label} (Ctrl+Shift+Z)"),
                None => "Nothing to redo".to_string(),
            });
        if redo.clicked() {
            actions.push(command("redo", json!({})));
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
        ui.label(RichText::new(&ed.status).weak());
    });
}

// ---- hierarchy ----------------------------------------------------------

fn hierarchy(ed: &mut Editor, ui: &mut egui::Ui, world: &World, actions: &mut Vec<Action>) {
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
    egui::ScrollArea::vertical()
        .id_salt("re_editor.hierarchy")
        .auto_shrink(false)
        .show(ui, |ui| {
            if roots.is_empty() {
                ui.label(RichText::new("The scene is empty.").weak());
            }
            for (_, e) in roots {
                tree_node(ui, world, e, ed.selected.as_deref(), &mut clicked);
            }
        });
    if let Some(id) = clicked {
        select(ed, Some(id));
    }
}

fn select(ed: &mut Editor, id: Option<String>) {
    if ed.selected != id {
        ed.pending = Pending::default();
    }
    ed.selected = id;
}

fn tree_node(
    ui: &mut egui::Ui,
    world: &World,
    e: Entity,
    selected: Option<&str>,
    clicked: &mut Option<String>,
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
        if ui.selectable_label(is_selected, text).clicked() {
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
            if ui.selectable_label(is_selected, text).clicked() {
                *clicked = Some(id.to_string());
            }
        })
        .body(|ui| {
            for kid in kids {
                tree_node(ui, world, *kid, selected, clicked);
            }
        });
}

// ---- inspector ----------------------------------------------------------

fn inspector(ed: &mut Editor, ui: &mut egui::Ui, world: &World, actions: &mut Vec<Action>) {
    ui.heading("Inspector");
    let Some(id) = ed.selected.clone() else {
        ui.label(RichText::new("Select an entity in the hierarchy.").weak());
        return;
    };
    let Ok(e) = world.entity(&id) else {
        ui.label(RichText::new(format!("{id} no longer exists.")).weak());
        return;
    };
    let name = world.name_of(e);
    ui.label(RichText::new(if name.is_empty() { &id } else { name }).strong());
    ui.label(RichText::new(&id).monospace().weak());
    if let Some(g) = world.ecs.get::<Generated>(e) {
        let source = world.id_of(g.source).unwrap_or("?");
        ui.label(
            RichText::new(format!(
                "Generated by {source} (key {}): edits are kept as overrides.",
                g.key
            ))
            .weak(),
        );
    }
    let registry = &world.registry;
    let present = registry.components_of(&world.ecs, e);
    for key in &present {
        let schema = registry.schema(*key);
        let values = registry.get_all(&world.ecs, e, *key).unwrap_or_default();
        egui::CollapsingHeader::new(RichText::new(&schema.name).strong())
            .id_salt(("re_editor.component", &id, &schema.name))
            .default_open(true)
            .show(ui, |ui| {
                if !schema.doc.is_empty() {
                    ui.label(RichText::new(&schema.doc).weak().small());
                }
                egui::Grid::new(("re_editor.fields", &id, &schema.name))
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for (field, value) in schema.fields.iter().zip(&values) {
                            ui.label(&field.name).on_hover_text(&field.doc);
                            let target = FieldTarget {
                                id: &id,
                                component: &schema.name,
                                field: &field.name,
                                ty: &field.ty,
                            };
                            field_widget(ui, &mut ed.pending, world, &target, value, actions);
                            ui.end_row();
                        }
                    });
                if ui.small_button("Remove component").clicked() {
                    actions.push(command(
                        "component.remove",
                        json!({ "id": id, "component": schema.name }),
                    ));
                }
            });
    }
    let have: BTreeSet<&str> = present
        .iter()
        .map(|k| registry.schema(*k).name.as_str())
        .collect();
    let mut missing: Vec<&str> = registry
        .schemas()
        .map(|s| s.name.as_str())
        .filter(|n| !have.contains(n))
        .collect();
    missing.sort_unstable();
    ui.separator();
    egui::ComboBox::from_id_salt(("re_editor.add_component", &id))
        .selected_text("Add component…")
        .show_ui(ui, |ui| {
            for name in missing {
                if ui.selectable_label(false, name).clicked() {
                    actions.push(command(
                        "component.add",
                        json!({ "id": id, "component": name }),
                    ));
                }
            }
        });
}

/// The field an inspector widget edits.
struct FieldTarget<'a> {
    id: &'a str,
    component: &'a str,
    field: &'a str,
    ty: &'a FieldType,
}

impl FieldTarget<'_> {
    fn key(&self) -> String {
        format!("{}\u{1f}{}\u{1f}{}", self.id, self.component, self.field)
    }

    fn set(&self, value: Value) -> Action {
        Action::SetField {
            id: self.id.to_string(),
            component: self.component.to_string(),
            field: self.field.to_string(),
            ty: self.ty.clone(),
            value,
        }
    }
}

/// One widget per field type. Values being dragged or typed are kept in
/// `pending` and committed once (one `entity.set`, one undo step) when the
/// drag ends or the field loses focus.
fn field_widget(
    ui: &mut egui::Ui,
    pending: &mut Pending,
    world: &World,
    target: &FieldTarget<'_>,
    current: &Value,
    actions: &mut Vec<Action>,
) {
    let key = target.key();
    let shown = pending
        .values
        .get(&key)
        .cloned()
        .unwrap_or_else(|| current.clone());
    match (target.ty, &shown) {
        (FieldType::Bool, Value::Bool(b)) => {
            let mut b = *b;
            if ui.checkbox(&mut b, "").changed() {
                actions.push(target.set(Value::Bool(b)));
            }
        }
        (FieldType::I32 | FieldType::I64, Value::Int(i)) => {
            let mut i = *i;
            let mut drag = egui::DragValue::new(&mut i).speed(0.1);
            if *target.ty == FieldType::I32 {
                drag = drag.range(i32::MIN..=i32::MAX);
            }
            let r = ui.add(drag);
            settle(
                ui,
                &r,
                pending,
                key,
                Value::Int(i),
                current,
                target,
                actions,
            );
        }
        (FieldType::F32 | FieldType::F64, Value::Float(x)) => {
            let mut x = *x;
            let r = ui.add(egui::DragValue::new(&mut x).speed(0.01).max_decimals(6));
            settle(
                ui,
                &r,
                pending,
                key,
                Value::Float(x),
                current,
                target,
                actions,
            );
        }
        (FieldType::Vec3, Value::Vec3(v)) => {
            let mut a = v.to_array();
            let r = ui
                .horizontal(|ui| {
                    let mut r = ui.add(egui::DragValue::new(&mut a[0]).speed(0.01).prefix("x "));
                    r |= ui.add(egui::DragValue::new(&mut a[1]).speed(0.01).prefix("y "));
                    r |= ui.add(egui::DragValue::new(&mut a[2]).speed(0.01).prefix("z "));
                    r
                })
                .inner;
            let value = Value::Vec3(Vec3::from_array(a));
            settle(ui, &r, pending, key, value, current, target, actions);
        }
        (FieldType::Quat, Value::Quat(q)) => {
            // Shown as Euler angles in degrees (yaw, pitch, roll: Y, X, Z).
            let (y, x, z) = q.to_euler(EulerRot::YXZ);
            let mut a = [x.to_degrees(), y.to_degrees(), z.to_degrees()];
            let r = ui
                .horizontal(|ui| {
                    let mut r = ui.add(egui::DragValue::new(&mut a[0]).speed(0.5).suffix("°"));
                    r |= ui.add(egui::DragValue::new(&mut a[1]).speed(0.5).suffix("°"));
                    r |= ui.add(egui::DragValue::new(&mut a[2]).speed(0.5).suffix("°"));
                    r
                })
                .inner
                .on_hover_text("rotation about X, Y and Z in degrees (applied Y, then X, then Z)");
            let value = if r.changed() {
                Value::Quat(Quat::from_euler(
                    EulerRot::YXZ,
                    a[1].to_radians(),
                    a[0].to_radians(),
                    a[2].to_radians(),
                ))
            } else {
                shown.clone()
            };
            settle(ui, &r, pending, key, value, current, target, actions);
        }
        (FieldType::Color, Value::Color(c)) => {
            let mut rgba = [c.r, c.g, c.b, c.a];
            let r = ui.color_edit_button_rgba_unmultiplied(&mut rgba);
            let value = Value::Color(Color::rgba(rgba[0], rgba[1], rgba[2], rgba[3]));
            settle(ui, &r, pending, key, value, current, target, actions);
        }
        (FieldType::String, Value::Str(s)) => {
            if let Some(text) = text_field(ui, pending, &key, s) {
                actions.push(target.set(Value::Str(text)));
            }
        }
        (FieldType::Entity, Value::Entity(e)) => {
            let shown_id = e.and_then(|e| world.id_of(e)).unwrap_or("");
            if let Some(text) = text_field(ui, pending, &key, shown_id) {
                let text = text.trim();
                if text.is_empty() {
                    actions.push(target.set(Value::Entity(None)));
                } else {
                    match world.entity(text) {
                        Ok(e) => actions.push(target.set(Value::Entity(Some(e)))),
                        Err(_) => actions.push(Action::Report(
                            Diagnostic::error(
                                codes::INVALID_FIELD_TEXT,
                                format!(
                                    "no entity has the id {text:?} (for {}.{} of {})",
                                    target.component, target.field, target.id
                                ),
                            )
                            .with_hint(
                                "type the id of an existing entity, as the hierarchy shows it, \
                                 or clear the field for no entity",
                            ),
                        )),
                    }
                }
            }
        }
        (FieldType::Enum(variants), Value::Enum(v)) => {
            egui::ComboBox::from_id_salt(("re_editor.enum", &key))
                .selected_text(v.as_str())
                .show_ui(ui, |ui| {
                    for variant in variants.iter() {
                        if ui.selectable_label(v == variant, *variant).clicked() && v != variant {
                            actions.push(target.set(Value::Enum((*variant).to_string())));
                        }
                    }
                });
        }
        (_, other) => {
            ui.label(RichText::new(other.describe()).weak());
        }
    }
}

/// Keeps a dragged or typed value pending while its widget is in use, and
/// commits it once when the widget is released.
#[allow(clippy::too_many_arguments)]
fn settle(
    ui: &egui::Ui,
    r: &egui::Response,
    pending: &mut Pending,
    key: String,
    value: Value,
    current: &Value,
    target: &FieldTarget<'_>,
    actions: &mut Vec<Action>,
) {
    if r.changed() {
        pending.values.insert(key.clone(), value);
    }
    let in_use = r.dragged()
        || r.has_focus()
        || ui.ctx().dragged_id().is_some()
        || ui.input(|i| i.pointer.any_down());
    if !in_use
        && let Some(v) = pending.values.remove(&key)
        && v != *current
    {
        actions.push(target.set(v));
    }
}

/// A single-line text field that returns its text when it is committed
/// (Enter or focus lost) and differs from `current`.
fn text_field(
    ui: &mut egui::Ui,
    pending: &mut Pending,
    key: &str,
    current: &str,
) -> Option<String> {
    let mut text = pending
        .texts
        .get(key)
        .cloned()
        .unwrap_or_else(|| current.to_string());
    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(180.0));
    if r.lost_focus() {
        pending.texts.remove(key);
        return (text != current).then_some(text);
    }
    if r.has_focus() {
        pending.texts.insert(key.to_string(), text);
    } else {
        pending.texts.remove(key);
    }
    None
}

// ---- graph parameters ---------------------------------------------------

fn graph(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    ui.heading("Graph parameters");
    let world = app.world();
    let registry = &world.registry;
    let (Some(proc_key), graph_key) = (
        registry.try_key("Procedural"),
        registry.try_key("GraphSource"),
    ) else {
        return;
    };
    let mut sources: Vec<(&str, Entity)> = world
        .ids()
        .iter()
        .filter(|(_, e)| registry.has(&world.ecs, *e, proc_key))
        .collect();
    sources.sort();
    if sources.is_empty() {
        ui.label(RichText::new("No procedural sources in this scene.").weak());
        return;
    }
    let has_set_param = app.commands().spec("graph.set_param").is_some();
    let has_regenerate = app.commands().spec("graph.regenerate").is_some();
    for (id, e) in sources {
        let generator = match world.get(id, "Procedural", "generator") {
            Ok(Value::Str(s)) => s,
            _ => String::new(),
        };
        let seed = match world.get(id, "Procedural", "seed") {
            Ok(Value::Int(s)) => s,
            _ => 0,
        };
        let header = format!("{}  ({generator})", display(world, e, id));
        egui::CollapsingHeader::new(header)
            .id_salt(("re_editor.graph", id))
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("seed");
                    let target = FieldTarget {
                        id,
                        component: "Procedural",
                        field: "seed",
                        ty: &FieldType::I64,
                    };
                    let key = format!("graph\u{1f}{}", target.key());
                    let shown = match ed.pending.values.get(&key) {
                        Some(Value::Int(i)) => *i,
                        _ => seed,
                    };
                    let mut s = shown;
                    let r = ui.add(egui::DragValue::new(&mut s).speed(1.0));
                    let mut seed_actions = Vec::new();
                    settle(
                        ui,
                        &r,
                        &mut ed.pending,
                        key,
                        Value::Int(s),
                        &Value::Int(seed),
                        &target,
                        &mut seed_actions,
                    );
                    if ui
                        .button("New seed")
                        .on_hover_text("Regenerate with the next seed")
                        .clicked()
                    {
                        seed_actions.push(target.set(Value::Int(next_seed(seed))));
                    }
                    for action in seed_actions {
                        actions.push(match action {
                            Action::SetField {
                                value: Value::Int(s),
                                ..
                            } if has_regenerate => {
                                command("graph.regenerate", json!({ "source": id, "seed": s }))
                            }
                            other => other,
                        });
                    }
                    let regenerate = ui
                        .add_enabled(has_regenerate, egui::Button::new("Regenerate"))
                        .on_hover_text("Run the generator again now (graph.regenerate)")
                        .on_disabled_hover_text(
                            "graph.regenerate is not registered in this build; changing the \
                             seed or a parameter regenerates the source",
                        );
                    if regenerate.clicked() {
                        actions.push(command("graph.regenerate", json!({ "source": id })));
                    }
                });
                if let Some(gk) = graph_key
                    && registry.has(&world.ecs, e, gk)
                {
                    graph_params(ed, ui, world, id, has_set_param, actions);
                }
            });
    }
}

fn display(world: &World, e: Entity, id: &str) -> String {
    let name = world.name_of(e);
    if name.is_empty() {
        id.to_string()
    } else {
        format!("{name} {id}")
    }
}

/// The next seed of the "New seed" button: a fixed scramble of the current
/// one, so a session's seeds are reproducible.
fn next_seed(seed: i64) -> i64 {
    let mut z = (seed as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    ((z ^ (z >> 31)) >> 33) as i64
}

fn graph_params(
    ed: &mut Editor,
    ui: &mut egui::Ui,
    world: &World,
    id: &str,
    has_set_param: bool,
    actions: &mut Vec<Action>,
) {
    let text = |field| match world.get(id, "GraphSource", field) {
        Ok(Value::Str(s)) => s,
        _ => String::new(),
    };
    let (file, builder, params) = (text("graph"), text("builder"), text("params"));
    if !file.is_empty() {
        ui.label(RichText::new(format!("graph {file}")).weak());
    }
    if !builder.is_empty() {
        ui.label(RichText::new(format!("builder {builder}")).weak());
    }
    let overrides: Map<String, Json> = if params.trim().is_empty() {
        Map::new()
    } else {
        match serde_json::from_str::<Json>(&params) {
            Ok(Json::Object(m)) => m,
            _ => {
                ui.label(
                    RichText::new("GraphSource.params is not a JSON object").color(Color32::RED),
                );
                return;
            }
        }
    };
    let set = |node: &str, param: &str, value: Json| {
        set_param(&overrides, id, node, param, value, has_set_param)
    };
    if overrides.is_empty() {
        ui.label(RichText::new("No parameter overrides.").weak());
    }
    egui::Grid::new(("re_editor.params", id))
        .num_columns(3)
        .show(ui, |ui| {
            for (node, params) in &overrides {
                let Json::Object(params) = params else {
                    continue;
                };
                for (param, value) in params {
                    ui.label(format!("{node}.{param}"));
                    let key = format!("param\u{1f}{id}\u{1f}{node}\u{1f}{param}");
                    let shown = value.to_string();
                    if let Some(text) = text_field(ui, &mut ed.pending, &key, &shown) {
                        match serde_json::from_str::<Json>(&text) {
                            Ok(v) => actions.push(set(node, param, v)),
                            Err(e) => {
                                actions.push(Action::Report(bad_param(&text, node, param, &e)))
                            }
                        }
                    }
                    if ui
                        .small_button("×")
                        .on_hover_text("Remove this override")
                        .clicked()
                    {
                        actions.push(set(node, param, Json::Null));
                    }
                    ui.end_row();
                }
            }
        });
    let new = ed.graph_new.entry(id.to_string()).or_default();
    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut new[0])
                .hint_text("node")
                .desired_width(70.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut new[1])
                .hint_text("param")
                .desired_width(70.0),
        );
        ui.add(
            egui::TextEdit::singleline(&mut new[2])
                .hint_text("value")
                .desired_width(70.0),
        );
        let ready = !new[0].trim().is_empty() && !new[1].trim().is_empty() && !new[2].is_empty();
        if ui.add_enabled(ready, egui::Button::new("Set")).clicked() {
            let (node, param) = (new[0].trim(), new[1].trim());
            match serde_json::from_str::<Json>(&new[2]) {
                Ok(v) => {
                    actions.push(set(node, param, v));
                    *new = Default::default();
                }
                Err(e) => actions.push(Action::Report(bad_param(&new[2], node, param, &e))),
            }
        }
    });
}

fn bad_param(text: &str, node: &str, param: &str, e: &serde_json::Error) -> Diagnostic {
    Diagnostic::error(
        codes::INVALID_FIELD_TEXT,
        format!("`{text}` is not a JSON value for graph parameter {node}.{param}: {e}"),
    )
    .with_hint("type a JSON value: a number, true or false, a string in quotes, or a list")
}

/// Sets one parameter override: through `graph.set_param` (which checks the
/// value against the node's schema) when it is registered, else by writing
/// `GraphSource.params` with `entity.set`. A `null` value removes it.
fn set_param(
    overrides: &Map<String, Json>,
    id: &str,
    node: &str,
    param: &str,
    value: Json,
    has_set_param: bool,
) -> Action {
    if has_set_param {
        return command(
            "graph.set_param",
            json!({ "source": id, "node": node, "param": param, "value": value }),
        );
    }
    let mut all = overrides.clone();
    let entry = all
        .entry(node.to_string())
        .or_insert_with(|| Json::Object(Map::new()));
    if !entry.is_object() {
        *entry = Json::Object(Map::new());
    }
    let params = entry.as_object_mut().expect("made an object above");
    if value.is_null() {
        params.remove(param);
    } else {
        params.insert(param.to_string(), value);
    }
    if params.is_empty() {
        all.remove(node);
    }
    all.sort_keys();
    for v in all.values_mut() {
        if let Json::Object(m) = v {
            m.sort_keys();
        }
    }
    let text = if all.is_empty() {
        String::new()
    } else {
        Json::Object(all).to_string()
    };
    Action::SetField {
        id: id.to_string(),
        component: "GraphSource".to_string(),
        field: "params".to_string(),
        ty: FieldType::String,
        value: Value::Str(text),
    }
}

// ---- console ------------------------------------------------------------

fn console(ed: &mut Editor, ui: &mut egui::Ui) {
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

// ---- viewport -----------------------------------------------------------

fn viewport(ed: &mut Editor, ui: &mut egui::Ui) {
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
}

// ---- command palette ----------------------------------------------------

fn palette(ed: &mut Editor, ctx: &egui::Context, app: &App, actions: &mut Vec<Action>) {
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

    fn params(action: Action) -> String {
        match action {
            Action::SetField {
                component,
                field,
                value: Value::Str(s),
                ..
            } if component == "GraphSource" && field == "params" => s,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn set_param_without_the_command_writes_sorted_overrides() {
        let none = Map::new();
        let one = params(set_param(
            &none,
            "island",
            "terrain",
            "height",
            json!(12.5),
            false,
        ));
        assert_eq!(one, r#"{"terrain":{"height":12.5}}"#);
        let Json::Object(current) = serde_json::from_str(&one).unwrap() else {
            unreachable!()
        };
        let two = params(set_param(
            &current,
            "island",
            "rocks",
            "count",
            json!(40),
            false,
        ));
        assert_eq!(two, r#"{"rocks":{"count":40},"terrain":{"height":12.5}}"#);
        let cleared = params(set_param(
            &current,
            "island",
            "terrain",
            "height",
            Json::Null,
            false,
        ));
        assert_eq!(cleared, "");
    }

    #[test]
    fn set_param_uses_the_command_when_registered() {
        let action = set_param(&Map::new(), "island", "terrain", "height", json!(3), true);
        assert_eq!(
            action,
            command(
                "graph.set_param",
                json!({ "source": "island", "node": "terrain", "param": "height", "value": 3 })
            )
        );
    }

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

    #[test]
    fn next_seed_is_a_fixed_non_negative_scramble() {
        assert_eq!(next_seed(1234), next_seed(1234));
        assert_ne!(next_seed(1234), 1234);
        assert!((0..100).all(|s| next_seed(s) >= 0));
    }
}
