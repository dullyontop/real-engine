//! Headless offscreen render of the lit scene: no window, no surface.
//! Writes a PNG, asserts the image is non-trivial (and that depth testing and
//! lighting work), and measures setup, pipeline, frame and GPU timestamp times.
//!
//! Usage: headless [--backends vulkan|gl|all] [--fallback] [--size WxH] [--frames N] [--out PATH]
//! Exit code: 0 = all checks passed, 1 = image checks failed, 2 = setup failed.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use render_spike::checks::{check_scene_frame, depth_copies_supported, render_scene_frame};
use render_spike::gpu::{HeadlessGpu, HeadlessOptions, adapter_line};
use render_spike::readback::write_png_rgba8;
use render_spike::scene::{OFFSCREEN_COLOR_FORMAT, OffscreenTarget, SceneRenderer};
use render_spike::timing::{GpuPassTimer, summarize_ms};

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
    let (width, height) = arg(&args, "--size")
        .map(|s| {
            let (w, h) = s.split_once('x').expect("--size WxH");
            (w.parse().unwrap(), h.parse().unwrap())
        })
        .unwrap_or((640u32, 480u32));
    let frames: usize = arg(&args, "--frames")
        .map(|s| s.parse().unwrap())
        .unwrap_or(30);
    let out = PathBuf::from(arg(&args, "--out").unwrap_or("out/headless.png"));
    let force_fallback_adapter = args.iter().any(|a| a == "--fallback");

    let opts = HeadlessOptions {
        backends,
        force_fallback_adapter,
        ..Default::default()
    };
    let gpu = match HeadlessGpu::new_blocking(&opts) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("SETUP FAILED: {e}");
            return ExitCode::from(2);
        }
    };
    let info = gpu.adapter.get_info();
    println!("adapter: {}", adapter_line(&info));
    println!(
        "setup: Instance::new={:?} request_adapter={:?} request_device={:?}",
        gpu.timings.instance, gpu.timings.adapter, gpu.timings.device
    );
    println!(
        "device: TIMESTAMP_QUERY={} limits.min_uniform_buffer_offset_alignment={} build={}",
        gpu.device
            .features()
            .contains(wgpu::Features::TIMESTAMP_QUERY),
        gpu.device.limits().min_uniform_buffer_offset_alignment,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    let t = Instant::now();
    let scene = SceneRenderer::new(&gpu.device, OFFSCREEN_COLOR_FORMAT);
    println!(
        "SceneRenderer::new (WGSL module + pipeline + buffers) = {:?}",
        t.elapsed()
    );
    let target = OffscreenTarget::new(&gpu.device, width, height, OFFSCREEN_COLOR_FORMAT);
    let read_depth = depth_copies_supported(&gpu.adapter);

    let angle = 30f32.to_radians();
    let t = Instant::now();
    let frame =
        match render_scene_frame(&gpu.device, &gpu.queue, &scene, &target, angle, read_depth) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("RENDER FAILED: {e}");
                return ExitCode::from(2);
            }
        };
    println!(
        "first frame incl. color+depth readback = {:?} (read_depth={read_depth})",
        t.elapsed()
    );

    if let Err(e) = write_png_rgba8(&out, width, height, &frame.rgba) {
        eprintln!("PNG FAILED: {e}");
        return ExitCode::from(2);
    }
    println!("wrote {}", out.display());

    let check = check_scene_frame(&frame, &scene, angle);
    let s = &check.stats;
    println!(
        "image: {}x{} center={:?} clear(srgb8)={:?} distinct_colors={} non_clear_pixels={} ({:.1}%) luma[min={:.1} max={:.1}] luma_stddev(non-clear)={:.2}",
        s.width,
        s.height,
        s.center,
        check.clear_srgb8,
        s.distinct_colors,
        s.pixels_differing_from_clear,
        100.0 * s.pixels_differing_from_clear as f64 / f64::from(width * height),
        s.luma_min,
        s.luma_max,
        s.luma_stddev_non_clear
    );
    for f in &check.faces {
        println!(
            "  cube face n={:+.2?} at px {:?}: rgba={:?} luma={:.1}",
            f.normal_world.to_array(),
            f.pixel,
            f.rgba,
            f.luma
        );
    }
    println!(
        "depth: center={} top-left={}",
        check.depth_center, check.depth_corner
    );

    // Timed loop: update uniforms + encode + submit + wait for completion (no readback).
    let timer = GpuPassTimer::new(&gpu.device, &gpu.queue);
    let mut cpu_times = Vec::with_capacity(frames);
    let mut gpu_times: Vec<Duration> = Vec::new();
    for i in 0..frames {
        let t = Instant::now();
        scene.update(&gpu.queue, target.aspect(), angle + i as f32 * 0.05);
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("timed"),
            });
        scene.encode(
            &mut encoder,
            &target.color_view,
            &target.depth_view,
            timer.as_ref().map(|t| t.pass_writes()),
        );
        if let Some(timer) = &timer {
            timer.resolve(&mut encoder);
        }
        let index = gpu.queue.submit([encoder.finish()]);
        if let Err(e) = gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(index),
            timeout: Some(Duration::from_secs(60)),
        }) {
            eprintln!("POLL FAILED: {e}");
            return ExitCode::from(2);
        }
        cpu_times.push(t.elapsed());
        if let Some(timer) = &timer {
            match timer.read(&gpu.device) {
                Ok(d) => gpu_times.push(d),
                Err(e) => eprintln!("timestamp read failed: {e}"),
            }
        }
    }
    println!(
        "frame (update+encode+submit+wait), {width}x{height}: {}",
        summarize_ms(&cpu_times)
    );
    match &timer {
        Some(t) => println!(
            "GPU render-pass time via timestamp queries (period {} ns/tick): {}",
            t.period_ns(),
            summarize_ms(&gpu_times)
        ),
        None => println!("GPU timestamps: unavailable (no Features::TIMESTAMP_QUERY)"),
    }

    // Timed loop including the color readback (what an offscreen capture costs).
    let mut capture_times = Vec::new();
    for i in 0..frames.min(10) {
        let t = Instant::now();
        if let Err(e) = render_scene_frame(
            &gpu.device,
            &gpu.queue,
            &scene,
            &target,
            angle + i as f32 * 0.05,
            false,
        ) {
            eprintln!("capture failed: {e}");
            return ExitCode::from(2);
        }
        capture_times.push(t.elapsed());
    }
    println!(
        "frame + color readback to CPU, {width}x{height}: {}",
        summarize_ms(&capture_times)
    );

    if check.failures.is_empty() {
        println!("CHECKS PASSED");
        ExitCode::SUCCESS
    } else {
        for f in &check.failures {
            println!("CHECK FAILED: {f}");
        }
        ExitCode::from(1)
    }
}
