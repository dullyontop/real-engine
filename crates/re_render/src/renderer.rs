//! The forward renderer: GPU resources behind generation-checked handles, the
//! scene pass, the shadow pass, debug lines and egui painting.

use std::collections::{HashMap, HashSet};
use std::mem::size_of;
use std::num::NonZeroU64;

use glam::{Mat4, Vec3};
use re_core::Diagnostic;
use re_types::mesh::{Aabb, MeshData};
use re_types::render::{
    AlphaMode, DrawFlags, MaterialDesc, MaterialHandle, MeshHandle, RenderSnapshot, TextureData,
    TextureFormat, TextureHandle, TextureRef,
};

use crate::frame::{self, FrameUniform, InstanceRaw, LineVertex, MaterialUniform, PointLightRaw};
use crate::gpu::Gpu;
use crate::slots::Slots;
use crate::ui::{UiFrame, UiPainter};
use crate::{codes, shader};

const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const MSAA_SAMPLES: u32 = 4;
/// Render-target sets kept for different target sizes and formats.
const TARGET_CACHE: usize = 4;

/// True for formats that store gamma-encoded colour without hardware sRGB
/// conversion: shaders encode by hand when drawing into them.
pub(crate) fn stores_gamma(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Rgba8Unorm
            | wgpu::TextureFormat::Bgra8Unorm
            | wgpu::TextureFormat::Rgb10a2Unorm
    )
}

/// Renderer options that are not part of a [`RenderSnapshot`].
#[derive(Clone, Debug, PartialEq)]
pub struct RendererSettings {
    /// Multiplies every light, the ambient terms and emissive colour. Light units
    /// are lux for directional lights and lumens for point lights; at exposure 1,
    /// an illuminance of 1 lux makes a white diffuse surface white. For a
    /// physically sized sun (around 100,000 lux), use an exposure around 1e-5.
    pub exposure: f32,
    /// Ambient light from above, linear RGB (same units as a directional light).
    pub ambient_sky: [f32; 3],
    /// Ambient light from below, linear RGB.
    pub ambient_ground: [f32; 3],
    /// The side of the square shadow map in texels.
    pub shadow_map_size: u32,
    /// How far from the camera the sun's shadow reaches, in world units.
    pub shadow_distance: f32,
    /// Use 4x MSAA when the adapter supports it for the target format.
    pub msaa: bool,
}

impl Default for RendererSettings {
    fn default() -> Self {
        RendererSettings {
            exposure: 1.0,
            ambient_sky: [0.16, 0.19, 0.24],
            ambient_ground: [0.08, 0.07, 0.06],
            shadow_map_size: 2048,
            shadow_distance: 60.0,
            msaa: true,
        }
    }
}

/// The live count and GPU bytes of one kind of resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ResourceStats {
    /// Live resources.
    pub count: usize,
    /// GPU memory they use, in bytes (buffers and texture levels).
    pub bytes: u64,
}

/// What one call of [`Renderer::render`] drew.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Draw calls in the scene pass (each draws one or more instances).
    pub draw_calls: u32,
    /// Draw items drawn.
    pub items_drawn: u32,
    /// Draw items skipped because a handle was stale.
    pub items_skipped: u32,
    /// Instances drawn into the shadow map.
    pub shadow_casters: u32,
    /// Triangles submitted in the scene pass.
    pub triangles: u64,
    /// Debug lines drawn.
    pub debug_lines: u32,
    /// Point lights drawn.
    pub point_lights: u32,
    /// MSAA sample count of the scene pass.
    pub samples: u32,
}

/// The result of [`Renderer::render`]: what was drawn and the warnings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FrameReport {
    /// What was drawn.
    pub stats: FrameStats,
    /// Problems that did not stop the frame: stale handles (once per handle) and
    /// ignored lights.
    pub warnings: Vec<Diagnostic>,
}

/// Live resources and the last frame, from [`Renderer::stats`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    /// Uploaded meshes.
    pub meshes: ResourceStats,
    /// Uploaded textures.
    pub textures: ResourceStats,
    /// Created materials.
    pub materials: ResourceStats,
    /// The last rendered frame.
    pub last_frame: FrameStats,
}

struct GpuMesh {
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    index_count: u32,
    aabb: Option<Aabb>,
    bytes: u64,
}

struct GpuTexture {
    view: wgpu::TextureView,
    bytes: u64,
}

struct GpuMaterial {
    bind_group: wgpu::BindGroup,
    blend: bool,
    double_sided: bool,
}

impl GpuMaterial {
    /// Index into [`FormatPipelines::forward`].
    fn variant(&self) -> usize {
        usize::from(self.double_sided) | (usize::from(self.blend) << 1)
    }
}

/// Pipelines for one (target format, sample count).
struct FormatPipelines {
    /// Indexed by `double_sided | blend << 1`.
    forward: [wgpu::RenderPipeline; 4],
    lines: wgpu::RenderPipeline,
}

struct FrameTargets {
    depth: wgpu::TextureView,
    msaa: Option<wgpu::TextureView>,
}

struct ShadowMap {
    size: u32,
    view: wgpu::TextureView,
    frame_bind_group: wgpu::BindGroup,
}

struct Layouts {
    frame: wgpu::BindGroupLayout,
    shadow_frame: wgpu::BindGroupLayout,
    material: wgpu::BindGroupLayout,
    forward: wgpu::PipelineLayout,
    shadow: wgpu::PipelineLayout,
}

struct GrowBuffer {
    buffer: wgpu::Buffer,
    label: &'static str,
    usage: wgpu::BufferUsages,
}

impl GrowBuffer {
    fn new(device: &wgpu::Device, label: &'static str, usage: wgpu::BufferUsages) -> GrowBuffer {
        GrowBuffer {
            buffer: Self::create(device, label, usage, 64 * 1024),
            label,
            usage,
        }
    }

