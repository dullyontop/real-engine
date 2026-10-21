//! [`Editor`]: the editor's UI state, its panels and the commands they
//! dispatch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use re_app::App;
use re_command::{CommandOutcome, Origin, PolicyCheck};
use re_core::{Diagnostic, Severity};
use re_reflect::{Doc, FieldType, Value};
use serde_json::{Value as Json, json};

use crate::gizmo::{Gizmo, GizmoSettings};
use crate::layout::{self, PanelId};
use crate::panels;

/// The most console lines kept; older ones are dropped.
const CONSOLE_LINES: usize = 2000;

/// One of the editor's panels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Panel {
    /// The toolbar along the top (undo, redo, palette, status).
    Toolbar,
    /// The entity tree.
    Hierarchy,
    /// The selected entity's components.
    Inspector,
    /// Diagnostics, command results and the transaction log.
    Console,
    /// Procedural sources: seed, parameter overrides, regenerate.
    Graph,
    /// The command palette window (only while it is open).
    Palette,
    /// The game view.
    Viewport,
}

impl Panel {
    /// Every panel, in drawing order.
    pub const ALL: [Panel; 7] = [
        Panel::Toolbar,
        Panel::Hierarchy,
        Panel::Inspector,
        Panel::Console,
        Panel::Graph,
        Panel::Palette,
        Panel::Viewport,
    ];

    fn index(self) -> usize {
        self as usize
    }
}

/// Where each panel was drawn in the last frame, in egui points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PanelRects([Option<egui::Rect>; 7]);

impl PanelRects {
    /// The rectangle of `panel`, or `None` if it was not drawn.
    pub fn get(&self, panel: Panel) -> Option<egui::Rect> {
        self.0[panel.index()]
    }

    pub(crate) fn set(&mut self, panel: Panel, rect: egui::Rect) {
        self.0[panel.index()] = Some(rect);
    }
}

/// How serious a console line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    /// A transaction, a command result.
    Info,
    /// A warning diagnostic.
    Warning,
    /// An error diagnostic.
    Error,
}

/// One console line.
#[derive(Clone, Debug, PartialEq)]
pub struct ConsoleLine {
    /// How serious it is.
    pub kind: LineKind,
    /// The text (one or more lines).
    pub text: String,
}

/// A change the panels asked for while they were drawn; applied after drawing,
/// when the world can be borrowed mutably.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Action {
    /// Dispatch a command.
    Command {
        name: String,
        args: Json,
        dry_run: bool,
        /// Show the command's result data in the console (palette runs).
        show_data: bool,
    },
    /// Set one field through `entity.set`.
    SetField {
        id: String,
        component: String,
        field: String,
        ty: FieldType,
        value: Value,
    },
    /// Report a problem found while reading the panels' input.
    Report(Diagnostic),
}

/// The command palette's state.
#[derive(Clone, Debug, Default)]
pub(crate) struct Palette {
    pub open: bool,
    pub filter: String,
    pub selected: Option<String>,
    pub args: String,
    pub dry_run: bool,
    pub focus_filter: bool,
}

/// Input that is being edited and not yet committed: a value being dragged,
/// or text in a focused field.
#[derive(Clone, Debug, Default)]
pub(crate) struct Pending {
    pub values: HashMap<String, Value>,
    pub texts: HashMap<String, String>,
    /// Where each field's widgets were drawn in the last frame.
    pub rects: HashMap<String, Vec<egui::Rect>>,
}

impl Pending {
    /// Records where a field's widgets were drawn this frame.
    pub fn widgets(&mut self, key: &str, rs: &[egui::Response]) {
        self.rects
            .insert(key.to_string(), rs.iter().map(|r| r.rect).collect());
    }
}

/// The key of one hierarchy row in [`Pending::rects`].
pub(crate) fn row_key(id: &str) -> String {
    format!("row\u{1f}{id}")
}

/// The key of one inspector field in [`Pending`].
pub(crate) fn field_key(id: &str, component: &str, field: &str) -> String {
    format!("{id}\u{1f}{component}\u{1f}{field}")
}

