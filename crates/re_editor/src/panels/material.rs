//! The Material panel (ADR-0064 §3): one material file's keys, each edited
//! with the widget the material schema gives it ([`crate::widgets::schema`]):
//! colours for `base_color` and `emissive`, sliders for the factors in
//! `[0, 1]`, texture pickers for the texture paths, a list for `alpha_mode`.
//!
//! The material is chosen from the project's `*.material.json` files, taken
//! from the selected entity's `MaterialRef`, or made with "New material"
//! (`material.create` in `materials/`). Each finished edit is one
//! `material.edit`, so one undo step, and × removes a key so that its default
//! applies; a key the file does not set shows its default, dimmed. While a
//! drag goes on nothing is sent. A texture switched on waits for its file to
//! be chosen before anything is sent, since a material may only name files
//! that exist.
//!
//! The panel shows the material as its open document holds it (a command
//! opens it), else as its file does, read again at most once a second. A file
//! the material loader refuses is shown read-only with the reason: the
//! material commands refuse to open it, so it is fixed by hand.

use std::sync::Arc;

use egui::{Color32, Id, RichText};
use re_app::App;
use re_app::documents::material as doc;
use re_core::Diagnostic;
use re_reflect::Value;
use serde_json::{Map, Value as Json, json};

use crate::editor::{Action, Editor, Pending, command};
use crate::layout::PanelId;
use crate::widgets::schema::{self, Change, Env, Shape};

/// How long a material file's content is reused before it is read again,
/// in seconds.
const READ_AGE: f64 = 1.0;

/// Where "New material" puts the file.
const FOLDER: &str = "materials";

/// The preview's width and height in points.
const PREVIEW_POINTS: f32 = 128.0;

/// How far a drag of one point turns the preview sphere, in radians.
const TURN_PER_POINT: f32 = 0.01;

/// A place in the Material panel, in egui points, as drawn in the last
/// frame: for tests and tools that drive the panel with the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaterialSpot<'a> {
    /// A part of the material chooser: `""` its path box, `"…"` its list
    /// button, `"=<path>"` an item of the list while it is open.
    Choose(&'a str),
    /// A button by its text: `"From selection"` or `"New material"`.
    Button(&'a str),
    /// The new material's name box.
    NewName,
    /// A part of one key's row: `(key, part)`, where the part is `"name"`,
    /// `"×"` (removes the key), or a part of its widget: `""` the widget,
    /// `"0"`, `"1"`… a vector's components, `"set"` a nullable value's
    /// checkbox, `"…"` an asset picker's list button and `"=<value>"` an item
    /// of a list.
    Key(&'a str, &'a str),
    /// The preview sphere: a drag turns it.
    Preview,
}

impl MaterialSpot<'_> {
    fn key(self) -> String {
        match self {
            MaterialSpot::Choose(part) => format!("material\u{1f}choose\u{1f}{part}"),
            MaterialSpot::Button(b) => format!("material\u{1f}button\u{1f}{b}"),
            MaterialSpot::NewName => "material\u{1f}new".to_string(),
            MaterialSpot::Key(k, part) => format!("material\u{1f}key\u{1f}{k}\u{1f}{part}"),
            MaterialSpot::Preview => "material\u{1f}preview".to_string(),
        }
    }
}

/// A material file's content, or why it cannot be shown.
type Content = Result<Map<String, Json>, String>;

/// A read of a material file: its path, when it was read (egui time) and its
/// content.
type Read = (String, f64, Content);

/// The Material panel's state.
#[derive(Debug, Default)]
pub(crate) struct MaterialPanel {
    /// The material shown, relative to the project.
    path: Option<String>,
    /// The new material's name, as typed.
    new_name: String,
    /// The material schema, made once.
    schema: Option<Arc<Json>>,
    /// The last read of a material file that is not open.
    read: Option<Read>,
    /// A texture switched on whose file is not chosen yet: `(path, key)`.
    armed: Option<(String, String)>,
    /// True when the panel's tab is to come to the front once the dock is
    /// back in the editor (it is taken out while the panels are drawn).
    front: bool,
    /// The preview's egui texture, once the game view has drawn it
    /// ([`MaterialPreview`](crate::MaterialPreview)).
    texture: Option<egui::TextureId>,
    /// Why the preview could not draw the material.
    preview_error: Option<String>,
    /// How far drags have turned the preview sphere, in radians.
    turn: f32,
    /// The value under the pointer while a widget is dragged, this frame:
    /// `(key, value)`. Only the preview shows it.
    live: Option<(String, Json)>,
}

