//! The field widgets that the inspector and the procedural panel share: one
//! widget per field type, and the rules that keep a dragged or typed value
//! pending until it is committed, once.

use egui::RichText;
use glam::{EulerRot, Quat, Vec3};
use re_core::Diagnostic;
use re_reflect::{Color, FieldType, Value};
use re_scene::World;

use crate::codes;
use crate::editor::{Action, Pending};

pub(crate) mod schema;

/// The field an inspector widget edits.
pub(crate) struct FieldTarget<'a> {
    pub(crate) id: &'a str,
    pub(crate) component: &'a str,
    pub(crate) field: &'a str,
    pub(crate) ty: &'a FieldType,
}

impl FieldTarget<'_> {
    pub(crate) fn key(&self) -> String {
        crate::editor::field_key(self.id, self.component, self.field)
    }

    pub(crate) fn set(&self, value: Value) -> Action {
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
pub(crate) fn field_widget(
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
            let mut drag = egui::DragValue::new(&mut i)
                .speed(0.1)
                .update_while_editing(false);
            if *target.ty == FieldType::I32 {
                drag = drag.range(i32::MIN..=i32::MAX);
            }
            let rs = [ui.add(drag)];
            pending.widgets(&key, &rs);
            let value = Value::Int(i);
            settle(ui, &rs, pending, key, value, current, target, actions);
        }
        (FieldType::F32 | FieldType::F64, Value::Float(x)) => {
            let mut x = *x;
            let rs = [ui.add(
                egui::DragValue::new(&mut x)
                    .speed(0.01)
                    .max_decimals(6)
                    .update_while_editing(false),
            )];
            pending.widgets(&key, &rs);
            let value = Value::Float(x);
            settle(ui, &rs, pending, key, value, current, target, actions);
        }
        (FieldType::Vec3, Value::Vec3(v)) => {
            let mut a = v.to_array();
            let rs = drag3(ui, &mut a, 0.01, ["x ", "y ", "z "], "");
            pending.widgets(&key, &rs);
            let value = Value::Vec3(Vec3::from_array(a));
            settle(ui, &rs, pending, key, value, current, target, actions);
        }
        (FieldType::Quat, Value::Quat(q)) => {
            // Shown as Euler angles in degrees (yaw, pitch, roll: Y, X, Z).
            let (y, x, z) = q.to_euler(EulerRot::YXZ);
            let mut a = [x.to_degrees(), y.to_degrees(), z.to_degrees()];
            let rs = drag3(ui, &mut a, 0.5, ["", "", ""], "°");
            pending.widgets(&key, &rs);
            let value = if rs.iter().any(egui::Response::changed) {
                Value::Quat(Quat::from_euler(
                    EulerRot::YXZ,
                    a[1].to_radians(),
                    a[0].to_radians(),
                    a[2].to_radians(),
                ))
            } else {
                shown.clone()
            };
            settle(ui, &rs, pending, key, value, current, target, actions);
        }
        (FieldType::Color, Value::Color(c)) => {
            let mut rgba = [c.r, c.g, c.b, c.a];
            let rs = [ui.color_edit_button_rgba_unmultiplied(&mut rgba)];
            pending.widgets(&key, &rs);
            let value = Value::Color(Color::rgba(rgba[0], rgba[1], rgba[2], rgba[3]));
            settle(ui, &rs, pending, key, value, current, target, actions);
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

/// Three drag values in a row (a Vec3, or a rotation as Euler angles). They
/// change their value only when typing is confirmed, never per keystroke.
fn drag3(
    ui: &mut egui::Ui,
    a: &mut [f32; 3],
    speed: f64,
    prefixes: [&str; 3],
    suffix: &str,
) -> Vec<egui::Response> {
    ui.horizontal(|ui| {
        a.iter_mut()
            .zip(prefixes)
            .map(|(v, prefix)| {
                ui.add(
                    egui::DragValue::new(v)
                        .speed(speed)
                        .prefix(prefix)
                        .suffix(suffix)
                        .update_while_editing(false),
                )
            })
            .collect()
    })
    .inner
}

/// Keeps a dragged or typed value pending while its widget is in use, and
/// commits it once when the widget is released.
#[allow(clippy::too_many_arguments)]
pub(crate) fn settle(
    ui: &egui::Ui,
    rs: &[egui::Response],
    pending: &mut Pending,
    key: String,
    value: Value,
    current: &Value,
    target: &FieldTarget<'_>,
    actions: &mut Vec<Action>,
) {
    if rs.iter().any(egui::Response::changed) {
        pending.values.insert(key.clone(), value);
    }
    // Each widget is checked on its own (a union of responses keeps only the
    // first one's id, hiding the others' focus); an open popup, such as the
    // colour picker, counts as in use.
    let in_use = rs.iter().any(|r| r.dragged() || r.has_focus())
        || ui.ctx().dragged_id().is_some()
        || ui.ctx().any_popup_open()
        || ui.input(|i| i.pointer.any_down());
    if !in_use
        && let Some(v) = pending.values.remove(&key)
        && v != *current
    {
        actions.push(target.set(v));
    }
}

/// A single-line text field that returns its text when it is committed
/// (Enter or focus lost, not Escape) and differs from `current`.
pub(crate) fn text_field(
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
        // Escape cancels the edit.
        let cancelled = ui.input(|i| i.key_pressed(egui::Key::Escape));
        return (!cancelled && text != current).then_some(text);
    }
    if r.has_focus() {
        pending.texts.insert(key.to_string(), text);
    } else {
        pending.texts.remove(key);
    }
    None
}
