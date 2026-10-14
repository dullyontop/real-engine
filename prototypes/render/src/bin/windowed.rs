//! Windowed path: winit 0.30 `ApplicationHandler` + wgpu 30 surface + the lit
//! scene + an egui overlay (egui-winit input, egui-wgpu rendering). Renders
//! `--frames N` frames, then exits and prints a report (surface capabilities,
//! chosen configuration, per-frame `get_current_texture` outcomes, timings).
//!
//! Usage: windowed [--backends vulkan|gl] [--frames N] [--size WxH]
//! Run headless with: xvfb-run -a -s "-screen 0 1280x720x24" windowed

use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use render_spike::gpu::{adapter_line, install_error_logger, request_device};
use render_spike::gui::{DemoState, GuiRenderer};
use render_spike::scene::{SceneRenderer, create_depth_view};
use render_spike::timing::summarize_ms;
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

struct Gfx {
    // Field order matters for drop order: the surface must be dropped before the window.
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    depth_view: wgpu::TextureView,
    scene: SceneRenderer,
    gui: GuiRenderer,
    egui_state: egui_winit::State,
}

struct App {
    backends: wgpu::Backends,
    max_frames: u32,
    present_mode: wgpu::PresentMode,
    /// Keep the window mapped this long after the last frame (for screen capture).
    hold: Duration,
    done_at: Option<Instant>,
    size: (u32, u32),
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    demo: DemoState,
    presented: u32,
    outcomes: Vec<String>,
    frame_times: Vec<Duration>,
    started: Option<Instant>,
    error: Option<String>,
}