impl Editor {
    /// Where the Material panel drew `spot` in the last frame, in egui
    /// points.
    pub fn material_spot(&self, spot: MaterialSpot<'_>) -> Option<egui::Rect> {
        self.pending
            .rects
            .get(&spot.key())
            .and_then(|r| r.first().copied())
    }

    /// Shows the material at `path` (relative to the project) in the
    /// Material panel, and brings the panel's tab to the front.
    pub fn show_material(&mut self, path: &str) {
        self.material.path = Some(path.to_string());
        self.material.front = true;
        self.material_to_front();
    }

    /// The material the Material panel shows.
    pub fn shown_material(&self) -> Option<&str> {
        self.material.path.as_deref()
    }

    /// Hands the panel the preview's texture and its last error (the game
    /// view's [`MaterialPreview`](crate::MaterialPreview) does, each frame).
    pub(crate) fn set_material_preview(
        &mut self,
        texture: Option<egui::TextureId>,
        error: Option<String>,
    ) {
        self.material.texture = texture;
        self.material.preview_error = error;
    }

    /// What the preview draws: the shown material's path, its content (the
    /// open document's, else the file's as the panel last read it) with the
    /// value under the pointer, and the sphere's turn. `None` when there is
    /// no content to draw.
    pub(crate) fn material_preview_value(&self, app: &App) -> Option<(String, Json, f32)> {
        let path = self.material.path.clone()?;
        let mut content = match app.world().doc(&path) {
            Some(open) => open.value.as_object()?.clone(),
            None => match &self.material.read {
                Some((p, _, Ok(content))) if *p == path => content.clone(),
                _ => return None,
            },
        };
        if let Some((key, value)) = &self.material.live {
            if value.is_null() {
                content.remove(key);
            } else {
                content.insert(key.clone(), value.clone());
            }
        }
        Some((path, Json::Object(content), self.material.turn))
    }

    /// Brings the Material panel's tab to the front if
    /// [`Editor::show_material`] asked for it while the dock was out.
    pub(crate) fn material_to_front(&mut self) {
        if !self.material.front {
            return;
        }
        if let Some(tab) = self.dock.find_tab(&PanelId::Material) {
            let _ = self.dock.set_active_tab(tab);
            self.material.front = false;
        }
    }
}

fn record(pending: &mut Pending, spot: MaterialSpot<'_>, rect: egui::Rect) {
    pending.rects.insert(spot.key(), vec![rect]);
}

