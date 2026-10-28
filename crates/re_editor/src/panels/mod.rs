//! The panels: toolbar, hierarchy, inspector, procedural sources, console,
//! command palette and the viewport, one module each. They read the [`App`]
//! and push [`Action`]s; [`Editor::ui`](crate::Editor::ui) applies them
//! afterwards. The field widgets that the inspector and the procedural panel
//! share are in [`crate::widgets`].

mod console;
mod hierarchy;
mod inspector;
mod palette;
mod procedural;
mod toolbar;
mod viewport;

use re_app::App;

use crate::Panel;
use crate::editor::{Action, Editor, Pending};
use crate::layout::PanelId;

pub(crate) use palette::palette_action;

/// Draws every panel into the root `ui`.
pub(crate) fn draw(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    // A selection clicked in the last frame is applied after this one: a
    // field being edited in the inspector sees its focus loss one frame after
    // the click, and commits to the entity it belongs to first.
    let select_now = ed.select_next.take();
    // Undo and Redo clicks wait a frame for the same reason.
    let later = std::mem::take(&mut ed.later);
    let r = egui::Panel::top("re_editor.toolbar").show(ui, |ui| toolbar::toolbar(ed, ui, app));
    ed.rects.set(Panel::Toolbar, r.response.rect);

    // The other panels are tabs in the dock (ADR-0044). The dock is taken out
    // of the editor while it is drawn, so the tabs can borrow the editor.
    let mut dock = std::mem::replace(&mut ed.dock, egui_dock::DockState::new(Vec::new()));
    let mut style = egui_dock::Style::from_egui(ui.style());
    style.dock_area_padding = None;
    // How close a split may be dragged to its edge; also the room egui_dock
    // keeps on each side of a split when the window is small.
    style.separator.extra = 60.0;
    egui::CentralPanel::no_frame().show(ui, |ui| {
        egui_dock::DockArea::new(&mut dock)
            .id(egui::Id::new("re_editor.dock"))
            .style(style)
            .show_close_buttons(false)
            .show_add_buttons(false)
            .show_leaf_close_all_buttons(false)
            .show_inside(
                ui,
                &mut Tabs {
                    ed: &mut *ed,
                    app,
                    actions: &mut *actions,
                },
            );
    });
    ed.dock = dock;

    palette::palette(ed, ui.ctx(), app, actions);

    actions.extend(later);
    if let Some(id) = select_now {
        select(ed, Some(id));
    }
}

/// Draws the dock's tabs.
struct Tabs<'a> {
    ed: &'a mut Editor,
    app: &'a App,
    actions: &'a mut Vec<Action>,
}

impl egui_dock::TabViewer for Tabs<'_> {
    type Tab = PanelId;

    fn id(&mut self, tab: &mut PanelId) -> egui::Id {
        egui::Id::new(("re_editor.tab", *tab))
    }

    fn title(&mut self, tab: &mut PanelId) -> egui::WidgetText {
        tab.title().into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut PanelId) {
        let (ed, app, actions) = (&mut *self.ed, self.app, &mut *self.actions);
        let rect = ui.max_rect();
        let panel = match tab {
            PanelId::Viewport => {
                // Its rectangle is its image's, set by the panel.
                viewport::viewport(ed, ui, app, actions);
                return;
            }
            PanelId::Hierarchy => {
                hierarchy::hierarchy(ed, ui, app.world(), actions);
                Panel::Hierarchy
            }
            PanelId::Inspector => {
                egui::ScrollArea::vertical()
                    .id_salt("re_editor.inspector")
                    .auto_shrink(false)
                    .show(ui, |ui| inspector::inspector(ed, ui, app.world(), actions));
                Panel::Inspector
            }
            PanelId::Graph => {
                egui::ScrollArea::vertical()
                    .id_salt("re_editor.graph")
                    .auto_shrink(false)
                    .show(ui, |ui| procedural::graph(ed, ui, app, actions));
                Panel::Graph
            }
            PanelId::Console => {
                console::console(ed, ui);
                Panel::Console
            }
            PanelId::Debugger => {
                crate::debugger::ui(&mut ed.debug, ui);
                return;
            }
            PanelId::Canvas => {
                crate::canvas::ui(ed, ui, app, actions);
                Panel::Canvas
            }
        };
        ed.rects.set(panel, rect);
    }

    fn is_closeable(&self, _tab: &PanelId) -> bool {
        false
    }

    fn scroll_bars(&self, _tab: &PanelId) -> [bool; 2] {
        // Each panel scrolls itself; the game view never does.
        [false, false]
    }
}

fn select(ed: &mut Editor, id: Option<String>) {
    if ed.selected != id {
        ed.pending = Pending::default();
    }
    ed.selected = id;
}
