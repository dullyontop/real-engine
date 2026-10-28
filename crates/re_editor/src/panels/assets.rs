//! The Assets panel (ADR-0064 §7): the project's files from the asset index
//! ([`crate::assets`]) in a folder tree and a grid of tiles or a list, with
//! a kind filter and a search over every folder.
//!
//! A click selects an asset and shows its details: its path, kind, size,
//! asset id, import state, and the open scene's entities whose asset fields
//! name it (the fields with a [`FieldHint`](re_reflect::FieldHint)); a click
//! on one of those selects it. A double click, or "Open", opens the asset
//! by its kind: a material in the Material panel, a graph on the node
//! canvas (for the procedural source that names it), a scene or a prefab
//! with `scene.open`, and a script's place on disk is written to the
//! console. An asset dragged from the panel carries its path and kind
//! ([`AssetDrag`]).
//!
//! "New material", "New prefab from selection" and "New scene" make the
//! file named in the name box (or a free default name) in the folder shown,
//! else in `materials/`, `prefabs/` or `scenes/`: `material.create`,
//! `prefab.create` of the active entity, and `scene.new`. "Import" copies a
//! file from outside the project with `asset.import`, into the folder shown
//! when it is under `assets/`, else into `assets/`; files dropped on the
//! window from the system are imported the same way. A button whose command
//! is not registered in this build is disabled.
//!
//! The folder, the view (grid or list) and the kind filter are saved with
//! the layout, under `Assets`.

use egui::{Color32, RichText};
use re_app::App;
use re_scene::World;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::assets::{AssetFile, AssetIndex, AssetKind, ImportState};
use crate::editor::{Action, Editor, LineKind, Pending, command};
use crate::layout::PanelId;
use crate::selection::Selection;

/// A tile's thumbnail, in points.
pub(crate) const THUMB: f32 = 96.0;

/// The space between tiles, in points.
const GAP: f32 = 6.0;

/// How long the list of an asset's users is kept before it is made again,
/// in seconds.
const USERS_AGE: f64 = 0.5;

/// The most users the details list by id.
const USERS_SHOWN: usize = 12;

