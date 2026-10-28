//! The editor's state saved per project in `.real/editor/layout.json`
//! (ADR-0044 and its amendment for WP42, §1): the docked panel layout, the
//! gizmo settings, the editor camera, the view's toggles, the selection and
//! each panel's own view state.
//!
//! The layout is editor state, not project content: it is not a command, not
//! in the undo history, and `.real/` is not under version control. A missing
//! file gives the default layout; a file that cannot be read gives the
//! default layout and an `RE8804` warning in the console. The editor reads
//! version 1 and 2 files and writes version 2.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use egui_dock::DockState;
use re_core::Diagnostic;
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::camera::EditorCamera;
use crate::codes;
use crate::gizmo::GizmoSettings;
use crate::overlay::ViewSettings;

/// A dockable panel. Each appears once in the layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PanelId {
    /// The game view, with the gizmo.
    Viewport,
    /// The entity tree.
    Hierarchy,
    /// The selected entity's components.
    Inspector,
    /// Procedural sources: seed, parameter overrides, regenerate.
    Graph,
    /// Diagnostics, command results and the transaction log.
    Console,
    /// The script debugger: breakpoints, stack and variables.
    Debugger,
    /// The node canvas of a graph file.
    Canvas,
}

impl PanelId {
    /// Every panel.
    pub const ALL: [PanelId; 7] = [
        PanelId::Viewport,
        PanelId::Canvas,
        PanelId::Hierarchy,
        PanelId::Inspector,
        PanelId::Graph,
        PanelId::Console,
        PanelId::Debugger,
    ];

    /// The tab's title.
    pub fn title(self) -> &'static str {
        match self {
            PanelId::Viewport => "Game view",
            PanelId::Hierarchy => "Hierarchy",
            PanelId::Inspector => "Inspector",
            PanelId::Graph => "Procedural",
            PanelId::Console => "Console",
            PanelId::Debugger => "Debugger",
            PanelId::Canvas => "Node graph",
        }
    }
}

/// The version of the layout file this editor writes; it also reads
/// version 1 (no camera, view, selection or panel state).
pub const LAYOUT_VERSION: u32 = 2;

/// The node canvas's collapsed nodes: node ids by graph file path.
pub type Collapsed = BTreeMap<String, BTreeSet<String>>;

/// Each panel's own view state, by panel id: the panel reads its entry with
/// its own types and takes its defaults when the entry is missing or does
/// not parse. Entries this editor does not know are kept.
pub type PanelStates = BTreeMap<String, Json>;

/// The selection as saved: restored when the project is opened again, minus
/// the entities that no longer exist.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Selection {
    /// The selected entities' ids, in the order they were selected.
    pub ids: Vec<String>,
    /// The active entity, the one selected last (one of `ids`).
    pub active: Option<String>,
}

impl Selection {
    /// Nothing selected.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty() && self.active.is_none()
    }
}

/// A project's saved editor layout.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layout {
    /// The dock: which panel is where.
    pub dock: DockState<PanelId>,
    /// The gizmo's mode, orientation and snapping.
    #[serde(default)]
    pub gizmo: GizmoSettings,
    /// The nodes collapsed on the node canvas: view state, not part of the
    /// graph file.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub collapsed: Collapsed,
    /// The editor camera; `None` until the editor has had one, which then
    /// starts where the game's camera is. A camera that is not finite is
    /// not written.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "finite_camera"
    )]
    pub camera: Option<EditorCamera>,
    /// What the view shows: the overlays, and whether it looks through the
    /// game's camera.
    #[serde(default)]
    pub view: ViewSettings,
    /// The selection.
    #[serde(default, skip_serializing_if = "Selection::is_empty")]
    pub selection: Selection,
    /// Each panel's own view state.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub panels: PanelStates,
}

impl Layout {
    /// The default layout ([`default_dock`]), with default settings and
    /// nothing selected.
    pub fn new() -> Layout {
        Layout {
            dock: default_dock(),
            gizmo: GizmoSettings::default(),
            collapsed: Collapsed::new(),
            camera: None,
            view: ViewSettings::default(),
            selection: Selection::default(),
            panels: PanelStates::new(),
        }
    }
}

impl Default for Layout {
    fn default() -> Self {
        Layout::new()
    }
}

