//! Headless GUI: one egui frame (editor-style panels) rendered through
//! egui-wgpu into an offscreen texture, with the lit 3D scene rendered first into
//! its own texture and shown in the central panel via `register_native_texture`
//! (the editor-viewport composition path). Writes a PNG and checks it.
//!
//! Usage: gui_offscreen [--backends vulkan|gl] [--out PATH] [--ppp F] [--frames N]

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use render_spike::gpu::{HeadlessGpu, HeadlessOptions, adapter_line};
use render_spike::gui::{DemoState, GuiRenderer, demo_ui};
use render_spike::readback::{image_stats, read_texture, write_png_rgba8};
use render_spike::scene::{OFFSCREEN_COLOR_FORMAT, OffscreenTarget, SceneRenderer};
use render_spike::timing::summarize_ms;

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
        .unwrap_or(wgpu::Backends::VULKAN | wgpu::Backends::GL);
    let out = PathBuf::from(arg(&args, "--out").unwrap_or("out/gui-offscreen.png"));
    let ppp: f32 = arg(&args, "--ppp")
        .map(|s| s.parse().unwrap())
        .unwrap_or(1.0);
    let frames: usize = arg(&args, "--frames")
        .map(|s| s.parse().unwrap())
        .unwrap_or(20);
    let (width, height) = (1024u32, 640u32);

    let gpu = match HeadlessGpu::new_blocking(&HeadlessOptions {
        backends,
        ..Default::default()
    }) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SETUP FAILED: {e}");
            return ExitCode::from(2);
        }
    };
    let (device, queue) = (&gpu.device, &gpu.queue);
    let info = gpu.adapter.get_info();
    println!("adapter: {}", adapter_line(&info));

    // 1) Engine viewport: render the 3D scene into its own sRGB texture.
    let view_formats_ok = gpu
        .adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::VIEW_FORMATS);
    let viewport =
        OffscreenTarget::with_options(device, 480, 320, OFFSCREEN_COLOR_FORMAT, view_formats_ok);
    let scene = SceneRenderer::new(device, OFFSCREEN_COLOR_FORMAT);
    scene.update(queue, viewport.aspect(), 30f32.to_radians());
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("viewport"),
    });
    scene.encode(
        &mut encoder,
        &viewport.color_view,
        &viewport.depth_view,
        None,
    );
    queue.submit([encoder.finish()]);

    // 2) GUI renderer targeting a gamma-space Rgba8Unorm texture (egui's preferred format).
    let gui_format = wgpu::TextureFormat::Rgba8Unorm;
    let t = Instant::now();
    let mut gui = GuiRenderer::new(device, gui_format);
    println!("egui_wgpu::Renderer::new = {:?}", t.elapsed());
    // egui samples user textures as gamma-encoded bytes, so hand it an Rgba8Unorm
    // view of the sRGB scene texture (needs DownlevelFlags::VIEW_FORMATS).
    let egui_view = if view_formats_ok {
        viewport.non_srgb_color_view()
    } else {
        println!(
            "NOTE: no DownlevelFlags::VIEW_FORMATS; registering the sRGB view (viewport will look darker)"
        );
        viewport
            .color
            .create_view(&wgpu::TextureViewDescriptor::default())
    };
    let viewport_id =
        gui.renderer
            .register_native_texture(device, &egui_view, wgpu::FilterMode::Linear);
    let viewport_tex = egui::load::SizedTexture::new(viewport_id, egui::vec2(480.0, 320.0));

    let target = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gui-target"),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: gui_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let max_tex = device.limits().max_texture_dimension_2d;
    let mut state = DemoState {
        adapter_line: format!("{} ({:?})", info.name, info.backend),
        ..Default::default()
    };
    let clear = wgpu::Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    // 3) One GUI frame -> PNG.
    let t = Instant::now();
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("gui") });
    let raw = gui.headless_input(width, height, ppp, max_tex);
    let (cmds, _platform, stats) = gui.render(
        device,
        queue,
        &mut encoder,
        &target_view,
        [width, height],
        raw,
        wgpu::LoadOp::Clear(clear),
        |ui| demo_ui(ui, &mut state, Some(viewport_tex)),
    );
    queue.submit(cmds.into_iter().chain([encoder.finish()]));
    let pixels = match read_texture(device, queue, &target, wgpu::TextureAspect::All) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("READBACK FAILED: {e}");
            return ExitCode::from(2);
        }
    };
    println!(
        "first GUI frame (run_ui+tessellate+upload+render+readback) = {:?}; ui_build={:?} tessellate={:?} primitives={} vertices={} indices={} texture_deltas_applied={} freed={}",
        t.elapsed(),
        stats.ui_build,
        stats.tessellate,
        stats.primitives,
        stats.vertices,
        stats.indices,
        stats.textures_set,
        stats.textures_freed
    );
    if let Err(e) = write_png_rgba8(&out, width, height, &pixels) {
        eprintln!("PNG FAILED: {e}");
        return ExitCode::from(2);
    }
    println!("wrote {}", out.display());

    let st = image_stats(width, height, &pixels, [0, 0, 0, 255]);
    println!(
        "image: distinct_colors={} non_black_pixels={} luma[min={:.1} max={:.1}] stddev={:.2}",
        st.distinct_colors,
        st.pixels_differing_from_clear,
        st.luma_min,
        st.luma_max,
        st.luma_stddev_non_clear
    );
    // The viewport image (orange cube) must show up somewhere: count cube-orange pixels.
    let orange = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 150 && p[1] > 70 && p[1] < 150 && p[2] < 90)
        .count();
    println!("cube-orange pixels from the registered viewport texture: {orange}");

    // 4) Steady-state cost over more frames (no readback).
    let mut frame_times: Vec<Duration> = Vec::new();
    for _ in 0..frames {
        let t = Instant::now();
        state.frame += 1;
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("gui") });
        let raw = gui.headless_input(width, height, ppp, max_tex);
        let (cmds, _p, _s) = gui.render(
            device,
            queue,
            &mut encoder,
            &target_view,
            [width, height],
            raw,
            wgpu::LoadOp::Clear(clear),
            |ui| demo_ui(ui, &mut state, Some(viewport_tex)),
        );
        let index = queue.submit(cmds.into_iter().chain([encoder.finish()]));
        if let Err(e) = device.poll(wgpu::PollType::Wait {
            submission_index: Some(index),
            timeout: Some(Duration::from_secs(60)),
        }) {
            eprintln!("POLL FAILED: {e}");
            return ExitCode::from(2);
        }
        frame_times.push(t.elapsed());
    }
    println!(
        "GUI frame {width}x{height} (run_ui+tessellate+upload+render+wait): {}",
        summarize_ms(&frame_times)
    );
    // Also keep the last steady-state frame for comparison with the first one.
    if let Ok(last) = read_texture(device, queue, &target, wgpu::TextureAspect::All) {
        let last_path = out.with_file_name(format!(
            "{}-frame{}.png",
            out.file_stem().unwrap().to_string_lossy(),
            frames + 1
        ));
        if write_png_rgba8(&last_path, width, height, &last).is_ok() {
            let changed = last
                .as_chunks::<4>()
                .0
                .iter()
                .zip(pixels.as_chunks::<4>().0.iter())
                .filter(|(a, b)| a != b)
                .count();
            println!(
                "wrote {} ({changed} pixels differ from the first frame)",
                last_path.display()
            );
        }
    }

    let mut failures = Vec::new();
    if st.distinct_colors < 50 {
        failures.push(format!("only {} distinct colors", st.distinct_colors));
    }
    if st.pixels_differing_from_clear < (width * height / 2) as usize {
        failures.push("GUI covers less than half of the target".to_string());
    }
    if orange < 2000 {
        failures.push(format!(
            "viewport texture not visible ({orange} orange pixels)"
        ));
    }
    if failures.is_empty() {
        println!("GUI CHECKS PASSED");
        ExitCode::SUCCESS
    } else {
        for f in failures {
            println!("GUI CHECK FAILED: {f}");
        }
        ExitCode::from(1)
    }
}
