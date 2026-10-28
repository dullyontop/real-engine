//! The Inspector's rows (ADR-0064 §9): a field's name, marked when a
//! procedural source keeps an override of it on a generated entity (with
//! Revert, `proc.revert`), and its widget. A string field with an asset hint
//! gets the canvas's asset picker, listing the asset index's files of its
//! kind, which also takes an asset of that kind dropped on it from the
//! Assets panel. An `entity` field gets an entity picker: its id typed, a
//! list of the world's entities searched by name and id, and "pick", which
//! takes the next click in the viewport ([`take_pick`]). A list gets a row
//! per item with add, remove and reorder; a map, a key, a type and a value
//! per entry; a record, a widget per field. However many widgets a field
//! has, a finished edit is one `entity.set` of its whole value, so one undo
//! step.

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::{Color32, Id, Key, Modifiers, RichText};
use glam::{EulerRot, Quat, Vec3};
use re_core::Diagnostic;
use re_reflect::{Color, ComponentKey, FieldType, RecordId, Value};
use re_scene::{Entity, Generated, Overrides, World};
use serde_json::{Value as Json, json};

use super::schema::{self, Change, Env, Shape};
use super::{FieldTarget, drag3, entity_text, field_widget, in_use, list_menu, text_field};
use crate::assets::KindLists;
use crate::codes;
use crate::editor::{Action, Editor, Pending, command};
use crate::panels::AssetDrag;

/// The colour of an overridden field's name.
const OVERRIDDEN: Color32 = Color32::from_rgb(110, 170, 255);

/// What the Inspector's rows share in one frame: the edits in progress, the
/// world they are read from, and the asset index's lists for the pickers.
pub(crate) struct Fields<'a> {
    pending: &'a mut Pending,
    world: &'a World,
    assets: Arc<KindLists>,
}

impl<'a> Fields<'a> {
    pub(crate) fn new(ed: &'a mut Editor, world: &'a World) -> Fields<'a> {
        Fields {
            assets: ed.assets.lists(),
            pending: &mut ed.pending,
            world,
        }
    }
}

/// An entity field waiting for the next click in the viewport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Picking {
    id: String,
    component: String,
    field: String,
}

impl Picking {
    fn of(target: &FieldTarget<'_>) -> Picking {
        Picking {
            id: target.id.to_string(),
            component: target.component.to_string(),
            field: target.field.to_string(),
        }
    }
}

/// Gives a viewport click's pick to the entity field waiting for one ("pick"
/// in its picker): one `entity.set` of the picked entity's id, or nothing
/// when the click hit no entity. True when a field took the click, so the
/// selection is left as it is. A field waits only while its entity is the
/// active one, which is the one the Inspector shows.
pub(crate) fn take_pick(ed: &mut Editor, picked: Option<&str>, actions: &mut Vec<Action>) -> bool {
    let Some(p) = ed.pending.picking.take() else {
        return false;
    };
    if ed.selection.active() != Some(p.id.as_str()) {
        return false;
    }
    if let Some(picked) = picked {
        actions.push(command(
            "entity.set",
            json!({
                "id": p.id,
                "component": p.component,
                "field": p.field,
                "value": picked,
            }),
        ));
    }
    true
}

/// The key of one part of a field's widget in [`Pending::rects`]: `""` the
/// text of a picker, `"…"` its list button, `"search"` and `"=<value>"` the
/// list's search box and items (`"="` for none), `"pick"` the entity
/// picker's viewport button, `"revert"` the override's button; for
/// containers `"0"`, `"1"`… an item (`"<key>"` a map entry's value, a record
/// field's name), each with its own parts after a `/` (a vector's `0`, `1`
/// and `2`), `"+"` the add button, `"×0"`, `"⏶0"`, `"⏷0"` an item's buttons
/// (`"×<key>"` an entry's), and `"<key>/key"`, `"<key>/type"` and
/// `"<key>/type=<type>"` a map entry's key, type and the type list's items.
pub(crate) fn part_key(field_key: &str, part: &str) -> String {
    format!("{field_key}\u{1f}{part}")
}