/// Writes `camera` when it is finite, else `null` (read back as none):
/// JSON has no NaN.
fn finite_camera<S: serde::Serializer>(
    camera: &Option<EditorCamera>,
    s: S,
) -> Result<S::Ok, S::Error> {
    camera.filter(EditorCamera::is_finite).serialize(s)
}

/// The file's contents.
#[derive(Deserialize)]
struct LayoutFile {
    version: u32,
    #[serde(flatten)]
    layout: Layout,
}

/// The layout file of the project at `root`.
pub fn layout_path(root: &Path) -> PathBuf {
    root.join(".real").join("editor").join("layout.json")
}

/// The default layout: the hierarchy on the left, the inspector over the
/// procedural panel on the right, the console and the debugger (tabs) under
/// the game view, and the node canvas a tab behind the game view.
pub fn default_dock() -> DockState<PanelId> {
    let mut dock = DockState::new(vec![PanelId::Viewport, PanelId::Canvas]);
    let tree = dock.main_surface_mut();
    let root = egui_dock::NodeIndex::root();
    let [centre, _left] = tree.split_left(root, 0.2, vec![PanelId::Hierarchy]);
    let [centre, right] = tree.split_right(centre, 0.72, vec![PanelId::Inspector]);
    tree.split_below(right, 0.6, vec![PanelId::Graph]);
    tree.split_below(centre, 0.75, vec![PanelId::Console, PanelId::Debugger]);
    dock
}

/// Panels missing from `dock` are added to its first leaf; a panel named
/// twice keeps its first place.
fn complete(dock: &mut DockState<PanelId>) {
    let mut seen = Vec::new();
    dock.retain_tabs(|t| {
        let first = !seen.contains(t);
        seen.push(*t);
        first
    });
    for p in PanelId::ALL {
        if dock.find_tab(&p).is_none() {
            dock.push_to_first_leaf(p);
        }
    }
}

/// Reads the layout of the project at `root`. `Ok(None)` when there is no
/// file; an `RE8804` warning when it cannot be read.
pub fn load(root: &Path) -> Result<Option<Layout>, Diagnostic> {
    let path = layout_path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(bad_layout(&path, &e.to_string())),
    };
    let file: LayoutFile =
        serde_json::from_str(&text).map_err(|e| bad_layout(&path, &e.to_string()))?;
    if !(1..=LAYOUT_VERSION).contains(&file.version) {
        return Err(bad_layout(
            &path,
            &format!(
                "layout version {} (this editor reads 1 to {LAYOUT_VERSION})",
                file.version
            ),
        ));
    }
    let mut layout = file.layout;
    complete(&mut layout.dock);
    Ok(Some(layout))
}

/// The JSON written for a layout.
pub fn to_json(layout: &Layout) -> String {
    #[derive(Serialize)]
    struct Out<'a> {
        version: u32,
        #[serde(flatten)]
        layout: &'a Layout,
    }
    // Node rectangles are recomputed every frame from the split fractions;
    // they are written as zero, so resizing the window is not a change, and
    // because a node never drawn has an infinite rectangle, which JSON
    // cannot hold.
    let mut layout = layout.clone();
    for (_, node) in layout.dock.iter_all_nodes_mut() {
        node.set_rect(egui::Rect::ZERO);
        if let egui_dock::Node::Leaf(leaf) = node {
            leaf.viewport = egui::Rect::ZERO;
        }
    }
    let mut text = serde_json::to_string_pretty(&Out {
        version: LAYOUT_VERSION,
        layout: &layout,
    })
    .expect("a layout always serializes");
    text.push('\n');
    text
}

/// Writes `text` as the layout of the project at `root` (through a temporary
/// file, so a crash never leaves half a layout).
pub fn save(root: &Path, text: &str) -> std::io::Result<()> {
    let path = layout_path(root);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &path)
}

