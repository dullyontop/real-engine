//! [`run_windowed`]: plays an [`App`] in a window, with keyboard, mouse and
//! gamepad input, the audio device and the HUD.

use std::sync::Arc;
use std::time::Duration;

use re_core::Diagnostic;
use re_core::time::Instant;
use re_input::Gamepads;
use re_render::{Gpu, wgpu};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::app::App;
use crate::codes;
use crate::gfx::{self, Graphics};

/// How [`run_windowed`] runs.
#[derive(Clone, Debug)]
pub struct WindowOptions {
    /// Stop after this many presented frames (`None`: until the window closes).
    pub frames: Option<u64>,
    /// Give up when no frame is presented for this long (a stuck display).
    pub stall_timeout: Duration,
}

impl Default for WindowOptions {
    fn default() -> Self {
        WindowOptions {
            frames: None,
            stall_timeout: Duration::from_secs(30),
        }
    }
}

/// What [`run_windowed`] returns.
#[derive(Debug, Default)]
pub struct WindowReport {
    /// Frames simulated and presented.
    pub frames: u64,
    /// Frames the surface could not give a texture for (resizes, occlusion).
    pub skipped: u64,
    /// The adapter line (`name (backend, type, driver)`).
    pub adapter: String,
    /// Everything reported during the run.
    pub diagnostics: Vec<Diagnostic>,
}

impl WindowReport {
    /// True when no error was reported.
    pub fn ok(&self) -> bool {
        !self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

struct Surface {
    // Field order is drop order: the surface goes before the window.
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    window: Arc<Window>,
}

struct Runner<'a> {
    app: &'a mut App,
    options: WindowOptions,
    surface: Option<Surface>,
    gamepads: Gamepads,
    last: Option<Instant>,
    last_present: Instant,
    report: WindowReport,
    fatal: Option<Diagnostic>,
}

/// The largest window icon handed to the OS, in pixels across.
const ICON_SIZE: u32 = 256;

/// The window icon from `[package] icon` (ADR-0037), shrunk to at most
/// [`ICON_SIZE`] pixels. A missing or unreadable icon only logs a warning:
/// `real build` reports it as an error. macOS ignores it (an app's icon is
/// its bundle's).
fn window_icon(app: &App) -> Option<winit::window::Icon> {
    let path = app.config().package.as_ref()?.icon.clone()?;
    let icon = app
        .files()
        .read_source(&path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| {
            re_assets::decode_image(
                &bytes,
                re_types::render::TextureFormat::Rgba8Srgb,
                "the window icon",
                &path,
            )
            .map_err(|d| d.message)
        })
        .and_then(|image| {
            let (width, height, pixels) =
                shrink(image.width, image.height, &image.pixels, ICON_SIZE);
            winit::window::Icon::from_rgba(pixels, width, height).map_err(|e| e.to_string())
        });
    match icon {
        Ok(icon) => Some(icon),
        Err(why) => {
            tracing::warn!(icon = %path, "the window icon is not set: {why}");
            None
        }
    }
}

/// RGBA8 `pixels` (`width` × `height`) shrunk by a whole factor until no
/// side is over `max`: each pixel is the mean of its block, colours weighted
/// by alpha.
fn shrink(width: u32, height: u32, pixels: &[u8], max: u32) -> (u32, u32, Vec<u8>) {
    let k = width.max(height).div_ceil(max).max(1);
    if k == 1 {
        return (width, height, pixels.to_vec());
    }
    let (w, h) = ((width / k).max(1), (height / k).max(1));
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for oy in 0..h {
        for ox in 0..w {
            let mut sum = [0u64; 4];
            let mut count = 0u64;
            for y in oy * k..((oy + 1) * k).min(height) {
                for x in ox * k..((ox + 1) * k).min(width) {
                    let i = ((y * width + x) * 4) as usize;
                    let a = u64::from(pixels[i + 3]);
                    for c in 0..3 {
                        sum[c] += u64::from(pixels[i + c]) * a;
                    }
                    sum[3] += a;
                    count += 1;
                }
            }
            for c in 0..3 {
                let mean = (sum[c] + sum[3] / 2).checked_div(sum[3]).unwrap_or(0);
                out.push(mean as u8);
            }
            out.push(((sum[3] + count / 2) / count.max(1)) as u8);
        }
    }
    (w, h, out)
}

fn window_failed(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::WINDOW_FAILED, message).with_hint(
        "run with --headless, or under a virtual display: xvfb-run -a -s \"-screen 0 1280x720x24\" real run",
    )
}