impl App {
    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let attrs = Window::default_attributes()
            .with_title("Real Engine render spike")
            .with_inner_size(winit::dpi::PhysicalSize::new(self.size.0, self.size.1));
        let t = Instant::now();
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| format!("create_window: {e}"))?,
        );
        println!(
            "window: created in {:?}, inner_size={:?}, scale_factor={}",
            t.elapsed(),
            window.inner_size(),
            window.scale_factor()
        );

        // wgpu 29+: an instance used for presentation gets the display handle up front
        // (required for GLES/EGL on Wayland; unused by Vulkan/Metal/DX12 today).
        let mut desc = wgpu::InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        ));
        desc.backends = self.backends;
        let instance = wgpu::Instance::new(desc);

        let t = Instant::now();
        // Arc<Window> is 'static, so the surface is Surface<'static> and can live in App.
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| format!("create_surface: {e}"))?;
        println!("surface: created in {:?}", t.elapsed());

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
            apply_limit_buckets: false,
        }))
        .map_err(|e| format!("request_adapter(compatible_surface): {e}"))?;
        println!("adapter: {}", adapter_line(&adapter.get_info()));
        let (device, queue) = pollster::block_on(request_device(&adapter, wgpu::Features::empty()))
            .map_err(|e| format!("request_device: {e}"))?;
        install_error_logger(&device);

        let caps = surface.get_capabilities(&adapter);
        println!("surface caps: formats={:?}", caps.formats);
        println!("surface caps: present_modes={:?}", caps.present_modes);
        println!(
            "surface caps: alpha_modes={:?} usages={:?}",
            caps.alpha_modes, caps.usages
        );
        for fc in &caps.format_capabilities {
            println!("surface caps: format_capabilities {fc:?}");
        }
        println!("display_hdr_info: {:?}", surface.display_hdr_info(&adapter));

        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let size = window.inner_size();
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto, // new in wgpu 30
            width: size.width.max(1),
            height: size.height.max(1),
            // Fifo is the only mode guaranteed everywhere; others are opt-in if supported.
            present_mode: if caps.present_modes.contains(&self.present_mode) {
                self.present_mode
            } else {
                println!(
                    "present mode {:?} unsupported here, falling back to Fifo",
                    self.present_mode
                );
                wgpu::PresentMode::Fifo
            },
            desired_maximum_frame_latency: 2,
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
        };
        surface.configure(&device, &config);
        println!("surface configured: {config:?}");
        println!(
            "default config would be: {:?}",
            surface.get_default_config(&adapter, config.width, config.height)
        );

        let depth_view = create_depth_view(&device, config.width, config.height);
        let scene = SceneRenderer::new(&device, format);
        let gui = GuiRenderer::new(&device, format);
        let egui_state = egui_winit::State::new(
            gui.ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        self.demo.adapter_line = format!(
            "{} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );
        window.request_redraw();
        self.window = Some(window);
        self.gfx = Some(Gfx {
            surface,
            device,
            queue,
            config,
            depth_view,
            scene,
            gui,
            egui_state,
        });
        self.started = Some(Instant::now());
        Ok(())
    }

    fn reconfigure(gfx: &mut Gfx, width: u32, height: u32) {
        gfx.config.width = width.max(1);
        gfx.config.height = height.max(1);
        gfx.surface.configure(&gfx.device, &gfx.config);
        gfx.depth_view = create_depth_view(&gfx.device, gfx.config.width, gfx.config.height);
    }

    fn redraw(&mut self) {
        let (Some(window), Some(gfx)) = (self.window.clone(), self.gfx.as_mut()) else {
            return;
        };
        let t = Instant::now();
        // wgpu 29+: get_current_texture returns an enum instead of Result<_, SurfaceError>.
        let frame = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => {
                self.outcomes.push("Success".into());
                frame
            }
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.outcomes.push("Suboptimal".into());
                frame // still presentable; reconfigure after presenting
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.outcomes.push("Timeout/Occluded (skipped)".into());
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.outcomes.push("Outdated/Lost (reconfigured)".into());
                let s = window.inner_size();
                Self::reconfigure(gfx, s.width, s.height);
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.outcomes.push("Validation".into());
                self.error = Some("get_current_texture: validation error".into());
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let (w, h) = (gfx.config.width, gfx.config.height);

        self.demo.frame += 1;
        gfx.scene.update(
            &gfx.queue,
            w as f32 / h as f32,
            (30.0 + self.demo.frame as f32 * 3.0).to_radians(),
        );
        let mut encoder = gfx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame"),
            });
        gfx.scene.encode(&mut encoder, &view, &gfx.depth_view, None);

        let raw_input = gfx.egui_state.take_egui_input(&window);
        let demo = &mut self.demo;
        let (user_cmds, platform_output, _stats) = gfx.gui.render(
            &gfx.device,
            &gfx.queue,
            &mut encoder,
            &view,
            [w, h],
            raw_input,
            wgpu::LoadOp::Load, // overlay on top of the 3D scene
            |ui| {
                egui::Window::new("Real Engine")
                    .default_pos([12.0, 12.0])
                    .show(ui.ctx(), |ui| {
                        ui.label(format!("frame {}", demo.frame));
                        ui.label(&demo.adapter_line);
                        ui.add(egui::Slider::new(&mut demo.cube_yaw_deg, 0.0..=360.0).text("yaw"));
                    });
            },
        );
        gfx.egui_state
            .handle_platform_output(&window, platform_output);
        gfx.queue
            .submit(user_cmds.into_iter().chain([encoder.finish()]));
        window.pre_present_notify();
        // wgpu 30: Queue::present replaces SurfaceTexture::present.
        gfx.queue.present(frame);
        self.presented += 1;
        self.frame_times.push(t.elapsed());
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.init(event_loop) {
            eprintln!("INIT FAILED: {e}");
            self.error = Some(e);
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let (Some(window), Some(gfx)) = (self.window.as_ref(), self.gfx.as_mut()) {
            let _response = gfx.egui_state.on_window_event(window, &event);
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    Self::reconfigure(gfx, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                if self.done_at.is_some() {
                    return; // holding the last presented frame on screen
                }
                self.redraw();
                if self.error.is_some() {
                    event_loop.exit();
                } else if self.presented >= self.max_frames {
                    self.done_at = Some(Instant::now());
                    println!("last frame presented; holding window for {:?}", self.hold);
                    event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + self.hold));
                } else if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(done) = self.done_at
            && done.elapsed() >= self.hold
        {
            event_loop.exit();
            return;
        }
        // Safety net so a display that never delivers RedrawRequested cannot hang CI.
        if let Some(start) = self.started
            && start.elapsed() > Duration::from_secs(60)
        {
            self.error = Some(format!("timeout after {} presented frames", self.presented));
            event_loop.exit();
        }
    }
}