impl Editor {
    /// Where the Inspector drew one part of a field's widget in the last
    /// frame (in egui points), if it was shown; see [`part_key`] for the
    /// names of the parts.
    pub fn field_part(
        &self,
        id: &str,
        component: &str,
        field: &str,
        part: &str,
    ) -> Option<egui::Rect> {
        let key = part_key(&crate::editor::field_key(id, component, field), part);
        self.pending
            .rects
            .get(&key)
            .and_then(|r| r.first().copied())
    }

    /// True while an entity field of the Inspector waits for a click in the
    /// viewport.
    pub fn is_picking(&self) -> bool {
        self.pending.picking.is_some()
    }
}

/// One field of the active entity `id`, as a row of the Inspector's grid:
/// the name cell, the value cell, and the end of the row.
pub(crate) fn field_row(
    ui: &mut egui::Ui,
    fields: &mut Fields<'_>,
    id: &str,
    key: ComponentKey,
    index: usize,
    value: &Value,
    actions: &mut Vec<Action>,
) {
    let world = fields.world;
    let schema = world.registry.schema(key);
    let field = &schema.fields[index];
    let target = FieldTarget {
        id,
        component: &schema.name,
        field: &field.name,
        ty: &field.ty,
    };
    name_cell(ui, fields, &target, &field.doc, actions);
    match (&field.ty, world.registry.field_hint(key, index).asset) {
        (FieldType::String, Some(kind)) => asset_field(ui, fields, &target, kind, value, actions),
        (FieldType::Entity, _) => entity_field(ui, fields, &target, value, actions),
        (FieldType::List(_) | FieldType::Map | FieldType::Record(_), _) => {
            container_field(ui, fields, &target, value, actions);
        }
        _ => field_widget(ui, fields.pending, world, &target, value, actions),
    }
    ui.end_row();
}

/// The field's name (its doc on hover), in colour with a Revert button when
/// the generated entity's procedural source keeps an override of it.
fn name_cell(
    ui: &mut egui::Ui,
    fields: &mut Fields<'_>,
    target: &FieldTarget<'_>,
    doc: &str,
    actions: &mut Vec<Action>,
) {
    let Some(source) = overridden_by(fields.world, target) else {
        ui.label(target.field).on_hover_text(doc);
        return;
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new(target.field).color(OVERRIDDEN).strong())
            .on_hover_text(format!(
                "{doc}\n\nOverridden: {source} keeps this value over the one it generates."
            ));
        let revert = ui.small_button("revert").on_hover_text(format!(
            "Put back the value {source} generates (proc.revert)"
        ));
        fields
            .pending
            .rects
            .insert(part_key(&target.key(), "revert"), vec![revert.rect]);
        if revert.clicked() {
            actions.push(command(
                "proc.revert",
                json!({
                    "id": target.id,
                    "component": target.component,
                    "field": target.field,
                }),
            ));
        }
    });
}

/// The id of the procedural source that keeps an override of this field of
/// a generated entity.
fn overridden_by<'w>(world: &'w World, target: &FieldTarget<'_>) -> Option<&'w str> {
    let e = world.entity(target.id).ok()?;
    let generated = world.ecs.get::<Generated>(e)?;
    let overrides = world.ecs.get::<Overrides>(generated.source)?;
    overrides.get(&generated.key, target.component, target.field)?;
    world.id_of(generated.source)
}

/// A string field naming an asset of `kind`: the canvas's asset picker (its
/// text, and the index's files of that kind to search), which also takes an
/// asset of that kind dropped on it.
fn asset_field(
    ui: &mut egui::Ui,
    fields: &mut Fields<'_>,
    target: &FieldTarget<'_>,
    kind: &str,
    value: &Value,
    actions: &mut Vec<Action>,
) {
    let Value::Str(current) = value else {
        return field_widget(ui, fields.pending, fields.world, target, value, actions);
    };
    let lists = Arc::clone(&fields.assets);
    let assets = move |k: &str| lists.of(k);
    let mut env = Env::new(&assets);
    let key = target.key();
    let shape = Shape::Asset(kind.to_string());
    let shown = Json::String(current.clone());
    let r = ui.horizontal(|ui| {
        schema::edit(
            ui,
            Id::new(("re_editor.field", &key)),
            &shape,
            &shown,
            &mut env,
        )
    });
    let dropped = drop_target(ui, &r.response, |d: &AssetDrag| d.kind.name() == kind);
    for (part, rect) in env.parts {
        let name = if part.is_empty() { String::new() } else { part };
        fields
            .pending
            .rects
            .insert(part_key(&key, &name), vec![rect]);
    }
    fields
        .pending
        .widgets(&key, std::slice::from_ref(&r.response));
    let chosen = match (dropped, r.inner) {
        (Some(d), _) => Some(d.path),
        (None, Some(Change::Done(Json::String(s)))) => Some(s),
        _ => None,
    };
    if let Some(path) = chosen.filter(|p| p != current) {
        actions.push(target.set(Value::Str(path)));
    }
}

