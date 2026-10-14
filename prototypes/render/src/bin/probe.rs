//! Enumerate wgpu adapters for a backend set without any surface, print a report
//! for each, try to create a device on each, then show what `request_adapter`
//! returns with and without `force_fallback_adapter`.
//!
//! Usage: probe [--backends vulkan|gl|all|primary|noop]
//! Select the Vulkan ICD with VK_ICD_FILENAMES (read by the Vulkan loader).

use std::time::Instant;

use render_spike::gpu::{adapter_line, adapter_report, headless_instance, request_device};

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let args: Vec<String> = std::env::args().collect();
    let backends = args
        .iter()
        .position(|a| a == "--backends")
        .and_then(|i| args.get(i + 1))
        .map(|s| render_spike::parse_backends(s).unwrap_or_else(|| panic!("unknown backend {s}")))
        .unwrap_or(wgpu::Backends::all());

    println!(
        "wgpu compile-time backends: {:?}",
        wgpu::Instance::enabled_backend_features()
    );
    println!(
        "env: VK_ICD_FILENAMES={:?} WGPU_BACKEND={:?} DISPLAY={:?} WAYLAND_DISPLAY={:?}",
        std::env::var("VK_ICD_FILENAMES").ok(),
        std::env::var("WGPU_BACKEND").ok(),
        std::env::var("DISPLAY").ok(),
        std::env::var("WAYLAND_DISPLAY").ok()
    );
    let flags = wgpu::InstanceFlags::from_build_config().with_env();
    println!("instance flags: {flags:?}");

    let t = Instant::now();
    let instance = headless_instance(backends, Some(flags));
    println!("Instance::new({backends:?}) took {:?}", t.elapsed());

    let t = Instant::now();
    let adapters = pollster::block_on(instance.enumerate_adapters(backends));
    println!(
        "enumerate_adapters -> {} adapter(s) in {:?}",
        adapters.len(),
        t.elapsed()
    );

    for (i, adapter) in adapters.iter().enumerate() {
        println!("--- adapter #{i}");
        print!("{}", adapter_report(adapter));
        let t = Instant::now();
        let wanted = wgpu::Features::TIMESTAMP_QUERY
            | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
            | wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES;
        match pollster::block_on(request_device(adapter, wanted)) {
            Ok((device, queue)) => {
                let f = device.features();
                println!(
                    "  request_device: OK in {:?}; TIMESTAMP_QUERY={} INSIDE_ENCODERS={} INSIDE_PASSES={} timestamp_period={}ns",
                    t.elapsed(),
                    f.contains(wgpu::Features::TIMESTAMP_QUERY),
                    f.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
                    f.contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_PASSES),
                    queue.get_timestamp_period()
                );
            }
            Err(e) => println!("  request_device: ERROR after {:?}: {e}", t.elapsed()),
        }
    }

    for force_fallback_adapter in [false, true] {
        let r = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter,
            compatible_surface: None,
            apply_limit_buckets: false,
        }));
        match r {
            Ok(a) => println!(
                "request_adapter(force_fallback_adapter={force_fallback_adapter}) -> {}",
                adapter_line(&a.get_info())
            ),
            Err(e) => println!(
                "request_adapter(force_fallback_adapter={force_fallback_adapter}) -> ERROR: {e}"
            ),
        }
    }
}