    fn create(
        device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
        size: u64,
    ) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size,
            usage: usage | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Writes `data` at offset 0, growing the buffer to a power of two first if needed.
    fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[u8]) {
        let needed = data.len() as u64;
        if needed > self.buffer.size() {
            self.buffer = Self::create(device, self.label, self.usage, needed.next_power_of_two());
        }
        if !data.is_empty() {
            queue.write_buffer(&self.buffer, 0, data);
        }
    }
}

/// Default textures for empty material slots.
struct Defaults {
    white_srgb: wgpu::TextureView,
    white_linear: wgpu::TextureView,
    flat_normal: wgpu::TextureView,
}

/// A resource slot: `(index, generation)`.
type Slot = (u32, u32);

/// One batch of the scene pass: consecutive instances of one mesh and material.
struct Batch {
    variant: usize,
    material: Slot,
    mesh: Slot,
    first_instance: u32,
    instances: u32,
}

/// The forward renderer (ADR-0010).
///
/// One renderer can draw into targets of any colour format the adapter can render
/// to; pipelines are created per format on first use. Resources are uploaded once
/// and referred to by handles from `re_types`. A released handle is detected (each
/// slot has a generation), so it never reaches the resource that reuses its slot.
pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    adapter: wgpu::Adapter,
    settings: RendererSettings,
    layouts: Layouts,
    forward_shader: wgpu::ShaderModule,
    lines_shader: wgpu::ShaderModule,
    shadow_pipeline: wgpu::RenderPipeline,
    pipelines: HashMap<(wgpu::TextureFormat, u32), FormatPipelines>,
    targets: HashMap<(u32, u32, wgpu::TextureFormat, u32), FrameTargets>,
    frame_buffer: wgpu::Buffer,
    shadow_sampler: wgpu::Sampler,
    material_sampler: wgpu::Sampler,
    shadow: ShadowMap,
    instances: GrowBuffer,
    lines: GrowBuffer,
    defaults: Defaults,
    meshes: Slots<GpuMesh>,
    textures: Slots<GpuTexture>,
    materials: Slots<GpuMaterial>,
    ui: UiPainter,
    last_frame: FrameStats,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("settings", &self.settings)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

fn mesh_vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4
    ];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<re_types::mesh::Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

fn position_only_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![0 => Float32x3];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<re_types::mesh::Vertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

fn instance_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 8] = wgpu::vertex_attr_array![
        4 => Float32x4, 5 => Float32x4, 6 => Float32x4, 7 => Float32x4,
        8 => Float32x4, 9 => Float32x4, 10 => Float32x4, 11 => Float32x4
    ];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<InstanceRaw>() as u64,
        step_mode: wgpu::VertexStepMode::Instance,
        attributes: &ATTRIBUTES,
    }
}

fn line_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x4];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<LineVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

fn uniform_entry(
    binding: u32,
    visibility: wgpu::ShaderStages,
    size: usize,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: NonZeroU64::new(size as u64),
        },
        count: None,
    }
}

fn texture_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Float { filterable: true },
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

fn create_layouts(device: &wgpu::Device) -> Layouts {
    let both = wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT;
    let frame = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("re_render frame"),
        entries: &[
            uniform_entry(0, both, size_of::<FrameUniform>()),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
        ],
    });
    let shadow_frame = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("re_render shadow frame"),
        entries: &[uniform_entry(
            0,
            wgpu::ShaderStages::VERTEX,
            size_of::<FrameUniform>(),
        )],
    });
    let material = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("re_render material"),
        entries: &[
            uniform_entry(
                0,
                wgpu::ShaderStages::FRAGMENT,
                size_of::<MaterialUniform>(),
            ),
            texture_entry(1),
            texture_entry(2),
            texture_entry(3),
            wgpu::BindGroupLayoutEntry {
                binding: 4,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let forward = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("re_render forward"),
        bind_group_layouts: &[Some(&frame), Some(&material)],
        immediate_size: 0,
    });
    let shadow = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("re_render shadow"),
        bind_group_layouts: &[Some(&shadow_frame)],
        immediate_size: 0,
    });
    Layouts {
        frame,
        shadow_frame,
        material,
        forward,
        shadow,
    }
}

fn solid_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    label: &str,
    format: wgpu::TextureFormat,
    rgba: [u8; 4],
) -> wgpu::TextureView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        texture.as_image_copy(),
        &rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(4),
            rows_per_image: Some(1),
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// The mip chain of an RGBA8 image, level 0 first, by 2x2 box filtering. Colour
/// channels of sRGB images are averaged in linear space.
pub(crate) fn mip_chain(
    width: u32,
    height: u32,
    pixels: &[u8],
    srgb: bool,
) -> Vec<(u32, u32, Vec<u8>)> {
    let to_linear: Vec<f32> = (0..=255u8)
        .map(|v| {
            let c = f32::from(v) / 255.0;
            if srgb {
                if c <= 0.04045 {
                    c / 12.92
                } else {
                    ((c + 0.055) / 1.055).powf(2.4)
                }
            } else {
                c
            }
        })
        .collect();
    let encode = |c: f32| -> u8 {
        if srgb {
            crate::capture::linear_to_srgb_u8(c)
        } else {
            (c.clamp(0.0, 1.0) * 255.0).round() as u8
        }
    };
    let mut levels = vec![(width, height, pixels.to_vec())];
    while let Some((w, h, src)) = levels.last().filter(|(w, h, _)| *w > 1 || *h > 1) {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut out = Vec::with_capacity((nw * nh * 4) as usize);
        for y in 0..nh {
            for x in 0..nw {
                let mut sum = [0f32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let sx = (x * 2 + dx).min(w - 1);
                    let sy = (y * 2 + dy).min(h - 1);
                    let i = ((sy * w + sx) * 4) as usize;
                    for c in 0..3 {
                        sum[c] += to_linear[src[i + c] as usize];
                    }
                    sum[3] += f32::from(src[i + 3]) / 255.0;
                }
                for (c, s) in sum.iter().enumerate() {
                    let avg = s / 4.0;
                    out.push(if c == 3 {
                        (avg * 255.0).round() as u8
                    } else {
                        encode(avg)
                    });
                }
            }
        }
        levels.push((nw, nh, out));
    }
    levels
}

