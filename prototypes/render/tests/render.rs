//! Integration tests: run with `cargo test -- --nocapture --test-threads=1` to see
//! the reported values. They need a wgpu adapter (lavapipe / llvmpipe here).

use std::sync::mpsc;
use std::time::Duration;

use render_spike::checks::{
    check_scene_frame, depth_copies_supported, ensure_ok, render_scene_frame,
};
use render_spike::gpu::{HeadlessGpu, HeadlessOptions, adapter_line, headless_instance};
use render_spike::gui::{DemoState, GuiRenderer, demo_ui};
use render_spike::readback::{image_stats, padded_bytes_per_row, read_texture};
use render_spike::scene::{OFFSCREEN_COLOR_FORMAT, OffscreenTarget, SceneRenderer};

fn gpu() -> HeadlessGpu {
    let backends = std::env::var("SPIKE_BACKENDS")
        .ok()
        .and_then(|s| render_spike::parse_backends(&s))
        .unwrap_or(wgpu::Backends::VULKAN | wgpu::Backends::GL);
    let gpu = HeadlessGpu::new_blocking(&HeadlessOptions {
        backends,
        ..Default::default()
    })
    .expect("no headless adapter/device");
    println!("adapter: {}", adapter_line(&gpu.adapter.get_info()));
    gpu
}

#[test]
fn row_padding_follows_copy_alignment() {
    assert_eq!(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 256);
    assert_eq!(padded_bytes_per_row(64, 4), 256);
    assert_eq!(padded_bytes_per_row(150, 4), 768); // 600 -> 768
    assert_eq!(padded_bytes_per_row(640, 4), 2560); // already aligned
    assert_eq!(padded_bytes_per_row(1, 4), 256);
}

/// Odd width (150 px = 600 bytes per row, padded to 768) exercises the unpadding.
#[test]
fn lit_scene_renders_offscreen_with_depth_and_padding() {
    let gpu = gpu();
    let scene = SceneRenderer::new(&gpu.device, OFFSCREEN_COLOR_FORMAT);
    let target = OffscreenTarget::new(&gpu.device, 150, 100, OFFSCREEN_COLOR_FORMAT);
    let angle = 30f32.to_radians();
    let frame = render_scene_frame(
        &gpu.device,
        &gpu.queue,
        &scene,
        &target,
        angle,
        depth_copies_supported(&gpu.adapter),
    )
    .expect("render");
    assert_eq!(frame.rgba.len(), 150 * 100 * 4);
    let check = check_scene_frame(&frame, &scene, angle);
    println!(
        "150x100: center={:?} distinct={} faces={:?} depth_center={}",
        check.stats.center,
        check.stats.distinct_colors,
        check.faces.iter().map(|f| f.luma).collect::<Vec<_>>(),
        check.depth_center
    );
    ensure_ok(&check).expect("scene checks");
}

#[test]
fn gui_frame_renders_offscreen() {
    let gpu = gpu();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut gui = GuiRenderer::new(&gpu.device, format);
    let (w, h) = (640u32, 400u32);
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gui-test"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&Default::default());
    let mut state = DemoState::default();
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    let raw = gui.headless_input(w, h, 1.0, gpu.device.limits().max_texture_dimension_2d);
    let (cmds, _out, stats) = gui.render(
        &gpu.device,
        &gpu.queue,
        &mut encoder,
        &view,
        [w, h],
        raw,
        wgpu::LoadOp::Clear(wgpu::Color::BLACK),
        |ui| demo_ui(ui, &mut state, None),
    );
    gpu.queue.submit(cmds.into_iter().chain([encoder.finish()]));
    let px = read_texture(&gpu.device, &gpu.queue, &target, wgpu::TextureAspect::All).unwrap();
    let st = image_stats(w, h, &px, [0, 0, 0, 255]);
    println!("gui: {stats:?} distinct_colors={}", st.distinct_colors);
    assert!(
        stats.primitives > 0 && stats.vertices > 100,
        "egui produced no geometry"
    );
    assert!(
        stats.textures_set >= 1,
        "font atlas was not uploaded on the first frame"
    );
    assert!(st.distinct_colors > 30, "GUI image looks empty");
}

/// Error-scope API (guard-based since wgpu 28) catches a validation error.
#[test]
fn error_scope_catches_validation_error() {
    let gpu = gpu();
    let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
    // MAP_READ may only be combined with COPY_DST without MAPPABLE_PRIMARY_BUFFERS.
    let _bad = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("invalid-usage"),
        size: 256,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::UNIFORM,
        mapped_at_creation: false,
    });
    let err = pollster::block_on(scope.pop());
    println!("error scope caught: {err:?}");
    assert!(matches!(err, Some(wgpu::Error::Validation { .. })));
}

/// request_adapter / request_device failure modes and their Display text.
#[test]
fn request_errors_are_reported() {
    // No adapter for a backend set that has nothing here (Metal is not compiled on Linux).
    let instance = headless_instance(wgpu::Backends::METAL, None);
    let r = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default()));
    match &r {
        Err(e) => println!("request_adapter(METAL) error: {e}"),
        Ok(a) => panic!("unexpected adapter {:?}", a.get_info()),
    }
    assert!(matches!(r, Err(wgpu::RequestAdapterError::NotFound { .. })));

    // Device with limits above what the adapter offers.
    let gpu = gpu();
    let mut limits = gpu.adapter.limits();
    limits.max_texture_dimension_2d = limits.max_texture_dimension_2d.saturating_mul(4);
    let r = pollster::block_on(gpu.adapter.request_device(&wgpu::DeviceDescriptor {
        label: None,
        required_limits: limits,
        ..Default::default()
    }));
    match &r {
        Err(e) => println!("request_device(excess limits) error: {e}"),
        Ok(_) => panic!("device with excessive limits was created"),
    }
    assert!(r.is_err());
}

/// map_buffer_on_submit (wgpu 27+) schedules the map with the submission, so no
/// separate map_async call is needed; poll still drives the callback.
#[test]
fn map_buffer_on_submit_reads_back() {
    let gpu = gpu();
    let src = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("src"),
        size: 16,
        usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    gpu.queue
        .write_buffer(&src, 0, bytemuck::cast_slice(&[1u32, 2, 3, 4]));
    let dst = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("dst"),
        size: 16,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(&src, 0, &dst, 0, None);
    let (tx, rx) = mpsc::channel();
    encoder.map_buffer_on_submit(&dst, wgpu::MapMode::Read, .., move |r| {
        let _ = tx.send(r);
    });
    let index = gpu.queue.submit([encoder.finish()]);
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(index),
            timeout: Some(Duration::from_secs(10)),
        })
        .expect("poll");
    rx.recv().unwrap().expect("map failed");
    let values: Vec<u32> = bytemuck::cast_slice(&dst.get_mapped_range(..).unwrap()).to_vec();
    dst.unmap();
    assert_eq!(values, vec![1, 2, 3, 4]);
}
