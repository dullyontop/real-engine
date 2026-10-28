//! [`Editor`]: the editor's UI state, its panels and the commands they
//! dispatch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use re_app::App;
use re_command::{CommandOutcome, Origin, PolicyCheck};
use re_core::{Diagnostic, Severity};
use re_reflect::{Doc, FieldType, Value};
use re_scene::{Generated, World};
use serde_json::{Value as Json, json};

use crate::camera::{EditorCamera, Navigation};
use crate::clipboard::{self, Clip, Place};
use crate::gizmo::{Gizmo, GizmoSettings};
use crate::layout::{self, Layout, PanelId, PanelStates};
use crate::overlay::{self, ViewSettings};
use crate::panels::{self, GENERATED_NAME, Rename};
use crate::selection::Selection;

/// The most console lines kept; older ones are dropped.
const CONSOLE_LINES: usize = 2000;

/// How long a change of the editor camera made without a pointer press (the
/// wheel, a typed speed) waits before it is saved, in seconds: a zoom is
/// saved once, not every frame.
const CAMERA_SAVE_DELAY: f64 = 0.5;

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
    /// The node canvas.
    Canvas,
    /// One material file's keys.
    Material,
    /// The project's files by folder and kind.
    Assets,
}

impl Panel {
    /// Every panel, in drawing order.
    pub const ALL: [Panel; 10] = [
        Panel::Toolbar,
        Panel::Hierarchy,
        Panel::Inspector,
        Panel::Console,
        Panel::Graph,
        Panel::Palette,
        Panel::Viewport,
        Panel::Canvas,
        Panel::Material,
        Panel::Assets,
    ];

    fn index(self) -> usize {
        self as usize
    }
}

/// Where each panel was drawn in the last frame, in egui points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PanelRects([Option<egui::Rect>; 10]);

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
    /// Paste copied entities with fresh ids (a paste or a duplicate).
    Paste { clip: Clip, place: Place },
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
/// text in a focused field, or an entity field waiting for a click in the
/// viewport.
#[derive(Clone, Debug, Default)]
pub(crate) struct Pending {
    pub values: HashMap<String, Value>,
    pub texts: HashMap<String, String>,
    /// Where each field's widgets were drawn in the last frame.
    pub rects: HashMap<String, Vec<egui::Rect>>,
    /// The entity field whose picker waits for the next click in the
    /// viewport ([`crate::widgets::take_pick`]).
    pub picking: Option<crate::widgets::Picking>,
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

/// The key of an entity's note field in [`Pending`].
pub(crate) fn note_key(id: &str) -> String {
    format!("note\u{1f}{id}")
}

/// The key of one context menu item in [`Pending::rects`].
pub(crate) fn menu_key(label: &str) -> String {
    format!("menu\u{1f}{label}")
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
    /// The selected entities and the active one.
    pub(crate) selection: Selection,
    pub(crate) console: Vec<ConsoleLine>,
    pub(crate) palette: Palette,
    pub(crate) pending: Pending,
    pub(crate) graph_new: HashMap<String, [String; 3]>,
    /// A selection clicked in the last frame, applied after this one.
    pub(crate) select_next: Option<Selection>,
    /// The entity being renamed in the Hierarchy.
    pub(crate) renaming: Option<Rename>,
    /// The active entity the Hierarchy last scrolled to.
    pub(crate) revealed: Option<String>,
    /// Where a paste from a menu goes, until the host hands over the
    /// clipboard's text ([`Editor::paste_clipboard`]).
    pub(crate) paste_request: Option<Place>,
    pub(crate) later: Vec<Action>,
    pub(crate) rects: PanelRects,
    pub(crate) status: String,
    pub(crate) viewport_texture: Option<(egui::TextureId, [u32; 2])>,
    pub(crate) viewport_px: Option<[u32; 2]>,
    pub(crate) gizmo: Gizmo,
    /// The camera the viewport draws from, once the editor has one: from the
    /// layout, else where the game's camera is (first frame).
    pub(crate) camera: Option<EditorCamera>,
    /// What the pointer is doing to the camera.
    pub(crate) nav: Navigation,
    /// What the view shows.
    pub(crate) view: ViewSettings,
    pub(crate) debug: crate::debugger::DebugPanel,
    pub(crate) canvas: crate::canvas::Canvas,
    pub(crate) material: crate::panels::MaterialPanel,
    /// The project's files by kind ([`crate::assets`]).
    pub(crate) assets: crate::assets::AssetIndex,
    pub(crate) assets_panel: crate::panels::AssetsPanel,
    pub(crate) dock: egui_dock::DockState<PanelId>,
    /// Each panel's own view state, as read from the layout (kept and
    /// written back).
    panel_states: PanelStates,
    /// The project whose layout the dock shows, once it has been read.
    layout_root: Option<PathBuf>,
    /// The layout as last read or written.
    layout_saved: String,
    /// The gizmo settings as last read or written.
    gizmo_saved: GizmoSettings,
    /// The view settings as last read or written.
    view_saved: ViewSettings,
    /// The selection as last read or written.
    selection_saved: Selection,
    /// The camera as last read or written.
    camera_saved: Option<EditorCamera>,
    /// When (egui time) the camera first differed from the saved one without
    /// a pointer press; it is saved [`CAMERA_SAVE_DELAY`] after.
    camera_since: Option<f64>,
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
            .field("selection", &self.selection)
            .field("console", &self.console.len())
            .field("rects", &self.rects)
            .finish_non_exhaustive()
    }
}

