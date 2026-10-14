//! Instance / adapter / device creation without a surface (headless), plus
//! adapter reporting used by the `probe` binary.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::Result;

/// Options for creating a headless GPU context.
#[derive(Debug, Clone)]
pub struct HeadlessOptions {
    pub backends: wgpu::Backends,
    pub force_fallback_adapter: bool,
    pub power_preference: wgpu::PowerPreference,
    /// Instance flags. `None` means `InstanceFlags::from_build_config().with_env()`
    /// (debug builds enable validation when the layers exist).
    pub flags: Option<wgpu::InstanceFlags>,
    /// Features that are requested only if the adapter supports them.
    pub optional_features: wgpu::Features,
}

impl Default for HeadlessOptions {
    fn default() -> Self {
        Self {
            backends: wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            force_fallback_adapter: false,
            power_preference: wgpu::PowerPreference::HighPerformance,
            flags: None,
            optional_features: wgpu::Features::TIMESTAMP_QUERY
                | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
        }
    }
}

/// How long each setup step took (wall clock).
#[derive(Debug, Clone, Copy, Default)]
pub struct SetupTimings {
    pub instance: Duration,
    pub adapter: Duration,
    pub device: Duration,
}

/// A device with no surface, for offscreen rendering, tests and CI.
pub struct HeadlessGpu {
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub timings: SetupTimings,
}

/// Create an instance that is not connected to any display server.
///
/// wgpu 29+ removed `InstanceDescriptor::default()`; the constructor names force
/// the caller to decide whether a display handle is involved.
pub fn headless_instance(
    backends: wgpu::Backends,
    flags: Option<wgpu::InstanceFlags>,
) -> wgpu::Instance {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;
    desc.flags = flags.unwrap_or_else(|| wgpu::InstanceFlags::from_build_config().with_env());
    wgpu::Instance::new(desc)
}

/// Limits to request: WebGPU defaults when the adapter meets them, otherwise the
/// downlevel defaults (needed for some GL drivers), always with the adapter's
/// maximum texture resolution.
pub fn pick_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
    let supported = adapter.limits();
    let wanted = wgpu::Limits::default();
    let base = if wanted.check_limits(&supported) {
        wanted
    } else {
        wgpu::Limits::downlevel_defaults()
    };
    base.using_resolution(supported)
}

pub async fn request_device(
    adapter: &wgpu::Adapter,
    optional_features: wgpu::Features,
) -> std::result::Result<(wgpu::Device, wgpu::Queue), wgpu::RequestDeviceError> {
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("spike-device"),
            required_features: adapter.features() & optional_features,
            required_limits: pick_limits(adapter),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        })
        .await
}

/// Make validation errors visible instead of panicking inside wgpu's default
/// handler, so a spike run reports them.
pub fn install_error_logger(device: &wgpu::Device) {
    device.on_uncaptured_error(Arc::new(|err: wgpu::Error| {
        log::error!("uncaptured wgpu error: {err}");
        eprintln!("UNCAPTURED WGPU ERROR: {err}");
    }));
}

impl HeadlessGpu {
    pub async fn new(opts: &HeadlessOptions) -> Result<Self> {
        let t0 = Instant::now();
        let instance = headless_instance(opts.backends, opts.flags);
        let t_instance = t0.elapsed();

        let t1 = Instant::now();
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: opts.power_preference,
                force_fallback_adapter: opts.force_fallback_adapter,
                compatible_surface: None,
                apply_limit_buckets: false,
            })
            .await?;
        let t_adapter = t1.elapsed();

        let t2 = Instant::now();
        let (device, queue) = request_device(&adapter, opts.optional_features).await?;
        let t_device = t2.elapsed();
        install_error_logger(&device);

        Ok(Self {
            instance,
            adapter,
            device,
            queue,
            timings: SetupTimings {
                instance: t_instance,
                adapter: t_adapter,
                device: t_device,
            },
        })
    }

    /// Blocking convenience wrapper (native only).
    pub fn new_blocking(opts: &HeadlessOptions) -> Result<Self> {
        pollster::block_on(Self::new(opts))
    }
}

/// One-line adapter summary.
pub fn adapter_line(info: &wgpu::AdapterInfo) -> String {
    format!(
        "{} | backend={:?} type={:?} vendor=0x{:04x} device=0x{:04x} driver='{}' driver_info='{}'",
        info.name,
        info.backend,
        info.device_type,
        info.vendor,
        info.device,
        info.driver,
        info.driver_info
    )
}

