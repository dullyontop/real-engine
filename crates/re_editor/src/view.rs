//! [`GameView`]: the scene drawn into an offscreen texture that the viewport
//! panel shows.

use re_app::App;
use re_core::Diagnostic;
use re_render::{Capture, Renderer, SceneExtras, wgpu};

use crate::Editor;
use crate::preview::MaterialPreview;

/// The game view's texture format. egui treats texture colour as
/// gamma-encoded; the renderer gamma-encodes into `Rgba8Unorm` on every backend
/// (see `Renderer::register_ui_texture`).
pub(crate) const VIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The scene as the editor shows it: the World in edit mode (nothing
/// simulates), drawn from the editor camera (or through the game's camera,
/// when the view is set to) into a texture the size of the viewport panel,
/// with the editor's overlays: the grid, the selected camera's or light's
/// reach and the selection's outline ([`crate::overlay`]).
///
/// The editor's GPU device and scene renderer belong to the [`App`] (its
/// [`Graphics`](re_app::Graphics)), so the app uploads the meshes and
/// materials the World names to the renderer that draws them. The editor's
/// panels are painted by a second renderer on the same device (the UI
/// renderer), which also shows this texture: the app's renderer paints the
/// game's HUD with its own egui context (for screenshots), and two egui
/// contexts cannot share one UI painter (their font atlases have the same
/// texture id). Each frame:
/// [`GameView::sync`] resizes the texture to the size the viewport had in the
/// last pass and gives its egui id to the [`Editor`]; after the egui pass,
/// [`GameView::render`] draws the scene into it, before the UI is painted.
///
/// It also keeps the Material panel's preview sphere ([`MaterialPreview`]),
/// which the same two renderers draw and show.
#[derive(Debug, Default)]
pub struct GameView {
    target: Option<(Capture, egui::TextureId)>,
    preview: MaterialPreview,
}

impl GameView {
    /// A game view with no texture yet.
    pub fn new() -> GameView {
        GameView::default()
    }

    /// The Material panel's preview sphere.
    pub fn material_preview(&self) -> &MaterialPreview {
        &self.preview
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
    pub fn sync(
        &mut self,
        app: &App,
        ui: &mut Renderer,
        editor: &mut Editor,
    ) -> Result<(), Diagnostic> {
        let device = &app.graphics().ok_or_else(no_graphics)?.gpu().device;
        let want = editor.viewport_size_px().unwrap_or([64, 64]);
        let limit = device.limits().max_texture_dimension_2d;
        let want = [want[0].clamp(1, limit), want[1].clamp(1, limit)];
        if self.size() != Some(want) {
            let capture = Capture::with_format(device, want[0], want[1], VIEW_FORMAT)?;
            let renderer = &mut *ui;
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
        self.preview.sync(app, ui, editor)
    }

    /// Draws the World into the texture as `editor` shows it:
    /// [`App::edit_frame`] regenerates changed procedural sources, uploads new
    /// meshes and materials and rebuilds the render snapshot from the World as
    /// it is (edit mode); a copy of it is drawn from the editor camera, with
    /// the editor's overlays, the project's clear colour and the runtime's
    /// default material for entities without one, as `real run` draws it.
    /// The app's own snapshot, which `screenshot` draws, is left as built.
    ///
    /// The game's HUD is not drawn here.
    ///
    /// # Errors
    ///
    /// `RE0516` when the app has no GPU side, and the renderer's errors
    /// (`RE6005`, `RE6007`); nothing is drawn then.
    pub fn render(&mut self, app: &mut App, editor: &Editor) -> Result<(), Diagnostic> {
        let Some((capture, _)) = &self.target else {
            return Ok(());
        };
        app.edit_frame();
        // First, so that the renderer's last frame is the game view's.
        self.preview.render(app, editor);
        let mut snapshot = app.snapshot().clone();
        let mut extras = SceneExtras::default();
        let render = &app.config().render;
        let game_aspect = render.width.max(1) as f32 / render.height.max(1) as f32;
        crate::overlay::dress(
            editor,
            app.world(),
            app.render_origin(),
            game_aspect,
            &mut snapshot,
            &mut extras,
        );
        let gfx = app.graphics_mut().ok_or_else(no_graphics)?;
        let (device, queue) = (gfx.gpu().device.clone(), gfx.gpu().queue.clone());
        gfx.renderer_mut()
            .render_with(&device, &queue, capture.view(), &snapshot, &extras)?;
        Ok(())
    }

    /// Forgets the textures (before the UI renderer is dropped).
    pub fn release(&mut self, ui: &mut Renderer) {
        if let Some((_, id)) = self.target.take() {
            ui.free_ui_texture(id);
        }
        self.preview.release(ui);
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
