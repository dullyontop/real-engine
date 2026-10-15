//! [`HeadlessEditor`]: the editor drawn into an offscreen [`Capture`], for
//! tests and screenshots.

use re_app::App;
use re_command::PolicyCheck;
use re_core::Diagnostic;
use re_render::{Capture, Gpu, Image, Renderer, UiFrame};

use crate::Editor;
use crate::view::{self, GameView};

/// The editor without a window: the same [`Editor`] and [`GameView`] as
/// [`run_editor`](crate::run_editor), painted into a [`Capture`].
///
/// egui lays out a panel in the pass after it first sees it, so the first
/// frame is a warm-up: read the image from the second frame on.
pub struct HeadlessEditor {
    /// The editor's UI state.
    pub editor: Editor,
    /// The game view.
    pub view: GameView,
    ctx: egui::Context,
    capture: Capture,
    renderer: Renderer,
    gpu: Gpu,
    time: f64,
}

impl std::fmt::Debug for HeadlessEditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeadlessEditor")
            .field("editor", &self.editor)
            .field("view", &self.view)
            .field("size", &[self.capture.width(), self.capture.height()])
            .finish_non_exhaustive()
    }
}

impl HeadlessEditor {
    /// A headless editor of `width` × `height` pixels on the backends of
    /// `RE_BACKENDS` (else the platform's primary API).
    ///
    /// # Errors
    ///
    /// `RE6002`/`RE6003` when no GPU device can be created, `RE6006` for an
    /// unknown `RE_BACKENDS`, `RE6005` for an invalid size.
    pub fn new(width: u32, height: u32) -> Result<HeadlessEditor, Diagnostic> {
        let gpu = Gpu::headless(re_render::backends_from_env()?)?;
        let renderer = Renderer::new(&gpu, Capture::FORMAT);
        let capture = Capture::new(&gpu.device, width, height)?;
        Ok(HeadlessEditor {
            editor: Editor::new(),
            view: GameView::new(),
            ctx: egui::Context::default(),
            capture,
            renderer,
            gpu,
            time: 0.0,
        })
    }

    /// The GPU.
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// The egui context.
    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    /// Runs one editor frame with `input` (its screen rectangle and time are
    /// filled in): the egui pass and the changes it asked for, the game view,
    /// and the UI painted over it. Returns the frame's image.
    ///
    /// # Errors
    ///
    /// A renderer or readback error.
    pub fn frame(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
        mut input: egui::RawInput,
    ) -> Result<Image, Diagnostic> {
        let size = [self.capture.width(), self.capture.height()];
        self.view
            .sync(&self.gpu, &mut self.renderer, &mut self.editor)?;
        // The screen in points: the capture's pixels over the scale the input
        // asks for (1 unless it sets `native_pixels_per_point`).
        let ppp = input
            .viewports
            .get(&input.viewport_id)
            .and_then(|v| v.native_pixels_per_point)
            .unwrap_or(1.0);
        input.screen_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(size[0] as f32, size[1] as f32) / ppp,
        ));
        self.time += 1.0 / 60.0;
        input.time = Some(self.time);
        let editor = &mut self.editor;
        let mut output = self.ctx.run_ui(input, |ui| editor.ui(ui, app, policy));
        let ui = UiFrame::from_output(&self.ctx, &mut output, size);
        self.view.render(&self.gpu, &mut self.renderer, app)?;
        view::clear(&self.gpu, self.capture.view());
        self.renderer
            .paint_ui(&self.gpu.device, &self.gpu.queue, self.capture.view(), &ui)?;
        self.capture.read(&self.gpu.device, &self.gpu.queue)
    }
}

impl Drop for HeadlessEditor {
    fn drop(&mut self) {
        self.view.release(&mut self.renderer);
    }
}
