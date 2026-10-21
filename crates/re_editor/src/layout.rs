//! The docked panel layout and the gizmo settings, saved per project in
//! `.real/editor/layout.json` (ADR-0044).
//!
//! The layout is UI state, not project content: it is not a command, not in
//! the undo history, and `.real/` is not under version control. A missing
//! file gives the default layout; a file that cannot be read gives the
//! default layout and an `RE8804` warning in the console.

use std::path::{Path, PathBuf};

use egui_dock::DockState;
use re_core::Diagnostic;
use serde::{Deserialize, Serialize};

use crate::codes;
use crate::gizmo::GizmoSettings;

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
}

impl PanelId {
    /// Every panel.
    pub const ALL: [PanelId; 6] = [
        PanelId::Viewport,
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
        }
    }
}

/// The version of the layout file this editor writes.
pub const LAYOUT_VERSION: u32 = 1;

/// The file's contents.
#[derive(Serialize, Deserialize)]
struct LayoutFile {
    version: u32,
    dock: DockState<PanelId>,
    #[serde(default)]
    gizmo: GizmoSettings,
}

/// The layout file of the project at `root`.
pub fn layout_path(root: &Path) -> PathBuf {
    root.join(".real").join("editor").join("layout.json")
}

/// The default layout: the hierarchy on the left, the inspector over the
/// procedural panel on the right, the console and the debugger (tabs) under
/// the game view.
pub fn default_dock() -> DockState<PanelId> {
    let mut dock = DockState::new(vec![PanelId::Viewport]);
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
pub fn load(root: &Path) -> Result<Option<(DockState<PanelId>, GizmoSettings)>, Diagnostic> {
    let path = layout_path(root);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(bad_layout(&path, &e.to_string())),
    };
    let file: LayoutFile =
        serde_json::from_str(&text).map_err(|e| bad_layout(&path, &e.to_string()))?;
    if file.version != LAYOUT_VERSION {
        return Err(bad_layout(
            &path,
            &format!(
                "layout version {} (this editor reads {LAYOUT_VERSION})",
                file.version
            ),
        ));
    }
    let mut dock = file.dock;
    complete(&mut dock);
    Ok(Some((dock, file.gizmo)))
}

/// The JSON written for a layout.
pub fn to_json(dock: &DockState<PanelId>, gizmo: &GizmoSettings) -> String {
    #[derive(Serialize)]
    struct Out<'a> {
        version: u32,
        dock: &'a DockState<PanelId>,
        gizmo: &'a GizmoSettings,
    }
    // Node rectangles are recomputed every frame from the split fractions;
    // they are written as zero, so resizing the window is not a change, and
    // because a node never drawn has an infinite rectangle, which JSON
    // cannot hold.
    let mut dock = dock.clone();
    for (_, node) in dock.iter_all_nodes_mut() {
        node.set_rect(egui::Rect::ZERO);
        if let egui_dock::Node::Leaf(leaf) = node {
            leaf.viewport = egui::Rect::ZERO;
        }
    }
    let mut text = serde_json::to_string_pretty(&Out {
        version: LAYOUT_VERSION,
        dock: &dock,
        gizmo,
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
        let mut dock = default_dock();
        dock.main_surface_mut().push_to_first_leaf(PanelId::Console); // a duplicate
        save(dir.path(), &to_json(&dock, &gizmo)).unwrap();
        let (back, g) = load(dir.path()).unwrap().unwrap();
        assert_eq!(g, gizmo);
        assert_eq!(
            back.iter_all_tabs()
                .filter(|(_, t)| **t == PanelId::Console)
                .count(),
            1
        );

        std::fs::write(
            layout_path(dir.path()),
            r#"{"version": 1, "dock": {"surfaces": 3}}"#,
        )
        .unwrap();
        let e = load(dir.path()).err().unwrap();
        assert_eq!(e.code, codes::LAYOUT_UNREADABLE);
        // An unknown panel name (a newer editor's layout) is refused, not guessed.
        let text = to_json(&default_dock(), &gizmo).replace("\"Console\"", "\"Profiler\"");
        std::fs::write(layout_path(dir.path()), text).unwrap();
        assert!(load(dir.path()).is_err());
        let text = to_json(&default_dock(), &gizmo).replace("\"version\": 1", "\"version\": 9");
        std::fs::write(layout_path(dir.path()), text).unwrap();
        assert!(
            load(dir.path())
                .err()
                .unwrap()
                .message
                .contains("version 9")
        );
    }
}