/// The editor's UI: selection, console, palette and in-progress edits.
///
/// [`Editor::ui`] draws every panel for one frame and then applies what they
/// asked for. Each change is a command dispatched with
/// [`App::dispatch`] as [`Origin::User`], through the editor's policy, so each
/// is one undo step: an inspector field commits when its drag ends, when
/// Enter is pressed or when it loses focus, never while it is being dragged.
pub struct Editor {
    pub(crate) selected: Option<String>,
    pub(crate) console: Vec<ConsoleLine>,
    pub(crate) palette: Palette,
    pub(crate) pending: Pending,
    pub(crate) graph_new: HashMap<String, [String; 3]>,
    pub(crate) select_next: Option<String>,
    pub(crate) later: Vec<Action>,
    pub(crate) rects: PanelRects,
    pub(crate) status: String,
    pub(crate) viewport_texture: Option<(egui::TextureId, [u32; 2])>,
    pub(crate) viewport_px: Option<[u32; 2]>,
    pub(crate) gizmo: Gizmo,
    pub(crate) debug: crate::debugger::DebugPanel,
    pub(crate) dock: egui_dock::DockState<PanelId>,
    /// The project whose layout the dock shows, once it has been read.
    layout_root: Option<PathBuf>,
    /// The layout as last read or written.
    layout_saved: String,
    /// The gizmo settings as last read or written.
    gizmo_saved: GizmoSettings,
    /// The dock as it was when a pointer button went down: the dock is saved
    /// only when a press changed it (or [`Editor::dock_mut`] was called),
    /// never because the window was resized. egui_dock clamps each split to
    /// the window's size every frame; saving those clamps would lose the
    /// layout after a moment in a small window.
    layout_at_press: Option<String>,
    layout_dirty: bool,
    log_seq: u64,
}

impl Default for Editor {
    fn default() -> Self {
        Editor::new()
    }
}

impl std::fmt::Debug for Editor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Editor")
            .field("selected", &self.selected)
            .field("console", &self.console.len())
            .field("rects", &self.rects)
            .finish_non_exhaustive()
    }
}

impl Editor {
    /// An editor with nothing selected and an empty console.
    pub fn new() -> Editor {
        Editor {
            selected: None,
            console: Vec::new(),
            palette: Palette {
                args: "{}".to_string(),
                ..Palette::default()
            },
            pending: Pending::default(),
            graph_new: HashMap::new(),
            select_next: None,
            later: Vec::new(),
            rects: PanelRects::default(),
            status: String::new(),
            viewport_texture: None,
            viewport_px: None,
            gizmo: Gizmo::default(),
            debug: Default::default(),
            dock: layout::default_dock(),
            layout_root: None,
            layout_saved: String::new(),
            gizmo_saved: GizmoSettings::default(),
            layout_at_press: None,
            layout_dirty: false,
            log_seq: 0,
        }
    }

    /// The selected entity's id.
    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// Selects the entity with id `id` (or nothing).
    pub fn select(&mut self, id: Option<&str>) {
        self.selected = id.map(str::to_string);
    }

    /// Where each panel was drawn in the last frame.
    pub fn rects(&self) -> &PanelRects {
        &self.rects
    }

    /// Where the inspector drew the widgets of one field in the last frame
    /// (one rectangle per sub-widget: three for a vector), in egui points.
    /// Empty when the field was not shown.
    pub fn field_rects(&self, id: &str, component: &str, field: &str) -> &[egui::Rect] {
        self.pending
            .rects
            .get(&field_key(id, component, field))
            .map_or(&[], Vec::as_slice)
    }

    /// Where the hierarchy drew the row of entity `id` in the last frame (in
    /// egui points), if it was shown.
    pub fn hierarchy_row(&self, id: &str) -> Option<egui::Rect> {
        self.pending
            .rects
            .get(&row_key(id))
            .and_then(|r| r.first().copied())
    }

    /// The console's lines, oldest first.
    pub fn console(&self) -> &[ConsoleLine] {
        &self.console
    }

    /// Sets the status text shown in the toolbar (the project lock, the socket).
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    /// Opens or closes the command palette (`Ctrl+P`).
    pub fn set_palette_open(&mut self, open: bool) {
        if open && !self.palette.open {
            self.palette.focus_filter = true;
        }
        self.palette.open = open;
    }

    /// Selects `command` in the palette and sets its arguments text.
    pub fn set_palette_command(&mut self, command: &str, args: &str) {
        self.palette.selected = Some(command.to_string());
        self.palette.args = args.to_string();
    }