fn bad_layout(path: &Path, why: &str) -> Diagnostic {
    Diagnostic::warning(
        codes::LAYOUT_UNREADABLE,
        format!(
            "the editor layout {} could not be read: {why}",
            path.display()
        ),
    )
    .with_hint("the default layout is used; it replaces the file when the layout next changes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_layout_has_every_panel_once() {
        let dock = default_dock();
        for p in PanelId::ALL {
            assert_eq!(
                dock.iter_all_tabs().filter(|(_, t)| **t == p).count(),
                1,
                "{p:?}"
            );
        }
    }

    #[test]
    fn a_layout_round_trips_and_a_bad_one_warns() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(dir.path()).unwrap().is_none());
        let gizmo = GizmoSettings {
            snap: true,
            ..GizmoSettings::default()
        };
        let mut layout = Layout {
            gizmo,
            collapsed: Collapsed::from([(
                "graphs/a.graph.json".to_string(),
                BTreeSet::from(["h".to_string()]),
            )]),
            camera: Some(EditorCamera {
                pivot: glam::Vec3::new(1.5, -2.0, 1e5),
                yaw: 0.1,
                pitch: -0.7,
                distance: 3.25,
                fov_y_deg: 50.0,
                speed: 0.3,
            }),
            view: ViewSettings {
                game_camera: true,
                grid: false,
                ..ViewSettings::default()
            },
            selection: Selection {
                ids: vec!["a".to_string(), "b".to_string()],
                active: Some("b".to_string()),
            },
            panels: PanelStates::from([(
                "Assets".to_string(),
                serde_json::json!({ "folder": "models" }),
            )]),
            ..Layout::new()
        };
        layout
            .dock
            .main_surface_mut()
            .push_to_first_leaf(PanelId::Console); // a duplicate
        let text = to_json(&layout);
        assert!(text.contains("\"version\": 2"), "{text}");
        save(dir.path(), &text).unwrap();
        let back = load(dir.path()).unwrap().unwrap();
        assert_eq!(back.gizmo, layout.gizmo);
        assert_eq!(back.collapsed, layout.collapsed);
        assert_eq!(back.camera, layout.camera);
        assert_eq!(back.view, layout.view);
        assert_eq!(back.selection, layout.selection);
        assert_eq!(back.panels, layout.panels);
        assert_eq!(
            back.dock
                .iter_all_tabs()
                .filter(|(_, t)| **t == PanelId::Console)
                .count(),
            1
        );
        // Written again, it is the same text: the file changes only when
        // the layout does.
        assert_eq!(to_json(&back), to_json(&load(dir.path()).unwrap().unwrap()));

        std::fs::write(
            layout_path(dir.path()),
            r#"{"version": 1, "dock": {"surfaces": 3}}"#,
        )
        .unwrap();
        let e = load(dir.path()).err().unwrap();
        assert_eq!(e.code, codes::LAYOUT_UNREADABLE);
        // An unknown panel name (a newer editor's layout) is refused, not guessed.
        let text = to_json(&Layout::new()).replace("\"Console\"", "\"Profiler\"");
        std::fs::write(layout_path(dir.path()), text).unwrap();
        assert!(load(dir.path()).is_err());
        let text = to_json(&Layout::new()).replace("\"version\": 2", "\"version\": 9");
        std::fs::write(layout_path(dir.path()), text).unwrap();
        assert!(
            load(dir.path())
                .err()
                .unwrap()
                .message
                .contains("version 9")
        );
    }

    /// A version 1 file (before the editor camera) is read, with the
    /// defaults for what it does not hold.
    #[test]
    fn a_version_1_layout_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let gizmo = GizmoSettings {
            snap: true,
            ..GizmoSettings::default()
        };
        let v2 = to_json(&Layout {
            gizmo,
            ..Layout::new()
        });
        let v1 = v2.replace("\"version\": 2", "\"version\": 1");
        // Version 1 had only the dock, the gizmo and the collapsed nodes.
        let mut json: serde_json::Value = serde_json::from_str(&v1).unwrap();
        let fields: BTreeSet<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(fields, BTreeSet::from(["dock", "gizmo", "version", "view"]));
        json.as_object_mut().unwrap().remove("view");
        save(dir.path(), &json.to_string()).unwrap();
        let back = load(dir.path()).unwrap().unwrap();
        assert_eq!(back.gizmo, gizmo);
        assert_eq!(back.camera, None);
        assert_eq!(back.view, ViewSettings::default());
        assert!(back.selection.is_empty() && back.panels.is_empty());
        assert!(to_json(&back).contains("\"version\": 2"));
    }
}
