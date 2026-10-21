//! [`play`]: an [`App`] in a browser page, on winit's web event loop.
//!
//! The desktop driver ([`crate::run_windowed`]) blocks in winit's `run_app` and
//! opens the GPU synchronously; a browser allows neither. Here winit's
//! `spawn_app` returns at once and the page's event loop drives the frames, and
//! the GPU device arrives later from a future: until it does, the app waits in a
//! pending state and simulates nothing. The audio device opens on the first key
//! or click, because browsers start audio only after a user gesture.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use re_core::Diagnostic;
use re_core::time::Instant;
use re_input::Gamepads;
use re_render::{Gpu, wgpu};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::platform::web::{EventLoopExtWebSys, WindowAttributesExtWebSys};
use winit::window::{Window, WindowId};

use crate::app::App;
use crate::codes;
use crate::gfx::Graphics;

/// Receives what the page shows: `("running", adapter line)` once the first
/// frame is drawn, `("problem", rendered diagnostic)` for each error and
/// `("warning", rendered diagnostic)` for each other diagnostic the game or the
/// GPU reports while it keeps running (wasm has no log output), and
/// `("error", rendered diagnostic)` when the run stops.
pub type Report = fn(kind: &str, message: &str);

/// What the GPU future delivers.
type Pending = Rc<RefCell<Option<Result<(Gpu, wgpu::Surface<'static>), Diagnostic>>>>;

struct Surface {
    // Field order is drop order: the surface goes before the window.
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    window: Arc<Window>,
}

struct Runner {
    app: App,
    report: Report,
    window: Option<Arc<Window>>,
    pending: Pending,
    surface: Option<Surface>,
    gamepads: Gamepads,
    last: Option<Instant>,
    audio: bool,
    running: bool,
    stopped: bool,
}

impl Runner {
    fn stop(&mut self, event_loop: &ActiveEventLoop, d: &Diagnostic) {
        if !self.stopped {
            self.report_diagnostics();
            self.stopped = true;
            (self.report)("error", &d.to_string());
        }
        event_loop.exit();
    }

    /// Takes the GPU once its future has delivered it.
    fn take_gpu(&mut self) -> Result<(), Diagnostic> {
        let Some(delivered) = self.pending.borrow_mut().take() else {
            return Ok(());
        };
        let (gpu, surface) = delivered?;
        let window = self
            .window
            .clone()
            .expect("the window exists before its GPU");
        let size = window.inner_size();
        let config = gpu.surface_config(&surface, size.width.max(1), size.height.max(1))?;
        surface.configure(&gpu.device, &config);
        let adapter = gpu.adapter_line();
        self.app.set_graphics(Graphics::new(gpu, config.format));
        self.surface = Some(Surface {
            surface,
            config,
            window,
        });
        tracing::info!(%adapter, "canvas ready");
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

    fn redraw(&mut self) -> Result<(), Diagnostic> {
        self.take_gpu()?;
        let Some(s) = &self.surface else {
            return Ok(()); // still waiting for the GPU
        };
        let now = Instant::now();
        // A hidden tab stops animation frames; that pause is not simulated time.
        let dt = self.last.map_or(self.app.step(), |l| {
            (now - l).min(Duration::from_millis(250))
        });
        self.last = Some(now);
        self.gamepads.poll(self.app.raw_input_mut());
        self.app.frame(dt);
        let frame = match s.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Lost => {
                let size = s.window.inner_size();
                self.reconfigure(size.width, size.height);
                self.report_diagnostics();
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(Diagnostic::error(
                    codes::WEB_CANVAS_FAILED,
                    "the canvas's WebGPU surface reported a validation error",
                ));
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let size = [s.config.width, s.config.height];
        let scale = s.window.scale_factor() as f32;
        let window = s.window.clone();
        self.app.draw_into(&view, size, scale)?;
        window.pre_present_notify();
        if let Some(gfx) = self.app.graphics() {
            gfx.gpu().queue.present(frame);
            if !self.running {
                self.running = true;
                (self.report)("running", &gfx.gpu().adapter_line());
            }
        }
        self.report_diagnostics();
        Ok(())
    }

    /// Sends the game's and the GPU's diagnostics so far to the page.
    fn report_diagnostics(&mut self) {
        let gpu_errors = self.app.graphics().map(Graphics::take_errors);
        for d in self
            .app
            .take_diagnostics()
            .into_iter()
            .chain(gpu_errors.into_iter().flatten())
        {
            let kind = if d.is_error() { "problem" } else { "warning" };
            (self.report)(kind, &d.to_string());
        }
    }

    /// Opens the audio device on the first user gesture.
    fn gesture(&mut self) {
        if !self.audio {
            self.audio = true;
            self.app.set_audio(re_audio::Audio::new_or_headless());
        }
    }
}

impl ApplicationHandler for Runner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let config = self.app.config();
        let attrs = Window::default_attributes()
            .with_title(config.project.name.clone())
            .with_inner_size(winit::dpi::PhysicalSize::new(
                config.render.width,
                config.render.height,
            ))
            .with_append(true)
            .with_focusable(true);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                let d = Diagnostic::error(
                    codes::WEB_CANVAS_FAILED,
                    format!("cannot create the page's canvas: {e}"),
                );
                return self.stop(event_loop, &d);
            }
        };
        self.window = Some(window.clone());
        let pending = self.pending.clone();
        // The adapter and the device arrive on later turns of the page's event
        // loop, so they are awaited, never blocked on.
        wasm_bindgen_futures::spawn_local(async move {
            // WebGPU when the browser has it, else WebGL2. A canvas keeps the first
            // kind of context it gives, so the choice comes before the surface.
            let backends = if wgpu::util::is_browser_webgpu_supported().await {
                wgpu::Backends::BROWSER_WEBGPU
            } else {
                wgpu::Backends::GL
            };
            let gpu = Gpu::for_surface_async(window.clone(), backends)
                .await
                .map_err(|d| {
                    Diagnostic::error(
                        codes::WEB_NO_GPU,
                        format!("the browser gave no {backends:?} device for the canvas"),
                    )
                    .with_hint("use a current Chrome or Edge, served over HTTPS or from localhost")
                    .with_related(d)
                });
            *pending.borrow_mut() = Some(gpu);
            window.request_redraw();
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if self.stopped {
            return;
        }
        self.app.raw_input_mut().on_window_event(&event);
        match event {
            WindowEvent::KeyboardInput { .. } | WindowEvent::MouseInput { .. } => self.gesture(),
            WindowEvent::Resized(size) => self.reconfigure(size.width, size.height),
            WindowEvent::RedrawRequested => {
                if let Err(d) = self.redraw() {
                    return self.stop(event_loop, &d);
                }
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _el: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        self.app.raw_input_mut().on_device_event(&event);
    }
}

/// Plays `app` in a canvas appended to the page, until the page closes. Returns
/// at once: the page's event loop runs the frames. What the page should show is
/// sent to `report`.
///
/// # Errors
///
/// `RE0620` when winit's event loop cannot be created. Later failures (no
/// WebGPU device, a failing canvas) go to `report` as `"error"`.
pub fn play(app: App, report: Report) -> Result<(), Diagnostic> {
    let event_loop = EventLoop::new().map_err(|e| {
        Diagnostic::error(
            codes::WEB_NO_GPU,
            format!("cannot create the page's event loop: {e}"),
        )
    })?;
    // Frames come from the browser's animation frames (request_redraw).
    event_loop.set_control_flow(ControlFlow::Wait);
    event_loop.spawn_app(Runner {
        app,
        report,
        window: None,
        pending: Rc::default(),
        surface: None,
        gamepads: Gamepads::new(),
        last: None,
        audio: false,
        running: false,
        stopped: false,
    });
    Ok(())
}