/// The payload of type `T` released over `r` this frame, when `accepts` it;
/// while one is dragged over `r`, outlines it (in the accent colour when it
/// would be taken).
fn drop_target<T: Clone + Send + Sync + 'static>(
    ui: &egui::Ui,
    r: &egui::Response,
    accepts: impl Fn(&T) -> bool,
) -> Option<T> {
    if let Some(hovered) = r.dnd_hover_payload::<T>() {
        let stroke = if accepts(&*hovered) {
            ui.visuals().selection.stroke
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke
        };
        ui.painter()
            .rect_stroke(r.rect.expand(2.0), 2.0, stroke, egui::StrokeKind::Outside);
    }
    let released = r.dnd_release_payload::<T>()?;
    accepts(&*released).then(|| (*released).clone())
}

/// An entity field: its id typed (as before), a list of the world's
/// entities to search by name or id, and "pick", which waits for the next
/// click in the viewport (Escape cancels).
fn entity_field(
    ui: &mut egui::Ui,
    fields: &mut Fields<'_>,
    target: &FieldTarget<'_>,
    value: &Value,
    actions: &mut Vec<Action>,
) {
    let Value::Entity(current) = value else {
        return field_widget(ui, fields.pending, fields.world, target, value, actions);
    };
    let key = target.key();
    let world = fields.world;
    ui.horizontal(|ui| {
        let text = entity_text(ui, fields.pending, world, target, *current, actions);
        fields
            .pending
            .rects
            .insert(part_key(&key, ""), vec![text.rect]);
        let (menu, _) = list_menu(ui, |ui| {
            let items = entity_menu(ui, world, &key, *current, target, actions);
            for (part, rect) in items {
                fields
                    .pending
                    .rects
                    .insert(part_key(&key, &part), vec![rect]);
            }
        });
        let menu = menu.on_hover_text("Choose an entity");
        fields
            .pending
            .rects
            .insert(part_key(&key, "…"), vec![menu.rect]);
        let picking = Picking::of(target);
        let waiting = fields.pending.picking.as_ref() == Some(&picking);
        let pick = ui
            .selectable_label(waiting, if waiting { "picking…" } else { "pick" })
            .on_hover_text(if waiting {
                "Click an entity in the viewport to set this field; Escape cancels"
            } else {
                "Pick an entity in the viewport: the next click there sets this field"
            });
        fields
            .pending
            .rects
            .insert(part_key(&key, "pick"), vec![pick.rect]);
        if pick.clicked() {
            fields.pending.picking = (!waiting).then_some(picking);
        } else if waiting && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            fields.pending.picking = None;
        }
    });
}