/// Multi-line adapter report: info, downlevel caps, selected limits, feature list,
/// and format support for the formats the engine will use.
pub fn adapter_report(adapter: &wgpu::Adapter) -> String {
    let info = adapter.get_info();
    let limits = adapter.limits();
    let features = adapter.features();
    let downlevel = adapter.get_downlevel_capabilities();
    let mut s = String::new();
    let _ = writeln!(s, "adapter: {}", adapter_line(&info));
    let _ = writeln!(
        s,
        "  subgroup size: {}..={}  transient_saves_memory: {:?}  pci_bus_id: '{}'",
        info.subgroup_min_size,
        info.subgroup_max_size,
        info.transient_saves_memory,
        info.device_pci_bus_id
    );
    let _ = writeln!(
        s,
        "  downlevel: shader_model={:?} flags_is_webgpu_compliant={} flags={:?}",
        downlevel.shader_model,
        downlevel.is_webgpu_compliant(),
        downlevel.flags
    );
    let _ = writeln!(
        s,
        "  limits: max_texture_dimension_2d={} max_texture_dimension_3d={} max_texture_array_layers={} \
         max_bind_groups={} max_bindings_per_bind_group={} max_dynamic_uniform_buffers_per_pipeline_layout={} \
         max_sampled_textures_per_shader_stage={} max_samplers_per_shader_stage={} max_storage_buffers_per_shader_stage={} \
         max_storage_textures_per_shader_stage={} max_uniform_buffers_per_shader_stage={} \
         max_uniform_buffer_binding_size={} max_storage_buffer_binding_size={} max_buffer_size={} \
         max_vertex_buffers={} max_vertex_attributes={} max_vertex_buffer_array_stride={} \
         min_uniform_buffer_offset_alignment={} min_storage_buffer_offset_alignment={} \
         max_inter_stage_shader_variables={} max_color_attachments={} max_color_attachment_bytes_per_sample={} \
         max_compute_workgroup_storage_size={} max_compute_invocations_per_workgroup={} \
         max_compute_workgroup_size=({},{},{}) max_compute_workgroups_per_dimension={} max_immediate_size={}",
        limits.max_texture_dimension_2d,
        limits.max_texture_dimension_3d,
        limits.max_texture_array_layers,
        limits.max_bind_groups,
        limits.max_bindings_per_bind_group,
        limits.max_dynamic_uniform_buffers_per_pipeline_layout,
        limits.max_sampled_textures_per_shader_stage,
        limits.max_samplers_per_shader_stage,
        limits.max_storage_buffers_per_shader_stage,
        limits.max_storage_textures_per_shader_stage,
        limits.max_uniform_buffers_per_shader_stage,
        limits.max_uniform_buffer_binding_size,
        limits.max_storage_buffer_binding_size,
        limits.max_buffer_size,
        limits.max_vertex_buffers,
        limits.max_vertex_attributes,
        limits.max_vertex_buffer_array_stride,
        limits.min_uniform_buffer_offset_alignment,
        limits.min_storage_buffer_offset_alignment,
        limits.max_inter_stage_shader_variables,
        limits.max_color_attachments,
        limits.max_color_attachment_bytes_per_sample,
        limits.max_compute_workgroup_storage_size,
        limits.max_compute_invocations_per_workgroup,
        limits.max_compute_workgroup_size_x,
        limits.max_compute_workgroup_size_y,
        limits.max_compute_workgroup_size_z,
        limits.max_compute_workgroups_per_dimension,
        limits.max_immediate_size,
    );
    let _ = writeln!(
        s,
        "  meets WebGPU default limits: {}  meets downlevel_defaults: {}",
        wgpu::Limits::default().check_limits(&limits),
        wgpu::Limits::downlevel_defaults().check_limits(&limits)
    );
    let names: Vec<String> = features.iter_names().map(|(n, _)| n.to_string()).collect();
    let _ = writeln!(s, "  features ({}): {}", names.len(), names.join(" "));
    for fmt in [
        wgpu::TextureFormat::Rgba8UnormSrgb,
        wgpu::TextureFormat::Bgra8UnormSrgb,
        wgpu::TextureFormat::Rgba16Float,
        wgpu::TextureFormat::Depth32Float,
        wgpu::TextureFormat::Depth24PlusStencil8,
        wgpu::TextureFormat::Bc1RgbaUnormSrgb,
    ] {
        let ff = adapter.get_texture_format_features(fmt);
        let _ = writeln!(
            s,
            "  format {fmt:?}: usages={:?} flags={:?}",
            ff.allowed_usages, ff.flags
        );
    }
    s
}