impl Renderer {
    /// A renderer with default [`RendererSettings`], ready to draw into `format`
    /// targets (pipelines for other formats are created when first used).
    pub fn new(gpu: &Gpu, format: wgpu::TextureFormat) -> Renderer {
        Self::with_settings(gpu, format, RendererSettings::default())
    }

    /// A renderer with the given settings.
    pub fn with_settings(
        gpu: &Gpu,
        format: wgpu::TextureFormat,
        settings: RendererSettings,
    ) -> Renderer {
        let device = gpu.device.clone();
        let queue = gpu.queue.clone();
        let layouts = create_layouts(&device);
        let forward_shader = shader::module(&device, "forward.wgsl");
        let lines_shader = shader::module(&device, "lines.wgsl");
        let shadow_shader = shader::module(&device, "shadow.wgsl");
        let shadow_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("re_render shadow"),
            layout: Some(&layouts.shadow),
            vertex: wgpu::VertexState {
                module: &shadow_shader,
                entry_point: Some("vs_shadow"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[Some(position_only_layout()), Some(instance_layout())],
            },
            primitive: wgpu::PrimitiveState {
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: SHADOW_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::LessEqual),
                stencil: wgpu::StencilState::default(),
                // No clamp: GL lacks DownlevelFlags::DEPTH_BIAS_CLAMP.
                bias: wgpu::DepthBiasState {
                    constant: 2,
                    slope_scale: 2.0,
                    clamp: 0.0,
                },
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: None,
            multiview_mask: None,
            cache: None,
        });
        let frame_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("re_render frame"),
            size: size_of::<FrameUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("re_render shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let material_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("re_render material"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::Repeat,
            address_mode_w: wgpu::AddressMode::Repeat,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let defaults = Defaults {
            white_srgb: solid_texture(
                &device,
                &queue,
                "re_render white",
                wgpu::TextureFormat::Rgba8UnormSrgb,
                [255; 4],
            ),
            white_linear: solid_texture(
                &device,
                &queue,
                "re_render white linear",
                wgpu::TextureFormat::Rgba8Unorm,
                [255; 4],
            ),
            flat_normal: solid_texture(
                &device,
                &queue,
                "re_render flat normal",
                wgpu::TextureFormat::Rgba8Unorm,
                [128, 128, 255, 255],
            ),
        };
        let shadow = Self::create_shadow_map(
            &device,
            &layouts,
            &frame_buffer,
            &shadow_sampler,
            settings.shadow_map_size.max(1),
        );
        let mut renderer = Renderer {
            instances: GrowBuffer::new(&device, "re_render instances", wgpu::BufferUsages::VERTEX),
            lines: GrowBuffer::new(&device, "re_render lines", wgpu::BufferUsages::VERTEX),
            ui: UiPainter::new(&device),
            adapter: gpu.adapter.clone(),
            device,
            queue,
            settings,
            layouts,
            forward_shader,
            lines_shader,
            shadow_pipeline,
            pipelines: HashMap::new(),
            targets: HashMap::new(),
            frame_buffer,
            shadow_sampler,
            material_sampler,
            shadow,
            defaults,
            meshes: Slots::default(),
            textures: Slots::default(),
            materials: Slots::default(),
            last_frame: FrameStats::default(),
        };
        let samples = renderer.sample_count(format);
        renderer.pipelines_for(format, samples);
        renderer
    }

    /// The settings.
    pub fn settings(&self) -> &RendererSettings {
        &self.settings
    }

    /// The settings, to change them. They apply from the next frame.
    pub fn settings_mut(&mut self) -> &mut RendererSettings {
        &mut self.settings
    }

