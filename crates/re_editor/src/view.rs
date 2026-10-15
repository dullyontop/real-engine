//! [`GameView`]: the scene drawn into an offscreen texture that the viewport
//! panel shows.

use re_app::App;
use re_core::Diagnostic;
use re_render::{Capture, wgpu};

use crate::Editor;

/// The game view's texture format. egui treats texture colour as
/// gamma-encoded; the renderer gamma-encodes into `Rgba8Unorm` on every backend
/// (see `Renderer::register_ui_texture`).
const VIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The scene as the editor shows it: the World in edit mode (nothing
/// simulates), drawn from its own camera into a texture the size of the
/// viewport panel.
///
/// The editor's GPU device and renderer belong to the [`App`] (its
/// [`Graphics`](re_app::Graphics)), so the app uploads the meshes and
/// materials the World names to the renderer that draws them. Each frame:
/// [`GameView::sync`] resizes the texture to the size the viewport had in the
/// last pass and gives its egui id to the [`Editor`]; after the egui pass,
/// [`GameView::render`] draws the scene into it, before the UI is painted.
#[derive(Debug, Default)]
pub struct GameView {
    target: Option<(Capture, egui::TextureId)>,
}

impl GameView {
    /// A game view with no texture yet.
    pub fn new() -> GameView {
        GameView { target: None }
    }

    /// The texture's size in pixels.
    pub fn size(&self) -> Option<[u32; 2]> {
        self.target.as_ref().map(|(c, _)| [c.width(), c.height()])
    }

    /// Makes the texture the size the editor's viewport had in its last pass
    /// (64×64 before the first) and hands its egui id to `editor`.
    ///
    /// # Errors
    ///
    /// `RE0516` when the app has no GPU side, `RE6005` when the texture cannot
    /// be created.
    pub fn sync(&mut self, app: &mut App, editor: &mut Editor) -> Result<(), Diagnostic> {
        let gfx = app.graphics_mut().ok_or_else(no_graphics)?;
        let want = editor.viewport_size_px().unwrap_or([64, 64]);
        let limit = gfx.gpu().device.limits().max_texture_dimension_2d;
        let want = [want[0].clamp(1, limit), want[1].clamp(1, limit)];
        if self.size() != Some(want) {
            let capture = Capture::with_format(&gfx.gpu().device, want[0], want[1], VIEW_FORMAT)?;
            let renderer = gfx.renderer_mut();
            let id = match self.target.take() {
                Some((_, id)) => {
                    renderer.update_ui_texture(id, capture.view());
                    id
                }
                None => renderer.register_ui_texture(capture.view()),
            };
            self.target = Some((capture, id));
        }
        editor.set_viewport_texture(
            self.target
                .as_ref()
                .map(|(c, id)| (*id, [c.width(), c.height()])),
        );
        Ok(())
    }

    /// Draws the World into the texture: [`App::edit_frame`] regenerates
    /// changed procedural sources, uploads new meshes and materials and
    /// rebuilds the render snapshot from the World as it is (edit mode), which
    /// is then drawn with the project's clear colour. A default material is
    /// created the first time, so entities without a material are drawn.
    ///
    /// The game's HUD is not drawn here: it is egui too, and two egui
    /// contexts cannot share the renderer's UI painter in one frame.
    ///
    /// # Errors
    ///
    /// `RE0516` when the app has no GPU side, and the renderer's errors
    /// (`RE6005`, `RE6007`); nothing is drawn then.
    pub fn render(&mut self, app: &mut App) -> Result<(), Diagnostic> {
        let Some((capture, _)) = &self.target else {
            return Ok(());
        };
        if app.render_assets_mut().default_material.is_none() {
            let gfx = app.graphics_mut().ok_or_else(no_graphics)?;
            let material = gfx
                .renderer_mut()
                .create_material(&re_types::MaterialDesc::default())?;
            app.render_assets_mut().default_material = Some(material);
        }
        app.edit_frame();
        let snapshot = app.snapshot().clone();
        let gfx = app.graphics_mut().ok_or_else(no_graphics)?;
        let (device, queue) = (gfx.gpu().device.clone(), gfx.gpu().queue.clone());
        gfx.renderer_mut()
            .render(&device, &queue, capture.view(), &snapshot)?;
        Ok(())
    }

    /// Forgets the texture (before the app's renderer is dropped).
    pub fn release(&mut self, app: &mut App) {
        if let Some((_, id)) = self.target.take()
            && let Some(gfx) = app.graphics_mut()
        {
            gfx.renderer_mut().free_ui_texture(id);
        }
    }
}

pub(crate) fn no_graphics() -> Diagnostic {
    Diagnostic::error(
        re_app::codes::SCREENSHOT_FAILED,
        "the editor's app has no GPU side to draw the game view with",
    )
}

/// Clears `target` to the editor's background colour, so the panels are
/// painted over a known colour (window surfaces start undefined).
pub(crate) fn clear(device: &wgpu::Device, queue: &wgpu::Queue, target: &wgpu::TextureView) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("re_editor clear"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("re_editor clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.01,
                        g: 0.01,
                        b: 0.012,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
    }
    queue.submit([encoder.finish()]);
}