/// The entity picker's list: a search over names and ids, "(none)", and the
/// matching entities by id (only the rows in view are drawn). Choosing one
/// sets the field. Returns where its items were drawn, by part name.
fn entity_menu(
    ui: &mut egui::Ui,
    world: &World,
    key: &str,
    current: Option<Entity>,
    target: &FieldTarget<'_>,
    actions: &mut Vec<Action>,
) -> Vec<(String, egui::Rect)> {
    let search_id = Id::new(("re_editor.entity_search", key));
    let mut search = ui
        .data(|d| d.get_temp::<String>(search_id))
        .unwrap_or_default();
    let search_box =
        ui.add(egui::TextEdit::singleline(&mut search).hint_text("search by name or id"));
    ui.data_mut(|d| d.insert_temp(search_id, search.clone()));
    let needle = search.to_lowercase();
    let mut found: Vec<(&str, Entity)> = world
        .ids()
        .iter()
        .filter(|&(id, e)| {
            needle.is_empty()
                || id.to_lowercase().contains(&needle)
                || world.name_of(e).to_lowercase().contains(&needle)
        })
        .collect();
    found.sort_unstable_by_key(|&(id, _)| id);
    let mut parts = vec![("search".to_string(), search_box.rect)];
    let mut set = |e: Option<Entity>, ui: &mut egui::Ui| {
        if e != current {
            actions.push(target.set(Value::Entity(e)));
        }
        ui.close();
    };
    let none = ui.selectable_label(current.is_none(), "(none)");
    parts.push(("=".to_string(), none.rect));
    if none.clicked() {
        set(None, ui);
    }
    let row = ui.spacing().interact_size.y;
    egui::ScrollArea::vertical()
        .id_salt(search_id.with("rows"))
        .max_height(240.0)
        .show_rows(ui, row, found.len(), |ui, rows| {
            for &(id, e) in &found[rows] {
                let name = world.name_of(e);
                let label = if name.is_empty() || name == id {
                    id.to_string()
                } else {
                    format!("{name}  ({id})")
                };
                let r = ui.selectable_label(current == Some(e), label);
                parts.push((format!("={id}"), r.rect));
                if r.clicked() {
                    set(Some(e), ui);
                }
            }
        });
    if found.is_empty() {
        ui.label(RichText::new("No entity matches.").weak());
    }
    parts
}

/// A list, map or record field: a widget per item, entry or field, drawn on
/// a copy of the value that is kept pending while any of them is in use and
/// set whole (one `entity.set`) when the edit is finished.
fn container_field(
    ui: &mut egui::Ui,
    fields: &mut Fields<'_>,
    target: &FieldTarget<'_>,
    current: &Value,
    actions: &mut Vec<Action>,
) {
    let key = target.key();
    let mut shown = fields
        .pending
        .values
        .get(&key)
        .cloned()
        .unwrap_or_else(|| current.clone());
    let mut cx = Cx {
        pending: &mut *fields.pending,
        key: &key,
        target,
        rs: Vec::new(),
        changed: false,
        actions: &mut *actions,
    };
    ui.vertical(|ui| value_ui(ui, &mut cx, "", target.ty, &mut shown));
    let Cx { rs, changed, .. } = cx;
    fields.pending.widgets(&key, &rs);
    if changed {
        fields.pending.values.insert(key.clone(), shown);
    }
    if !in_use(ui, &rs)
        && let Some(v) = fields.pending.values.remove(&key)
        && v != *current
    {
        actions.push(target.set(v));
    }
}

/// What a container's widgets share while they are drawn.
struct Cx<'a, 'p> {
    pending: &'p mut Pending,
    /// The field's key in [`Pending`].
    key: &'a str,
    target: &'a FieldTarget<'a>,
    /// Every widget drawn, for the rule that commits when none is in use.
    rs: Vec<egui::Response>,
    /// True when an item, entry or field changed this frame.
    changed: bool,
    actions: &'p mut Vec<Action>,
}

impl Cx<'_, '_> {
    /// Records `r` as the part at `path`.
    fn part(&mut self, path: &str, r: &egui::Response) {
        self.pending
            .rects
            .insert(part_key(self.key, path), vec![r.rect]);
        self.rs.push(r.clone());
    }

    fn id(&self, path: &str) -> Id {
        Id::new(("re_editor.field", self.key, path))
    }

    /// A text field at `path`, its text kept in [`Pending::texts`] while it
    /// has focus; returns the text when it is committed changed.
    fn text(&mut self, ui: &mut egui::Ui, path: &str, current: &str) -> Option<String> {
        let (r, text) = text_field(ui, self.pending, &part_key(self.key, path), current);
        self.part(path, &r);
        text
    }

    /// Reports a refused edit (RE8803): nothing changes.
    fn refuse(&mut self, message: String, hint: &str) {
        self.actions.push(Action::Report(
            Diagnostic::error(
                codes::INVALID_FIELD_TEXT,
                format!(
                    "{message} (in {}.{} of {})",
                    self.target.component, self.target.field, self.target.id
                ),
            )
            .with_hint(hint),
        ));
    }
}

/// `path/name`, or `name` at the top.
fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}/{name}")
    }
}