pub(super) fn material(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    ed.material.live = None;
    let files = app.files().clone();
    let assets = move |kind: &str| schema::project_assets(&files, kind);
    chooser(ed, ui, app, &assets, actions);
    ui.separator();
    let Some(path) = ed.material.path.clone() else {
        ui.label(
            RichText::new(
                "Choose one of the project's materials, take the selected entity's, or make one.",
            )
            .weak(),
        );
        return;
    };
    if ed.material.armed.as_ref().is_some_and(|(p, _)| *p != path) {
        ed.material.armed = None;
    }
    let now = ui.input(|i| i.time);
    let open = app.world().doc(&path);
    let content = match content(&mut ed.material, app, &path, now) {
        Ok(content) => content,
        Err(why) => {
            ui.label(RichText::new(why).color(Color32::RED));
            return;
        }
    };
    // An open document is checked on every change; a file is checked here.
    let refused = match open {
        Some(_) => None,
        None => doc::validate(&Json::Object(content.clone())).err(),
    };
    if let Some(why) = &refused {
        ui.label(
            RichText::new(format!(
                "{path} is not a valid material: {why}. The material commands do not open it: \
                 fix the file by hand."
            ))
            .color(Color32::RED),
        );
    } else if open.is_some_and(|d| d.dirty) {
        ui.label(RichText::new("Unsaved: saving the project writes the file.").weak());
    }
    if refused.is_none() {
        preview(ed, ui);
    }
    let root = ed
        .material
        .schema
        .get_or_insert_with(|| Arc::new(doc::schema()))
        .clone();
    let Some(props) = root.get("properties").and_then(Json::as_object) else {
        return;
    };
    let can_edit = app.commands().spec("material.edit").is_some();
    let armed = ed.material.armed.as_ref().map(|(_, k)| k.clone());
    let mut edits = Vec::new();
    ui.add_enabled_ui(can_edit && refused.is_none(), |ui| {
        egui::Grid::new(("re_editor.material.keys", path.as_str()))
            .num_columns(3)
            .striped(true)
            .show(ui, |ui| {
                for (key, prop) in props {
                    // The reference to the schema file is not the material's.
                    if key == "$schema" {
                        continue;
                    }
                    let row = Row {
                        path: &path,
                        key,
                        prop,
                        root: &root,
                        set: content.get(key),
                        armed: armed.as_deref() == Some(key.as_str()),
                    };
                    if let Some(edit) = row.show(ui, &mut ed.pending, &assets) {
                        edits.push((key.clone(), edit));
                    }
                    ui.end_row();
                }
                // Keys the schema does not know make the file invalid (the
                // message above says so): shown so that they can be found.
                for (key, value) in &content {
                    if key != "$schema" && !props.contains_key(key) {
                        ui.label(RichText::new(key).color(Color32::RED))
                            .on_hover_text("Not a material key");
                        ui.label(RichText::new(value.to_string()).weak());
                        ui.label("");
                        ui.end_row();
                    }
                }
            });
    })
    .response
    .on_disabled_hover_text(if refused.is_some() {
        "The file is not a valid material"
    } else {
        "material.edit is not registered in this build"
    });
    for (key, edit) in edits {
        match edit {
            Edit::Send(args) => {
                if armed.as_deref() == Some(key.as_str()) {
                    ed.material.armed = None;
                }
                actions.push(command("material.edit", args));
            }
            Edit::Arm => ed.material.armed = Some((path.clone(), key)),
            Edit::Disarm => ed.material.armed = None,
            Edit::Live(value) => ed.material.live = Some((key, value)),
            Edit::Report(d) => actions.push(Action::Report(d)),
        }
    }
}

/// The preview sphere, which a drag turns, and why it cannot be drawn.
fn preview(ed: &mut Editor, ui: &mut egui::Ui) {
    let size = egui::Vec2::splat(PREVIEW_POINTS);
    let r = match ed.material.texture {
        Some(id) => ui.add(egui::Image::new((id, size)).sense(egui::Sense::drag())),
        // Until the game view has drawn it, the place it will take.
        None => ui.allocate_exact_size(size, egui::Sense::drag()).1,
    }
    .on_hover_text("Drag to turn the sphere");
    record(&mut ed.pending, MaterialSpot::Preview, r.rect);
    if r.dragged() {
        ed.material.turn =
            (ed.material.turn + r.drag_delta().x * TURN_PER_POINT) % std::f32::consts::TAU;
    }
    if let Some(why) = &ed.material.preview_error {
        ui.label(RichText::new(why).small().color(Color32::RED));
    }
}