/// A place in the Assets panel, in egui points, as drawn in the last frame:
/// for tests and tools that drive the panel with the pointer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetSpot<'a> {
    /// A folder of the tree, by path; `""` is the project's top.
    Folder(&'a str),
    /// An asset's tile or row, by path (`<model>#<name>` for a clip or a
    /// skin).
    Item(&'a str),
    /// The kind filter's button.
    Filter,
    /// An entry of the kind filter's list while it is open: a kind's name,
    /// or `"all"`.
    Kind(&'a str),
    /// The search box.
    Search,
    /// A text box: `"name"` (the new file's name) or `"import"` (the file
    /// to import).
    Text(&'a str),
    /// A button by its text: `"Grid"`, `"List"`, `"Open"`, `"New material"`,
    /// `"New prefab from selection"`, `"New scene"` or `"Import"`.
    Button(&'a str),
    /// An entity in the selected asset's details that uses it, by id.
    User(&'a str),
}

impl AssetSpot<'_> {
    fn key(self) -> String {
        match self {
            AssetSpot::Folder(p) => format!("assets\u{1f}folder\u{1f}{p}"),
            AssetSpot::Item(p) => format!("assets\u{1f}item\u{1f}{p}"),
            AssetSpot::Filter => "assets\u{1f}filter".to_string(),
            AssetSpot::Kind(k) => format!("assets\u{1f}kind\u{1f}{k}"),
            AssetSpot::Search => "assets\u{1f}search".to_string(),
            AssetSpot::Text(t) => format!("assets\u{1f}text\u{1f}{t}"),
            AssetSpot::Button(b) => format!("assets\u{1f}button\u{1f}{b}"),
            AssetSpot::User(id) => format!("assets\u{1f}user\u{1f}{id}"),
        }
    }
}

/// What an asset dragged from the Assets panel carries (egui's drag and
/// drop payload), for the places it can be dropped (ADR-0064 §8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetDrag {
    /// The asset's path (`<model>#<name>` for a clip or a skin).
    pub path: String,
    /// What it is.
    pub kind: AssetKind,
}

/// The panel's view, saved with the layout.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct View {
    /// The folder shown; `""` is the project's top.
    folder: String,
    /// A list instead of the grid of tiles.
    list: bool,
    /// The kind shown, by name; `None` shows every kind.
    kind: Option<String>,
}

/// The Assets panel's state that is not saved.
#[derive(Debug, Default)]
pub(crate) struct AssetsPanel {
    /// The search, over every folder.
    search: String,
    /// The selected asset's path.
    selected: Option<String>,
    /// The name box of the "New" buttons.
    name: String,
    /// The file "Import" copies in.
    import: String,
    /// A tab to bring to the front once the dock is back in the editor (it
    /// is taken out while the panels are drawn).
    front: Option<PanelId>,
    /// The entities that use the selected asset: its path, when (egui time)
    /// they were found, and their ids.
    users: Option<(String, f64, Vec<String>)>,
    /// The folder shown when the tree last opened it and the folders above
    /// it: a folder chosen (or read from the layout) is opened once.
    revealed: Option<String>,
}

impl Editor {
    /// Where the Assets panel drew `spot` in the last frame, in egui points.
    pub fn asset_spot(&self, spot: AssetSpot<'_>) -> Option<egui::Rect> {
        self.pending
            .rects
            .get(&spot.key())
            .and_then(|r| r.first().copied())
    }

    /// The asset selected in the Assets panel.
    pub fn selected_asset(&self) -> Option<&str> {
        self.assets_panel.selected.as_deref()
    }

    /// Selects the asset at `path` in the Assets panel (or nothing).
    pub fn select_asset(&mut self, path: Option<&str>) {
        self.assets_panel.selected = path.map(str::to_string);
    }

    /// Brings the tab the Assets panel asked for to the front, once the dock
    /// is back in the editor.
    pub(crate) fn assets_to_front(&mut self) {
        let Some(panel) = self.assets_panel.front.take() else {
            return;
        };
        if let Some(tab) = self.dock.find_tab(&panel) {
            let _ = self.dock.set_active_tab(tab);
        }
    }
}

fn record(pending: &mut Pending, spot: AssetSpot<'_>, rect: egui::Rect) {
    pending.rects.insert(spot.key(), vec![rect]);
}

/// One tile or row: a file, or a model's clip or skin.
struct Item<'a> {
    /// The asset's path (`<model>#<name>` for a clip or a skin).
    path: String,
    /// What the tile shows under its thumbnail.
    name: String,
    kind: AssetKind,
    file: &'a AssetFile,
}

pub(super) fn assets(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    let mut view: View = ed.panel_state(PanelId::Assets);
    if !view.folder.is_empty() && !ed.assets.folders().contains(&view.folder) && ed.assets.ready() {
        // The folder went away.
        view.folder.clear();
    }
    toolbar(ed, ui, app, &mut view, actions);
    ui.separator();
    let index = std::mem::take(&mut ed.assets);
    let reveal = ed.assets_panel.revealed.as_deref() != Some(view.folder.as_str());
    ed.assets_panel.revealed = Some(view.folder.clone());
    egui::Panel::left("re_editor.assets.folders")
        .resizable(true)
        .default_size(160.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .id_salt("re_editor.assets.folders")
                .auto_shrink(false)
                .show(ui, |ui| {
                    folders(ui, &index, &mut view, reveal, &mut ed.pending)
                });
        });
    let selected = ed.assets_panel.selected.clone();
    let shown = selected.as_deref().and_then(|p| item(&index, p));
    if let Some(shown) = &shown {
        egui::Panel::right("re_editor.assets.details")
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("re_editor.assets.details")
                    .auto_shrink(false)
                    .show(ui, |ui| details(ed, ui, app, &index, shown, actions));
            });
    }
    let mut open = None;
    egui::CentralPanel::default().show(ui, |ui| {
        let items = items(&index, &view, &ed.assets_panel.search);
        if !index.ready() && index.files().next().is_none() {
            ui.label(RichText::new("Looking through the project…").weak());
        } else if items.is_empty() {
            ui.label(RichText::new(empty_text(&view, &ed.assets_panel.search)).weak());
        }
        let clicked = if view.list {
            list(ui, app, &index, &items, &selected, &mut ed.pending)
        } else {
            grid(ui, &items, &selected, &mut ed.pending)
        };
        if let Some((path, double)) = clicked {
            if double {
                open = Some(path.clone());
            }
            ed.assets_panel.selected = Some(path);
        }
    });
    ed.assets = index;
    if let Some(path) = open
        && let Some(item) = item(&ed.assets, &path)
    {
        let kind = item.kind;
        open_asset(ed, app, &path, kind, actions);
    }
    ed.set_panel_state(PanelId::Assets, &view);
}

