//! `re_editor`: editor-lite for Real Engine (docs/M1_PLAN.md, WP8).
//!
//! `real editor` opens a project, takes its single-writer lock as the editor
//! and shows the game view with five egui panels over it:
//!
//! * **hierarchy**: every entity by name and id, nested by `ChildOf` /
//!   `Children`; spawn, delete, undo and redo; a click selects one entity,
//!   Ctrl+click adds or removes one and Shift+click a run of rows;
//! * **inspector**: the active entity's components, one widget per
//!   [`FieldType`](re_reflect::FieldType), generated from the registry's
//!   [`ComponentSchema`](re_reflect::ComponentSchema)s;
//! * **console**: diagnostics, the outcome of each command and every
//!   transaction in the log, whoever issued it (the editor, `real do`, an
//!   agent);
//! * **graph parameters**: each procedural source's seed and parameter
//!   overrides, with a regenerate button;
//! * **command palette** (`Ctrl+P`): any registered command with JSON
//!   arguments.
//!
//! The **node graph** panel ([`CanvasSpot`]; ADR-0044) draws a procedural
//! source's graph file as nodes and wires (egui-snarl). It never edits the
//! file itself: connecting, cutting, moving, adding and removing nodes are
//! the `graph.*` commands, each one undo step, and the graph stays an
//! unsaved open document until the scene is saved.
//!
//! The panels other than the toolbar and the palette are tabs in a dock
//! (egui_dock): they can be dragged into other splits, stacked or floated.
//! The arrangement, the gizmo settings, the editor camera, the view's
//! toggles and the selection are saved per project in
//! `.real/editor/layout.json` ([`layout`]), as editor state, not as commands.
//!
//! The game view is drawn from the editor's own camera ([`camera`]): it
//! orbits, pans, zooms and flies, frames the selection (`F`) or everything
//! (`Shift+F`), and can look through the game's camera instead. It is editor
//! state, never a command, saved with the layout. Over the scene the view
//! shows a grid, an axis indicator, icons for lights and cameras, the
//! selected camera's frustum or light's reach and the selection's outline,
//! each with a toggle in the view's menu ([`ViewSettings`]); none of them
//! reaches `screenshot`, which draws the app's own snapshot.
//!
//! Several entities can be selected ([`Selection`]), one of them active (the
//! one selected last): the inspector shows the active one, `Ctrl+Up` and
//! `Ctrl+Down` select the selection's parents or children, and Delete
//! removes the selected entities in one undo step.
//!
//! The game view carries translate, rotate and scale gizmos for the
//! selection ([`gizmo`]; `W`, `E`, `R`), standing on the active entity or at
//! the centre of the selection's bounds. A drag moves each selected entity
//! without a selected ancestor as it goes, with `scene.apply` edits that
//! share one coalesce key, so the drag is one undo step; Escape puts them
//! back.
//!
//! Every action is a command dispatched through
//! [`App::dispatch`](re_app::App::dispatch), so each one is a single undo step
//! and lands in the transaction log. Other writers reach the editor through
//! its socket ([`EditorSocket`], `.real/editor.sock`): their commands are
//! applied between frames, on the editor's thread, with the editor's policy.
//!
//! [`run_editor`] is the windowed editor. [`HeadlessEditor`] draws the same
//! panels into an offscreen [`Capture`](re_render::Capture), for tests and
//! screenshots.
//!
//! Diagnostic codes are `RE8800`–`RE8899` ([`codes`]). Refusing a second
//! writer is `re_agent`'s `RE8003`, never a code of this crate.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod camera;
mod canvas;
pub mod codes;
mod debugger;
mod editor;
pub mod gizmo;
mod headless;
pub mod layout;
mod overlay;
mod panels;
mod selection;
mod socket;
mod view;
mod widgets;
mod windowed;

pub use canvas::CanvasSpot;
pub use editor::{ConsoleLine, Editor, LineKind, Panel, PanelRects};
pub use headless::HeadlessEditor;
pub use overlay::ViewSettings;
pub use selection::Selection;
pub use socket::EditorSocket;
pub use view::GameView;
pub use windowed::run_editor;

use re_command::{AllowAll, PolicyCheck};
use re_core::DiagInfo;

/// How [`run_editor`] runs.
pub struct EditorOptions {
    /// The policy every command is checked against: the editor's own actions
    /// and the commands that arrive on the editor socket. Default: allow all
    /// (the person at the editor is the project's user).
    pub policy: Box<dyn PolicyCheck + Send>,
    /// The graphics backends to try; `None` takes `RE_BACKENDS`, else the
    /// platform's primary API.
    pub backends: Option<re_render::wgpu::Backends>,
    /// The window's initial size in physical pixels; `None` takes the
    /// project's `[render]` size.
    pub size: Option<[u32; 2]>,
    /// Close after this many presented frames (smoke tests); `None` runs until
    /// the window is closed.
    pub max_frames: Option<u64>,
    /// Register `re_agent`'s commands (`script.write`, `checkpoint.restore`,
    /// `file.delete`) and start an agent session once the lock is held, so
    /// that `real mcp` works through the editor and `checkpoint.restore`
    /// returns to the scene as the editor opened it. Default: on.
    pub agent_commands: bool,
    /// The agent state the caller registered its agent-facing commands with
    /// (`real editor` registers the same set as `real mcp`). With
    /// [`EditorOptions::agent_commands`] the session starts on it; `None`:
    /// a new one, with only `re_agent`'s commands registered.
    /// Ignored (and dropped) when `agent_commands` is off.
    pub agent: Option<re_agent::Agent>,
    /// The project lock and socket, when the caller took them before opening
    /// the project (as `real editor` does, so that no other writer can
    /// change the scene between loading it and locking it). `None`: taken
    /// by [`run_editor`].
    pub socket: Option<EditorSocket>,
    /// The script debugger the debugger panel shows and the game's scripts
    /// attach to (ADR-0045), when the caller also serves it over DAP.
    /// `None`: a new one, used by the panel only.
    pub debugger: Option<re_script::debug::Debugger>,
}

impl Default for EditorOptions {
    fn default() -> Self {
        EditorOptions {
            policy: Box::new(AllowAll),
            backends: None,
            size: None,
            max_frames: None,
            agent_commands: true,
            agent: None,
            socket: None,
            debugger: None,
        }
    }
}

impl std::fmt::Debug for EditorOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditorOptions")
            .field("backends", &self.backends)
            .field("size", &self.size)
            .field("max_frames", &self.max_frames)
            .field("agent_commands", &self.agent_commands)
            .finish_non_exhaustive()
    }
}

/// Every diagnostic code `re_editor` owns, with its title and explain page.
pub const DIAGNOSTICS: &[DiagInfo] = &[
    DiagInfo {
        code: codes::NO_DISPLAY,
        title: "The editor window could not be opened",
        page: include_str!("../explain/RE8801.md"),
    },
    DiagInfo {
        code: codes::PALETTE_ARGS,
        title: "Invalid command-palette arguments",
        page: include_str!("../explain/RE8802.md"),
    },
    DiagInfo {
        code: codes::INVALID_FIELD_TEXT,
        title: "Inspector text is not a valid value",
        page: include_str!("../explain/RE8803.md"),
    },
    DiagInfo {
        code: codes::LAYOUT_UNREADABLE,
        title: "The editor layout could not be read",
        page: include_str!("../explain/RE8804.md"),
    },
];

/// The code range this crate owns (ARCHITECTURE §7).
pub const CODE_RANGE: std::ops::RangeInclusive<u16> = 8800..=8899;