/// The chooser rows: the material's path with the project's materials,
/// "From selection", and "New material" with its name.
fn chooser(
    ed: &mut Editor,
    ui: &mut egui::Ui,
    app: &App,
    assets: &dyn Fn(&str) -> Vec<String>,
    actions: &mut Vec<Action>,
) {
    ui.horizontal_wrapped(|ui| {
        ui.label("Material");
        let shown = Json::String(ed.material.path.clone().unwrap_or_default());
        let mut env = Env::new(assets);
        let change = schema::edit(
            ui,
            Id::new("re_editor.material.choose"),
            &Shape::Asset("material".into()),
            &shown,
            &mut env,
        );
        for (part, rect) in &env.parts {
            record(&mut ed.pending, MaterialSpot::Choose(part), *rect);
        }
        if let Some(Change::Done(Json::String(p))) = change {
            let p = p.trim();
            ed.material.path = (!p.is_empty()).then(|| p.to_string());
        }

        let selected =
            ed.selected()
                .and_then(|id| match app.world().get(id, "MaterialRef", "asset") {
                    Ok(Value::Str(s)) if !s.is_empty() => Some(s),
                    _ => None,
                });
        let from = ui
            .add_enabled(selected.is_some(), egui::Button::new("From selection"))
            .on_hover_text("Show the selected entity's material (its MaterialRef)")
            .on_disabled_hover_text("The selected entity has no MaterialRef naming a material");
        record(
            &mut ed.pending,
            MaterialSpot::Button("From selection"),
            from.rect,
        );
        if from.clicked() {
            ed.material.path = selected;
        }
    });
    ui.horizontal_wrapped(|ui| {
        let name = ui.add(
            egui::TextEdit::singleline(&mut ed.material.new_name)
                .hint_text("name")
                .desired_width(120.0),
        );
        record(&mut ed.pending, MaterialSpot::NewName, name.rect);
        let can_create = app.commands().spec("material.create").is_some();
        let path = new_path(&ed.material.new_name);
        let new = ui
            .add_enabled(can_create, egui::Button::new("New material"))
            .on_hover_text(format!("Make {path} (material.create)"))
            .on_disabled_hover_text("material.create is not registered in this build");
        record(
            &mut ed.pending,
            MaterialSpot::Button("New material"),
            new.rect,
        );
        if new.clicked() {
            actions.push(command("material.create", json!({ "path": path })));
            ed.material.path = Some(path);
            ed.material.new_name.clear();
        }
    });
}

/// The file "New material" makes for the name typed:
/// `materials/<name>.material.json`.
fn new_path(name: &str) -> String {
    let name = name.trim();
    let name = if name.is_empty() { "material" } else { name };
    if name.ends_with(doc::SUFFIX) {
        format!("{FOLDER}/{name}")
    } else {
        format!("{FOLDER}/{name}{}", doc::SUFFIX)
    }
}

/// The material at `path` as the panel shows it: its open document's
/// content, else its file's (read again after [`READ_AGE`]).
fn content(panel: &mut MaterialPanel, app: &App, path: &str, now: f64) -> Content {
    if let Some(open) = app.world().doc(path) {
        panel.read = None;
        if open.is_absent() {
            return Err(format!(
                "{path} does not exist: making it was undone, and saving removes its file"
            ));
        }
        return object(path, &open.value);
    }
    if let Some((p, at, read)) = &panel.read
        && p == path
        && (0.0..READ_AGE).contains(&(now - at))
    {
        return read.clone();
    }
    let read = app
        .files()
        .read_str(path)
        .map_err(|e| format!("cannot read {path}: {e}"))
        .and_then(|text| {
            serde_json::from_str::<Json>(&text).map_err(|e| format!("{path} is not JSON: {e}"))
        })
        .and_then(|value| object(path, &value));
    panel.read = Some((path.to_string(), now, read.clone()));
    read
}

fn object(path: &str, value: &Json) -> Result<Map<String, Json>, String> {
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{path} is not a material: it is not a JSON object"))
}

/// What a row asks for.
enum Edit {
    /// `material.edit` with these arguments.
    Send(Json),
    /// A texture was switched on: its picker shows until a file is chosen.
    Arm,
    /// The texture switched on was switched off again.
    Disarm,
    /// The value under the pointer while a widget is dragged, for the
    /// preview.
    Live(Json),
    /// Text that is not a value of the key.
    Report(Diagnostic),
}

/// One key of the material: its name, its widget and ×.
struct Row<'a> {
    path: &'a str,
    key: &'a str,
    prop: &'a Json,
    root: &'a Json,
    /// The value the file sets, if it sets one.
    set: Option<&'a Json>,
    /// True when the key is a texture switched on, waiting for its file.
    armed: bool,
}