/// The text shown when no asset is.
fn empty_text(view: &View, search: &str) -> String {
    let kind = view.kind.as_deref().map_or("files", |k| k);
    if !search.trim().is_empty() {
        format!("No {kind} match the search.")
    } else if view.folder.is_empty() {
        format!("No {kind} at the project's top.")
    } else {
        format!("No {kind} in {}.", view.folder)
    }
}

/// The asset at `path` (a file, or `<model>#<name>`) as an item.
fn item<'a>(index: &'a AssetIndex, path: &str) -> Option<Item<'a>> {
    if let Some(file) = index.file(path) {
        return Some(Item {
            path: path.to_string(),
            name: file_name(path).to_string(),
            kind: file.kind,
            file,
        });
    }
    let (model, part) = path.split_once('#')?;
    let file = index.file(model)?;
    let kind = if file.clips.iter().any(|c| c == part) {
        AssetKind::Clip
    } else if file.skins.iter().any(|s| s == part) {
        AssetKind::Skin
    } else {
        return None;
    };
    Some(Item {
        path: path.to_string(),
        name: format!("{}#{part}", file_name(model)),
        kind,
        file,
    })
}

/// The items to show: the folder's files, with each model's clips and
/// skins after it, of the kind shown; with a search, those of every folder
/// whose path holds it (in any letter case).
fn items<'a>(index: &'a AssetIndex, view: &View, search: &str) -> Vec<Item<'a>> {
    let kind = view.kind.as_deref().and_then(AssetKind::from_name);
    let needle = search.trim().to_lowercase();
    let mut out = Vec::new();
    for file in index.files() {
        if needle.is_empty() && parent(&file.path) != view.folder {
            continue;
        }
        let base = file_name(&file.path);
        let parts = std::iter::once((file.path.clone(), base.to_string(), file.kind))
            .chain(file.clips.iter().map(|c| {
                (
                    format!("{}#{c}", file.path),
                    format!("{base}#{c}"),
                    AssetKind::Clip,
                )
            }))
            .chain(file.skins.iter().map(|s| {
                (
                    format!("{}#{s}", file.path),
                    format!("{base}#{s}"),
                    AssetKind::Skin,
                )
            }));
        for (path, name, k) in parts {
            if kind.is_none_or(|want| want == k)
                && (needle.is_empty() || path.to_lowercase().contains(&needle))
            {
                out.push(Item {
                    path,
                    name,
                    kind: k,
                    file,
                });
            }
        }
    }
    out
}