    fn create_shadow_map(
        device: &wgpu::Device,
        layouts: &Layouts,
        frame_buffer: &wgpu::Buffer,
        sampler: &wgpu::Sampler,
        size: u32,
    ) -> ShadowMap {
        let size = size.min(device.limits().max_texture_dimension_2d);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("re_render shadow map"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let frame_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("re_render frame"),
            layout: &layouts.frame,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: frame_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(sampler),
                },
            ],
        });
        ShadowMap {
            size,
            view,
            frame_bind_group,
        }
    }

    fn sample_count(&self, format: wgpu::TextureFormat) -> u32 {
        let supports = |f: wgpu::TextureFormat| {
            self.adapter
                .get_texture_format_features(f)
                .flags
                .sample_count_supported(MSAA_SAMPLES)
        };
        if self.settings.msaa && supports(format) && supports(DEPTH_FORMAT) {
            MSAA_SAMPLES
        } else {
            1
        }
    }

    fn pipelines_for(&mut self, format: wgpu::TextureFormat, samples: u32) -> &FormatPipelines {
        if !self.pipelines.contains_key(&(format, samples)) {
            let pipelines = self.create_pipelines(format, samples);
            self.pipelines.insert((format, samples), pipelines);
        }
        &self.pipelines[&(format, samples)]
    }

    fn create_pipelines(&self, format: wgpu::TextureFormat, samples: u32) -> FormatPipelines {
        let gamma = stores_gamma(format);
        let multisample = wgpu::MultisampleState {
            count: samples,
            ..Default::default()
        };
        let forward = |double_sided: bool, blend: bool| {
            self.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("re_render forward"),
                    layout: Some(&self.layouts.forward),
                    vertex: wgpu::VertexState {
                        module: &self.forward_shader,
                        entry_point: Some("vs_forward"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[Some(mesh_vertex_layout()), Some(instance_layout())],
                    },
                    primitive: wgpu::PrimitiveState {
                        topology: wgpu::PrimitiveTopology::TriangleList,
                        front_face: wgpu::FrontFace::Ccw,
                        cull_mode: if double_sided {
                            None
                        } else {
                            Some(wgpu::Face::Back)
                        },
                        ..Default::default()
                    },
                    depth_stencil: Some(wgpu::DepthStencilState {
                        format: DEPTH_FORMAT,
                        depth_write_enabled: Some(!blend),
                        depth_compare: Some(wgpu::CompareFunction::Less),
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState::default(),
                    }),
                    multisample,
                    fragment: Some(wgpu::FragmentState {
                        module: &self.forward_shader,
                        entry_point: Some(if gamma {
                            "fs_forward_gamma"
                        } else {
                            "fs_forward_linear"
                        }),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format,
                            blend: blend.then_some(wgpu::BlendState::ALPHA_BLENDING),
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    multiview_mask: None,
                    cache: None,
                })
        };
        let lines_layout = self
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("re_render lines"),
                bind_group_layouts: &[Some(&self.layouts.frame)],
                immediate_size: 0,
            });
        let lines = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("re_render lines"),
                layout: Some(&lines_layout),
                vertex: wgpu::VertexState {
                    module: &self.lines_shader,
                    entry_point: Some("vs_line"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(line_layout())],
                },
                primitive: wgpu::PrimitiveState {
                    topology: wgpu::PrimitiveTopology::LineList,
                    ..Default::default()
                },
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::LessEqual),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample,
                fragment: Some(wgpu::FragmentState {
                    module: &self.lines_shader,
                    entry_point: Some(if gamma {
                        "fs_line_gamma"
                    } else {
                        "fs_line_linear"
                    }),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
        FormatPipelines {
            forward: [
                forward(false, false),
                forward(true, false),
                forward(false, true),
                forward(true, true),
            ],
            lines,
        }
    }

    fn frame_targets(
        &mut self,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> &FrameTargets {
        let key = (width, height, format, samples);
        if !self.targets.contains_key(&key) {
            if self.targets.len() >= TARGET_CACHE {
                self.targets.clear();
            }
            let make = |label: &str, format: wgpu::TextureFormat| {
                self.device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: samples,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default())
            };
            let targets = FrameTargets {
                depth: make("re_render depth", DEPTH_FORMAT),
                msaa: (samples > 1).then(|| make("re_render msaa colour", format)),
            };
            self.targets.insert(key, targets);
        }
        &self.targets[&key]
    }

    // ----- Resources -----------------------------------------------------------

    /// Uploads a mesh. Fails with `RE6014` when the data is malformed. An empty mesh
    /// is accepted and draws nothing.
    pub fn upload_mesh(&mut self, mesh: &MeshData) -> Result<MeshHandle, Diagnostic> {
        let bad = |message: String| Diagnostic::error(codes::BAD_MESH, message);
        if !mesh.indices.len().is_multiple_of(3) {
            return Err(bad(format!(
                "{} indices is not a whole number of triangles",
                mesh.indices.len()
            )));
        }
        if let Some((at, &index)) = mesh
            .indices
            .iter()
            .enumerate()
            .find(|(_, i)| **i as usize >= mesh.vertices.len())
        {
            return Err(bad(format!(
                "index {at} is {index}, but the mesh has {} vertices",
                mesh.vertices.len()
            )));
        }
        if let Some(at) = mesh.vertices.iter().position(|v| {
            !v.pos
                .iter()
                .chain(&v.normal)
                .chain(&v.uv)
                .chain(&v.color)
                .all(|c| c.is_finite())
        }) {
            return Err(bad(format!("vertex {at} has a non-finite value")));
        }
        let index_count = u32::try_from(mesh.indices.len())
            .map_err(|_| bad(format!("{} indices is too many", mesh.indices.len())))?;
        let vertex_bytes: Vec<u8> = mesh
            .vertices
            .iter()
            .flat_map(|v| {
                let floats = [
                    v.pos[0],
                    v.pos[1],
                    v.pos[2],
                    v.normal[0],
                    v.normal[1],
                    v.normal[2],
                    v.uv[0],
                    v.uv[1],
                    v.color[0],
                    v.color[1],
                    v.color[2],
                    v.color[3],
                ];
                bytemuck::cast::<[f32; 12], [u8; 48]>(floats)
            })
            .collect();
        let index_bytes: &[u8] = bytemuck::cast_slice(&mesh.indices);
        let max = self.device.limits().max_buffer_size;
        if vertex_bytes.len() as u64 > max || index_bytes.len() as u64 > max {
            return Err(bad(format!(
                "the mesh is larger than the GPU's {max}-byte buffer limit"
            )));
        }
        let buffer = |label: &str, usage: wgpu::BufferUsages, data: &[u8]| {
            // Zero-sized buffers cannot be bound; an empty mesh gets a small one.
            let size = (data.len() as u64)
                .max(16)
                .next_multiple_of(wgpu::COPY_BUFFER_ALIGNMENT);
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size,
                usage: usage | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            if !data.is_empty() {
                let mut padded = data.to_vec();
                padded.resize(padded.len().next_multiple_of(4), 0);
                self.queue.write_buffer(&buffer, 0, &padded);
            }
            buffer
        };
        let vertices = buffer(
            "re_render mesh vertices",
            wgpu::BufferUsages::VERTEX,
            &vertex_bytes,
        );
        let indices = buffer(
            "re_render mesh indices",
            wgpu::BufferUsages::INDEX,
            index_bytes,
        );
        let bytes = vertices.size() + indices.size();
        let (index, generation) = self.meshes.insert(GpuMesh {
            vertices,
            indices,
            index_count,
            aabb: mesh.aabb(),
            bytes,
        });
        Ok(MeshHandle::new(index, generation))
    }

    /// Uploads a texture with a full mip chain. Fails with `RE6015` when the data is
    /// malformed or larger than the GPU allows.
    pub fn upload_texture(&mut self, data: &TextureData) -> Result<TextureHandle, Diagnostic> {
        let max = self.device.limits().max_texture_dimension_2d;
        if data.width == 0 || data.height == 0 || data.width > max || data.height > max {
            return Err(Diagnostic::error(
                codes::BAD_TEXTURE,
                format!(
                    "a {}x{} texture is not possible: sizes go from 1 to {max}",
                    data.width, data.height
                ),
            ));
        }
        if !data.is_well_formed() {
            return Err(Diagnostic::error(
                codes::BAD_TEXTURE,
                format!(
                    "a {}x{} RGBA8 texture needs {} bytes of pixels, not {}",
                    data.width,
                    data.height,
                    u64::from(data.width) * u64::from(data.height) * 4,
                    data.pixels.len()
                ),
            ));
        }
        let (format, srgb) = match data.format {
            TextureFormat::Rgba8Srgb => (wgpu::TextureFormat::Rgba8UnormSrgb, true),
            TextureFormat::Rgba8Unorm => (wgpu::TextureFormat::Rgba8Unorm, false),
        };
        let levels = mip_chain(data.width, data.height, &data.pixels, srgb);
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("re_render texture"),
            size: wgpu::Extent3d {
                width: data.width,
                height: data.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: levels.len() as u32,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut bytes = 0u64;
        for (level, (w, h, pixels)) in levels.iter().enumerate() {
            bytes += pixels.len() as u64;
            self.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: level as u32,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(w * 4),
                    rows_per_image: Some(*h),
                },
                wgpu::Extent3d {
                    width: *w,
                    height: *h,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let (index, generation) = self.textures.insert(GpuTexture { view, bytes });
        Ok(TextureHandle::new(index, generation))
    }

    fn texture_view(
        &self,
        slot: Option<&TextureRef>,
        default: &wgpu::TextureView,
    ) -> Result<wgpu::TextureView, Diagnostic> {
        match slot {
            None => Ok(default.clone()),
            Some(TextureRef::Asset(id)) => Err(Diagnostic::error(
                codes::UNRESOLVED_TEXTURE,
                format!("the material names the texture asset `{id}`, not an uploaded texture"),
            )
            .with_hint(
                "upload the texture with Renderer::upload_texture and replace the reference \
                 with TextureRef::Handle before Renderer::create_material",
            )),
            Some(TextureRef::Handle(h)) => self
                .textures
                .get(h.index, h.generation)
                .map(|t| t.view.clone())
                .ok_or_else(|| stale(codes::STALE_TEXTURE, "texture", h.index, h.generation)),
        }
    }

    /// Creates a material. Every texture must be an uploaded handle (`RE6013` for an
    /// asset reference, `RE6011` for a released handle), and every value finite
    /// (`RE6016`).
    ///
    /// The material keeps its textures alive on the GPU: releasing a texture that a
    /// live material uses frees its memory only when the material is released.
    pub fn create_material(&mut self, desc: &MaterialDesc) -> Result<MaterialHandle, Diagnostic> {
        let base =
            self.texture_view(desc.base_color_texture.as_ref(), &self.defaults.white_srgb)?;
        let metallic_roughness = self.texture_view(
            desc.metallic_roughness_texture.as_ref(),
            &self.defaults.white_linear,
        )?;
        let normal = self.texture_view(desc.normal_texture.as_ref(), &self.defaults.flat_normal)?;
        let cutoff = match desc.alpha_mode {
            AlphaMode::Mask { cutoff } => cutoff,
            AlphaMode::Opaque | AlphaMode::Blend => -1.0,
        };
        let values = desc.base_color.iter().chain(&desc.emissive).chain([
            &desc.metallic,
            &desc.roughness,
            &desc.emissive_strength,
            &cutoff,
        ]);
        if values.clone().any(|v| !v.is_finite()) {
            return Err(Diagnostic::error(
                codes::BAD_MATERIAL,
                "the material has a non-finite value (NaN or infinity)",
            ));
        }
        let blend = matches!(desc.alpha_mode, AlphaMode::Blend);
        let flag = |b: bool| f32::from(u8::from(b));
        let uniform = MaterialUniform {
            base_color: desc.base_color,
            emissive: frame::rgb_w(
                (Vec3::from_array(desc.emissive) * desc.emissive_strength).to_array(),
                0.0,
            ),
            params: [desc.metallic, desc.roughness, cutoff, flag(desc.unlit)],
            flags: [
                flag(desc.normal_texture.is_some()),
                flag(desc.double_sided),
                flag(blend),
                0.0,
            ],
        };
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("re_render material"),
            size: size_of::<MaterialUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.queue
            .write_buffer(&buffer, 0, bytemuck::bytes_of(&uniform));
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("re_render material"),
            layout: &self.layouts.material,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&base),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&metallic_roughness),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&normal),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: wgpu::BindingResource::Sampler(&self.material_sampler),
                },
            ],
        });
        let (index, generation) = self.materials.insert(GpuMaterial {
            bind_group,
            blend,
            double_sided: desc.double_sided,
        });
        Ok(MaterialHandle::new(index, generation))
    }

    /// Releases a mesh. Fails with `RE6010` for a released or unknown handle.
    pub fn release_mesh(&mut self, handle: MeshHandle) -> Result<(), Diagnostic> {
        // Dropping the buffers frees them once the GPU is done with any frame that
        // still uses them.
        self.meshes
            .remove(handle.index, handle.generation)
            .map(drop)
            .ok_or_else(|| stale(codes::STALE_MESH, "mesh", handle.index, handle.generation))
    }

    /// Releases a texture. Fails with `RE6011` for a released or unknown handle.
    pub fn release_texture(&mut self, handle: TextureHandle) -> Result<(), Diagnostic> {
        self.textures
            .remove(handle.index, handle.generation)
            .map(drop)
            .ok_or_else(|| {
                stale(
                    codes::STALE_TEXTURE,
                    "texture",
                    handle.index,
                    handle.generation,
                )
            })
    }

    /// Releases a material. Fails with `RE6012` for a released or unknown handle.
    pub fn release_material(&mut self, handle: MaterialHandle) -> Result<(), Diagnostic> {
        self.materials
            .remove(handle.index, handle.generation)
            .map(drop)
            .ok_or_else(|| {
                stale(
                    codes::STALE_MATERIAL,
                    "material",
                    handle.index,
                    handle.generation,
                )
            })
    }

    /// The model-space bounds of a live mesh (`None` for an empty or stale mesh).
    pub fn mesh_bounds(&self, handle: MeshHandle) -> Option<Aabb> {
        self.meshes.get(handle.index, handle.generation)?.aabb
    }

    /// Live resource counts and bytes, and what the last frame drew.
    pub fn stats(&self) -> RenderStats {
        let meshes = ResourceStats {
            count: self.meshes.len(),
            bytes: self.meshes.values().map(|m| m.bytes).sum(),
        };
        let textures = ResourceStats {
            count: self.textures.len(),
            bytes: self.textures.values().map(|t| t.bytes).sum(),
        };
        let materials = ResourceStats {
            count: self.materials.len(),
            bytes: self.materials.len() as u64 * size_of::<MaterialUniform>() as u64,
        };
        RenderStats {
            meshes,
            textures,
            materials,
            last_frame: self.last_frame,
        }
    }

    // ----- Frames --------------------------------------------------------------

    /// Draws `snapshot` into `target`, clearing it first with `snapshot.clear`.
    ///
    /// `target` must be a view of a single-sample 2D texture with
    /// `RENDER_ATTACHMENT` usage in a colour format the adapter can render to.
    /// sRGB formats store the linear result with hardware encoding; `Rgba8Unorm`,
    /// `Bgra8Unorm` and `Rgb10a2Unorm` get gamma-encoded colour from the shader.
    ///
    /// Items with stale handles are skipped and reported once each in the
    /// returned [`FrameReport`]. Errors (`RE6005`, `RE6007`) mean nothing was drawn.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        snapshot: &RenderSnapshot,
    ) -> Result<FrameReport, Diagnostic> {
        let texture = target.texture();
        let (width, height, format) = (texture.width(), texture.height(), texture.format());
        self.check_target(texture)?;
        let aspect = width as f32 / height as f32;
        let projection = frame::projection(&snapshot.camera, aspect)?;
        let view_proj = projection * snapshot.camera.view();
        let camera_pos = snapshot.camera.position();

        if self.shadow.size != self.settings.shadow_map_size.max(1) {
            self.shadow = Self::create_shadow_map(
                device,
                &self.layouts,
                &self.frame_buffer,
                &self.shadow_sampler,
                self.settings.shadow_map_size.max(1),
            );
        }
        let samples = self.sample_count(format);
        let mut report = FrameReport::default();
        report.stats.samples = samples;

        // Resolve every item; skip (and report once) the ones with stale handles.
        let mut reported = HashSet::new();
        let mut opaque = Vec::new();
        let mut blended = Vec::new();
        let mut casters = Vec::new();
        let mut scene_bounds: Option<Aabb> = None;
        let mut caster_bounds: Option<Aabb> = None;
        for (i, item) in snapshot.items.iter().enumerate() {
            let mesh = self.meshes.get(item.mesh.index, item.mesh.generation);
            let material = self
                .materials
                .get(item.material.index, item.material.generation);
            let (Some(mesh), Some(material)) = (mesh, material) else {
                report.stats.items_skipped += 1;
                if mesh.is_none() && reported.insert((0, item.mesh.index, item.mesh.generation)) {
                    report.warnings.push(stale_item(
                        codes::STALE_MESH,
                        "mesh",
                        item.mesh.index,
                        item.mesh.generation,
                        item.entity.0,
                    ));
                }
                if material.is_none()
                    && reported.insert((1, item.material.index, item.material.generation))
                {
                    report.warnings.push(stale_item(
                        codes::STALE_MATERIAL,
                        "material",
                        item.material.index,
                        item.material.generation,
                        item.entity.0,
                    ));
                }
                continue;
            };
            report.stats.items_drawn += 1;
            if mesh.index_count == 0 {
                continue;
            }
            let world_box = mesh.aabb.map(|b| frame::transform_aabb(&b, &item.world));
            if let Some(b) = world_box {
                scene_bounds = Some(scene_bounds.map_or(b, |s| s.union(&b)));
            }
            let key = (material.variant(), item.material.index, item.mesh.index);
            if material.blend {
                let depth = world_box
                    .map_or(item.world.w_axis.truncate(), |b| b.center())
                    .distance_squared(camera_pos);
                blended.push((depth, i, key));
            } else {
                opaque.push((key, i));
            }
            if item.flags.contains(DrawFlags::CAST_SHADOWS) {
                if let Some(b) = world_box {
                    caster_bounds = Some(caster_bounds.map_or(b, |s| s.union(&b)));
                }
                casters.push((item.mesh.index, i));
            }
        }
        // Batch by pipeline, material and mesh; draw blended items back to front.
        opaque.sort_by_key(|&(key, i)| (key, i));
        blended.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        casters.sort_unstable();

        let mut instances: Vec<InstanceRaw> =
            Vec::with_capacity(opaque.len() + blended.len() + casters.len());
        let mut batches: Vec<Batch> = Vec::new();
        let ordered = opaque
            .iter()
            .map(|&(key, i)| (key, i))
            .chain(blended.iter().map(|&(_, i, key)| (key, i)));
        for (key, i) in ordered {
            let item = &snapshot.items[i];
            instances.push(InstanceRaw::new(
                item.world,
                item.flags.contains(DrawFlags::RECEIVE_SHADOWS),
                item.flags.contains(DrawFlags::SELECTED),
            ));
            let material = (item.material.index, item.material.generation);
            let mesh = (item.mesh.index, item.mesh.generation);
            match batches.last_mut() {
                Some(b) if b.variant == key.0 && b.material == material && b.mesh == mesh => {
                    b.instances += 1
                }
                _ => batches.push(Batch {
                    variant: key.0,
                    material,
                    mesh,
                    first_instance: instances.len() as u32 - 1,
                    instances: 1,
                }),
            }
        }
        let mut shadow_batches: Vec<Batch> = Vec::new();
        for &(_, i) in &casters {
            let item = &snapshot.items[i];
            instances.push(InstanceRaw::new(item.world, false, false));
            let mesh = (item.mesh.index, item.mesh.generation);
            match shadow_batches.last_mut() {
                Some(b) if b.mesh == mesh => b.instances += 1,
                _ => shadow_batches.push(Batch {
                    variant: 0,
                    material: (0, 0),
                    mesh,
                    first_instance: instances.len() as u32 - 1,
                    instances: 1,
                }),
            }
        }

        // Lights and the shadow fit.
        let lights = frame::select_lights(&snapshot.lights, camera_pos, self.settings.exposure);
        report.warnings.extend(lights.warnings);
        report.stats.point_lights = lights.points.len() as u32;
        let shadow_fit = match (lights.sun, scene_bounds, caster_bounds) {
            (Some(sun), Some(scene), Some(casters)) if sun.shadows => Some(frame::fit_shadow(
                sun.direction,
                &snapshot.camera,
                aspect,
                self.settings.shadow_distance,
                &scene,
                &casters,
                self.shadow.size,
            )),
            _ => None,
        };
        let exposure = self.settings.exposure;
        let mut points = [PointLightRaw::default(); frame::MAX_POINT_LIGHTS];
        points[..lights.points.len()].copy_from_slice(&lights.points);
        let uniform = FrameUniform {
            view_proj: view_proj.to_cols_array_2d(),
            light_view_proj: shadow_fit
                .map_or(Mat4::IDENTITY, |f| f.light_view_proj)
                .to_cols_array_2d(),
            camera_pos: camera_pos.extend(1.0).to_array(),
            sun_dir: lights
                .sun
                .map_or([0.0; 4], |s| (-s.direction).extend(1.0).to_array()),
            sun_color: lights.sun.map_or([0.0; 4], |s| {
                s.radiance
                    .extend(f32::from(u8::from(shadow_fit.is_some())))
                    .to_array()
            }),
            ambient_sky: frame::rgb_w(
                (Vec3::from_array(self.settings.ambient_sky) * exposure).to_array(),
                0.0,
            ),
            ambient_ground: frame::rgb_w(
                (Vec3::from_array(self.settings.ambient_ground) * exposure).to_array(),
                0.0,
            ),
            params: [
                1.0 / self.shadow.size as f32,
                shadow_fit.map_or(0.0, |f| f.texel_world * 1.5),
                lights.points.len() as f32,
                exposure,
            ],
            points,
        };
        queue.write_buffer(&self.frame_buffer, 0, bytemuck::bytes_of(&uniform));
        self.instances
            .write(device, queue, bytemuck::cast_slice(&instances));
        let line_vertices: Vec<LineVertex> = snapshot
            .debug_lines
            .iter()
            .flat_map(|l| {
                [
                    LineVertex {
                        position: l.a.to_array(),
                        color: l.color,
                    },
                    LineVertex {
                        position: l.b.to_array(),
                        color: l.color,
                    },
                ]
            })
            .collect();
        self.lines
            .write(device, queue, bytemuck::cast_slice(&line_vertices));
        report.stats.debug_lines = snapshot.debug_lines.len() as u32;

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("re_render frame"),
        });
        let instance_stride = size_of::<InstanceRaw>() as u64;
        if shadow_fit.is_some() {
            let shadow_frame = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("re_render shadow frame"),
                layout: &self.layouts.shadow_frame,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.frame_buffer.as_entire_binding(),
                }],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("re_render shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow.view,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.shadow_pipeline);
            pass.set_bind_group(0, &shadow_frame, &[]);
            for batch in &shadow_batches {
                let mesh = self
                    .meshes
                    .get(batch.mesh.0, batch.mesh.1)
                    .expect("resolved above");
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                let start = u64::from(batch.first_instance) * instance_stride;
                pass.set_vertex_buffer(
                    1,
                    self.instances
                        .buffer
                        .slice(start..start + u64::from(batch.instances) * instance_stride),
                );
                pass.draw_indexed(0..mesh.index_count, 0, 0..batch.instances);
                report.stats.shadow_casters += batch.instances;
            }
        }

        self.pipelines_for(format, samples);
        self.frame_targets(width, height, format, samples);
        let pipelines = &self.pipelines[&(format, samples)];
        let targets = &self.targets[&(width, height, format, samples)];
        // The clear colour is linear; a gamma target stores it encoded.
        let mut clear = snapshot.clear.map(|c| if c.is_finite() { c } else { 0.0 });
        if stores_gamma(format) {
            for c in &mut clear[..3] {
                *c = f32::from(crate::capture::linear_to_srgb_u8(*c)) / 255.0;
            }
        }
        let [r, g, b, a] = clear.map(f64::from);
        {
            let (view, resolve_target) = match &targets.msaa {
                Some(msaa) => (msaa, Some(target)),
                None => (target, None),
            };
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("re_render scene"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color { r, g, b, a }),
                        store: if resolve_target.is_some() {
                            wgpu::StoreOp::Discard
                        } else {
                            wgpu::StoreOp::Store
                        },
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.shadow.frame_bind_group, &[]);
            // The pipeline, material and mesh currently bound.
            let mut current: (Option<usize>, Option<Slot>, Option<Slot>) = (None, None, None);
            for batch in &batches {
                if current.0 != Some(batch.variant) {
                    pass.set_pipeline(&pipelines.forward[batch.variant]);
                    current.0 = Some(batch.variant);
                }
                if current.1 != Some(batch.material) {
                    let material = self
                        .materials
                        .get(batch.material.0, batch.material.1)
                        .expect("resolved above");
                    pass.set_bind_group(1, &material.bind_group, &[]);
                    current.1 = Some(batch.material);
                }
                let mesh = self
                    .meshes
                    .get(batch.mesh.0, batch.mesh.1)
                    .expect("resolved above");
                if current.2 != Some(batch.mesh) {
                    pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                    pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                    current.2 = Some(batch.mesh);
                }
                let start = u64::from(batch.first_instance) * instance_stride;
                pass.set_vertex_buffer(
                    1,
                    self.instances
                        .buffer
                        .slice(start..start + u64::from(batch.instances) * instance_stride),
                );
                pass.draw_indexed(0..mesh.index_count, 0, 0..batch.instances);
                report.stats.draw_calls += 1;
                report.stats.triangles +=
                    u64::from(mesh.index_count / 3) * u64::from(batch.instances);
            }
            if !line_vertices.is_empty() {
                pass.set_pipeline(&pipelines.lines);
                pass.set_vertex_buffer(
                    0,
                    self.lines
                        .buffer
                        .slice(..(line_vertices.len() * size_of::<LineVertex>()) as u64),
                );
                pass.draw(0..line_vertices.len() as u32, 0..1);
            }
        }
        queue.submit([encoder.finish()]);
        self.last_frame = report.stats;
        Ok(report)
    }

    fn check_target(&self, texture: &wgpu::Texture) -> Result<(), Diagnostic> {
        let format = texture.format();
        let problem = if !texture
            .usage()
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        {
            Some("its texture lacks RENDER_ATTACHMENT usage".to_string())
        } else if texture.sample_count() != 1 || texture.dimension() != wgpu::TextureDimension::D2 {
            Some("it must be a single-sample 2D texture".to_string())
        } else if format.is_depth_stencil_format()
            || !self
                .adapter
                .get_texture_format_features(format)
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        {
            Some(format!("the adapter cannot render to {format:?}"))
        } else {
            None
        };
        match problem {
            Some(why) => Err(Diagnostic::error(
                codes::INVALID_TARGET,
                format!("cannot render into this view: {why}"),
            )),
            None => Ok(()),
        }
    }

    // ----- UI ------------------------------------------------------------------

    /// Paints a tessellated egui frame over `target`, keeping what is already there.
    ///
    /// `ui.size_px` must be the target's size (`RE6005` otherwise). Texture updates
    /// in `ui.textures_delta` are applied before painting and frees after. The
    /// result looks the same on every backend (see the `ui` module).
    pub fn paint_ui(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        ui: &UiFrame,
    ) -> Result<(), Diagnostic> {
        self.ui.paint(device, queue, target, ui)
    }

    /// Makes a texture view usable in egui (for example a scene [`Capture`](crate::Capture)
    /// shown in an editor viewport) and returns its id.
    ///
    /// egui treats texture colour as gamma-encoded. A view of a non-sRGB texture
    /// that the renderer drew into (such as a `Capture` in `Rgba8Unorm`) holds
    /// gamma-encoded colour on every backend; an sRGB view would show too dark.
    pub fn register_ui_texture(&mut self, view: &wgpu::TextureView) -> egui::TextureId {
        self.ui
            .egui()
            .register_native_texture(&self.device, view, wgpu::FilterMode::Linear)
    }

    /// Points a registered egui texture at another view (after a resize).
    pub fn update_ui_texture(&mut self, id: egui::TextureId, view: &wgpu::TextureView) {
        self.ui.egui().update_egui_texture_from_wgpu_texture(
            &self.device,
            view,
            wgpu::FilterMode::Linear,
            id,
        );
    }

    /// Forgets a texture registered with [`Renderer::register_ui_texture`].
    pub fn free_ui_texture(&mut self, id: egui::TextureId) {
        self.ui.egui().free_texture(&id);
    }

    /// The queue the renderer was created with (uploads go through it).
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
}