impl Editor {
    /// An editor with nothing selected and an empty console.
    pub fn new() -> Editor {
        Editor {
            selection: Selection::new(),
            console: Vec::new(),
            palette: Palette {
                args: "{}".to_string(),
                ..Palette::default()
            },
            pending: Pending::default(),
            graph_new: HashMap::new(),
            select_next: None,
            renaming: None,
            revealed: None,
            paste_request: None,
            later: Vec::new(),
            rects: PanelRects::default(),
            status: String::new(),
            viewport_texture: None,
            viewport_px: None,
            gizmo: Gizmo::default(),
            camera: None,
            nav: Navigation::default(),
            view: ViewSettings::default(),
            debug: Default::default(),
            canvas: Default::default(),
            material: Default::default(),
            assets: Default::default(),
            assets_panel: Default::default(),
            dock: layout::default_dock(),
            panel_states: PanelStates::new(),
            layout_root: None,
            layout_saved: String::new(),
            gizmo_saved: GizmoSettings::default(),
            view_saved: ViewSettings::default(),
            selection_saved: Selection::new(),
            camera_saved: None,
            camera_since: None,
            layout_at_press: None,
            layout_dirty: false,
            log_seq: 0,
        }
    }

    /// The active entity's id: the one the Inspector shows (the one selected
    /// last).
    pub fn selected(&self) -> Option<&str> {
        self.selection.active()
    }

    /// The selected entities, with the active one.
    pub fn selection(&self) -> &Selection {
        &self.selection
    }

    /// Selects only the entity with id `id` (or nothing).
    pub fn select(&mut self, id: Option<&str>) {
        self.selection = id.map(Selection::one).unwrap_or_default();
    }