/// Draws the widget of `v`, a value of `ty`, and changes `v` in place.
fn value_ui(ui: &mut egui::Ui, cx: &mut Cx<'_, '_>, path: &str, ty: &FieldType, v: &mut Value) {
    match (ty, v) {
        (FieldType::Bool, Value::Bool(b)) => {
            let r = ui.checkbox(b, "");
            cx.changed |= r.changed();
            cx.part(path, &r);
        }
        (FieldType::I32 | FieldType::I64, Value::Int(i)) => {
            let mut drag = egui::DragValue::new(i)
                .speed(0.1)
                .update_while_editing(false);
            if *ty == FieldType::I32 {
                drag = drag.range(i32::MIN..=i32::MAX);
            }
            let r = ui.add(drag);
            cx.changed |= r.changed();
            cx.part(path, &r);
        }
        (FieldType::F32 | FieldType::F64, Value::Float(x)) => {
            let r = ui.add(
                egui::DragValue::new(x)
                    .speed(0.01)
                    .max_decimals(6)
                    .update_while_editing(false),
            );
            cx.changed |= r.changed();
            cx.part(path, &r);
        }
        (FieldType::Vec3, Value::Vec3(v3)) => {
            let mut a = v3.to_array();
            let rs = drag3(ui, &mut a, 0.01, ["x ", "y ", "z "], "");
            vector_parts(cx, path, &rs);
            if rs.iter().any(egui::Response::changed) {
                *v3 = Vec3::from_array(a);
                cx.changed = true;
            }
        }
        (FieldType::Quat, Value::Quat(q)) => {
            // Euler angles in degrees (yaw, pitch, roll: Y, X, Z), as for a
            // field; the rotation is rebuilt only when one is dragged.
            let (y, x, z) = q.to_euler(EulerRot::YXZ);
            let mut a = [x.to_degrees(), y.to_degrees(), z.to_degrees()];
            let rs = drag3(ui, &mut a, 0.5, ["", "", ""], "°");
            vector_parts(cx, path, &rs);
            if rs.iter().any(egui::Response::changed) {
                *q = Quat::from_euler(
                    EulerRot::YXZ,
                    a[1].to_radians(),
                    a[0].to_radians(),
                    a[2].to_radians(),
                );
                cx.changed = true;
            }
        }
        (FieldType::Color, Value::Color(c)) => {
            let mut rgba = [c.r, c.g, c.b, c.a];
            let r = ui.color_edit_button_rgba_unmultiplied(&mut rgba);
            if r.changed() {
                *c = Color::rgba(rgba[0], rgba[1], rgba[2], rgba[3]);
                cx.changed = true;
            }
            cx.part(path, &r);
        }
        (FieldType::String, Value::Str(s)) => {
            if let Some(text) = cx.text(ui, path, s) {
                *s = text;
                cx.changed = true;
            }
        }
        (FieldType::Enum(variants), Value::Enum(e)) => {
            let r = egui::ComboBox::from_id_salt(cx.id(path))
                .selected_text(e.as_str())
                .show_ui(ui, |ui| {
                    for variant in variants.iter() {
                        if ui.selectable_label(e == variant, *variant).clicked() && e != variant {
                            *e = (*variant).to_string();
                            cx.changed = true;
                        }
                    }
                });
            cx.part(path, &r.response);
        }
        (FieldType::List(item), Value::List(items)) => list_ui(ui, cx, path, Some(&**item), items),
        (FieldType::Map, Value::Map(entries)) => map_ui(ui, cx, path, entries),
        (FieldType::Record(id), Value::Record(of, values)) if id == of => {
            record_ui(ui, cx, path, *id, values);
        }
        (_, other) => {
            ui.label(RichText::new(other.describe()).weak());
        }
    }
}

/// Records a vector's three drags as its parts `0`, `1` and `2`.
fn vector_parts(cx: &mut Cx<'_, '_>, path: &str, rs: &[egui::Response]) {
    for (i, r) in rs.iter().enumerate() {
        cx.part(&join(path, &i.to_string()), r);
    }
    if let Some(first) = rs.first() {
        cx.pending
            .rects
            .insert(part_key(cx.key, path), vec![first.rect]);
    }
}

