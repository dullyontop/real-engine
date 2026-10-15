//! [`GameView`]: the scene drawn into an offscreen texture that the viewport
//! panel shows.

use re_app::App;
use re_app::sync::physics::PhysicsSync;
use re_app::sync::render::{self, RenderInputs};
use re_core::Diagnostic;
use re_render::{Capture, FrameReport, Gpu, Renderer, wgpu};
use re_types::MaterialDesc;

use crate::Editor;

/// The game view's texture format. egui treats texture colour as
/// gamma-encoded; the renderer gamma-encodes into `Rgba8Unorm` on every backend
/// (see `Renderer::register_ui_texture`).
const VIEW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// The scene as the editor shows it: the World in edit mode (nothing
/// simulates), drawn from its own camera into a texture the size of the
/// viewport panel.
///
/// Each frame: [`GameView::sync`] resizes the texture to the size the
/// viewport had in the last pass and gives its egui id to the [`Editor`];
/// after the egui pass, [`GameView::render`] draws the scene into it, before
/// the UI is painted.
pub struct GameView {
    target: Option<(Capture, egui::TextureId)>,
    snapshot_physics: PhysicsSync,
    last: FrameReport,
}

impl std::fmt::Debug for GameView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameView")
            .field("size", &self.size())
            .field("last", &self.last)
            .finish_non_exhaustive()
    }
}

impl Default for GameView {
    fn default() -> Self {
        GameView::new()
    }
}

impl GameView {
    /// A game view with no texture yet.
    pub fn new() -> GameView {
        GameView {
            target: None,
            snapshot_physics: PhysicsSync::new(),
            last: FrameReport::default(),
        }
    }

    /// The texture's size in pixels.
    pub fn size(&self) -> Option<[u32; 2]> {
        self.target.as_ref().map(|(c, _)| [c.width(), c.height()])
    }

    /// What the last [`GameView::render`] drew.
    pub fn last_report(&self) -> &FrameReport {
        &self.last
    }

    /// Makes the texture the size the editor's viewport had in its last pass
    /// (64×64 before the first) and hands its egui id to `editor`.
    ///
    /// # Errors
    ///
    /// `RE6005` when the texture cannot be created.
    pub fn sync(
        &mut self,
        gpu: &Gpu,
        renderer: &mut Renderer,
        editor: &mut Editor,
    ) -> Result<(), Diagnostic> {
        let want = editor.viewport_size_px().unwrap_or([64, 64]);
        let limit = gpu.device.limits().max_texture_dimension_2d;
        let want = [want[0].clamp(1, limit), want[1].clamp(1, limit)];
        if self.size() != Some(want) {
            let capture = Capture::with_format(&gpu.device, want[0], want[1], VIEW_FORMAT)?;
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

    /// Draws the World into the texture: the render snapshot is built from the
    /// World as it is (edit mode), with the project's clear colour. A default
    /// material is created the first time, so entities without a material
    /// are drawn; meshes are drawn once the app's `RenderAssets` has a handle
    /// for them.
    ///
    /// # Errors
    ///
    /// The renderer's error (`RE6005`, `RE6007`); nothing is drawn then.
    pub fn render(
        &mut self,
        gpu: &Gpu,
        renderer: &mut Renderer,
        app: &mut App,
    ) -> Result<&FrameReport, Diagnostic> {
        let Some((capture, _)) = &self.target else {
            return Ok(&self.last);
        };
        if app.render_assets_mut().default_material.is_none() {
            let material = renderer.create_material(&MaterialDesc::default())?;
            app.render_assets_mut().default_material = Some(material);
        }
        let clear = app.config().render.clear;
        let assets = std::mem::take(app.render_assets_mut());
        let inputs = RenderInputs {
            assets: &assets,
            physics: &self.snapshot_physics,
            alpha: 0.0,
            clear,
        };
        let snapshot = render::build(app.world_mut(), &inputs);
        *app.render_assets_mut() = assets;
        self.last = renderer.render(&gpu.device, &gpu.queue, capture.view(), &snapshot)?;
        Ok(&self.last)
    }

    /// Forgets the texture (before the renderer is dropped).
    pub fn release(&mut self, renderer: &mut Renderer) {
        if let Some((_, id)) = self.target.take() {
            renderer.free_ui_texture(id);
        }
    }
}

/// Clears `target` to the editor's background colour, so the panels are
/// painted over a known colour (window surfaces start undefined).
pub(crate) fn clear(gpu: &Gpu, target: &wgpu::TextureView) {
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
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
    gpu.queue.submit([encoder.finish()]);
}
