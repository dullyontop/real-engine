//! [`run_editor`]: the windowed editor (winit, a wgpu surface, egui).

use std::sync::Arc;
use std::time::Duration;

use re_app::{App, Graphics};
use re_core::Diagnostic;
use re_core::time::Instant;
use re_render::{Capture, Gpu, Renderer, UiFrame, wgpu};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

use crate::view::{self, GameView};

/// The time between frames when nothing else wakes the editor.
const FRAME: Duration = Duration::from_millis(16);
use crate::{Editor, EditorOptions, EditorSocket, codes};

/// Runs the editor on `app` until its window is closed (or for
/// [`EditorOptions::max_frames`] frames).
///
/// It first takes the project's single-writer lock as the editor and serves
/// `.real/editor.sock` ([`EditorSocket`]); commands that other writers send
/// are applied between frames. With [`EditorOptions::agent_commands`] it also
/// registers `re_agent`'s commands (unless [`EditorOptions::agent`] says
/// the caller did) and starts an agent session. Then it opens a window with the game view and
/// the panels. The lock is released and the socket removed when it returns.
///
/// Must run on the main thread (winit's rule).
///
/// # Errors
///
/// A [`Diagnostic`]: `RE8003` when another process holds the project,
/// `RE8801` when no window can be opened, or a GPU error (`RE6002`,
/// `RE6004`, …).
pub fn run_editor(mut app: App, mut options: EditorOptions) -> anyhow::Result<()> {
    let socket = match options.socket.take() {
        Some(socket) => socket,
        None => EditorSocket::open(app.project())?,
    };
    if options.agent_commands {
        // After the lock: the session's start checkpoint is the scene as the
        // editor opened it, before any agent edit arrives on the socket.
        let agent = match options.agent.take() {
            Some(agent) => agent,
            None => {
                let agent = re_agent::Agent::new(app.project());
                re_agent::register_commands(app.commands_mut(), &agent);
                agent
            }
        };
        agent.start_session(app.world())?;
    }
    let mut status = match socket.path() {
        Some(_) => format!(
            "editor · {}",
            re_agent::lock::socket_path(app.project()).display()
        ),
        None => "editor · no socket on this platform".to_string(),
    };
    if let Some(w) = socket.warning() {
        eprint!("{}", w.render_human());
        status.push_str(" (other writers are refused)");
    }
    let backends = match options.backends {
        Some(b) => b,
        None => re_render::backends_from_env()?,
    };
    let event_loop = EventLoop::new()
        .map_err(|e| no_display(format!("the event loop could not be created: {e}")))?;
    event_loop.set_control_flow(ControlFlow::Wait);
    let size = options
        .size
        .unwrap_or([app.config().render.width, app.config().render.height]);
    let mut editor = Editor::new();
    editor.set_status(status);
    let mut host = Host {
        app,
        socket,
        options,
        backends,
        size,
        editor,
        window: None,
        gfx: None,
        frames: 0,
        last_render_error: None,
        error: None,
    };
    event_loop
        .run_app(&mut host)
        .map_err(|e| no_display(format!("the editor's event loop failed: {e}")))?;
    if let Some(gfx) = host.gfx.as_mut() {
        gfx.view.release(&mut gfx.ui);
    }
    // The surface before the window; the app keeps the device and renderer.
    host.gfx = None;
    host.window = None;
    match host.error {
        Some(d) => Err(d.into()),
        None => Ok(()),
    }
}

fn no_display(message: String) -> Diagnostic {
    Diagnostic::error(
        codes::NO_DISPLAY,
        format!("cannot open the editor window: {message}"),
    )
    .with_hint("run it in a desktop session, or under a virtual display: xvfb-run -a real editor")
}

/// The window's side of the GPU. The device and the scene renderer belong to
/// the app ([`App::graphics`]), which uploads the World's meshes and
/// materials to them; the panels are painted by the editor's own UI renderer
/// on that device (see [`GameView`]).
struct Gfx {
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    ui: Renderer,
    view: GameView,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
}

struct Host {
    socket: EditorSocket,
    options: EditorOptions,
    backends: wgpu::Backends,
    size: [u32; 2],
    editor: Editor,
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    frames: u64,
    last_render_error: Option<String>,
    error: Option<Diagnostic>,
    // Last: the app holds the GPU device, dropped after the surface and the
    // window on every path.
    app: App,
}