/// A list's items, each with reorder and remove buttons, and an add button.
/// `item` is the items' type; `None` for a list inside a map, whose items
/// each have their own (bools, numbers, strings, vectors, colours and
/// records), chosen per item.
fn list_ui(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_, '_>,
    path: &str,
    item: Option<&FieldType>,
    items: &mut Vec<Value>,
) {
    let len = items.len();
    let mut moved = None;
    let mut removed = None;
    for (i, v) in items.iter_mut().enumerate() {
        let at = join(path, &i.to_string());
        ui.horizontal(|ui| {
            match item {
                Some(ty) => value_ui(ui, cx, &at, ty, v),
                None => dynamic_ui(ui, cx, &at, v, false),
            }
            let up = ui.add_enabled(i > 0, egui::Button::new("⏶").small());
            cx.part(&join(path, &format!("⏶{i}")), &up);
            let down = ui.add_enabled(i + 1 < len, egui::Button::new("⏷").small());
            cx.part(&join(path, &format!("⏷{i}")), &down);
            let remove = ui.small_button("×").on_hover_text("Remove this item");
            cx.part(&join(path, &format!("×{i}")), &remove);
            if up.clicked() {
                moved = Some((i, i - 1));
            } else if down.clicked() {
                moved = Some((i, i + 1));
            } else if remove.clicked() {
                removed = Some(i);
            }
        });
    }
    let add = ui.small_button("+").on_hover_text("Add an item");
    cx.part(&join(path, "+"), &add);
    if let Some((a, b)) = moved {
        items.swap(a, b);
        cx.changed = true;
    } else if let Some(i) = removed {
        items.remove(i);
        cx.changed = true;
    } else if add.clicked() {
        let new = match (item, items.last()) {
            (Some(ty), _) => ty.default_value(),
            // A new item of a list in a map takes its last item's type.
            (None, Some(last)) => Dynamic::of(last).default_value(),
            (None, None) => Dynamic::Number.default_value(),
        };
        items.push(new);
        cx.changed = true;
    }
}

/// A record's fields, a name and a widget each.
fn record_ui(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_, '_>,
    path: &str,
    id: RecordId,
    values: &mut [Value],
) {
    let schema = id.schema();
    egui::Grid::new(cx.id(path).with("record"))
        .num_columns(2)
        .show(ui, |ui| {
            for (f, v) in schema.fields.iter().zip(values.iter_mut()) {
                ui.label(&f.name);
                value_ui(ui, cx, &join(path, &f.name), &f.ty, v);
                ui.end_row();
            }
        });
}

/// A map's entries, each a key, a type and a value, with a remove button,
/// and an add button. A key typed over another one is refused.
fn map_ui(
    ui: &mut egui::Ui,
    cx: &mut Cx<'_, '_>,
    path: &str,
    entries: &mut BTreeMap<String, Value>,
) {
    let mut renamed = None;
    let mut removed = None;
    egui::Grid::new(cx.id(path).with("map"))
        .num_columns(4)
        .show(ui, |ui| {
            let keys: Vec<String> = entries.keys().cloned().collect();
            for k in &keys {
                let at = join(path, k);
                if let Some(new) = cx.text(ui, &join(&at, "key"), k) {
                    renamed = Some((k.clone(), new));
                }
                if let Some(v) = entries.get_mut(k) {
                    dynamic_ui(ui, cx, &at, v, true);
                }
                let remove = ui.small_button("×").on_hover_text("Remove this entry");
                cx.part(&join(path, &format!("×{k}")), &remove);
                if remove.clicked() {
                    removed = Some(k.clone());
                }
                ui.end_row();
            }
        });
    let add = ui.small_button("+").on_hover_text("Add an entry");
    cx.part(&join(path, "+"), &add);
    if let Some((old, new)) = renamed {
        let new = new.trim().to_string();
        if new.is_empty() {
            cx.refuse(
                format!("the key {old:?} cannot be renamed to nothing"),
                "type a name for the key, or remove the entry with ×",
            );
        } else if entries.contains_key(&new) {
            cx.refuse(
                format!("the key {new:?} is already used"),
                "type a key no other entry has",
            );
        } else if let Some(v) = entries.remove(&old) {
            entries.insert(new, v);
            cx.changed = true;
        }
    } else if let Some(k) = removed {
        entries.remove(&k);
        cx.changed = true;
    } else if add.clicked() {
        let free = (1..)
            .map(|n| {
                if n == 1 {
                    "key".to_string()
                } else {
                    format!("key {n}")
                }
            })
            .find(|k| !entries.contains_key(k))
            .unwrap_or_default();
        entries.insert(free, Dynamic::Number.default_value());
        cx.changed = true;
    }
}