/// The folder holding `path`; `""` for the project's top.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The last segment of `path`.
fn file_name(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// The view, filter and search row, and the "New" and "Import" buttons.
fn toolbar(
    ed: &mut Editor,
    ui: &mut egui::Ui,
    app: &App,
    view: &mut View,
    actions: &mut Vec<Action>,
) {
    let pending = &mut ed.pending;
    let panel = &mut ed.assets_panel;
    let mut show_material = None;
    ui.horizontal_wrapped(|ui| {
        for (list, text) in [(false, "Grid"), (true, "List")] {
            let r = ui.selectable_label(view.list == list, text);
            record(pending, AssetSpot::Button(text), r.rect);
            if r.clicked() {
                view.list = list;
            }
        }
        let shown = view.kind.as_deref().unwrap_or("all kinds");
        let combo = egui::ComboBox::from_id_salt("re_editor.assets.kind")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                let r = ui.selectable_label(view.kind.is_none(), "all kinds");
                record(pending, AssetSpot::Kind("all"), r.rect);
                if r.clicked() {
                    view.kind = None;
                }
                for kind in AssetKind::ALL {
                    let r =
                        ui.selectable_label(view.kind.as_deref() == Some(kind.name()), kind.name());
                    record(pending, AssetSpot::Kind(kind.name()), r.rect);
                    if r.clicked() {
                        view.kind = Some(kind.name().to_string());
                    }
                }
            });
        record(pending, AssetSpot::Filter, combo.response.rect);
        let r = ui.add(
            egui::TextEdit::singleline(&mut panel.search)
                .hint_text("search every folder")
                .desired_width(140.0),
        );
        record(pending, AssetSpot::Search, r.rect);
    });
    ui.horizontal_wrapped(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut panel.name)
                .hint_text("name")
                .desired_width(100.0),
        );
        record(pending, AssetSpot::Text("name"), r.rect);
        let active = ed.selection.active().map(str::to_string);
        let news = [
            (
                AssetKind::Material,
                "New material",
                "material.create",
                "materials",
            ),
            (
                AssetKind::Prefab,
                "New prefab from selection",
                "prefab.create",
                "prefabs",
            ),
            (AssetKind::Scene, "New scene", "scene.new", "scenes"),
        ];
        for (kind, text, name, folder) in news {
            let folder = if view.folder.is_empty() {
                folder
            } else {
                view.folder.as_str()
            };
            // Found only when it is wanted: it looks at the disk.
            let path = || free_path(app, &ed.assets, folder, &panel.name, kind);
            let registered = app.commands().spec(name).is_some();
            let needs_root = kind == AssetKind::Prefab && active.is_none();
            let r = ui
                .add_enabled(registered && !needs_root, egui::Button::new(text))
                .on_hover_ui(|ui| {
                    ui.label(format!("Make {} ({name})", path()));
                })
                .on_disabled_hover_text(if registered {
                    "Select the entity the prefab is made from".to_string()
                } else {
                    format!("{name} is not registered in this build")
                });
            record(pending, AssetSpot::Button(text), r.rect);
            if r.clicked() {
                let path = path();
                let args = match kind {
                    AssetKind::Prefab => json!({ "path": path, "root": active }),
                    _ => json!({ "path": path }),
                };
                actions.push(command(name, args));
                if kind == AssetKind::Material {
                    show_material = Some(path.clone());
                }
                panel.selected = Some(path);
                panel.name.clear();
            }
        }
        ui.separator();
        let r = ui.add(
            egui::TextEdit::singleline(&mut panel.import)
                .hint_text("file to import")
                .desired_width(160.0),
        );
        record(pending, AssetSpot::Text("import"), r.rect);
        let registered = app.commands().spec("asset.import").is_some();
        let source = panel.import.trim().to_string();
        let r = ui
            .add_enabled(
                registered && !source.is_empty(),
                egui::Button::new("Import"),
            )
            .on_hover_text(
                "Copy the file into the project and import it (asset.import; not undoable)",
            )
            .on_disabled_hover_text(if registered {
                "Type the path of a file outside the project"
            } else {
                "asset.import is not registered in this build"
            });
        record(pending, AssetSpot::Button("Import"), r.rect);
        if r.clicked() {
            let (args, dest) = import_args(&source, &view.folder);
            actions.push(command("asset.import", args));
            panel.selected = Some(dest);
            panel.import.clear();
        }
    });
    if let Some(path) = show_material {
        ed.show_material(&path);
    }
}

/// `asset.import`'s arguments for the file `source`, into `folder` when it
/// is under `assets/`, else into `assets/`; and where the file will be.
fn import_args(source: &str, folder: &str) -> (serde_json::Value, String) {
    let name = std::path::Path::new(source)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if folder == "assets" || folder.starts_with("assets/") {
        let dest = format!("{folder}/{name}");
        (json!({ "source": source, "dest": dest }), dest)
    } else {
        (json!({ "source": source }), format!("assets/{name}"))
    }
}

/// The file a "New" button makes: `<folder>/<name><suffix>`, from the name
/// typed (the kind's name when none is), with `_2`, `_3`… added while a file,
/// an index entry or an open document has that path.
fn free_path(app: &App, index: &AssetIndex, folder: &str, typed: &str, kind: AssetKind) -> String {
    let suffix = kind.suffixes()[0];
    let typed = typed.trim();
    let stem = typed.strip_suffix(suffix).unwrap_or(typed);
    let stem = if stem.is_empty() { kind.name() } else { stem };
    let taken = |p: &str| {
        index.file(p).is_some()
            || app.files().is_file(p)
            || app.world().doc(p).is_some_and(|d| !d.is_absent())
    };
    let mut path = format!("{folder}/{stem}{suffix}");
    let mut n = 2;
    while taken(&path) {
        path = format!("{folder}/{stem}_{n}{suffix}");
        n += 1;
    }
    path
}