fn arg<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().collect();
    let backends = arg(&args, "--backends")
        .map(|s| render_spike::parse_backends(s).expect("unknown backend"))
        .unwrap_or(wgpu::Backends::PRIMARY | wgpu::Backends::GL);
    let max_frames = arg(&args, "--frames")
        .map(|s| s.parse().unwrap())
        .unwrap_or(10);
    let present_mode = match arg(&args, "--present-mode").unwrap_or("fifo") {
        "fifo" => wgpu::PresentMode::Fifo,
        "immediate" => wgpu::PresentMode::Immediate,
        "mailbox" => wgpu::PresentMode::Mailbox,
        "auto-no-vsync" => wgpu::PresentMode::AutoNoVsync,
        other => panic!("unknown present mode {other}"),
    };
    let hold = Duration::from_millis(
        arg(&args, "--hold-ms")
            .map(|s| s.parse().unwrap())
            .unwrap_or(0),
    );
    let size = arg(&args, "--size")
        .map(|s| {
            let (w, h) = s.split_once('x').unwrap();
            (w.parse().unwrap(), h.parse().unwrap())
        })
        .unwrap_or((800, 600));
    println!(
        "env: DISPLAY={:?} WAYLAND_DISPLAY={:?} XDG_RUNTIME_DIR={:?}",
        std::env::var("DISPLAY").ok(),
        std::env::var("WAYLAND_DISPLAY").ok(),
        std::env::var("XDG_RUNTIME_DIR").ok()
    );

    let event_loop = match EventLoop::new() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("EventLoop::new failed: {e}");
            return ExitCode::from(2);
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        backends,
        max_frames,
        present_mode,
        hold,
        done_at: None,
        size,
        window: None,
        gfx: None,
        demo: DemoState::default(),
        presented: 0,
        outcomes: Vec::new(),
        frame_times: Vec::new(),
        started: None,
        error: None,
    };
    let t = Instant::now();
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("run_app failed: {e}");
        return ExitCode::from(2);
    }
    println!("event loop ran {:?}", t.elapsed());
    if let (Some(start), Some(done)) = (app.started, app.done_at) {
        let secs = done.duration_since(start).as_secs_f64();
        println!(
            "presenting phase: {} frames in {:.3}s = {:.1} frames/s (wall clock, includes present pacing)",
            app.presented,
            secs,
            f64::from(app.presented) / secs
        );
    }
    println!("presented frames: {}", app.presented);
    let mut counts = std::collections::BTreeMap::new();
    for o in &app.outcomes {
        *counts.entry(o.as_str()).or_insert(0) += 1;
    }
    println!("get_current_texture outcomes: {counts:?}");
    println!(
        "frame (acquire+encode scene+egui+submit+present): {}",
        summarize_ms(&app.frame_times)
    );
    // Drop GPU objects (surface first) before the window.
    app.gfx = None;
    app.window = None;
    match app.error {
        None if app.presented >= max_frames => {
            println!("WINDOWED OK");
            ExitCode::SUCCESS
        }
        None => {
            println!("WINDOWED INCOMPLETE");
            ExitCode::from(1)
        }
        Some(e) => {
            println!("WINDOWED FAILED: {e}");
            ExitCode::from(1)
        }
    }
}
