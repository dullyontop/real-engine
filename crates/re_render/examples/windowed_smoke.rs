//! Windowed smoke test: a winit window, a wgpu surface, a lit and shadowed scene
//! drawn by `re_render`, and an egui overlay.
//!
//! It presents `--frames N` frames and prints `WINDOWED OK` when every one was
//! presented and no GPU error was reported. A winit event loop can only be
//! created once per process and only on the main thread, which is why this is a
//! binary and not a `#[test]`. Without a display, run it under a virtual X server:
//!
//! ```sh
//! cargo build -p re_render --example windowed_smoke
//! xvfb-run -a -s "-screen 0 1280x720x24" target/debug/examples/windowed_smoke --frames 120
//! ```
//!
//! Options: `--frames N` (default 120), `--backends vulkan|gl|primary|all`
//! (default `RE_BACKENDS`, else the platform's primary API), `--size WxH`.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use glam::{Mat4, Quat, Vec3};
use re_core::time::Instant;
use re_render::{Gpu, Renderer, UiFrame};
use re_types::EntityId;
use re_types::mesh::{MeshData, Vertex};
use re_types::render::{
    Camera, DrawFlags, DrawItem, Light, LightKind, MaterialDesc, MaterialHandle, MeshHandle,
    RenderSnapshot,
};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId};

/// Gives up when the display stops delivering frames.
const TIMEOUT: Duration = Duration::from_secs(120);

struct Scene {
    cube: MeshHandle,
    plane: MeshHandle,
    orange: MaterialHandle,
    ground: MaterialHandle,
}

struct Gfx {
    // Field order is drop order: the surface goes before the GPU and the window.
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    renderer: Renderer,
    scene: Scene,
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    gpu: Gpu,
}

struct App {
    backends: wgpu::Backends,
    max_frames: u32,
    size: (u32, u32),
    window: Option<Arc<Window>>,
    gfx: Option<Gfx>,
    presented: u32,
    skipped: u32,
    frame_times: Vec<Duration>,
    started: Option<Instant>,
    error: Option<String>,
}