/// The folder tree: the project's top, then every folder, nested. With
/// `reveal`, the folder shown and those above it are opened.
fn folders(
    ui: &mut egui::Ui,
    index: &AssetIndex,
    view: &mut View,
    reveal: bool,
    pending: &mut Pending,
) {
    let r = ui.selectable_label(view.folder.is_empty(), "(project)");
    record(pending, AssetSpot::Folder(""), r.rect);
    if r.clicked() {
        view.folder.clear();
    }
    folder_children(ui, index, "", view, reveal, pending);
}

fn folder_children(
    ui: &mut egui::Ui,
    index: &AssetIndex,
    dir: &str,
    view: &mut View,
    reveal: bool,
    pending: &mut Pending,
) {
    let children: Vec<&String> = index
        .folders()
        .iter()
        .filter(|f| parent(f) == dir)
        .collect();
    for f in children {
        let name = file_name(f);
        let has_children = index.folders().iter().any(|g| parent(g) == f.as_str());
        let selected = view.folder == *f;
        if !has_children {
            ui.indent(("re_editor.assets.leaf", f.as_str()), |ui| {
                let r = ui.selectable_label(selected, name);
                record(pending, AssetSpot::Folder(f), r.rect);
                if r.clicked() {
                    view.folder = f.clone();
                }
            });
            continue;
        }
        let shown_inside = view.folder == *f || view.folder.starts_with(&format!("{f}/"));
        let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(
            ui.ctx(),
            ui.make_persistent_id(("re_editor.assets.folder", f.as_str())),
            shown_inside,
        );
        if reveal && shown_inside {
            state.set_open(true);
        }
        state
            .show_header(ui, |ui| {
                let r = ui.selectable_label(selected, name);
                record(pending, AssetSpot::Folder(f), r.rect);
                if r.clicked() {
                    view.folder = f.clone();
                }
            })
            .body(|ui| folder_children(ui, index, f, view, reveal, pending));
    }
}

/// The tiles, in rows as wide as the panel; only the rows in view are drawn
/// (a folder can hold thousands of files). Returns the clicked item and
/// whether it was a double click.
fn grid(
    ui: &mut egui::Ui,
    items: &[Item<'_>],
    selected: &Option<String>,
    pending: &mut Pending,
) -> Option<(String, bool)> {
    let size = egui::vec2(THUMB + 8.0, THUMB + 24.0);
    ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
    let width = ui.available_width() - ui.spacing().scroll.allocated_width();
    let columns = (((width + GAP) / (size.x + GAP)).floor() as usize).max(1);
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .id_salt("re_editor.assets.items")
        .auto_shrink(false)
        .show_rows(ui, size.y, items.len().div_ceil(columns), |ui, rows| {
            for row in items.chunks(columns).skip(rows.start).take(rows.len()) {
                ui.horizontal(|ui| {
                    for item in row {
                        if let Some(c) = tile(ui, item, size, selected, pending) {
                            clicked = Some(c);
                        }
                    }
                });
            }
        });
    clicked
}

/// One tile: the asset's icon, and its name under it. Returns its path when
/// it was clicked, and whether it was a double click.
fn tile(
    ui: &mut egui::Ui,
    item: &Item<'_>,
    size: egui::Vec2,
    selected: &Option<String>,
    pending: &mut Pending,
) -> Option<(String, bool)> {
    // By path, not by place: the tiles drawn change as the grid scrolls.
    let (_, rect) = ui.allocate_space(size);
    let id = egui::Id::new(("re_editor.assets.tile", item.path.as_str()));
    let r = ui.interact(rect, id, egui::Sense::click_and_drag());
    record(pending, AssetSpot::Item(&item.path), rect);
    let is_selected = selected.as_deref() == Some(item.path.as_str());
    let visuals = ui.visuals();
    if is_selected {
        ui.painter()
            .rect_filled(rect, 4.0, visuals.selection.bg_fill.gamma_multiply(0.6));
    } else if r.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let thumb =
        egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), egui::vec2(THUMB, THUMB));
    icon(ui, thumb, item.kind);
    let label = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + 2.0, thumb.max.y + 2.0),
        egui::pos2(rect.max.x - 2.0, rect.max.y),
    );
    let galley = ui.painter().layout(
        item.name.clone(),
        egui::FontId::proportional(11.0),
        visuals.text_color(),
        label.width(),
    );
    let text_pos = egui::pos2(
        label.center().x - galley.size().x.min(label.width()) / 2.0,
        label.min.y,
    );
    ui.painter()
        .with_clip_rect(label)
        .galley(text_pos, galley, visuals.text_color());
    let r = r.on_hover_text(&item.path);
    r.dnd_set_drag_payload(AssetDrag {
        path: item.path.clone(),
        kind: item.kind,
    });
    (r.clicked() || r.double_clicked()).then(|| (item.path.clone(), r.double_clicked()))
}

