//! The GPU side: the graphics, screenshots and drawing into a window, with a
//! compat game's view.

use std::path::Path;

use re_core::Diagnostic;

use re_render::Image;
use re_script::ContextKind;

use crate::codes;
use crate::gfx::{self, Graphics};
use crate::sync::render::{self, RenderAssets, RenderInputs};
use crate::ui::View;
use crate::ui::hud::{self, HudLine};

use super::{App, ScreenshotRequest};

impl App {
    /// The GPU side, when there is one.
    pub fn graphics(&self) -> Option<&Graphics> {
        self.graphics.as_ref()
    }

    /// The GPU side, mutably.
    pub fn graphics_mut(&mut self) -> Option<&mut Graphics> {
        self.graphics.as_mut()
    }

    /// Gives the app a GPU side (the windowed runner's, for its surface), and
    /// its renderer the shader files of the project ([`crate::shaders`]).
    pub fn set_graphics(&mut self, graphics: Graphics) {
        self.render_assets = RenderAssets::default();
        // The UI host's context lays out and paints every UI the game draws:
        // a new one for the new renderer, which then gets every texture whole.
        self.ui.attach();
        self.graphics = Some(graphics);
        self.install_shaders();
    }

    /// Gives the renderer, when there is one, the current shader files, and
    /// returns the problems of the shaders they change (also in the app's
    /// diagnostics).
    pub(super) fn install_shaders(&mut self) -> Vec<Diagnostic> {
        let Some(gfx) = &mut self.graphics else {
            return Vec::new();
        };
        let problems = self.shaders.install(gfx.renderer_mut());
        self.diagnostics.extend(problems.iter().cloned());
        problems
    }

    /// Takes the GPU side away (meshes are uploaded again by the next owner).
    pub fn take_graphics(&mut self) -> Option<Graphics> {
        self.render_assets = RenderAssets::default();
        self.ui.detach();
        self.graphics.take()
    }

    /// The GPU side, opening a headless device on first use
    /// (`real.toml [render] backends`, or `RE_BACKENDS`).
    ///
    /// # Errors
    ///
    /// `RE0516` when no GPU device can be opened.
    pub fn ensure_graphics(&mut self) -> Result<&mut Graphics, Diagnostic> {
        if self.graphics.is_none() {
            let fail = |d: Diagnostic| {
                Diagnostic::error(
                    codes::SCREENSHOT_FAILED,
                    "no GPU device to draw frames with",
                )
                .with_hint("install a Vulkan or GL driver (lavapipe and llvmpipe work), or set RE_BACKENDS")
                .with_related(d)
            };
            let backends = gfx::backends(&self.config.render.backends).map_err(fail)?;
            let graphics = Graphics::headless(backends).map_err(fail)?;
            self.set_graphics(graphics);
        }
        Ok(self.graphics.as_mut().expect("set above"))
    }

    /// Draws the current frame (the last Sync's snapshot and HUD) offscreen at
    /// the configured size and returns it; with `path`, also writes it as a PNG
    /// (relative paths are under the project). Under [`Lateness::Wait`] (the
    /// default), streamed terrain is drawn whole, however fast its chunks were
    /// made ([`re_proc::stream::complete_view`]).
    ///
    /// # Errors
    ///
    /// `RE0516` when no GPU device can be opened or the file cannot be written,
    /// and the renderer's errors.
    ///
    /// [`Lateness::Wait`]: re_proc::stream::Lateness::Wait
    pub fn screenshot(&mut self, path: Option<&Path>) -> Result<Image, Diagnostic> {
        self.ensure_graphics()?;
        self.complete_terrain_view();
        self.upload_assets();
        let gfx = self.graphics.as_mut().expect("ensured");
        let inputs = RenderInputs {
            assets: &self.render_assets,
            physics: &self.physics_sync,
            skins: &self.skin_sync,
            alpha: self.alpha,
            clear: self.config.render.clear,
            post: self.config.render.post,
            shadows: self.config.render.shadows,
            origin: self.physics.origin(),
        };
        let origin = inputs.origin;
        self.snapshot = render::build(&mut self.world, &inputs);
        self.hud = hud::collect(&mut self.world);
        let size = [self.config.render.width, self.config.render.height];
        let image = if self.compat.is_some() {
            self.capture_compat(size)?
        } else {
            let view = View {
                size_px: size,
                pixels_per_point: 1.0,
            };
            self.ui.sync(
                &mut self.world,
                &self.snapshot.camera,
                origin,
                view,
                &mut self.diagnostics,
            );
            gfx.capture(size, &self.snapshot, &mut self.ui, &self.hud)?
        };
        let gfx = self.graphics.as_mut().expect("ensured");
        self.perf.draw(gfx.take_draw_time());
        self.diagnostics.extend(gfx.take_errors());
        if let Some(path) = path {
            gfx::save_png(&image, &self.output_path(path)?)?;
        }
        Ok(image)
    }