impl Host {
    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Diagnostic> {
        let title = format!("{} · Real Engine editor", self.app.config().project.name);
        let attrs = Window::default_attributes()
            .with_title(title)
            .with_inner_size(winit::dpi::PhysicalSize::new(self.size[0], self.size[1]));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| no_display(format!("the window could not be created: {e}")))?,
        );
        let (gpu, surface) = Gpu::for_surface(window.clone(), self.backends)?;
        let inner = window.inner_size();
        let config = gpu.surface_config(&surface, inner.width, inner.height)?;
        surface.configure(&gpu.device, &config);
        let max_texture = gpu.device.limits().max_texture_dimension_2d as usize;
        let renderer = Renderer::new(&gpu, Capture::FORMAT);
        let ui = Renderer::new(&gpu, config.format);
        self.app.set_graphics(Graphics::from_parts(gpu, renderer));
        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(max_texture),
        );
        window.request_redraw();
        self.window = Some(window);
        self.gfx = Some(Gfx {
            surface,
            config,
            ui,
            view: GameView::new(),
            egui_ctx,
            egui_state,
        });
        Ok(())
    }

    fn reconfigure(gfx: &mut Gfx, app: &App, width: u32, height: u32) {
        let Some(graphics) = app.graphics() else {
            return;
        };
        gfx.config.width = width.max(1);
        gfx.config.height = height.max(1);
        gfx.surface.configure(&graphics.gpu().device, &gfx.config);
    }

    /// One frame: serve the socket, run the panels, draw the game view, paint
    /// the UI and present.
    fn redraw(&mut self) -> Result<(), Diagnostic> {
        let (Some(window), Some(gfx)) = (self.window.clone(), self.gfx.as_mut()) else {
            return Ok(());
        };
        let policy = &*self.options.policy;
        self.socket.serve_pending(&mut self.app, policy);
        let frame = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                let s = window.inner_size();
                Self::reconfigure(gfx, &self.app, s.width, s.height);
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(Diagnostic::error(
                    re_render::codes::SURFACE_FAILED,
                    "the window surface reported a validation error",
                ));
            }
        };
        let target = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        gfx.view.sync(&self.app, &mut gfx.ui, &mut self.editor)?;
        let input = gfx.egui_state.take_egui_input(&window);
        let editor = &mut self.editor;
        let app = &mut self.app;
        let mut output = gfx.egui_ctx.run_ui(input, |ui| editor.ui(ui, app, policy));
        let platform = std::mem::take(&mut output.platform_output);
        let ui = UiFrame::from_output(
            &gfx.egui_ctx,
            &mut output,
            [gfx.config.width, gfx.config.height],
        );
        match gfx.view.render(&mut self.app) {
            // A persistent render error is logged once, not every frame.
            Err(d) if self.last_render_error.as_ref() != Some(&d.message) => {
                self.editor.log_diagnostic(&d);
                self.last_render_error = Some(d.message);
            }
            Err(_) => {}
            Ok(_) => self.last_render_error = None,
        }
        let graphics = self.app.graphics().ok_or_else(view::no_graphics)?;
        let (device, queue) = (&graphics.gpu().device, &graphics.gpu().queue);
        view::clear(device, queue, &target);
        gfx.ui.paint_ui(device, queue, &target, &ui)?;
        for d in graphics.take_errors() {
            self.editor.log_diagnostic(&d);
        }
        gfx.egui_state.handle_platform_output(&window, platform);
        window.pre_present_notify();
        queue.present(frame);
        self.frames += 1;
        Ok(())
    }
}

impl ApplicationHandler for Host {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(d) = self.init(event_loop) {
            self.error = Some(d);
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let (Some(window), Some(gfx)) = (self.window.as_ref(), self.gfx.as_mut())
            && gfx.egui_state.on_window_event(window, &event).repaint
        {
            window.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    Self::reconfigure(gfx, &self.app, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(d) = self.redraw() {
                    self.error = Some(d);
                    event_loop.exit();
                    return;
                }
                if self.options.max_frames.is_some_and(|n| self.frames >= n) {
                    event_loop.exit();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Serve the socket here too, not only when drawing: a hidden or
        // minimized window may get no redraws, and a writer waiting on the
        // socket must not hang. Then redraw at most every ~16 ms.
        if self
            .socket
            .serve_pending(&mut self.app, &*self.options.policy)
            > 0
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
        let next = Instant::now() + FRAME;
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }

    fn new_events(&mut self, _event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        if matches!(cause, winit::event::StartCause::ResumeTimeReached { .. })
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }
}