/// The rows: name, kind, size and import state; only the rows in view are
/// drawn.
fn list(
    ui: &mut egui::Ui,
    app: &App,
    index: &AssetIndex,
    items: &[Item<'_>],
    selected: &Option<String>,
    pending: &mut Pending,
) -> Option<(String, bool)> {
    let height = ui.spacing().interact_size.y;
    let mut clicked = None;
    egui::ScrollArea::vertical()
        .id_salt("re_editor.assets.items")
        .auto_shrink(false)
        .show_rows(ui, height, items.len(), |ui, rows| {
            egui::Grid::new("re_editor.assets.list")
                .num_columns(4)
                .striped(true)
                .min_row_height(height)
                .start_row(rows.start)
                .show(ui, |ui| {
                    for item in &items[rows] {
                        let is_selected = selected.as_deref() == Some(item.path.as_str());
                        let r = ui
                            .add(
                                egui::Button::selectable(is_selected, item.name.as_str())
                                    .sense(egui::Sense::click_and_drag()),
                            )
                            .on_hover_text(&item.path);
                        record(pending, AssetSpot::Item(&item.path), r.rect);
                        r.dnd_set_drag_payload(AssetDrag {
                            path: item.path.clone(),
                            kind: item.kind,
                        });
                        if r.clicked() || r.double_clicked() {
                            clicked = Some((item.path.clone(), r.double_clicked()));
                        }
                        ui.label(RichText::new(item.kind.name()).weak());
                        let size = match item.kind {
                            AssetKind::Clip | AssetKind::Skin => String::new(),
                            _ => size_text(item.file.size),
                        };
                        ui.label(RichText::new(size).weak());
                        match index.state(app, &item.path) {
                            Some(state) => ui.label(state_text(&state)),
                            None => ui.label(""),
                        };
                        ui.end_row();
                    }
                });
        });
    clicked
}

/// A tile's icon: the kind's name on its colour.
fn icon(ui: &egui::Ui, rect: egui::Rect, kind: AssetKind) {
    let painter = ui.painter();
    painter.rect_filled(rect, 6.0, kind_colour(kind).gamma_multiply(0.35));
    painter.rect_stroke(
        rect,
        6.0,
        egui::Stroke::new(1.0, kind_colour(kind)),
        egui::StrokeKind::Inside,
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        kind.name(),
        egui::FontId::proportional(13.0),
        ui.visuals().strong_text_color(),
    );
}

/// The colour of a kind's icon.
fn kind_colour(kind: AssetKind) -> Color32 {
    match kind {
        AssetKind::Material => Color32::from_rgb(0xd0, 0x8a, 0x40),
        AssetKind::Prefab => Color32::from_rgb(0x50, 0x90, 0xd8),
        AssetKind::Scene => Color32::from_rgb(0x9a, 0x70, 0xd0),
        AssetKind::Graph => Color32::from_rgb(0x40, 0xa8, 0x98),
        AssetKind::AnimGraph => Color32::from_rgb(0x58, 0xb0, 0x60),
        AssetKind::Script => Color32::from_rgb(0xc8, 0xb4, 0x40),
        AssetKind::Texture => Color32::from_rgb(0xd0, 0x60, 0x90),
        AssetKind::Model => Color32::from_rgb(0x70, 0x88, 0xa0),
        AssetKind::Clip => Color32::from_rgb(0x80, 0xc0, 0x50),
        AssetKind::Skin => Color32::from_rgb(0xb8, 0x90, 0x68),
        AssetKind::Audio => Color32::from_rgb(0xd0, 0x58, 0x50),
    }
}

/// An import state, coloured: a failure red, a stale import yellow.
fn state_text(state: &ImportState) -> RichText {
    let text = match state {
        ImportState::Loaded(v) => format!("loaded (version {v})"),
        ImportState::Failed(_) => "failed".to_string(),
        other => other.name().to_string(),
    };
    match state {
        ImportState::Failed(_) => RichText::new(text).color(Color32::RED),
        ImportState::Stale => RichText::new(text).color(Color32::YELLOW),
        ImportState::Loaded(_) | ImportState::Imported => {
            RichText::new(text).color(Color32::from_rgb(0x6c, 0xc0, 0x7a))
        }
        ImportState::Source => RichText::new(text).weak(),
    }
}

/// A size in bytes for people.
fn size_text(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        format!("{bytes} B")
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB)
    } else if b < KIB * KIB * KIB {
        format!("{:.1} MiB", b / (KIB * KIB))
    } else {
        format!("{:.2} GiB", b / (KIB * KIB * KIB))
    }
}