fn quad(mesh: &mut MeshData, c: Vec3, u: Vec3, v: Vec3) {
    let n = u.cross(v).normalize().to_array();
    let base = mesh.vertices.len() as u32;
    for (p, uv) in [
        (c - u - v, [0.0, 1.0]),
        (c + u - v, [1.0, 1.0]),
        (c + u + v, [1.0, 0.0]),
        (c - u + v, [0.0, 0.0]),
    ] {
        mesh.vertices.push(Vertex {
            pos: p.to_array(),
            normal: n,
            uv,
            color: [1.0; 4],
        });
    }
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

fn cube() -> MeshData {
    let mut mesh = MeshData::default();
    for (n, u, v) in [
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
    ] {
        quad(&mut mesh, n * 0.5, u * 0.5, v * 0.5);
    }
    mesh
}

impl App {
    fn init(&mut self, event_loop: &ActiveEventLoop) -> Result<(), String> {
        let attrs = Window::default_attributes()
            .with_title("re_render windowed smoke")
            .with_inner_size(winit::dpi::PhysicalSize::new(self.size.0, self.size.1));
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| format!("create_window: {e}"))?,
        );
        let (gpu, surface) =
            Gpu::for_surface(window.clone(), self.backends).map_err(|d| d.render_human())?;
        println!("adapter: {}", gpu.adapter_line());
        let size = window.inner_size();
        let config = gpu
            .surface_config(&surface, size.width, size.height)
            .map_err(|d| d.render_human())?;
        surface.configure(&gpu.device, &config);
        println!(
            "surface: {:?} {}x{} {:?}",
            config.format, config.width, config.height, config.present_mode
        );

        let mut renderer = Renderer::new(&gpu, config.format);
        let upload =
            |r: &mut Renderer, m: &MeshData| r.upload_mesh(m).map_err(|d| d.render_human());
        let material = |r: &mut Renderer, c: [f32; 4]| {
            r.create_material(&MaterialDesc {
                base_color: c,
                ..MaterialDesc::default()
            })
            .map_err(|d| d.render_human())
        };
        let mut plane = MeshData::default();
        quad(&mut plane, Vec3::ZERO, Vec3::X * 4.0, Vec3::NEG_Z * 4.0);
        let scene = Scene {
            cube: upload(&mut renderer, &cube())?,
            plane: upload(&mut renderer, &plane)?,
            orange: material(&mut renderer, [0.95, 0.42, 0.12, 1.0])?,
            ground: material(&mut renderer, [0.55, 0.6, 0.55, 1.0])?,
        };
        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(gpu.device.limits().max_texture_dimension_2d as usize),
        );
        window.request_redraw();
        self.window = Some(window);
        self.gfx = Some(Gfx {
            surface,
            config,
            renderer,
            scene,
            egui_ctx,
            egui_state,
            gpu,
        });
        self.started = Some(Instant::now());
        Ok(())
    }

    fn reconfigure(gfx: &mut Gfx, width: u32, height: u32) {
        gfx.config.width = width.max(1);
        gfx.config.height = height.max(1);
        gfx.surface.configure(&gfx.gpu.device, &gfx.config);
    }

    fn snapshot(scene: &Scene, frame: u32) -> RenderSnapshot {
        let eye = Vec3::new(2.6, 2.2, 3.4);
        let angle = (30.0 + frame as f32 * 3.0).to_radians();
        let shadowed = DrawFlags::CAST_SHADOWS | DrawFlags::RECEIVE_SHADOWS;
        RenderSnapshot {
            camera: Camera {
                world: glam::camera::rh::view::look_at_mat4(eye, Vec3::new(0.0, 0.5, 0.0), Vec3::Y)
                    .inverse(),
                fov_y_radians: 45f32.to_radians(),
                near: 0.1,
                far: 100.0,
            },
            lights: vec![
                Light {
                    kind: LightKind::Directional,
                    direction: Vec3::new(-0.45, -1.0, -0.65).normalize(),
                    intensity: 1.0,
                    shadows: true,
                    ..Light::default()
                },
                Light {
                    position: Vec3::new(-1.2, 1.4, 1.6),
                    color: [1.0, 0.55, 0.25],
                    intensity: 40.0,
                    range: 6.0,
                    ..Light::default()
                },
            ],
            items: vec![
                DrawItem {
                    entity: EntityId(1),
                    mesh: scene.cube,
                    material: scene.orange,
                    world: Mat4::from_rotation_translation(
                        Quat::from_rotation_y(angle),
                        Vec3::new(0.0, 0.5, 0.0),
                    ),
                    flags: shadowed,
                },
                DrawItem {
                    entity: EntityId(2),
                    mesh: scene.plane,
                    material: scene.ground,
                    world: Mat4::IDENTITY,
                    flags: shadowed,
                },
            ],
            debug_lines: Vec::new(),
            clear: [0.02, 0.03, 0.06, 1.0],
            ..Default::default()
        }
    }

    fn redraw(&mut self) {
        let (Some(window), Some(gfx)) = (self.window.clone(), self.gfx.as_mut()) else {
            return;
        };
        let t = Instant::now();
        let frame = match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.skipped += 1;
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.skipped += 1;
                let s = window.inner_size();
                Self::reconfigure(gfx, s.width, s.height);
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                self.error = Some("get_current_texture: validation error".into());
                return;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let snapshot = Self::snapshot(&gfx.scene, self.presented);
        if let Err(d) = gfx
            .renderer
            .render(&gfx.gpu.device, &gfx.gpu.queue, &view, &snapshot)
        {
            self.error = Some(d.render_human());
            return;
        }
        let raw_input = gfx.egui_state.take_egui_input(&window);
        let presented = self.presented;
        let adapter = gfx.gpu.adapter_line();
        let mut output = gfx.egui_ctx.run_ui(raw_input, |ui| {
            egui::Window::new("re_render")
                .default_pos([12.0, 12.0])
                .show(ui.ctx(), |ui| {
                    ui.label(format!("frame {presented}"));
                    ui.label(&adapter);
                });
        });
        let platform_output = std::mem::take(&mut output.platform_output);
        let ui = UiFrame::from_output(
            &gfx.egui_ctx,
            &mut output,
            [gfx.config.width, gfx.config.height],
        );
        if let Err(d) = gfx
            .renderer
            .paint_ui(&gfx.gpu.device, &gfx.gpu.queue, &view, &ui)
        {
            self.error = Some(d.render_human());
            return;
        }
        gfx.egui_state
            .handle_platform_output(&window, platform_output);
        window.pre_present_notify();
        gfx.gpu.queue.present(frame);
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
            self.error = Some(e);
            event_loop.exit();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        if let (Some(window), Some(gfx)) = (self.window.as_ref(), self.gfx.as_mut()) {
            let _ = gfx.egui_state.on_window_event(window, &event);
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    Self::reconfigure(gfx, size.width, size.height);
                }
            }
            WindowEvent::RedrawRequested => {
                self.redraw();
                if self.error.is_some() || self.presented >= self.max_frames {
                    event_loop.exit();
                } else if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.started.is_some_and(|s| s.elapsed() > TIMEOUT) {
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
    let args: Vec<String> = std::env::args().collect();
    let backends = match arg(&args, "--backends") {
        Some(name) => match re_render::parse_backends(name) {
            Some(b) => b,
            None => {
                eprintln!("unknown backend `{name}`");
                return ExitCode::from(2);
            }
        },
        None => match re_render::backends_from_env() {
            Ok(b) => b,
            Err(d) => {
                eprint!("{}", d.render_human());
                return ExitCode::from(2);
            }
        },
    };
    let max_frames = match arg(&args, "--frames").map(str::parse) {
        None => 120,
        Some(Ok(n)) => n,
        Some(Err(e)) => {
            eprintln!("--frames: {e}");
            return ExitCode::from(2);
        }
    };
    let size = arg(&args, "--size")
        .and_then(|s| s.split_once('x'))
        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
        .unwrap_or((800, 600));

    let event_loop = match EventLoop::new() {
        Ok(el) => el,
        Err(e) => {
            eprintln!("cannot create the event loop (no display? run under xvfb-run): {e}");
            return ExitCode::from(2);
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    let mut app = App {
        backends,
        max_frames,
        size,
        window: None,
        gfx: None,
        presented: 0,
        skipped: 0,
        frame_times: Vec::new(),
        started: None,
        error: None,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("run_app failed: {e}");
        return ExitCode::from(2);
    }
    let gpu_errors = app.gfx.as_ref().map_or(0, |g| g.gpu.errors().count());
    if let Some(gfx) = &app.gfx {
        for d in gfx.gpu.errors().take() {
            eprint!("{}", d.render_human());
        }
    }
    let mut times = app.frame_times.clone();
    times.sort();
    if let (Some(first), Some(median)) = (times.first(), times.get(times.len() / 2)) {
        println!(
            "frames: {} presented, {} skipped; frame time min {first:?}, median {median:?}",
            app.presented, app.skipped
        );
    }
    // Drop GPU objects (the surface first) before the window.
    app.gfx = None;
    app.window = None;
    match app.error {
        None if app.presented >= max_frames && gpu_errors == 0 => {
            println!("WINDOWED OK");
            ExitCode::SUCCESS
        }
        None if gpu_errors > 0 => {
            println!("WINDOWED FAILED: {gpu_errors} GPU errors");
            ExitCode::from(1)
        }
        None => {
            println!(
                "WINDOWED INCOMPLETE: {} of {max_frames} frames",
                app.presented
            );
            ExitCode::from(1)
        }
        Some(e) => {
            println!("WINDOWED FAILED: {e}");
            ExitCode::from(1)
        }
    }
}