impl Runner<'_> {
    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Diagnostic> {
        let config = self.app.config();
        let attrs = Window::default_attributes()
            .with_title(format!("{} - Real Engine", config.project.name))
            .with_inner_size(winit::dpi::PhysicalSize::new(
                config.render.width,
                config.render.height,
            ))
            .with_window_icon(window_icon(self.app));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| window_failed(format!("cannot create the window: {e}")))?,
        );
        let backends = gfx::backends(&config.render.backends)?;
        let (gpu, surface) = Gpu::for_surface(window.clone(), backends).map_err(|d| {
            window_failed("cannot open a GPU surface for the window").with_related(d)
        })?;
        let size = window.inner_size();
        let surface_config = gpu.surface_config(&surface, size.width.max(1), size.height.max(1))?;
        surface.configure(&gpu.device, &surface_config);
        self.report.adapter = gpu.adapter_line();
        tracing::info!(adapter = %self.report.adapter, "window opened");
        self.app
            .set_graphics(Graphics::new(gpu, surface_config.format));
        window.request_redraw();
        self.surface = Some(Surface {
            surface,
            config: surface_config,
            window,
        });
        self.last_present = Instant::now();
        Ok(())
    }

    fn reconfigure(&mut self, width: u32, height: u32) {
        let (Some(s), Some(gfx)) = (self.surface.as_mut(), self.app.graphics()) else {
            return;
        };
        s.config.width = width.max(1);
        s.config.height = height.max(1);
        s.surface.configure(&gfx.gpu().device, &s.config);
    }

    fn redraw(&mut self) {
        let now = Instant::now();
        // A long pause (a dragged window, a breakpoint) is not simulated time.
        let dt = self.last.map_or(self.app.step(), |l| {
            (now - l).min(Duration::from_millis(250))
        });
        self.last = Some(now);
        self.gamepads.poll(self.app.raw_input_mut());
        self.app.frame(dt);

        let Some(s) = &self.surface else {
            return;
        };
        if let Some((lock, icon)) = self.app.take_mouse_behavior() {
            apply_mouse(&s.window, lock, icon);
        }
        let frame = match s.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                // A minimised or hidden window is not a stuck display.
                self.report.skipped += 1;
                self.last_present = Instant::now();
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.report.skipped += 1;
                let size = s.window.inner_size();
                self.reconfigure(size.width, size.height);
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.fatal = Some(window_failed(
                    "the window surface reported a validation error",
                ));
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = [s.config.width, s.config.height];
        let scale = s.window.scale_factor() as f32;
        let window = s.window.clone();
        if let Err(d) = self.app.draw_into(&view, size, scale) {
            self.fatal = Some(d);
            return;
        }
        window.pre_present_notify();
        if let Some(gfx) = self.app.graphics() {
            gfx.gpu().queue.present(frame);
        }
        self.report.frames += 1;
        self.last_present = Instant::now();
    }

    fn done(&self) -> bool {
        self.fatal.is_some()
            || self.app.stopped()
            || self.options.frames.is_some_and(|n| self.report.frames >= n)
    }
}

impl ApplicationHandler for Runner<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.surface.is_some() {
            return;
        }
        if let Err(d) = self.init(event_loop) {
            self.fatal = Some(d);
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        self.app.raw_input_mut().on_window_event(&event);
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => self.reconfigure(size.width, size.height),
            WindowEvent::RedrawRequested => {
                self.redraw();
                if self.done() {
                    event_loop.exit();
                } else if let Some(s) = &self.surface {
                    s.window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        self.app.raw_input_mut().on_device_event(&event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.last_present.elapsed() > self.options.stall_timeout {
            self.fatal = Some(window_failed(format!(
                "no frame was presented for {:?} (after {} frames)",
                self.options.stall_timeout, self.report.frames
            )));
            event_loop.exit();
        }
    }
}

/// Plays `app` in a window until it closes or `options.frames` frames were
/// presented. Opens the audio device (falling back to silence) and drives the
/// frame loop from the display's refresh, with real frame times.
///
/// # Errors
///
/// `RE0521` when the event loop, the window or its GPU surface cannot be
/// created, or the surface fails while running. Errors reported by the frames
/// themselves are in the report instead.
pub fn run_windowed(app: &mut App, options: WindowOptions) -> Result<WindowReport, Diagnostic> {
    let event_loop = EventLoop::new()
        .map_err(|e| window_failed(format!("cannot create the event loop (no display?): {e}")))?;
    event_loop.set_control_flow(ControlFlow::Poll);
    app.set_audio(re_audio::Audio::new_or_headless());
    let mut runner = Runner {
        app,
        options,
        surface: None,
        gamepads: Gamepads::new(),
        last: None,
        last_present: Instant::now(),
        report: WindowReport::default(),
        fatal: None,
    };
    event_loop
        .run_app(&mut runner)
        .map_err(|e| window_failed(format!("the event loop failed: {e}")))?;
    if let Some(d) = runner.fatal.take() {
        return Err(d);
    }
    // The surface must go before the device that the app's graphics own.
    runner.surface = None;
    let mut report = std::mem::take(&mut runner.report);
    report.diagnostics = runner.app.take_diagnostics();
    Ok(report)
}

/// Holds or frees the cursor and shows or hides it, as a compat script asked
/// (`UserInputService.MouseBehavior`). A platform that cannot hold the cursor
/// in place falls back to confining it, then to leaving it free.
fn apply_mouse(window: &winit::window::Window, lock: re_types::input::MouseLock, icon: bool) {
    use re_types::input::MouseLock;
    use winit::window::CursorGrabMode;
    let grab = match lock {
        MouseLock::Free => CursorGrabMode::None,
        MouseLock::LockCenter | MouseLock::LockCurrentPosition => CursorGrabMode::Locked,
    };
    if window.set_cursor_grab(grab).is_err() && grab == CursorGrabMode::Locked {
        let _ = window.set_cursor_grab(CursorGrabMode::Confined);
    }
    window.set_cursor_visible(icon);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_icons_shrink_by_a_whole_factor() {
        // 4×2 pixels: left half opaque red, right half transparent.
        let mut pixels = Vec::new();
        for _ in 0..2 {
            pixels.extend([255, 0, 0, 255, 255, 0, 0, 255, 0, 0, 255, 0, 0, 0, 255, 0]);
        }
        assert_eq!(shrink(4, 2, &pixels, 4), (4, 2, pixels.clone()));
        let (w, h, out) = shrink(4, 2, &pixels, 2);
        assert_eq!((w, h), (2, 1));
        // A transparent pixel's colour does not tint its block.
        assert_eq!(out, [255, 0, 0, 255, 0, 0, 0, 0]);
        let (w, h, out) = shrink(1024, 3, &vec![200; 1024 * 3 * 4], ICON_SIZE);
        assert_eq!((w, h, out.len()), (256, 1, 256 * 4));
        assert!(out.iter().all(|&v| v == 200));
    }
}