/// The selected asset's details, and "Open".
fn details(
    ed: &mut Editor,
    ui: &mut egui::Ui,
    app: &App,
    index: &AssetIndex,
    item: &Item<'_>,
    actions: &mut Vec<Action>,
) {
    ui.label(RichText::new(&item.name).strong());
    egui::Grid::new("re_editor.assets.details.grid")
        .num_columns(2)
        .show(ui, |ui| {
            ui.label("path");
            ui.label(RichText::new(&item.path).monospace());
            ui.end_row();
            ui.label("kind");
            ui.label(item.kind.name());
            ui.end_row();
            if !matches!(item.kind, AssetKind::Clip | AssetKind::Skin) {
                ui.label("size");
                ui.label(size_text(item.file.size));
                ui.end_row();
            }
            if let Some(id) = &item.file.id {
                ui.label("asset id");
                ui.label(RichText::new(id).monospace().small());
                ui.end_row();
            }
            if let Some(state) = index.state(app, &item.path) {
                ui.label("import");
                let r = ui.label(state_text(&state));
                if let ImportState::Failed(why) = &state {
                    r.on_hover_text(why);
                }
                ui.end_row();
            }
        });
    let (can_open, why) = can_open(app, item.kind);
    let r = ui
        .add_enabled(can_open, egui::Button::new("Open"))
        .on_hover_text(open_text(item.kind))
        .on_disabled_hover_text(why);
    record(&mut ed.pending, AssetSpot::Button("Open"), r.rect);
    if r.clicked() {
        let (path, kind) = (item.path.clone(), item.kind);
        open_asset(ed, app, &path, kind, actions);
    }
    ui.separator();
    let now = ui.input(|i| i.time);
    let fresh = matches!(&ed.assets_panel.users, Some((p, at, _)) if *p == item.path && now - at < USERS_AGE && now >= *at);
    if !fresh {
        ed.assets_panel.users = Some((
            item.path.clone(),
            now,
            users(app.world(), &item.path, item.kind),
        ));
    }
    let Some((_, _, users)) = &ed.assets_panel.users else {
        return;
    };
    if users.is_empty() {
        ui.label(RichText::new("No entity in the open scene uses it.").weak());
        return;
    }
    ui.label(format!(
        "Used by {} entit{} in the open scene:",
        users.len(),
        if users.len() == 1 { "y" } else { "ies" }
    ));
    let mut select = None;
    for id in users.iter().take(USERS_SHOWN) {
        let r = ui.link(id.as_str());
        record(&mut ed.pending, AssetSpot::User(id), r.rect);
        if r.clicked() {
            select = Some(id.clone());
        }
    }
    if users.len() > USERS_SHOWN {
        ui.label(RichText::new(format!("and {} more", users.len() - USERS_SHOWN)).weak());
    }
    if let Some(id) = select {
        ed.select_next = Some(Selection::one(&id));
    }
}

/// Whether "Open" can open an asset of `kind` here, and why not.
fn can_open(app: &App, kind: AssetKind) -> (bool, String) {
    match kind {
        AssetKind::Material | AssetKind::Graph | AssetKind::Script => (true, String::new()),
        AssetKind::Scene | AssetKind::Prefab => {
            let registered = app.commands().spec("scene.open").is_some();
            (
                registered,
                "scene.open is not registered in this build".to_string(),
            )
        }
        other => (
            false,
            format!("The editor does not open {} assets", other.name()),
        ),
    }
}

/// What "Open" does for an asset of `kind`.
fn open_text(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Material => "Show it in the Material panel",
        AssetKind::Graph => "Show it on the node canvas, for the procedural source that names it",
        AssetKind::Script => "Write where it is on disk to the console",
        AssetKind::Scene | AssetKind::Prefab => "Edit it (scene.open)",
        _ => "",
    }
}