/// The types a map's values can have (ADR-0016's dynamic values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dynamic {
    Bool,
    Number,
    Text,
    Vec3,
    Quat,
    Colour,
    List,
    Record(RecordId),
}

impl Dynamic {
    fn of(v: &Value) -> Dynamic {
        match v {
            Value::Bool(_) => Dynamic::Bool,
            Value::Str(_) | Value::Enum(_) => Dynamic::Text,
            Value::Vec3(_) => Dynamic::Vec3,
            Value::Quat(_) => Dynamic::Quat,
            Value::Color(_) => Dynamic::Colour,
            Value::List(_) => Dynamic::List,
            Value::Record(id, _) => Dynamic::Record(*id),
            Value::Int(_) | Value::Float(_) | Value::Entity(_) | Value::Map(_) => Dynamic::Number,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Dynamic::Bool => "bool",
            Dynamic::Number => "number",
            Dynamic::Text => "string",
            Dynamic::Vec3 => "vec3",
            Dynamic::Quat => "quat",
            Dynamic::Colour => "color",
            Dynamic::List => "list",
            Dynamic::Record(id) => id.name(),
        }
    }

    fn field_type(self) -> Option<FieldType> {
        Some(match self {
            Dynamic::Bool => FieldType::Bool,
            Dynamic::Number => FieldType::F64,
            Dynamic::Text => FieldType::String,
            Dynamic::Vec3 => FieldType::Vec3,
            Dynamic::Quat => FieldType::Quat,
            Dynamic::Colour => FieldType::Color,
            Dynamic::Record(id) => FieldType::Record(id),
            Dynamic::List => return None,
        })
    }

    fn default_value(self) -> Value {
        match self.field_type() {
            Some(ty) => ty.default_value(),
            None => Value::List(Vec::new()),
        }
    }
}

/// A value whose type is chosen with it (a map's value, or an item of a
/// list in a map): the type's combo box, then the value's widget. Changing
/// the type puts the new type's zero value in. `lists` offers the list type,
/// which a list's items cannot have.
fn dynamic_ui(ui: &mut egui::Ui, cx: &mut Cx<'_, '_>, path: &str, v: &mut Value, lists: bool) {
    let now = Dynamic::of(v);
    let mut chosen = None;
    let mut items = Vec::new();
    let r = egui::ComboBox::from_id_salt(cx.id(path).with("type"))
        .selected_text(now.name())
        .show_ui(ui, |ui| {
            let plain = [
                Dynamic::Bool,
                Dynamic::Number,
                Dynamic::Text,
                Dynamic::Vec3,
                Dynamic::Quat,
                Dynamic::Colour,
            ];
            let list = lists.then_some(Dynamic::List);
            let records = RecordId::all().into_iter().map(Dynamic::Record);
            for d in plain.into_iter().chain(list).chain(records) {
                let item = ui.selectable_label(d == now, d.name());
                items.push((format!("type={}", d.name()), item.rect));
                if item.clicked() && d != now {
                    chosen = Some(d);
                }
            }
        });
    cx.part(&join(path, "type"), &r.response);
    for (name, rect) in items {
        cx.pending
            .rects
            .insert(part_key(cx.key, &join(path, &name)), vec![rect]);
    }
    if let Some(d) = chosen {
        *v = d.default_value();
        cx.changed = true;
    }
    // Numbers are kept as `f64`, as a map reads them.
    if let Value::Int(i) = *v {
        *v = Value::Float(i as f64);
    }
    match (Dynamic::of(v).field_type(), v) {
        (Some(ty), v) => value_ui(ui, cx, path, &ty, v),
        (None, Value::List(items)) => {
            ui.vertical(|ui| list_ui(ui, cx, path, None, items));
        }
        (None, _) => {}
    }
}