    /// Draws the last frame (snapshot and HUD) into `target`, `size_px`
    /// physical pixels at `pixels_per_point`, with the app's GPU side.
    ///
    /// # Errors
    ///
    /// `RE0516` when the app has no GPU side, and the renderer's errors.
    pub fn draw_into(
        &mut self,
        target: &re_render::wgpu::TextureView,
        size_px: [u32; 2],
        pixels_per_point: f32,
    ) -> Result<(), Diagnostic> {
        let gfx = self.graphics.as_mut().ok_or_else(|| {
            Diagnostic::error(
                codes::SCREENSHOT_FAILED,
                "the app has no GPU side to draw with",
            )
        })?;
        self.view_size = size_px;
        self.view_ppp = pixels_per_point;
        let drawn = if self.compat.is_some() {
            gfx.draw_compat(
                target,
                size_px,
                pixels_per_point,
                &mut self.compat_view,
                &mut self.ui,
                &self.hud,
            )
        } else {
            gfx.draw(
                target,
                size_px,
                pixels_per_point,
                &self.snapshot,
                &mut self.ui,
                &self.hud,
            )
        };
        self.perf.draw(gfx.take_draw_time());
        drawn
    }

    /// The HUD lines of the last frame.
    pub fn hud_lines(&self) -> &[HudLine] {
        &self.hud
    }

    /// Images taken for replay checks since the last call, with their requests.
    pub fn take_captures(&mut self) -> Vec<(ScreenshotRequest, Image)> {
        std::mem::take(&mut self.captures)
    }

    /// Builds the local player's view for drawing: the client's 3D frame
    /// and `passes` egui passes of its UI (the `PlayerGui`) and the HUD, for
    /// a target of `size_px` physical pixels at `pixels_per_point`.
    pub(super) fn compat_render(
        &mut self,
        size_px: [u32; 2],
        pixels_per_point: f32,
        passes: u32,
        build: bool,
    ) {
        if self.graphics.is_none() {
            return;
        }
        let ctx = self.ui.ctx().clone();
        if !self.compat_view.fonts {
            re_compat::gui::install_fonts(&ctx);
            self.compat_view.fonts = true;
        }
        let Some(c) = &self.compat else {
            return;
        };
        let roots: Vec<_> = re_compat::players::local_player_in(&c.client)
            .and_then(|p| re_compat::instance::tree::find_child(&c.client, p, "PlayerGui", false))
            .into_iter()
            .collect();
        let input = re_compat::render::RenderInput {
            viewport: glam::Vec2::new(size_px[0] as f32, size_px[1] as f32),
            top_inset: 0.0,
            dt: self.clock.dt_secs(),
        };
        let time = self.clock.tick() as f64 * f64::from(self.clock.dt_secs());
        let raw = hud::raw_input(size_px, pixels_per_point, time);
        let hud_lines = self.hud.clone();
        if let Some(cfg) = &self.compat_config {
            self.compat_view.surface_budget = cfg.gui.surface_redraws_per_frame as usize;
        }
        // Building advances effects and the camera's viewport, so it happens
        // once a frame unless the size changes.
        if build {
            let mut built = None;
            self.compat_call(ContextKind::Client, |vm, h| {
                vm.run(h, |lua| {
                    built = Some(re_compat::render::build(lua, &input)?);
                    Ok(())
                })
            });
            if let Some(mut frame) = built {
                self.diagnostics.append(&mut frame.diagnostics);
                self.compat_view.take(frame);
            }
        }
        let camera = self.compat_view.frame().map(|f| f.gui_camera);
        // The viewports need their textures before the UI pass shows them.
        if let Some(gfx) = self.graphics.as_mut()
            && let Err(d) = gfx.compat_images(&mut self.compat_view)
        {
            self.diagnostics.push(d);
        }
        let gui = re_compat::gui::GuiFrame {
            viewport: input.viewport / pixels_per_point,
            top_inset: 0.0,
            roots,
            camera,
            input: re_compat::gui::GuiInput::default(),
            images: self.compat_view.images(),
        };
        let mut ui = Vec::new();
        let mut textured = None;
        // Native roots draw around the compat GUI, in the same pass.
        let mut native = self.ui.take_frame();
        let view = View {
            size_px,
            pixels_per_point,
        };
        self.compat_call(ContextKind::Client, |vm, h| {
            vm.run(h, |lua| {
                for _ in 0..passes {
                    let mut failed = None;
                    let out = ctx.run_ui(raw.clone(), |ui| {
                        match re_compat::gui::step(lua, ui.ctx(), &gui) {
                            Ok(o) => {
                                let (world, screen) = native.shapes(ui.ctx(), view);
                                let shapes = world.into_iter().chain(o.shapes).chain(screen);
                                re_compat::gui::paint(ui.ctx(), shapes.collect());
                                hud::draw(ui.ctx(), &hud_lines);
                                textured = Some((o.surfaces, o.viewports, o.diagnostics));
                            }
                            Err(e) => failed = Some(e),
                        }
                    });
                    if let Some(e) = failed {
                        return Err(e);
                    }
                    ui.push(out);
                }
                Ok(())
            })
        });
        self.ui.put_frame(native);
        if let Some((surfaces, viewports, mut diagnostics)) = textured {
            self.diagnostics.append(&mut diagnostics);
            self.compat_view.set_textured(surfaces, viewports);
        }
        // The UI host's own passes' texture updates go first.
        if let Some(first) = ui.first_mut() {
            let mut delta = self.ui.take_pending();
            delta.append(std::mem::take(&mut first.textures_delta));
            first.textures_delta = delta;
        }
        for out in ui {
            self.compat_view.set_ui(out);
        }
    }

