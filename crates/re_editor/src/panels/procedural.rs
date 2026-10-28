//! The procedural panel: each procedural source's seed and regeneration, and
//! its graph parameter overrides.

use egui::{Color32, RichText};
use re_app::App;
use re_core::Diagnostic;
use re_reflect::{FieldType, Value};
use re_scene::{Entity, World};
use serde_json::{Map, Value as Json, json};

use crate::codes;
use crate::editor::{Action, Editor, command};
use crate::widgets::{FieldTarget, settle, text_field};

pub(super) fn graph(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
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
                    let r = ui.add(
                        egui::DragValue::new(&mut s)
                            .speed(1.0)
                            .update_while_editing(false),
                    );
                    let mut seed_actions = Vec::new();
                    settle(
                        ui,
                        &[r],
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

/// What the procedural tools draw over the viewport and the pointer input
/// they take there (WP43), after the gizmo. Returns true when they took the
/// pointer this frame, so the viewport does not also act on it. Nothing yet.
pub(super) fn viewport_overlay(
    _ed: &mut Editor,
    _ui: &mut egui::Ui,
    _app: &App,
    _view: &crate::camera::View,
    _actions: &mut Vec<Action>,
) -> bool {
    false
}

/// The procedural entries of the Hierarchy's context menu for the row of
/// entity `id` (WP43: `proc.replace`, `proc.insert`, `proc.clear_structure`
/// and `proc.bake`), after the editing ones. The menu acts on the selection,
/// which holds `id`. Nothing yet.
pub(super) fn entity_menu(
    _ed: &mut Editor,
    _ui: &mut egui::Ui,
    _app: &App,
    _id: &str,
    _actions: &mut Vec<Action>,
) {
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
    fn next_seed_is_a_fixed_non_negative_scramble() {
        assert_eq!(next_seed(1234), next_seed(1234));
        assert_ne!(next_seed(1234), 1234);
        assert!((0..100).all(|s| next_seed(s) >= 0));
    }
}