    /// The game view's egui texture and its size in pixels, drawn in the
    /// viewport panel (set by the [`GameView`](crate::GameView) host).
    pub fn set_viewport_texture(&mut self, texture: Option<(egui::TextureId, [u32; 2])>) {
        self.viewport_texture = texture;
    }

    /// The size in physical pixels the viewport panel had in the last frame.
    pub fn viewport_size_px(&self) -> Option<[u32; 2]> {
        self.viewport_px
    }

    /// The gizmo's mode, orientation and snapping.
    pub fn gizmo_settings(&self) -> &GizmoSettings {
        &self.gizmo.settings
    }

    /// Sets the gizmo's mode, orientation and snapping (saved with the layout).
    pub fn set_gizmo_settings(&mut self, settings: GizmoSettings) {
        self.gizmo.settings = settings;
    }

    /// Whether a gizmo drag is in progress (nothing is sent until it ends).
    pub fn gizmo_dragging(&self) -> bool {
        self.gizmo.drag.is_some()
    }

    /// Where the gizmo's `handle` on the selected entity was drawn in the
    /// last frame (a point that picks it, in egui points), if it was shown.
    pub fn gizmo_handle(&self, app: &App, handle: crate::gizmo::Handle) -> Option<egui::Pos2> {
        let rect = self.rects.get(Panel::Viewport)?;
        let id = self.selected.as_deref()?;
        crate::gizmo::handle_point(app, rect, &self.gizmo.settings, id, handle)
    }

    /// Shows `debugger` in the debugger panel (`None`: no debugger). The
    /// panel sends it commands and reads its state; whoever runs the scripts
    /// attaches their VMs to it.
    pub fn set_debugger(&mut self, debugger: Option<re_script::debug::Debugger>) {
        self.debug.debugger = debugger;
    }

    /// The debugger shown in the debugger panel.
    pub fn debugger(&self) -> Option<&re_script::debug::Debugger> {
        self.debug.debugger.as_ref()
    }

    /// The dock's layout: which panel is where.
    pub fn dock(&self) -> &egui_dock::DockState<PanelId> {
        &self.dock
    }

    /// The dock's layout, to rearrange panels; the change is saved after the
    /// next frame, as a drag in the dock is. (The first frame reads the
    /// project's layout, replacing changes made before it.)
    pub fn dock_mut(&mut self) -> &mut egui_dock::DockState<PanelId> {
        self.layout_dirty = true;
        &mut self.dock
    }

    /// Reads the layout of the project at `root` (`.real/editor/layout.json`),
    /// or the default layout when it has none. A file that cannot be read is
    /// reported in the console as `RE8804` and replaced by the default
    /// layout when the layout next changes. [`Editor::ui`] does this on its
    /// first frame.
    pub fn load_layout(&mut self, root: &Path) {
        let (dock, gizmo) = match layout::load(root) {
            Ok(Some(found)) => found,
            Ok(None) => (layout::default_dock(), GizmoSettings::default()),
            Err(d) => {
                self.log_diagnostic(&d);
                (layout::default_dock(), GizmoSettings::default())
            }
        };
        self.dock = dock;
        self.gizmo.settings = gizmo;
        self.gizmo.drag = None;
        self.layout_saved = layout::to_json(&self.dock, &self.gizmo.settings);
        self.gizmo_saved = self.gizmo.settings;
        self.layout_at_press = None;
        self.layout_dirty = false;
        self.layout_root = Some(root.to_path_buf());
    }

    /// Writes the layout to the project's `.real/editor/layout.json` if the
    /// person changed it since it was read or last written: a drag in the
    /// dock, a gizmo setting, or [`Editor::dock_mut`]. [`Editor::ui`] calls
    /// it after each frame in which no pointer button is held; the windowed
    /// editor calls it again on exit.
    pub fn save_layout(&mut self) {
        if self.gizmo.settings != self.gizmo_saved {
            self.layout_dirty = true;
        }
        if !std::mem::take(&mut self.layout_dirty) {
            return;
        }
        let Some(root) = &self.layout_root else {
            return;
        };
        let text = layout::to_json(&self.dock, &self.gizmo.settings);
        self.gizmo_saved = self.gizmo.settings;
        if text == self.layout_saved {
            return;
        }
        let result = layout::save(root, &text);
        // On failure the text is kept as if saved, so the warning is shown once.
        self.layout_saved = text;
        if let Err(e) = result {
            self.log(
                LineKind::Warning,
                format!("the editor layout could not be saved: {e}"),
            );
        }
    }