fn stale(code: re_core::Code, kind: &str, index: u32, generation: u32) -> Diagnostic {
    Diagnostic::error(
        code,
        format!("the {kind} handle {index}:{generation} was released or never issued"),
    )
    .with_hint(format!(
        "a released {kind} handle cannot be used again; upload the {kind} again for a new handle"
    ))
}

fn stale_item(
    code: re_core::Code,
    kind: &str,
    index: u32,
    generation: u32,
    entity: u64,
) -> Diagnostic {
    let mut d = stale(code, kind, index, generation);
    d.severity = re_core::Severity::Warning;
    d.message = format!(
        "a draw item of entity {entity} uses the {kind} handle {index}:{generation}, which was released or never issued; the item is skipped"
    );
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mip_chains_halve_to_one_pixel() {
        let pixels: Vec<u8> = (0..5 * 3)
            .flat_map(|i| [i as u8 * 10, 0, 255, 255])
            .collect();
        let levels = mip_chain(5, 3, &pixels, true);
        let sizes: Vec<(u32, u32)> = levels.iter().map(|(w, h, _)| (*w, *h)).collect();
        assert_eq!(sizes, vec![(5, 3), (2, 1), (1, 1)]);
        for (w, h, p) in &levels {
            assert_eq!(p.len(), (w * h * 4) as usize);
        }
        // Averaging white and black in linear space gives sRGB 188, not 128.
        let bw = [255, 255, 255, 255, 0, 0, 0, 255];
        assert_eq!(&mip_chain(2, 1, &bw, true)[1].2, &[188, 188, 188, 255]);
        assert_eq!(&mip_chain(2, 1, &bw, false)[1].2, &[128, 128, 128, 255]);
    }

    #[test]
    fn gamma_formats_are_the_8_and_10_bit_unorm_ones() {
        assert!(stores_gamma(wgpu::TextureFormat::Bgra8Unorm));
        assert!(!stores_gamma(wgpu::TextureFormat::Bgra8UnormSrgb));
        assert!(!stores_gamma(wgpu::TextureFormat::Rgba16Float));
    }
}