impl Row<'_> {
    fn spot<'s>(&'s self, part: &'s str) -> MaterialSpot<'s> {
        MaterialSpot::Key(self.key, part)
    }

    /// Draws the row; what it asks for, if anything.
    fn show(
        &self,
        ui: &mut egui::Ui,
        pending: &mut Pending,
        assets: &dyn Fn(&str) -> Vec<String>,
    ) -> Option<Edit> {
        let shape = Shape::of(self.prop, self.root);
        let default = schema::default(self.prop, self.root).unwrap_or(Json::Null);
        let value = match (self.set, &shape) {
            (Some(v), _) => v.clone(),
            (None, Shape::Nullable(inner)) if self.armed => inner.zero(),
            (None, _) => default.clone(),
        };
        let texture =
            matches!(&shape, Shape::Nullable(inner) if matches!(**inner, Shape::Asset(_)));

        let mut hover = schema::doc(self.prop, self.root);
        let text = RichText::new(self.key);
        let text = if self.set.is_some() {
            text
        } else {
            hover.push_str(&format!("\n\nNot set: the default ({default}) applies."));
            text.weak()
        };
        let name = ui.label(text).on_hover_text(hover.trim_start());
        record(pending, self.spot("name"), name.rect);

        // A value the widget cannot show is in a file the loader refuses,
        // which the panel shows read-only: as JSON text.
        let shape = if shape.fits(&value) {
            shape
        } else {
            shape.as_raw()
        };
        let mut env = Env::new(assets);
        let change = schema::edit(
            ui,
            Id::new(("re_editor.material", self.path, self.key)),
            &shape,
            &value,
            &mut env,
        );
        for (part, rect) in &env.parts {
            record(pending, self.spot(part), *rect);
        }
        if self.set.is_some() {
            let x = ui
                .small_button("×")
                .on_hover_text("Remove the key from the file: its default applies");
            record(pending, self.spot("×"), x.rect);
            if x.clicked() {
                return Some(Edit::Send(self.unset()));
            }
        } else {
            ui.label("");
        }
        match change? {
            // A nullable value switched off, or a texture's path emptied,
            // removes the key.
            Change::Done(Json::Null) => self.cleared(),
            Change::Done(Json::String(s)) if texture && s.trim().is_empty() => {
                if self.set.is_some() {
                    Some(Edit::Send(self.unset()))
                } else {
                    Some(Edit::Arm)
                }
            }
            Change::Done(v) if !schema::same(&v, &value) => Some(Edit::Send(json!({
                "path": self.path,
                "set": { self.key: v },
            }))),
            Change::Live(v) => Some(Edit::Live(v)),
            Change::Invalid(why) => Some(Edit::Report(schema::invalid_text(
                &why,
                &format!("`{}` of {}", self.key, self.path),
            ))),
            _ => None,
        }
    }

    fn cleared(&self) -> Option<Edit> {
        if self.set.is_some() {
            Some(Edit::Send(self.unset()))
        } else if self.armed {
            Some(Edit::Disarm)
        } else {
            None
        }
    }

    fn unset(&self) -> Json {
        json!({ "path": self.path, "unset": [self.key] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_material_goes_in_the_materials_folder() {
        assert_eq!(new_path(" red "), "materials/red.material.json");
        assert_eq!(new_path(""), "materials/material.material.json");
        assert_eq!(new_path("red.material.json"), "materials/red.material.json");
    }

    #[test]
    fn the_form_is_the_material_schema() {
        let root = doc::schema();
        let props = root["properties"].as_object().unwrap();
        let shape = |key: &str| Shape::of(&props[key], &root);
        assert_eq!(shape("base_color"), Shape::Colour { alpha: true });
        assert_eq!(shape("emissive"), Shape::Colour { alpha: false });
        assert!(matches!(
            shape("roughness"),
            Shape::Number { slider: true, .. }
        ));
        assert!(matches!(shape("alpha_mode"), Shape::Choice(v) if v.len() == 3));
        assert_eq!(
            shape("base_color_texture"),
            Shape::Nullable(Box::new(Shape::Asset("texture".into())))
        );
        assert_eq!(shape("double_sided"), Shape::Bool);
    }
}