    /// Draws a compat game's view offscreen at `size`. This frame's view is
    /// reused when it was built at that size; otherwise one is built for the
    /// capture and the window's own is put back afterwards.
    fn capture_compat(&mut self, size: [u32; 2]) -> Result<Image, Diagnostic> {
        let same = size == self.view_size && self.compat_view.has_frame();
        let again = if same && self.view_ppp == 1.0 {
            self.compat_view.ui_again()
        } else {
            None
        };
        let window = if same {
            None
        } else {
            self.compat_view.frame().cloned()
        };
        if again.is_none() {
            // Two UI passes: egui lays text out with the fonts of the last.
            self.compat_render(size, 1.0, 2, !same);
        }
        let gfx = self.graphics.as_mut().expect("checked by the callers");
        let image = gfx.capture_compat(size, &mut self.compat_view, &mut self.ui, &self.hud);
        if let Some(ui) = again {
            self.compat_view.set_ui(ui);
        } else if self.compat_view.has_frame() && (window.is_some() || same) {
            if let Some(f) = window {
                self.compat_view.restore_frame(f);
            }
            self.compat_render(self.view_size, self.view_ppp, 1, false);
        }
        image
    }

    /// Takes this frame's requested screenshots when the app has a GPU side:
    /// script requests are written to their path, replay checks are kept for
    /// [`App::take_captures`].
    pub(super) fn take_frame_screenshots(&mut self, f: u64) {
        let due: Vec<ScreenshotRequest> = self
            .screenshots
            .iter()
            .filter(|r| r.frame == f)
            .cloned()
            .collect();
        if due.is_empty() || self.graphics.is_none() {
            return;
        }
        let size = [self.config.render.width, self.config.render.height];
        let captured = if self.compat.is_some() {
            self.capture_compat(size)
        } else {
            let gfx = self.graphics.as_mut().expect("checked");
            gfx.capture(size, &self.snapshot, &mut self.ui, &self.hud)
        };
        if let Some(gfx) = self.graphics.as_mut() {
            self.perf.draw(gfx.take_draw_time());
        }
        let image = match captured {
            Ok(i) => i,
            Err(d) => {
                self.diagnostics.push(d);
                return;
            }
        };
        for request in due {
            match &request.path {
                Some(path) => {
                    if let Err(d) = self
                        .output_path(path)
                        .and_then(|p| gfx::save_png(&image, &p))
                    {
                        self.diagnostics.push(d);
                    }
                }
                None => self.captures.push((request, image.clone())),
            }
        }
    }
}