    /// Notes dock changes made by a pointer press, after the frame is drawn.
    fn track_layout(&mut self, ui: &egui::Ui) {
        let down = ui.input(|i| i.pointer.any_down());
        match (&self.layout_at_press, down) {
            (None, true) => {
                self.layout_at_press = Some(layout::to_json(&self.dock, &self.gizmo.settings));
            }
            (Some(before), false) => {
                if *before != layout::to_json(&self.dock, &self.gizmo.settings) {
                    self.layout_dirty = true;
                }
                self.layout_at_press = None;
            }
            _ => {}
        }
        if !down {
            self.save_layout();
        }
    }

    /// Adds a line to the console.
    pub fn log(&mut self, kind: LineKind, text: impl Into<String>) {
        self.console.push(ConsoleLine {
            kind,
            text: text.into(),
        });
        if self.console.len() > CONSOLE_LINES {
            let extra = self.console.len() - CONSOLE_LINES;
            self.console.drain(..extra);
        }
    }

    /// Adds a diagnostic to the console.
    pub fn log_diagnostic(&mut self, d: &Diagnostic) {
        let kind = match d.severity {
            Severity::Error => LineKind::Error,
            Severity::Warning => LineKind::Warning,
            _ => LineKind::Info,
        };
        self.log(kind, d.render_human().trim_end().to_string());
    }

    /// Draws every panel into `ui` (the root of an egui pass), then applies the
    /// changes they asked for. Commands that other writers sent are not
    /// served here: the host calls
    /// [`EditorSocket::serve_pending`](crate::EditorSocket::serve_pending)
    /// before each frame.
    pub fn ui(&mut self, ui: &mut egui::Ui, app: &mut App, policy: &dyn PolicyCheck) {
        if self.layout_root.as_deref() != Some(app.project()) {
            let root = app.project().to_path_buf();
            self.load_layout(&root);
        }
        self.pull_log(app);
        self.rects = PanelRects::default();
        self.pending.rects.clear();
        let mut actions = Vec::new();
        self.shortcuts(ui.ctx(), &mut actions);
        panels::draw(self, ui, app, &mut actions);
        // A field committed in the same frame as an Undo or Redo click (the
        // click took its focus) is applied first, so undo sees it.
        actions.sort_by_key(
            |a| matches!(a, Action::Command { name, .. } if name == "undo" || name == "redo"),
        );
        for action in actions {
            self.apply(app, policy, action);
        }
        self.pull_log(app);
        self.track_layout(ui);
    }

