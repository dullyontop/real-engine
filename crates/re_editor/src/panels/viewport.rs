//! The viewport: a row of view controls over the game view's texture, with
//! the editor camera's controls, the gizmo, icons for lights and cameras and
//! the axis indicator on the image (ADR-0044's amendment for WP42, §2 and §3).

use egui::{Key, Modifiers, RichText, Sense};
use glam::Vec3;
use re_app::App;

use super::procedural;
use crate::camera::{self, Navigation, SPEEDS, View};
use crate::editor::{Action, Editor};
use crate::{Panel, overlay};

/// Draws the viewport into `ui`. Its rectangle in [`Editor::rects`] is its
/// image's, under the row of view controls.
pub(super) fn viewport(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    view_bar(ed, ui, app, actions);
    let rect = ui.available_rect_before_wrap();
    ed.rects.set(Panel::Viewport, rect);
    let ppp = ui.ctx().pixels_per_point();
    let px = [
        (rect.width() * ppp).round().max(1.0) as u32,
        (rect.height() * ppp).round().max(1.0) as u32,
    ];
    ed.viewport_px = Some(px);
    match ed.viewport_texture {
        Some((texture, _)) => {
            let image =
                egui::Image::from_texture(egui::load::SizedTexture::new(texture, rect.size()));
            ui.put(rect, image);
        }
        None => {
            ui.put(
                rect,
                egui::Label::new(RichText::new("No game view (no GPU)").weak()),
            );
        }
    }
    // One response for the whole image: the camera, the gizmo, the icons and
    // the axis indicator all read it, in that order.
    let response = ui.interact(
        rect,
        ui.id().with("re_editor.viewport"),
        Sense::click_and_drag(),
    );

    // The camera moves first, so that everything drawn over the image this
    // frame matches the scene drawn after it. A gizmo drag keeps the pointer.
    if ed.view.game_camera {
        ed.nav = Navigation::default();
    } else if let Some(cam) = ed.camera.as_mut()
        && (ed.gizmo.drag.is_none() || ed.nav.active())
    {
        let before = View::new(rect, cam.render_camera(Vec3::ZERO));
        camera::navigate(cam, &mut ed.nav, ui, &response, before.screen.as_ref());
    }
    let view = View::new(rect, ed.view_camera(app));

    let selected = ed.selected.clone();
    let navigating = ed.nav.active();
    ed.gizmo.ui(
        ui,
        &response,
        &view,
        app,
        selected.as_deref(),
        navigating,
        actions,
    );
    let taken = procedural::viewport_overlay(ed, ui, app, &view, actions);

    // Icons and the axis indicator, lit under the pointer unless the camera
    // or the gizmo has it.
    let painter = ui.painter_at(rect);
    let pointer = ui
        .input(|i| i.pointer.hover_pos())
        .filter(|p| rect.contains(*p))
        .filter(|_| !navigating && !taken && ed.gizmo.drag.is_none() && ed.gizmo.hovered.is_none());
    let icon = match (&view.screen, ed.view.icons) {
        (Some(screen), true) => {
            let icons = overlay::icons(app.world());
            overlay::draw_icons(&painter, screen, &icons, selected.as_deref(), pointer)
        }
        _ => None,
    };
    let axis = if ed.view.axes {
        let rotation = overlay::rotation_of(&view.camera.world);
        let axis = overlay::axis_under(rect, rotation, pointer);
        overlay::draw_axes(&painter, rect, rotation, axis);
        axis
    } else {
        None
    };
    // A click (not a drag, not with Alt, which orbits) on an axis end looks
    // along it; on an icon, selects its light or camera. The gizmo's
    // handles take a click before both.
    let alt = ui.input(|i| i.modifiers.alt);
    if response.clicked() && !alt && pointer.is_some() {
        if let Some(axis) = axis {
            if !ed.view.game_camera
                && let Some(cam) = ed.camera.as_mut()
            {
                cam.look_from(axis);
            }
        } else if let Some(id) = icon {
            ed.select_next = Some(id);
        }
    }

    // F frames the selection, Shift+F everything.
    if !ui.ctx().egui_wants_keyboard_input() {
        let all = ui.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::F));
        let selection = !all && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::F));
        if all || selection {
            ed.frame_selection(app, all);
        }
    }
}

/// The row over the image: the View menu (overlays, framing, aligning the
/// game's camera), looking through the game's camera, and the fly speed.
fn view_bar(ed: &mut Editor, ui: &mut egui::Ui, app: &App, actions: &mut Vec<Action>) {
    ui.horizontal(|ui| {
        ui.menu_button("View", |ui| {
            ui.checkbox(&mut ed.view.grid, "Grid");
            ui.checkbox(&mut ed.view.axes, "Axis indicator");
            ui.checkbox(&mut ed.view.icons, "Light and camera icons");
            ui.checkbox(&mut ed.view.outline, "Selection outline");
            ui.separator();
            let has_selection = ed.selected.is_some();
            if ui
                .add_enabled(has_selection, egui::Button::new("Frame selection (F)"))
                .clicked()
            {
                ed.frame_selection(app, false);
                ui.close();
            }
            if ui.button("Frame all (Shift+F)").clicked() {
                ed.frame_selection(app, true);
                ui.close();
            }
            let align = ed.align_action(app);
            if ui
                .add_enabled(align.is_some(), egui::Button::new("Align game camera with view"))
                .on_hover_text("Move the game's active camera to where the view is (one undo step)")
                .on_disabled_hover_text("Needs an active camera in the scene, seen from the editor camera")
                .clicked()
            {
                actions.extend(align);
                ui.close();
            }
        });
        ui.toggle_value(&mut ed.view.game_camera, "Game camera")
            .on_hover_text("Look through the game's active camera, as `real run` draws");
        if let Some(cam) = ed.camera.as_mut() {
            ui.separator();
            ui.label("Speed");
            let step = (f64::from(cam.speed) * 0.01).max(0.001);
            ui.add_enabled(
                !ed.view.game_camera,
                egui::DragValue::new(&mut cam.speed)
                    .range(SPEEDS.0..=SPEEDS.1)
                    .speed(step)
                    .max_decimals(2)
                    .suffix(" m/s"),
            )
            .on_hover_text(
                "How fast the camera flies (right button and W, A, S, D, Q, E); the wheel changes it while flying",
            );
        }
    });
}
