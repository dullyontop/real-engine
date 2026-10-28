//! [`HeadlessEditor`]: the editor drawn into an offscreen [`Capture`], for
//! tests and screenshots.

use re_app::{App, Graphics};
use re_command::PolicyCheck;
use re_core::Diagnostic;
use re_render::{Capture, Image, Renderer, UiFrame};

use crate::Editor;
use crate::view::{self, GameView};

/// The editor without a window: the same [`Editor`] and [`GameView`] as
/// [`run_editor`](crate::run_editor), painted into a [`Capture`].
///
/// [`HeadlessEditor::new`] gives the app a GPU device and scene renderer
/// (its [`Graphics`]), which the game view draws with; the panels are painted
/// by the editor's own UI renderer on that device. Use it with that app.
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
    ui: Renderer,
    time: f64,
    /// The text last put on the clipboard.
    copied: Option<String>,
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
    /// A headless editor of `width` × `height` pixels for `app`, on a GPU
    /// device of the backends of `RE_BACKENDS` (else the platform's primary
    /// API) that it hands to `app`.
    ///
    /// # Errors
    ///
    /// `RE6002`/`RE6003` when no GPU device can be created, `RE6006` for an
    /// unknown `RE_BACKENDS`, `RE6005` for an invalid size.
    pub fn new(app: &mut App, width: u32, height: u32) -> Result<HeadlessEditor, Diagnostic> {
        let gpu = re_render::Gpu::headless(re_render::backends_from_env()?)?;
        let renderer = Renderer::new(&gpu, Capture::FORMAT);
        let ui = Renderer::new(&gpu, Capture::FORMAT);
        let capture = Capture::new(&gpu.device, width, height)?;
        app.set_graphics(Graphics::from_parts(gpu, renderer));
        Ok(HeadlessEditor {
            editor: Editor::new(),
            view: GameView::new(),
            ctx: egui::Context::default(),
            capture,
            ui,
            time: 0.0,
            copied: None,
        })
    }

    /// The text the editor last put on the clipboard (Ctrl+C or Ctrl+X, sent
    /// as `egui::Event::Copy` or `Cut`), as a windowed editor's system
    /// clipboard would hold it. Pasting is `egui::Event::Paste` with text.
    pub fn copied(&self) -> Option<&str> {
        self.copied.as_deref()
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
    /// `RE0516` when `app` has no GPU side, a renderer or readback error.
    pub fn frame(
        &mut self,
        app: &mut App,
        policy: &dyn PolicyCheck,
        mut input: egui::RawInput,
    ) -> Result<Image, Diagnostic> {
        let size = [self.capture.width(), self.capture.height()];
        self.view.sync(app, &mut self.ui, &mut self.editor)?;
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
        for command in &output.platform_output.commands {
            if let egui::OutputCommand::CopyText(text) = command {
                self.copied = Some(text.clone());
            }
        }
        // A menu's Paste reads the clipboard, which here is what was copied.
        if self.editor.wants_clipboard() {
            let text = self.copied.clone();
            self.editor.paste_clipboard(app, policy, text.as_deref());
        }
        let ui = UiFrame::from_output(&self.ctx, &mut output, size);
        self.view.render(app, &self.editor)?;
        let gpu = app.graphics().ok_or_else(view::no_graphics)?.gpu();
        let (device, queue) = (&gpu.device, &gpu.queue);
        view::clear(device, queue, self.capture.view());
        self.ui.paint_ui(device, queue, self.capture.view(), &ui)?;
        self.capture.read(device, queue)
    }
}

impl Drop for HeadlessEditor {
    fn drop(&mut self) {
        self.view.release(&mut self.ui);
    }
}