    /// Selects `selection` (saved with the layout, as a click is).
    pub fn set_selection(&mut self, selection: Selection) {
        self.selection = selection;
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

    /// Where the inspector drew the note field of entity `id` in the last
    /// frame (in egui points), if it was shown.
    pub fn note_field(&self, id: &str) -> Option<egui::Rect> {
        self.pending
            .rects
            .get(&note_key(id))
            .and_then(|r| r.first().copied())
    }

    /// Where the open context menu drew its item `label` in the last frame
    /// (in egui points), if a menu with that item was open.
    pub fn menu_item(&self, label: &str) -> Option<egui::Rect> {
        self.pending
            .rects
            .get(&menu_key(label))
            .and_then(|r| r.first().copied())
    }

    /// The entity being renamed in the Hierarchy, and the name typed so far.
    pub fn renaming(&self) -> Option<(&str, &str)> {
        self.renaming
            .as_ref()
            .map(|r| (r.id.as_str(), r.text.as_str()))
    }

    /// The project's files by kind, as the Assets panel and the asset
    /// pickers list them. It is built on a worker thread from the first
    /// frame and kept current from the App's file changes; see
    /// [`Editor::wait_for_assets`].
    pub fn assets(&self) -> &crate::assets::AssetIndex {
        &self.assets
    }

    /// Waits until the asset index has taken in every file change the App
    /// applied so far (starting it on `app`'s project if it has not
    /// started).
    pub fn wait_for_assets(&mut self, app: &mut App) {
        self.assets.wait(app);
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

    /// Where the gizmo's `handle` on the selection was drawn in the last
    /// frame (a point that picks it, in egui points), if it was shown.
    pub fn gizmo_handle(&self, app: &App, handle: crate::gizmo::Handle) -> Option<egui::Pos2> {
        let rect = self.rects.get(Panel::Viewport)?;
        let view = crate::camera::View::new(rect, self.view_camera(app));
        crate::gizmo::handle_point(app, &view, &self.gizmo.settings, &self.selection, handle)
    }

    /// The editor camera, once the editor has one (from the project's layout,
    /// else from the game's camera on the first frame).
    pub fn camera(&self) -> Option<&EditorCamera> {
        self.camera.as_ref()
    }

    /// Moves the editor camera (saved with the layout, as a move with the
    /// pointer is), brought into range ([`EditorCamera::clamped`]).
    pub fn set_camera(&mut self, camera: EditorCamera) {
        self.camera = Some(camera.clamped());
    }

    /// What the view shows: overlays, and whether it looks through the
    /// game's camera.
    pub fn view_settings(&self) -> &ViewSettings {
        &self.view
    }

    /// Sets what the view shows (saved with the layout).
    pub fn set_view_settings(&mut self, view: ViewSettings) {
        self.view = view;
    }

    /// The camera the viewport is drawn from, in world space: the editor
    /// camera, or the game's when the view looks through it (or before the
    /// editor has a camera).
    pub(crate) fn view_camera(&self, app: &App) -> re_types::render::Camera {
        match self.camera {
            Some(camera) if !self.view.game_camera => camera.render_camera(glam::Vec3::ZERO),
            _ => {
                // The snapshot is relative to the render origin.
                let mut camera = app.snapshot().camera;
                camera.world.w_axis += app.render_origin().extend(0.0);
                camera
            }
        }
    }

    /// Where the world point `at` was drawn in the viewport in the last frame,
    /// in egui points, or `None` when it is behind the camera or the
    /// viewport was not drawn.
    pub fn screen_point(&self, app: &App, at: glam::Vec3) -> Option<egui::Pos2> {
        let rect = self.rects.get(Panel::Viewport)?;
        let view = crate::camera::View::new(rect, self.view_camera(app));
        let p = view.screen?.project(at)?;
        Some(egui::pos2(p.x, p.y))
    }

    /// Moves the game's active camera to where the editor camera is, as the
    /// view menu's "Align game camera with view" does: one `scene.apply`
    /// setting its `Transform`'s translation and rotation, so one undo step.
    /// `None` (nothing is sent) when the scene has no active camera or the
    /// view looks through it.
    pub fn align_game_camera(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
    ) -> Option<CommandOutcome> {
        match self.align_action(app)? {
            Action::Command { name, args, .. } => Some(self.run(app, policy, &name, args, false)),
            _ => None,
        }
    }

    /// The edit [`Editor::align_game_camera`] sends: `set_field` ops on the
    /// game camera's local `Transform` (through its parent's world
    /// transform).
    pub(crate) fn align_action(&self, app: &App) -> Option<Action> {
        if self.view.game_camera {
            return None;
        }
        let camera = self.camera?;
        let world = app.world();
        let id = overlay::game_camera_id(world)?;
        let e = world.entity(&id).ok()?;
        let parent = re_scene::hierarchy::parent(&world.ecs, e)
            .and_then(|p| world.ecs.get::<re_scene::GlobalTransform>(p))
            .map_or(glam::Affine3A::IDENTITY, |g| g.0);
        let want = glam::Affine3A::from_rotation_translation(camera.rotation(), camera.eye());
        let (_, r, t) = (parent.inverse() * want).to_scale_rotation_translation();
        let r = r.normalize();
        let op = |field: &str, value: Json| {
            json!({
                "op": "set_field",
                "id": id,
                "component": "Transform",
                "field": field,
                "value": value,
            })
        };
        Some(command(
            "scene.apply",
            json!({
                "label": format!("align {id} with the view"),
                "ops": [
                    op("translation", json!([t.x, t.y, t.z])),
                    op("rotation", json!(r.to_array())),
                ],
            }),
        ))
    }

    /// Deletes the selected entities and everything under them, as one
    /// `scene.apply` of `despawn` ops for the topmost ones (one undo step),
    /// as the Hierarchy's Delete button does, and selects nothing. `None`
    /// (nothing is sent) when nothing selected exists.
    pub fn delete_selection(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
    ) -> Option<CommandOutcome> {
        match self.delete_action(app.world())? {
            Action::Command { name, args, .. } => {
                self.change_selection(Selection::new());
                Some(self.run(app, policy, &name, args, false))
            }
            _ => None,
        }
    }

    /// Selects `selection` now. When the active entity changes, the edits
    /// in progress in the Inspector are dropped (they belonged to the entity
    /// it showed).
    pub(crate) fn change_selection(&mut self, selection: Selection) {
        if self.selection.active() != selection.active() {
            // Where this frame's widgets were drawn stays true until the next.
            self.pending = Pending {
                rects: std::mem::take(&mut self.pending.rects),
                ..Pending::default()
            };
        }
        self.selection = selection;
    }

    /// The clipboard text Ctrl+C writes: the selected entities that have no
    /// selected ancestor, each with everything under it, in the format of
    /// [`clipboard`](crate::clipboard). Notes on what was left out go to the
    /// console. `None` when nothing could be copied.
    pub fn copy_selection(&mut self, world: &World) -> Option<String> {
        let copied = clipboard::copy(world, &self.selection);
        for d in &copied.notes {
            self.log_diagnostic(d);
        }
        copied.clip.map(|clip| clip.to_text())
    }

    /// Pastes clipboard `text`, as Ctrl+V does (after the active entity,
    /// under its parent) or, with `as_child`, as "Paste as child" does
    /// (under the active entity): one `scene.apply` of `restore` ops with
    /// fresh ids, so one undo step, after which the pasted roots are
    /// selected. `None` (nothing is sent) when the text holds no entities
    /// or the paste is refused; the console says why.
    pub fn paste(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
        text: &str,
        as_child: bool,
    ) -> Option<CommandOutcome> {
        let place = if as_child {
            Place::Under
        } else {
            Place::Beside
        };
        match Clip::parse(text) {
            Ok(clip) => self.paste_clip(app, policy, &clip, place),
            Err(d) => {
                self.log_diagnostic(&d);
                None
            }
        }
    }

    /// Whether a menu's Paste or "Paste as child" waits for the clipboard's
    /// text. egui cannot read the system clipboard, so the host reads it
    /// and calls [`Editor::paste_clipboard`] after the frame.
    pub fn wants_clipboard(&self) -> bool {
        self.paste_request.is_some()
    }

    /// Does the paste a menu asked for with `text`, the system clipboard's
    /// text (`None` when it holds no text, which the console notes). `None`
    /// when no paste was asked for, nothing could be pasted, or the paste
    /// was refused; the console says why.
    pub fn paste_clipboard(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
        text: Option<&str>,
    ) -> Option<CommandOutcome> {
        let place = self.paste_request.take()?;
        match Clip::parse(text.unwrap_or_default()) {
            Ok(clip) => self.paste_clip(app, policy, &clip, place),
            Err(d) => {
                self.log_diagnostic(&d);
                None
            }
        }
    }

    /// Starts renaming entity `id` in its Hierarchy row (F2, a double-click,
    /// or the menu's Rename). A generated entity is not renamed (the
    /// `rename` op refuses it); the console says why.
    pub(crate) fn start_rename(&mut self, world: &World, id: &str) {
        let Ok(e) = world.entity(id) else {
            return;
        };
        if world.ecs.get::<Generated>(e).is_some() {
            self.log(
                LineKind::Info,
                format!("{id} cannot be renamed: {GENERATED_NAME}"),
            );
            return;
        }
        self.renaming = Some(Rename::new(id, world.name_of(e)));
    }

    /// Duplicates the selection, as Ctrl+D does: a copy of each selected
    /// entity that has no selected ancestor, with everything under it, after
    /// its original, in one undo step; the copies are selected. The
    /// clipboard is not touched. `None` when nothing could be copied.
    pub fn duplicate_selection(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
    ) -> Option<CommandOutcome> {
        let clip = self.copy_clip(app.world())?;
        self.paste_clip(app, policy, &clip, Place::AfterOriginals)
    }

    pub(crate) fn copy_clip(&mut self, world: &World) -> Option<Clip> {
        let copied = clipboard::copy(world, &self.selection);
        for d in &copied.notes {
            self.log_diagnostic(d);
        }
        copied.clip
    }

    fn paste_clip(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
        clip: &Clip,
        place: Place,
    ) -> Option<CommandOutcome> {
        let pasted = match clipboard::paste(app.world_mut(), clip, &self.selection, place) {
            Ok(pasted) => pasted,
            Err(d) => {
                self.log_diagnostic(&d);
                return None;
            }
        };
        for d in &pasted.notes {
            self.log_diagnostic(d);
        }
        let outcome = self.run(app, policy, "scene.apply", pasted.args, false);
        if outcome.ok {
            self.change_selection(Selection::from_ids(pasted.roots, None));
        }
        Some(outcome)
    }

    /// The edit [`Editor::delete_selection`] sends.
    pub(crate) fn delete_action(&self, world: &World) -> Option<Action> {
        despawn(&self.selection.topmost(world))
    }

    /// Ctrl+X and the menu's Cut: the selection's clipboard text, as
    /// [`Editor::copy_selection`] writes it, and the edit that deletes what
    /// was copied (one undo step). An entity left out of the copy is not
    /// deleted; `None`, and nothing deleted, when nothing could be copied.
    pub(crate) fn cut(&mut self, world: &World) -> Option<(String, Action)> {
        let clip = self.copy_clip(world)?;
        let roots: Vec<String> = clip
            .subtrees
            .iter()
            .map(|s| s.entities[0].id.clone())
            .collect();
        Some((clip.to_text(), despawn(&roots)?))
    }

    /// Points the editor camera at the selection (F), or at everything
    /// drawn when `everything` is set or nothing is selected (Shift+F): the
    /// bounds of the draw items of the selected entities and the entities
    /// under them, or a metre around an entity without any. The view stops
    /// looking through the game's camera. Nothing happens when nothing is
    /// drawn.
    pub fn frame_selection(&mut self, app: &App, everything: bool) {
        let ids = (!everything && !self.selection.is_empty()).then(|| self.selection.ids());
        let Some(bounds) = overlay::bounds(app, ids) else {
            return;
        };
        let aspect = self
            .rects
            .get(Panel::Viewport)
            .filter(|r| r.height() > 0.0)
            .map_or(16.0 / 9.0, |r| r.width() / r.height());
        let mut camera = self
            .camera
            .filter(EditorCamera::is_finite)
            .unwrap_or_else(|| initial_camera(app));
        camera.frame(bounds, aspect);
        self.camera = Some(camera);
        self.view.game_camera = false;
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
    ///
    /// The editor camera, the view settings and each panel's state are taken
    /// from it. Its selection is taken when nothing is selected yet;
    /// [`Editor::ui`] then drops it if the entity no longer exists.
    pub fn load_layout(&mut self, root: &Path) {
        let found = match layout::load(root) {
            Ok(Some(found)) => found,
            Ok(None) => Layout::new(),
            Err(d) => {
                self.log_diagnostic(&d);
                Layout::new()
            }
        };
        self.dock = found.dock;
        self.gizmo.settings = found.gizmo;
        self.gizmo.drag = None;
        self.canvas.set_collapsed(found.collapsed);
        self.camera = found.camera.map(EditorCamera::clamped);
        self.nav = Navigation::default();
        self.view = found.view;
        if self.selection.is_empty() {
            self.selection.clone_from(&found.selection);
        }
        self.panel_states = found.panels;
        self.layout_saved = self.layout_json();
        self.gizmo_saved = self.gizmo.settings;
        self.view_saved = self.view;
        self.selection_saved = found.selection;
        self.camera_saved = self.camera;
        self.camera_since = None;
        self.layout_at_press = None;
        self.layout_dirty = false;
        self.layout_root = Some(root.to_path_buf());
    }

    /// Writes the layout to the project's `.real/editor/layout.json` if the
    /// person changed it since it was read or last written: a drag in the
    /// dock, a gizmo or view setting, the selection, the editor camera, or
    /// [`Editor::dock_mut`]. [`Editor::ui`] calls it after each frame in
    /// which no pointer button is held (a camera change made without a press
    /// waits a moment); the windowed editor calls it again on exit.
    pub fn save_layout(&mut self) {
        if self.camera != self.camera_saved {
            self.layout_dirty = true;
        }
        self.write_layout();
    }

    /// [`Editor::save_layout`], except that a camera change made without a
    /// press is written only once it is [`CAMERA_SAVE_DELAY`] old (`now`
    /// being egui's time).
    fn save_settled(&mut self, now: f64) {
        if self.camera == self.camera_saved {
            self.camera_since = None;
        } else {
            match self.camera_since {
                Some(at) if now - at >= CAMERA_SAVE_DELAY || now < at => {
                    self.layout_dirty = true;
                }
                Some(_) => {}
                None => self.camera_since = Some(now),
            }
        }
        self.write_layout();
    }

    fn write_layout(&mut self) {
        if self.gizmo.settings != self.gizmo_saved
            || self.view != self.view_saved
            || self.selection != self.selection_saved
        {
            self.layout_dirty = true;
        }
        if !std::mem::take(&mut self.layout_dirty) {
            return;
        }
        let Some(root) = &self.layout_root else {
            return;
        };
        let text = self.layout_json();
        self.gizmo_saved = self.gizmo.settings;
        self.view_saved = self.view;
        self.selection_saved.clone_from(&self.selection);
        self.camera_saved = self.camera;
        self.camera_since = None;
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

    /// Notes a change to the layout made without a press and release that
    /// [`Editor::ui`] would see (a click is both in one frame): it is saved
    /// once no button is held.
    pub(crate) fn layout_changed(&mut self) {
        self.layout_dirty = true;
    }

    /// A panel's own view state as the layout holds it, read with the
    /// panel's types; the defaults when there is none or it does not parse.
    pub(crate) fn panel_state<T: serde::de::DeserializeOwned + Default>(
        &self,
        panel: PanelId,
    ) -> T {
        self.panel_states
            .get(&panel_key(panel))
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }

    /// Sets a panel's own view state, saved with the layout when it
    /// changed.
    pub(crate) fn set_panel_state<T: serde::Serialize>(&mut self, panel: PanelId, state: &T) {
        let Ok(value) = serde_json::to_value(state) else {
            return;
        };
        let key = panel_key(panel);
        if self.panel_states.get(&key) != Some(&value) {
            self.panel_states.insert(key, value);
            self.layout_dirty = true;
        }
    }

    fn layout_json(&self) -> String {
        layout::to_json(&Layout {
            dock: self.dock.clone(),
            gizmo: self.gizmo.settings,
            collapsed: self.canvas.collapsed().clone(),
            camera: self.camera,
            view: self.view,
            selection: self.selection.clone(),
            panels: self.panel_states.clone(),
        })
    }

    /// Notes layout changes made by a pointer press, after the frame is
    /// drawn, and saves what changed once no button is held.
    fn track_layout(&mut self, ui: &egui::Ui) {
        let (down, now) = ui.input(|i| (i.pointer.any_down(), i.time));
        match (&self.layout_at_press, down) {
            (None, true) => {
                self.layout_at_press = Some(self.layout_json());
            }
            (Some(before), false) => {
                if *before != self.layout_json() {
                    self.layout_dirty = true;
                }
                self.layout_at_press = None;
            }
            _ => {}
        }
        if !down {
            self.save_settled(now);
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
        // Entities that no longer exist (an undo, a reload, a regeneration,
        // a cell unloading) leave the selection.
        let world = app.world();
        self.selection.retain(|id| world.entity(id).is_ok());
        if !self.camera.is_some_and(|c| c.is_finite()) {
            // Not a change of the layout: it is written with the next one.
            self.camera = Some(initial_camera(app));
            self.camera_saved = self.camera;
        }
        self.assets.poll(app);
        for note in self.assets.upload_thumbnails(ui.ctx()) {
            self.log_diagnostic(&note);
        }
        self.pull_log(app);
        self.rects = PanelRects::default();
        self.pending.rects.clear();
        let mut actions = Vec::new();
        self.shortcuts(ui.ctx(), app.world(), &mut actions);
        panels::draw(self, ui, app, &mut actions);
        // A rename the Hierarchy did not show (its tab is hidden) is dropped.
        if self.rects.get(Panel::Hierarchy).is_none() {
            self.renaming = None;
        }
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

    fn shortcuts(&mut self, ctx: &egui::Context, world: &World, actions: &mut Vec<Action>) {
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
        // While the camera flies, W, E and the others move it.
        if !self.nav.flying() {
            crate::gizmo::shortcuts(&mut self.gizmo.settings, ctx);
        }
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
        // Ctrl+Up selects the selection's parents, Ctrl+Down their children.
        let up = KeyboardShortcut::new(Modifiers::COMMAND, Key::ArrowUp);
        let down = KeyboardShortcut::new(Modifiers::COMMAND, Key::ArrowDown);
        if ctx.input_mut(|i| i.consume_shortcut(&up)) {
            self.select_next = Some(self.selection.parents(world));
        } else if ctx.input_mut(|i| i.consume_shortcut(&down)) {
            self.select_next = Some(self.selection.children(world));
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F2))
            && let Some(id) = self.selection.active().map(str::to_string)
        {
            self.start_rename(world, &id);
        }
        self.clipboard_keys(ctx, world, actions);
    }

    /// Ctrl+C, Ctrl+X and Ctrl+V (which egui turns into events), Ctrl+D, and
    /// Delete (Backspace too on macOS).
    fn clipboard_keys(&mut self, ctx: &egui::Context, world: &World, actions: &mut Vec<Action>) {
        use egui::{Event, Key, KeyboardShortcut, Modifiers};
        // Text selected in a label keeps Ctrl+C for itself.
        let label_text = ctx
            .with_plugin(|s: &mut egui::text_selection::LabelSelectionState| s.has_selection())
            .unwrap_or(false);
        for event in ctx.input(|i| i.events.clone()) {
            match event {
                Event::Copy if !label_text => {
                    if let Some(text) = self.copy_selection(world) {
                        ctx.copy_text(text);
                    }
                }
                Event::Cut if !label_text => {
                    if let Some((text, delete)) = self.cut(world) {
                        ctx.copy_text(text);
                        actions.push(delete);
                        self.change_selection(Selection::new());
                    }
                }
                Event::Paste(text) => match Clip::parse(&text) {
                    Ok(clip) => actions.push(Action::Paste {
                        clip,
                        place: Place::Beside,
                    }),
                    Err(d) => self.log_diagnostic(&d),
                },
                _ => {}
            }
        }
        let duplicate = KeyboardShortcut::new(Modifiers::COMMAND, Key::D);
        if ctx.input_mut(|i| i.consume_shortcut(&duplicate))
            && let Some(clip) = self.copy_clip(world)
        {
            actions.push(Action::Paste {
                clip,
                place: Place::AfterOriginals,
            });
        }
        let delete = ctx.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Delete)
                || (cfg!(target_os = "macos") && i.consume_key(Modifiers::NONE, Key::Backspace))
        });
        if delete && let Some(action) = self.delete_action(world) {
            actions.push(action);
            self.change_selection(Selection::new());
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
            Action::Paste { clip, place } => {
                self.paste_clip(app, policy, &clip, place);
            }
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
            // A live gizmo drag logs the same transaction every frame: one
            // line with a count.
            if let Some(last) = self.console.last_mut()
                && last.kind == LineKind::Info
                && let Some(rest) = last.text.strip_prefix(line.as_str())
            {
                let count = match rest.strip_prefix(" ×") {
                    _ if rest.is_empty() => Some(1),
                    Some(n) => n.parse::<u64>().ok(),
                    None => None,
                };
                if let Some(n) = count {
                    last.text = format!("{line} ×{}", n + 1);
                    continue;
                }
            }
            self.log(LineKind::Info, line);
        }
    }
}

/// A panel's key among the layout's panel states: its name in the dock.
fn panel_key(panel: PanelId) -> String {
    serde_json::to_value(panel)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// The editor camera a project starts with: where the game's camera is,
/// else where the game is drawn from without one.
fn initial_camera(app: &App) -> EditorCamera {
    overlay::game_camera(app.world())
        .map(|(world, fov)| EditorCamera::from_pose(world, fov))
        .filter(EditorCamera::is_finite)
        .unwrap_or_default()
}

/// One `scene.apply` that deletes `ids` and everything under them (one undo
/// step); `None` when there are none.
fn despawn(ids: &[String]) -> Option<Action> {
    let label = match ids {
        [] => return None,
        [one] => format!("delete {one}"),
        _ => format!("delete {} entities", ids.len()),
    };
    let ops: Vec<Json> = ids
        .iter()
        .map(|id| json!({ "op": "despawn", "id": id }))
        .collect();
    Some(command(
        "scene.apply",
        json!({ "label": label, "ops": ops }),
    ))
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