/// Opens the asset at `path` by its kind (see [`can_open`]).
fn open_asset(ed: &mut Editor, app: &App, path: &str, kind: AssetKind, actions: &mut Vec<Action>) {
    if !can_open(app, kind).0 {
        return;
    }
    match kind {
        AssetKind::Material => ed.show_material(path),
        AssetKind::Graph => {
            let world = app.world();
            let mut sources: Vec<&str> = world
                .ids()
                .iter()
                .filter(|(id, _)| {
                    matches!(world.get(id, "GraphSource", "graph"), Ok(re_reflect::Value::Str(g)) if g == path)
                })
                .map(|(id, _)| id)
                .collect();
            sources.sort_unstable();
            match sources.first() {
                Some(source) => {
                    let source = source.to_string();
                    ed.set_canvas_source(Some(&source));
                    ed.assets_panel.front = Some(PanelId::Canvas);
                }
                None => ed.log(
                    LineKind::Info,
                    format!(
                        "No procedural source of the open scene names {path}: set a source's \
                         GraphSource.graph to it to see it on the node canvas"
                    ),
                ),
            }
        }
        AssetKind::Script => {
            let at = app.project().join(path);
            ed.log(LineKind::Info, format!("{path} is at {}", at.display()));
        }
        AssetKind::Scene | AssetKind::Prefab => {
            actions.push(command("scene.open", json!({ "path": path })));
        }
        _ => {}
    }
}

/// The entities of `world` whose asset fields name the asset at `path`, of
/// `kind`, sorted by id: fields hinted with its kind holding its path, and
/// for a model also the clip and skin fields naming one of its parts.
fn users(world: &World, path: &str, kind: AssetKind) -> Vec<String> {
    let registry = &world.registry;
    let part = format!("{path}#");
    let mut fields = Vec::new();
    for key in registry.keys() {
        for i in 0..registry.schema(key).fields.len() {
            let Some(hinted) = registry.field_hint(key, i).asset else {
                continue;
            };
            let parts = kind == AssetKind::Model && matches!(hinted, "clip" | "skin");
            if hinted == kind.name() || parts {
                fields.push((key, i, parts));
            }
        }
    }
    let mut out: Vec<String> = world
        .ids()
        .iter()
        .filter(|(_, e)| {
            fields.iter().any(|&(key, i, parts)| {
                matches!(
                    registry.get_field(&world.ecs, *e, key, i),
                    Some(re_reflect::Value::Str(s)) if s == path || (parts && s.starts_with(&part))
                )
            })
        })
        .map(|(id, _)| id.to_string())
        .collect();
    out.sort();
    out
}

/// Imports the files dropped on the window from the system
/// (`asset.import`), into the folder the Assets panel shows when it is under
/// `assets/`.
pub(super) fn system_drops(
    ed: &mut Editor,
    ctx: &egui::Context,
    app: &App,
    actions: &mut Vec<Action>,
) {
    let dropped: Vec<std::path::PathBuf> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .collect()
    });
    if dropped.is_empty() || app.commands().spec("asset.import").is_none() {
        return;
    }
    let view: View = ed.panel_state(PanelId::Assets);
    for path in dropped {
        let (args, dest) = import_args(&path.display().to_string(), &view.folder);
        actions.push(command("asset.import", args));
        ed.assets_panel.selected = Some(dest);
    }
    ed.assets_panel.front = Some(PanelId::Assets);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_go_into_the_folder_shown_under_assets() {
        let (args, dest) = import_args("/tmp/x/rock.png", "assets/textures");
        assert_eq!(
            args,
            json!({ "source": "/tmp/x/rock.png", "dest": "assets/textures/rock.png" })
        );
        assert_eq!(dest, "assets/textures/rock.png");
        let (args, dest) = import_args("/tmp/x/rock.png", "materials");
        assert_eq!(args, json!({ "source": "/tmp/x/rock.png" }));
        assert_eq!(dest, "assets/rock.png");
        assert_eq!(
            import_args("/tmp/x/rock.png", "assets").1,
            "assets/rock.png"
        );
    }

    #[test]
    fn paths_and_sizes() {
        assert_eq!(parent("a/b/c.png"), "a/b");
        assert_eq!(parent("c.png"), "");
        assert_eq!(file_name("a/b/c.png"), "c.png");
        assert_eq!(size_text(512), "512 B");
        assert_eq!(size_text(1536), "1.5 KiB");
        assert_eq!(size_text(3 * 1024 * 1024), "3.0 MiB");
    }
}