    fn shortcuts(&mut self, ctx: &egui::Context, actions: &mut Vec<Action>) {
        use egui::{Key, KeyboardShortcut, Modifiers};
        let palette = KeyboardShortcut::new(Modifiers::COMMAND, Key::P);
        if ctx.input_mut(|i| i.consume_shortcut(&palette)) {
            self.set_palette_open(!self.palette.open);
        }
        if self.palette.open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.palette.open = false;
        }
        // Text fields keep their own undo.
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        crate::gizmo::shortcuts(&mut self.gizmo.settings, ctx);
        let redo = [
            KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z),
            KeyboardShortcut::new(Modifiers::COMMAND, Key::Y),
        ];
        let undo = KeyboardShortcut::new(Modifiers::COMMAND, Key::Z);
        if ctx.input_mut(|i| redo.iter().any(|s| i.consume_shortcut(s))) {
            actions.push(command("redo", json!({})));
        } else if ctx.input_mut(|i| i.consume_shortcut(&undo)) {
            actions.push(command("undo", json!({})));
        }
    }

    pub(crate) fn apply(&mut self, app: &mut App, policy: &dyn PolicyCheck, action: Action) {
        match action {
            Action::Command {
                name,
                args,
                dry_run,
                show_data,
            } => {
                self.run_command(app, policy, &name, args, dry_run, show_data);
            }
            Action::SetField {
                id,
                component,
                field,
                ty,
                value,
            } => {
                self.set_field(app, policy, &id, &component, &field, &ty, &value);
            }
            Action::Report(d) => self.log_diagnostic(&d),
        }
    }

    /// Dispatches `name` with `args` as the editor's user, logging failures
    /// and warnings to the console.
    pub fn run(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
        name: &str,
        args: Json,
        dry_run: bool,
    ) -> CommandOutcome {
        let outcome = app.dispatch(name, args, dry_run, policy, Origin::User);
        if !outcome.ok && outcome.diagnostics.is_empty() {
            self.log(LineKind::Error, format!("`{name}` failed"));
        }
        for d in &outcome.diagnostics {
            self.log_diagnostic(d);
        }
        outcome
    }

    /// Sets one field of one entity through `entity.set`: what an inspector
    /// widget does when its edit is committed. `value` is written in the
    /// field's file form (entity references as ids).
    #[allow(clippy::too_many_arguments)]
    pub fn set_field(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
        id: &str,
        component: &str,
        field: &str,
        ty: &FieldType,
        value: &Value,
    ) -> CommandOutcome {
        let world = app.world();
        let doc = Doc::from_value(value, ty, &|e| world.id_of(e).map(str::to_string));
        let args = json!({
            "id": id,
            "component": component,
            "field": field,
            "value": Json::from(&doc),
        });
        self.run(app, policy, "entity.set", args, false)
    }

    /// Undoes the last change (`undo`).
    pub fn undo(&mut self, app: &mut App, policy: &dyn PolicyCheck) -> CommandOutcome {
        self.run(app, policy, "undo", json!({}), false)
    }

    /// Redoes the last undone change (`redo`).
    pub fn redo(&mut self, app: &mut App, policy: &dyn PolicyCheck) -> CommandOutcome {
        self.run(app, policy, "redo", json!({}), false)
    }

    /// Runs the palette's selected command with its arguments text, as the
    /// palette's Run button does. Arguments that are not a JSON object are
    /// reported as `RE8802` and nothing runs.
    pub fn run_palette(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
    ) -> Option<CommandOutcome> {
        match panels::palette_action(&self.palette)? {
            Action::Command {
                name,
                args,
                dry_run,
                ..
            } => Some(self.run_command(app, policy, &name, args, dry_run, true)),
            other => {
                self.apply(app, policy, other);
                None
            }
        }
    }

    /// [`Editor::run`], plus a console line with the result data when
    /// `show_data` is set (palette runs).
    fn run_command(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
        name: &str,
        args: Json,
        dry_run: bool,
        show_data: bool,
    ) -> CommandOutcome {
        let outcome = self.run(app, policy, name, args, dry_run);
        if show_data && outcome.ok {
            let mut text = format!("`{name}`");
            if dry_run {
                text.push_str(" (dry run)");
            }
            text.push_str(&format!(": ok, {} change(s)", outcome.changes.len()));
            if !outcome.data.is_null() {
                let data = serde_json::to_string_pretty(&outcome.data).unwrap_or_default();
                text.push('\n');
                text.push_str(&truncate(&data, 8000));
            }
            self.log(LineKind::Info, text);
        }
        outcome
    }

    /// Moves new diagnostics and transaction-log entries into the console.
    fn pull_log(&mut self, app: &mut App) {
        for d in app.take_diagnostics() {
            self.log_diagnostic(&d);
        }
        let history = app.history();
        if history.last_seq() < self.log_seq {
            // The history started over (`world.restart()`).
            self.log_seq = 0;
        }
        let lines: Vec<String> = history
            .log()
            .iter()
            .filter(|e| e.seq > self.log_seq)
            .map(|e| {
                let origin = match &e.origin {
                    Origin::User => "user".to_string(),
                    Origin::Script(s) => format!("script {s}"),
                    Origin::Agent(a) => format!("agent {a}"),
                };
                let kind = serde_json::to_value(e.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                match &e.error {
                    Some(err) => format!("[{origin}] {kind} {}: {err}", e.label),
                    None => format!("[{origin}] {kind} {} ({} op)", e.label, e.ops.len()),
                }
            })
            .collect();
        self.log_seq = history.last_seq();
        for line in lines {
            self.log(LineKind::Info, line);
        }
    }
}

pub(crate) fn command(name: &str, args: Json) -> Action {
    Action::Command {
        name: name.to_string(),
        args,
        dry_run: false,
        show_data: false,
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… ({} more bytes)", &text[..end], text.len() - end)
}
