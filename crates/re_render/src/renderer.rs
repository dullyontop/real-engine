//! The forward renderer: GPU resources behind generation-checked handles, the
//! frame's passes and egui painting.
//!
//! A frame is prepared (`prepare`: items resolved, culled and batched, the
//! shadow cascades fitted, the frame's buffers written), then recorded: the
//! shadow cascades (`shadow`), the scene pass (`scene`), and the post chain,
//! highlights and on-top world quads (`post`).

use std::borrow::Cow;
use std::collections::HashMap;
use std::mem::size_of;
use std::num::NonZeroU64;
use std::time::Duration;

use glam::{Mat4, Vec3};
use re_core::Diagnostic;
use re_core::time::Instant;
use re_types::mesh::{Aabb, MeshData, SkinWeights};
use re_types::render::{
    AlphaMode, InstanceCluster, InstanceSetHandle, MaterialDesc, MaterialHandle, MeshHandle,
    Placement, RenderSnapshot, TextureData, TextureFormat, TextureHandle, TextureRef,
};

use crate::extras::{EffectVertex, MAX_HIGHLIGHTS, SceneExtras};
use crate::frame::{self, FrameUniform, InstanceRaw, LineVertex, MaterialUniform};
use crate::gpu::Gpu;
use crate::post::{DEPTH_COMPARE, DEPTH_COMPARE_EQUAL, DEPTH_FORMAT, HDR_FORMAT, PostChain};
use crate::slots::Slots;
use crate::ui::{UiFrame, UiPainter};
use crate::{capture, codes, instances, shader};

mod post;
mod prepare;
mod reload;
mod scene;
mod shadow;

pub(crate) use prepare::Slot;
pub use reload::ShaderReload;
pub(crate) use scene::Drawn;

const SHADOW_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
const MASK_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::R8Uint;
/// The width of a highlight's outline, in pixels.
const OUTLINE_PX: f32 = 2.0;
const MSAA_SAMPLES: u32 = 4;

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

/// A linear and a gamma target format: pipelines rebuilt from a reloaded shader
/// are made for both as well as for the formats drawn into so far, so the
/// device checks every entry point before a frame needs one (ADR-0038).
const CHECK_FORMATS: [wgpu::TextureFormat; 2] = [
    wgpu::TextureFormat::Rgba8UnormSrgb,
    wgpu::TextureFormat::Rgba8Unorm,
];

/// The formats `used` so far and [`CHECK_FORMATS`], each once.
pub(crate) fn with_checks<'a>(
    used: impl Iterator<Item = &'a wgpu::TextureFormat>,
) -> Vec<wgpu::TextureFormat> {
    let mut formats: Vec<wgpu::TextureFormat> = used.copied().collect();
    for format in CHECK_FORMATS {
        if !formats.contains(&format) {
            formats.push(format);
        }
    }
    formats
}

/// Renderer options that are not part of a [`RenderSnapshot`] (exposure and the
/// post chain are, in [`RenderSnapshot::post`]).
#[derive(Clone, Debug, PartialEq)]
pub struct RendererSettings {
    /// Ambient light from above, linear RGB (same units as a directional light).
    pub ambient_sky: [f32; 3],
    /// Ambient light from below, linear RGB.
    pub ambient_ground: [f32; 3],
    /// The side of each cascade's square shadow map in texels (the cascades
    /// and their reach are in [`RenderSnapshot::shadows`]).
    pub shadow_map_size: u32,
    /// Use 4x MSAA when the adapter supports it for the target format.
    pub msaa: bool,
}

impl Default for RendererSettings {
    fn default() -> Self {
        RendererSettings {
            ambient_sky: [0.16, 0.19, 0.24],
            ambient_ground: [0.08, 0.07, 0.06],
            shadow_map_size: 2048,
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
    /// Draw items left out because their bounds are outside the view.
    pub items_culled: u32,
    /// Instances drawn into the shadow map, summed over the cascades.
    pub shadow_casters: u32,
    /// Shadow cascades drawn.
    pub shadow_cascades: u32,
    /// Triangles submitted in the scene pass.
    pub triangles: u64,
    /// Debug lines drawn.
    pub debug_lines: u32,
    /// Point lights drawn.
    pub point_lights: u32,
    /// Spot lights drawn.
    pub spot_lights: u32,
    /// Sprite and ribbon quads drawn.
    pub effect_quads: u32,
    /// Highlights drawn.
    pub highlights: u32,
    /// MSAA sample count of the scene pass.
    pub samples: u32,
    /// World quads drawn.
    pub world_quads: u32,
    /// Passes of the post chain, the final one included.
    pub post_passes: u32,
    /// Skinned draw items drawn (also counted in `items_drawn`).
    pub skinned_items: u32,
    /// Instance sets with a cluster drawn in the scene pass (ADR-0052).
    pub instance_sets: u32,
    /// Copies of instance set parts drawn in the scene pass (their draws are
    /// in `draw_calls` and `triangles`, not in `items_drawn`).
    pub set_instances: u32,
    /// Instance set clusters drawn in the scene pass, counted once for each
    /// part drawn there.
    pub set_clusters: u32,
    /// Instance set clusters left out of the scene pass by the view or by a
    /// part's distance range, counted once for each part.
    pub set_clusters_culled: u32,
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

/// The time the renderer spent drawing on the calling thread, from
/// [`Renderer::take_draw_time`] (ADR-0038).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DrawTime {
    /// Preparing, encoding and finishing the command buffers of
    /// [`Renderer::render`], [`Renderer::render_with`] and
    /// [`Renderer::paint_ui`]: the work a render thread would take over.
    pub encode: Duration,
    /// Inside `Queue::submit`. Software rasterisers and some GL drivers wait
    /// there until the GPU is done with the previous frame, so on them this is
    /// mostly waiting.
    pub submit: Duration,
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
    /// Uploaded instance sets (ADR-0052).
    pub instance_sets: ResourceStats,
    /// The last rendered frame.
    pub last_frame: FrameStats,
}

pub(crate) struct GpuMesh {
    pub(crate) vertices: wgpu::Buffer,
    pub(crate) indices: wgpu::Buffer,
    pub(crate) index_count: u32,
    pub(crate) aabb: Option<Aabb>,
    bytes: u64,
    /// The skin stream of a mesh from [`Renderer::upload_skinned_mesh`].
    skin: Option<GpuSkin>,
}

struct GpuSkin {
    /// Four u16 joints and four unorm16 weights per vertex (see [`skin_layout`]).
    buffer: wgpu::Buffer,
    /// One more than the highest weighted joint index.
    joints_used: u32,
}

struct GpuTexture {
    view: wgpu::TextureView,
    bytes: u64,
}

pub(crate) struct GpuMaterial {
    pub(crate) bind_group: wgpu::BindGroup,
    pub(crate) blend: bool,
    double_sided: bool,
}

impl GpuMaterial {
    /// Index into [`ScenePipelines::forward`] and [`ScenePipelines::sets`].
    pub(crate) fn variant(&self) -> usize {
        usize::from(self.double_sided) | (usize::from(self.blend) << 1)
    }
}

/// Scene pipelines for one sample count (they draw into the HDR target).
struct ScenePipelines {
    /// Indexed by `double_sided | blend << 1`, then the same four skinned
    /// (`| SKINNED`) when the renderer can skin.
    forward: Vec<wgpu::RenderPipeline>,
    /// The same four for instance sets (ADR-0052).
    sets: Vec<wgpu::RenderPipeline>,
    lines: wgpu::RenderPipeline,
    effects: wgpu::RenderPipeline,
    sky: wgpu::RenderPipeline,
    /// Depth-tested world quads.
    quads: wgpu::RenderPipeline,
}

/// The highlight mask's targets for one size.
struct MaskTargets {
    size: (u32, u32),
    mask: wgpu::TextureView,
    depth: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

/// `struct Highlights` in highlight.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct HighlightUniform {
    colors: [[f32; 4]; 2 * MAX_HIGHLIGHTS],
    params: [f32; 4],
}

/// `struct Quad` in quad.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadUniform {
    world: [[f32; 4]; 4],
    params: [f32; 4],
}

/// The sun's cascaded shadow map: a `Depth32Float` 2D array, one layer per cascade.
struct ShadowMap {
    size: u32,
    layers: u32,
    /// One single-layer view per layer, to render into.
    layer_views: Vec<wgpu::TextureView>,
    frame_bind_group: wgpu::BindGroup,
}

/// `struct Cascade` in shadow.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
struct CascadeUniform {
    light_view_proj: [[f32; 4]; 4],
}

pub(crate) struct Layouts {
    pub(crate) frame: wgpu::BindGroupLayout,
    pub(crate) shadow_frame: wgpu::BindGroupLayout,
    pub(crate) cascade: wgpu::BindGroupLayout,
    pub(crate) material: wgpu::BindGroupLayout,
    highlight: wgpu::BindGroupLayout,
    quad: wgpu::BindGroupLayout,
    forward: wgpu::PipelineLayout,
    quad_pipeline: wgpu::PipelineLayout,
    /// The shadow pass: one cascade's matrix at a dynamic offset.
    shadow: wgpu::PipelineLayout,
    /// The highlight mask: the frame in the vertex stage.
    mask: wgpu::PipelineLayout,
    frame_only: wgpu::PipelineLayout,
    highlight_pipeline: wgpu::PipelineLayout,
}

/// GPU skinning (ADR-0039), present when the adapter can read storage buffers
/// in vertex shaders.
struct Skinning {
    /// The joint palette: a read-only storage buffer of the vertex stage.
    palette_layout: wgpu::BindGroupLayout,
    /// frame, material, palette.
    forward_layout: wgpu::PipelineLayout,
    forward_shader: wgpu::ShaderModule,
    shadow_pipeline: wgpu::RenderPipeline,
    /// The skinned variants of [`Renderer::mask_pipelines`].
    mask_pipelines: [wgpu::RenderPipeline; 2],
    palette: GrowBuffer,
    /// cascade, palette: kept to rebuild `shadow_pipeline`.
    shadow_layout: wgpu::PipelineLayout,
    /// frame, palette: kept to rebuild `mask_pipelines`.
    mask_layout: wgpu::PipelineLayout,
    /// The palette's bind group, for the buffer and the size it binds.
    palette_group: Option<(wgpu::Buffer, u64, wgpu::BindGroup)>,
}

/// The bit of [`prepare::Batch::variant`] that marks a skinned batch.
const SKINNED: usize = 4;

/// The largest joint palette: palette indices travel in an instance's f32
/// `flags.w`, which holds every integer up to 2^24 exactly.
const MAX_PALETTE: usize = 1 << 24;

pub(crate) struct GrowBuffer {
    pub(crate) buffer: wgpu::Buffer,
    label: &'static str,
    usage: wgpu::BufferUsages,
}

impl GrowBuffer {
    pub(crate) fn new(
        device: &wgpu::Device,
        label: &'static str,
        usage: wgpu::BufferUsages,
    ) -> GrowBuffer {
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
    pub(crate) fn write(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, data: &[u8]) {
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

/// The sun's shadow at one point, from [`Renderer::probe_sun_shadow`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadowProbe {
    /// How much of the sun reaches the point, 0 (shadowed) to 1 (lit).
    pub visibility: f32,
    /// The cascade the point is in; `None` beyond the shadow distance or
    /// without a shadowed sun.
    pub cascade: Option<u8>,
    /// How far the point is into the band where its cascade fades into the
    /// next (0: not in it, 1: at its end).
    pub blend: f32,
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
    effects_shader: wgpu::ShaderModule,
    sky_shader: wgpu::ShaderModule,
    highlight_shader: wgpu::ShaderModule,
    quad_shader: wgpu::ShaderModule,
    shadow_pipeline: wgpu::RenderPipeline,
    /// The depth-only pass and the numbering mask pipelines.
    mask_pipelines: [wgpu::RenderPipeline; 2],
    skinning: Option<Skinning>,
    /// Whether `RE6040` was given.
    warned_no_skinning: bool,
    /// Whether the device takes BC formats; without them, block-compressed
    /// textures are decoded at upload (ADR-0047).
    bc_native: bool,
    /// Whether decoding block-compressed textures was logged.
    warned_bc_decode: bool,
    highlight_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    mask_targets: Option<MaskTargets>,
    highlight_buffer: wgpu::Buffer,
    /// The frame uniform for the highlight mask's vertex stage.
    mask_frame_group: wgpu::BindGroup,
    pipelines: HashMap<u32, ScenePipelines>,
    /// On-top world quads, by output format.
    top_quad_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    post: PostChain,
    frame_buffer: wgpu::Buffer,
    shadow_sampler: wgpu::Sampler,
    material_sampler: wgpu::Sampler,
    quad_sampler: wgpu::Sampler,
    shadow: ShadowMap,
    /// Each cascade's [`CascadeUniform`], at aligned offsets.
    cascade_uniforms: GrowBuffer,
    /// The shadow probe's pipeline, made on first use.
    probe: Option<wgpu::RenderPipeline>,
    instances: GrowBuffer,
    lines: GrowBuffer,
    effect_vertices: GrowBuffer,
    effect_indices: GrowBuffer,
    /// The quads whose indices `effect_indices` holds.
    effect_index_quads: usize,
    quad_uniforms: GrowBuffer,
    /// The frame being drawn, its lists kept between frames.
    frame: prepare::PreparedFrame,
    /// Bind groups kept by their inputs.
    bind_groups: prepare::BindGroups,
    defaults: Defaults,
    meshes: Slots<GpuMesh>,
    textures: Slots<GpuTexture>,
    materials: Slots<GpuMaterial>,
    /// The uploaded instance sets and their pipelines (ADR-0052).
    sets: instances::Sets,
    ui: UiPainter,
    last_frame: FrameStats,
    draw_time: DrawTime,
    /// The shader files, with any set at run time (ADR-0038).
    sources: shader::ShaderSources,
    /// Each root shader file, preprocessed, as its pipelines were last built.
    built: HashMap<&'static str, String>,
}

impl std::fmt::Debug for Renderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Renderer")
            .field("settings", &self.settings)
            .field("stats", &self.stats())
            .finish_non_exhaustive()
    }
}

/// What a pipeline's vertex stage draws: draw items, skinned draw items or
/// instance sets (ADR-0052).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Geometry {
    Items,
    Skinned,
    Sets,
}

impl Geometry {
    /// The entry point of `pass`'s vertex stage for this geometry:
    /// `vs_shadow`, `vs_shadow_skinned` or `vs_shadow_set` for `shadow`.
    fn vertex_entry(self, pass: &str) -> String {
        match self {
            Geometry::Items => format!("vs_{pass}"),
            Geometry::Skinned => format!("vs_{pass}_skinned"),
            Geometry::Sets => format!("vs_{pass}_set"),
        }
    }

    /// The vertex buffers: `mesh` (slot 0), then each instance (slot 1) and a
    /// skinned mesh's skin (slot 2), or a set's placements (slot 1).
    fn buffers(
        self,
        mesh: wgpu::VertexBufferLayout<'static>,
    ) -> Vec<Option<wgpu::VertexBufferLayout<'static>>> {
        match self {
            Geometry::Items => vec![Some(mesh), Some(instance_layout())],
            Geometry::Skinned => vec![Some(mesh), Some(instance_layout()), Some(skin_layout())],
            Geometry::Sets => vec![Some(mesh), Some(instances::placement_layout())],
        }
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

/// The highlight mask's pipelines: the depth-only pass (0) and the numbering
/// pass (1), from `mask.wgsl`, `mask_skinned.wgsl` or `mask_set.wgsl`.
pub(crate) fn mask_pipelines(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    geometry: Geometry,
) -> [wgpu::RenderPipeline; 2] {
    let buffers = geometry.buffers(position_only_layout());
    let entry = geometry.vertex_entry("mask");
    [DEPTH_COMPARE, DEPTH_COMPARE_EQUAL]
        .into_iter()
        .enumerate()
        .map(|(i, compare)| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("re_render highlight mask"),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some(&entry),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &buffers,
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(compare),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs_mask"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: MASK_FORMAT,
                        blend: None,
                        // The depth-only pass (index 0) writes no number.
                        write_mask: if i == 0 {
                            wgpu::ColorWrites::empty()
                        } else {
                            wgpu::ColorWrites::ALL
                        },
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap_or_else(|_| unreachable!("two mask pipelines"))
}

/// The shadow pass's pipeline, from `shadow.wgsl`, `shadow_skinned.wgsl` or
/// `shadow_set.wgsl`.
pub(crate) fn shadow_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
    geometry: Geometry,
) -> wgpu::RenderPipeline {
    let buffers = geometry.buffers(position_only_layout());
    let entry = geometry.vertex_entry("shadow");
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("re_render shadow"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some(&entry),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &buffers,
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
    })
}

/// The shadow probe's target format.
const PROBE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The shadow probe's pipeline, from `probe.wgsl`: one point per probed position.
fn probe_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    module: &wgpu::ShaderModule,
) -> wgpu::RenderPipeline {
    const ATTRIBUTES: [wgpu::VertexAttribute; 3] =
        wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("re_render shadow probe"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module,
            entry_point: Some("vs_probe"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: 7 * 4,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &ATTRIBUTES,
            })],
        },
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::PointList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        fragment: Some(wgpu::FragmentState {
            module,
            entry_point: Some("fs_probe"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            targets: &[Some(PROBE_FORMAT.into())],
        }),
        multiview_mask: None,
        cache: None,
    })
}

/// The skin stream: joint indices at location 12, weights at 13.
fn skin_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 2] =
        wgpu::vertex_attr_array![12 => Uint16x4, 13 => Unorm16x4];
    wgpu::VertexBufferLayout {
        array_stride: 16,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

fn effect_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 4] = wgpu::vertex_attr_array![
        0 => Float32x3, 1 => Float32x2, 2 => Float32x4, 3 => Float32x2
    ];
    wgpu::VertexBufferLayout {
        array_stride: size_of::<EffectVertex>() as u64,
        step_mode: wgpu::VertexStepMode::Vertex,
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
                    view_dimension: wgpu::TextureViewDimension::D2Array,
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
    let cascade = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("re_render cascade"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: true,
                min_binding_size: NonZeroU64::new(size_of::<CascadeUniform>() as u64),
            },
            count: None,
        }],
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
        bind_group_layouts: &[Some(&cascade)],
        immediate_size: 0,
    });
    let mask = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("re_render highlight mask"),
        bind_group_layouts: &[Some(&shadow_frame)],
        immediate_size: 0,
    });
    let frame_only = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("re_render frame only"),
        bind_group_layouts: &[Some(&frame)],
        immediate_size: 0,
    });
    let highlight = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("re_render highlight"),
        entries: &[
            uniform_entry(
                0,
                wgpu::ShaderStages::FRAGMENT,
                size_of::<HighlightUniform>(),
            ),
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Uint,
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
        ],
    });
    let highlight_pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("re_render highlight"),
        bind_group_layouts: &[Some(&highlight)],
        immediate_size: 0,
    });
    let quad = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("re_render quad"),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: both,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(size_of::<QuadUniform>() as u64),
                },
                count: None,
            },
            texture_entry(1),
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let quad_pipeline = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some("re_render quad"),
        bind_group_layouts: &[Some(&frame), Some(&quad)],
        immediate_size: 0,
    });
    Layouts {
        frame,
        shadow_frame,
        cascade,
        material,
        highlight,
        quad,
        forward,
        quad_pipeline,
        shadow,
        mask,
        frame_only,
        highlight_pipeline,
    }
}

/// True when skinned meshes can be posed on the GPU: the vertex stage can read a
/// storage buffer.
fn supports_skinning(adapter: &wgpu::Adapter, device: &wgpu::Device) -> bool {
    adapter
        .get_downlevel_capabilities()
        .flags
        .contains(wgpu::DownlevelFlags::VERTEX_STORAGE)
        && device.limits().max_storage_buffers_per_shader_stage > 0
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

/// Why `data` (which [`TextureData::is_well_formed`] refused) cannot be
/// uploaded.
fn malformed_texture(data: &TextureData) -> String {
    let (w, h, f) = (data.width, data.height, data.format);
    if !f.is_compressed() {
        let want = f.level_bytes(w, h);
        return if data.pixels.len() as u64 == want {
            format!(
                "a {w}x{h} RGBA8 texture carries no mip levels (they are made at upload), \
                 not {}",
                data.levels.len()
            )
        } else {
            format!(
                "a {w}x{h} RGBA8 texture needs {want} bytes of pixels, not {}",
                data.pixels.len()
            )
        };
    }
    if !w.is_multiple_of(4) || !h.is_multiple_of(4) {
        return format!(
            "a {w}x{h} {f:?} texture is not possible: block-compressed sizes are multiples of 4"
        );
    }
    let sizes = TextureData::mip_sizes(w, h);
    if data.levels.len() + 1 != sizes.len() {
        return format!(
            "a {w}x{h} {f:?} texture needs {} mip levels below the top one, not {}",
            sizes.len() - 1,
            data.levels.len()
        );
    }
    let blocks = std::iter::once(&data.pixels).chain(&data.levels);
    for (level, (&(lw, lh), b)) in sizes.iter().zip(blocks).enumerate() {
        let want = f.level_bytes(lw, lh);
        if b.len() as u64 != want {
            return format!(
                "level {level} ({lw}x{lh}) of a {f:?} texture needs {want} bytes of blocks, not {}",
                b.len()
            );
        }
    }
    format!("the {w}x{h} {f:?} texture data is malformed")
}

/// One level of block-compressed `blocks` decoded to `width` x `height` RGBA8,
/// for a device without BC formats (ADR-0047). BC5's two channels become red
/// and green, with blue 0 and alpha 255.
pub(crate) fn decode_blocks(
    format: TextureFormat,
    width: u32,
    height: u32,
    blocks: &[u8],
) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let blocks_wide = w.div_ceil(4);
    let mut out = vec![0u8; w * h * 4];
    for (i, block) in blocks.as_chunks::<16>().0.iter().enumerate() {
        let (bx, by) = (i % blocks_wide * 4, i / blocks_wide * 4);
        let mut rgba = [0u8; 64];
        if format == TextureFormat::Bc5Unorm {
            let mut rg = [0u8; 32];
            bcdec_rs::bc5(block, &mut rg, 8, false);
            for (px, c) in rgba
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(rg.as_chunks::<2>().0)
            {
                *px = [c[0], c[1], 0, 255];
            }
        } else {
            bcdec_rs::bc7(block, &mut rgba, 16);
        }
        // The block's pixels inside the level (a level under 4x4 uses part
        // of its one block).
        for y in 0..4.min(h - by) {
            let row = (by + y) * w + bx;
            let n = 4.min(w - bx);
            out[row * 4..(row + n) * 4].copy_from_slice(&rgba[y * 16..y * 16 + n * 4]);
        }
    }
    out
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
                    libm::powf((c + 0.055) / 1.055, 2.4)
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
        let mask_shader = shader::module(&device, "mask.wgsl");
        let highlight_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("re_render highlights"),
            size: size_of::<HighlightUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let skinning = supports_skinning(&gpu.adapter, &device).then(|| {
            let palette_layout =
                device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                    label: Some("re_render joint palette"),
                    entries: &[wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::VERTEX,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Storage { read_only: true },
                            has_dynamic_offset: false,
                            min_binding_size: NonZeroU64::new(size_of::<Mat4>() as u64),
                        },
                        count: None,
                    }],
                });
            let forward_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("re_render forward skinned"),
                bind_group_layouts: &[
                    Some(&layouts.frame),
                    Some(&layouts.material),
                    Some(&palette_layout),
                ],
                immediate_size: 0,
            });
            let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("re_render shadow skinned"),
                bind_group_layouts: &[Some(&layouts.cascade), Some(&palette_layout)],
                immediate_size: 0,
            });
            let mask_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("re_render highlight mask skinned"),
                bind_group_layouts: &[Some(&layouts.shadow_frame), Some(&palette_layout)],
                immediate_size: 0,
            });
            Skinning {
                shadow_pipeline: shadow_pipeline(
                    &device,
                    &shadow_layout,
                    &shader::module(&device, "shadow_skinned.wgsl"),
                    Geometry::Skinned,
                ),
                mask_pipelines: mask_pipelines(
                    &device,
                    &mask_layout,
                    &shader::module(&device, "mask_skinned.wgsl"),
                    Geometry::Skinned,
                ),
                forward_shader: shader::module(&device, "forward_skinned.wgsl"),
                palette: GrowBuffer::new(
                    &device,
                    "re_render joint palette",
                    wgpu::BufferUsages::STORAGE,
                ),
                palette_layout,
                forward_layout,
                shadow_layout,
                mask_layout,
                palette_group: None,
            }
        });
        let mask_pipelines = mask_pipelines(&device, &layouts.mask, &mask_shader, Geometry::Items);
        let shadow_pipeline =
            shadow_pipeline(&device, &layouts.shadow, &shadow_shader, Geometry::Items);
        let sets = instances::Sets::new(&device, &layouts);
        let frame_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("re_render frame"),
            size: size_of::<FrameUniform>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mask_frame_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("re_render mask frame"),
            layout: &layouts.shadow_frame,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: frame_buffer.as_entire_binding(),
            }],
        });
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("re_render shadow"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            compare: Some(wgpu::CompareFunction::LessEqual),
            ..Default::default()
        });
        let quad_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("re_render quad"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
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
            1,
        );
        let mut renderer = Renderer {
            instances: GrowBuffer::new(&device, "re_render instances", wgpu::BufferUsages::VERTEX),
            lines: GrowBuffer::new(&device, "re_render lines", wgpu::BufferUsages::VERTEX),
            effect_vertices: GrowBuffer::new(
                &device,
                "re_render effect vertices",
                wgpu::BufferUsages::VERTEX,
            ),
            effect_indices: GrowBuffer::new(
                &device,
                "re_render effect indices",
                wgpu::BufferUsages::INDEX,
            ),
            effect_index_quads: 0,
            quad_uniforms: GrowBuffer::new(&device, "re_render quads", wgpu::BufferUsages::UNIFORM),
            frame: prepare::PreparedFrame::default(),
            bind_groups: prepare::BindGroups::default(),
            sets,
            cascade_uniforms: GrowBuffer::new(
                &device,
                "re_render cascades",
                wgpu::BufferUsages::UNIFORM,
            ),
            probe: None,
            quad_shader: shader::module(&device, "quad.wgsl"),
            post: PostChain::new(&device),
            top_quad_pipelines: HashMap::new(),
            quad_sampler,
            effects_shader: shader::module(&device, "effects.wgsl"),
            sky_shader: shader::module(&device, "sky.wgsl"),
            highlight_shader: shader::module(&device, "highlight.wgsl"),
            mask_pipelines,
            skinning,
            warned_no_skinning: false,
            bc_native: device
                .features()
                .contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            warned_bc_decode: false,
            highlight_pipelines: HashMap::new(),
            mask_targets: None,
            highlight_buffer,
            mask_frame_group,
            ui: UiPainter::new(&device),
            draw_time: DrawTime::default(),
            adapter: gpu.adapter.clone(),
            device,
            queue,
            settings,
            layouts,
            forward_shader,
            lines_shader,
            shadow_pipeline,
            pipelines: HashMap::new(),
            frame_buffer,
            shadow_sampler,
            material_sampler,
            shadow,
            defaults,
            meshes: Slots::default(),
            textures: Slots::default(),
            materials: Slots::default(),
            last_frame: FrameStats::default(),
            sources: shader::ShaderSources::default(),
            built: reload::built_in_roots(),
        };
        let samples = renderer.sample_count();
        renderer.pipelines_for(samples);
        // The final pass for `format` is created with the first frame.
        let _ = format;
        renderer
    }

    /// True when skinned items are posed on the GPU. Otherwise (`RE6040`) they
    /// are drawn in their bind pose.
    pub fn supports_skinning(&self) -> bool {
        self.skinning.is_some()
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
        layers: u32,
    ) -> ShadowMap {
        let size = size.min(device.limits().max_texture_dimension_2d);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("re_render shadow map"),
            size: wgpu::Extent3d {
                width: size,
                height: size,
                // At least two layers: GL makes a one-layer texture a plain 2D
                // texture, which a 2D array view cannot sample.
                depth_or_array_layers: layers.max(2),
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: SHADOW_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("re_render shadow map"),
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });
        let layer_views = (0..layers)
            .map(|layer| {
                texture.create_view(&wgpu::TextureViewDescriptor {
                    label: Some("re_render shadow cascade"),
                    dimension: Some(wgpu::TextureViewDimension::D2),
                    base_array_layer: layer,
                    array_layer_count: Some(1),
                    ..Default::default()
                })
            })
            .collect();
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
            layers,
            layer_views,
            frame_bind_group,
        }
    }

    fn sample_count(&self) -> u32 {
        let supports = |f: wgpu::TextureFormat| {
            self.adapter
                .get_texture_format_features(f)
                .flags
                .sample_count_supported(MSAA_SAMPLES)
        };
        if self.settings.msaa && supports(HDR_FORMAT) && supports(DEPTH_FORMAT) {
            MSAA_SAMPLES
        } else {
            1
        }
    }

    fn pipelines_for(&mut self, samples: u32) -> &ScenePipelines {
        if !self.pipelines.contains_key(&samples) {
            let pipelines = self.create_pipelines(samples);
            self.pipelines.insert(samples, pipelines);
        }
        &self.pipelines[&samples]
    }

    fn create_pipelines(&self, samples: u32) -> ScenePipelines {
        let format = HDR_FORMAT;
        let multisample = wgpu::MultisampleState {
            count: samples,
            ..Default::default()
        };
        let forward = |variant: usize, geometry: Geometry| {
            let (double_sided, blend) = (variant & 1 != 0, variant & 2 != 0);
            let (layout, module) = match (geometry, &self.skinning) {
                (Geometry::Skinned, Some(s)) => (&s.forward_layout, &s.forward_shader),
                (Geometry::Sets, _) => (&self.sets.forward_layout, &self.sets.forward_shader),
                _ => (&self.layouts.forward, &self.forward_shader),
            };
            let buffers = geometry.buffers(mesh_vertex_layout());
            let entry = geometry.vertex_entry("forward");
            self.device
                .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("re_render forward"),
                    layout: Some(layout),
                    vertex: wgpu::VertexState {
                        module,
                        entry_point: Some(&entry),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &buffers,
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
                        depth_compare: Some(DEPTH_COMPARE),
                        stencil: wgpu::StencilState::default(),
                        bias: wgpu::DepthBiasState::default(),
                    }),
                    multisample,
                    fragment: Some(wgpu::FragmentState {
                        module,
                        entry_point: Some("fs_forward"),
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
                    depth_compare: Some(DEPTH_COMPARE_EQUAL),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample,
                fragment: Some(wgpu::FragmentState {
                    module: &self.lines_shader,
                    entry_point: Some("fs_line"),
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
        let effects = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("re_render effects"),
                layout: Some(&self.layouts.frame_only),
                vertex: wgpu::VertexState {
                    module: &self.effects_shader,
                    entry_point: Some("vs_effect"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[Some(effect_layout())],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(DEPTH_COMPARE),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample,
                fragment: Some(wgpu::FragmentState {
                    module: &self.effects_shader,
                    entry_point: Some("fs_effect"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
        let sky = self
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("re_render sky"),
                layout: Some(&self.layouts.frame_only),
                vertex: wgpu::VertexState {
                    module: &self.sky_shader,
                    entry_point: Some("vs_sky"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: Some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(wgpu::CompareFunction::Always),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample,
                fragment: Some(wgpu::FragmentState {
                    module: &self.sky_shader,
                    entry_point: Some("fs_sky"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            });
        ScenePipelines {
            forward: (0..if self.skinning.is_some() {
                2 * SKINNED
            } else {
                SKINNED
            })
                .map(|variant| {
                    let geometry = if variant & SKINNED != 0 {
                        Geometry::Skinned
                    } else {
                        Geometry::Items
                    };
                    forward(variant, geometry)
                })
                .collect(),
            sets: (0..SKINNED).map(|v| forward(v, Geometry::Sets)).collect(),
            lines,
            effects,
            sky,
            quads: self.quad_pipeline(format, samples, true),
        }
    }

    /// World quads into the HDR scene (`tested`, with the depth test) or over
    /// the finished image in `format`.
    fn quad_pipeline(
        &self,
        format: wgpu::TextureFormat,
        samples: u32,
        tested: bool,
    ) -> wgpu::RenderPipeline {
        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("re_render quad"),
                layout: Some(&self.layouts.quad_pipeline),
                vertex: wgpu::VertexState {
                    module: &self.quad_shader,
                    entry_point: Some("vs_quad"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: tested.then_some(wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(false),
                    depth_compare: Some(DEPTH_COMPARE),
                    stencil: wgpu::StencilState::default(),
                    bias: wgpu::DepthBiasState::default(),
                }),
                multisample: wgpu::MultisampleState {
                    count: samples,
                    ..Default::default()
                },
                fragment: Some(wgpu::FragmentState {
                    module: &self.quad_shader,
                    entry_point: Some(if stores_gamma(format) {
                        "fs_quad_gamma"
                    } else {
                        "fs_quad_linear"
                    }),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
    }

    fn highlight_pipeline(&mut self, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        if !self.highlight_pipelines.contains_key(&format) {
            let pipeline = self.create_highlight_pipeline(format);
            self.highlight_pipelines.insert(format, pipeline);
        }
        &self.highlight_pipelines[&format]
    }

    fn create_highlight_pipeline(&self, format: wgpu::TextureFormat) -> wgpu::RenderPipeline {
        self.device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("re_render highlight"),
                layout: Some(&self.layouts.highlight_pipeline),
                vertex: wgpu::VertexState {
                    module: &self.highlight_shader,
                    entry_point: Some("vs_fullscreen"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                fragment: Some(wgpu::FragmentState {
                    module: &self.highlight_shader,
                    entry_point: Some(if stores_gamma(format) {
                        "fs_highlight_gamma"
                    } else {
                        "fs_highlight_linear"
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
            })
    }

    fn mask_targets(&mut self, width: u32, height: u32) -> &MaskTargets {
        if self.mask_targets.as_ref().map(|m| m.size) != Some((width, height)) {
            let make = |label: &str, format: wgpu::TextureFormat, usage: wgpu::TextureUsages| {
                self.device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d {
                            width,
                            height,
                            depth_or_array_layers: 1,
                        },
                        mip_level_count: 1,
                        sample_count: 1,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage,
                        view_formats: &[],
                    })
                    .create_view(&wgpu::TextureViewDescriptor::default())
            };
            let mask = make(
                "re_render highlight mask",
                MASK_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            );
            let depth = make(
                "re_render highlight depth",
                DEPTH_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            );
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("re_render highlight"),
                layout: &self.layouts.highlight,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.highlight_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&mask),
                    },
                ],
            });
            self.mask_targets = Some(MaskTargets {
                size: (width, height),
                mask,
                depth,
                bind_group,
            });
        }
        self.mask_targets.as_ref().expect("created above")
    }

    // ----- Resources -----------------------------------------------------------

    /// Uploads a mesh. Fails with `RE6014` when the data is malformed. An empty mesh
    /// is accepted and draws nothing.
    pub fn upload_mesh(&mut self, mesh: &MeshData) -> Result<MeshHandle, Diagnostic> {
        self.create_mesh(mesh, None)
    }

    /// Uploads a skinned mesh: a mesh with four joints and weights per vertex
    /// (ADR-0039), drawn by [`SkinnedDrawItem`](re_types::render::SkinnedDrawItem)s.
    /// Fails with `RE6014` when the mesh or the weights are malformed (one entry
    /// per vertex, each vertex's weights summing to 65,535).
    ///
    /// The mesh can also be drawn by a plain draw item, in its bind pose.
    pub fn upload_skinned_mesh(
        &mut self,
        mesh: &MeshData,
        weights: &SkinWeights,
    ) -> Result<MeshHandle, Diagnostic> {
        self.create_mesh(mesh, Some(weights))
    }

    fn create_mesh(
        &mut self,
        mesh: &MeshData,
        weights: Option<&SkinWeights>,
    ) -> Result<MeshHandle, Diagnostic> {
        let bad = |message: String| Diagnostic::error(codes::BAD_MESH, message);
        if let Some(w) = weights
            && !w.is_well_formed(mesh, usize::from(u16::MAX) + 1)
        {
            return Err(bad(format!(
                "the skin weights do not fit the mesh: {} vertices, {} joint and {} weight \
                 entries, and each vertex's weights must sum to 65535",
                mesh.vertices.len(),
                w.joints.len(),
                w.weights.len()
            )));
        }
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
        let skin = weights.map(|w| {
            // Unweighted slots point at joint 0, so the shader never reads past
            // the item's palette run.
            let mut joints_used = 0;
            let skin_bytes: Vec<u8> = w
                .joints
                .iter()
                .zip(&w.weights)
                .flat_map(|(j, w)| {
                    let mut out = [0u16; 8];
                    for k in 0..4 {
                        if w[k] > 0 {
                            out[k] = j[k];
                            joints_used = joints_used.max(u32::from(j[k]) + 1);
                        }
                        out[4 + k] = w[k];
                    }
                    bytemuck::cast::<[u16; 8], [u8; 16]>(out)
                })
                .collect();
            GpuSkin {
                buffer: buffer(
                    "re_render mesh skin",
                    wgpu::BufferUsages::VERTEX,
                    &skin_bytes,
                ),
                joints_used,
            }
        });
        let bytes = vertices.size() + indices.size() + skin.as_ref().map_or(0, |s| s.buffer.size());
        let (index, generation) = self.meshes.insert(GpuMesh {
            vertices,
            indices,
            index_count,
            aabb: mesh.aabb(),
            bytes,
            skin,
        });
        Ok(MeshHandle::new(index, generation))
    }

    /// Uploads a texture with a full mip chain. RGBA8 data gets its mips here;
    /// block-compressed data brings its own (ADR-0047) and is uploaded as it is
    /// when the device has `TEXTURE_COMPRESSION_BC`, or decoded to RGBA8 first
    /// when it does not (logged once).
    ///
    /// Fails with `RE6015` when the data is malformed
    /// ([`TextureData::is_well_formed`]) or larger than the GPU allows.
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
                malformed_texture(data),
            ));
        }
        let compressed = data.format.is_compressed();
        let native = compressed && self.bc_native;
        let format = match (data.format, native) {
            (TextureFormat::Bc7Srgb, true) => wgpu::TextureFormat::Bc7RgbaUnormSrgb,
            (TextureFormat::Bc7Unorm, true) => wgpu::TextureFormat::Bc7RgbaUnorm,
            (TextureFormat::Bc5Unorm, true) => wgpu::TextureFormat::Bc5RgUnorm,
            (f, _) if f.is_srgb() => wgpu::TextureFormat::Rgba8UnormSrgb,
            _ => wgpu::TextureFormat::Rgba8Unorm,
        };
        let levels: Vec<(u32, u32, Cow<'_, [u8]>)> = if compressed {
            if !native && !self.warned_bc_decode {
                self.warned_bc_decode = true;
                tracing::info!(
                    "the GPU has no BC texture formats; block-compressed textures are decoded \
                     to RGBA8 at upload"
                );
            }
            let blocks = std::iter::once(&data.pixels).chain(&data.levels);
            TextureData::mip_sizes(data.width, data.height)
                .into_iter()
                .zip(blocks)
                .map(|((w, h), b)| {
                    let level = if native {
                        Cow::Borrowed(b.as_slice())
                    } else {
                        Cow::Owned(decode_blocks(data.format, w, h, b))
                    };
                    (w, h, level)
                })
                .collect()
        } else {
            mip_chain(data.width, data.height, &data.pixels, data.format.is_srgb())
                .into_iter()
                .map(|(w, h, p)| (w, h, Cow::Owned(p)))
                .collect()
        };
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
            // Block rows for a compressed format, whose copies cover whole
            // blocks (a level smaller than a block is stored as one).
            let (bytes_per_row, rows, extent) = if native {
                let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
                (bw * 16, bh, (bw * 4, bh * 4))
            } else {
                (w * 4, *h, (*w, *h))
            };
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
                    bytes_per_row: Some(bytes_per_row),
                    rows_per_image: Some(rows),
                },
                wgpu::Extent3d {
                    width: extent.0,
                    height: extent.1,
                    depth_or_array_layers: 1,
                },
            );
        }
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let (index, generation) = self.textures.insert(GpuTexture { view, bytes });
        Ok(TextureHandle::new(index, generation))
    }

    /// A texture handle that samples `capture`, for a material or a
    /// [`WorldQuad`](re_types::render::WorldQuad) (a GUI painted with
    /// [`Renderer::paint_ui`] shown on a surface in the world, say).
    ///
    /// The handle reads the capture's current contents each frame. Release it
    /// with [`Renderer::release_texture`] before dropping the capture; its
    /// memory belongs to the capture, so it counts no bytes in [`RenderStats`].
    /// A frame must not draw a capture into itself. Use an sRGB capture
    /// ([`Capture::new`](crate::Capture::new)): a non-sRGB one holds
    /// gamma-encoded colour, which would be sampled as linear and look washed
    /// out.
    pub fn capture_texture(&mut self, capture: &crate::Capture) -> TextureHandle {
        let (index, generation) = self.textures.insert(GpuTexture {
            view: capture.view().clone(),
            bytes: 0,
        });
        TextureHandle::new(index, generation)
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
            .map(|_| self.bind_groups.forget_texture(handle.index))
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

    /// Uploads the placements of an instance set (ADR-0052), in the order of
    /// their `clusters`, which cover them: the first cluster holds the first
    /// `count` placements, the next one the `count` after them, and so on.
    /// Each [`InstanceSet`](re_types::render::InstanceSet) of a snapshot that
    /// names the handle draws its parts at every placement, culled cluster by
    /// cluster.
    ///
    /// Fails with `RE6100` when the set is malformed (no placements, an empty
    /// cluster, clusters that do not add up to the placements or whose bounds
    /// and largest scale do not hold them, a position, scale or bound that is
    /// not finite, or a zero rotation), and with `RE6101` when its placements
    /// do not fit in one of the GPU's buffers.
    pub fn upload_instance_set(
        &mut self,
        placements: &[Placement],
        clusters: &[InstanceCluster],
    ) -> Result<InstanceSetHandle, Diagnostic> {
        self.sets
            .upload(&self.device, &self.queue, placements, clusters)
    }

    /// Releases an instance set. Fails with `RE6104` for a released or unknown
    /// handle.
    pub fn release_instance_set(&mut self, handle: InstanceSetHandle) -> Result<(), Diagnostic> {
        self.sets.release(handle)
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
            instance_sets: self.sets.stats(),
            last_frame: self.last_frame,
        }
    }

    // ----- Frames --------------------------------------------------------------

    /// Draws `snapshot` into `target`, clearing it first with `snapshot.clear`.
    ///
    /// The scene is drawn in linear HDR colour into the renderer's own
    /// `Rgba16Float` target, then the post chain of `snapshot.post` (sun rays,
    /// bloom, blur, colour grading, tone mapping) writes the result into
    /// `target`; on-top world quads and highlights follow, in display colours.
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
        self.render_with(device, queue, target, snapshot, &SceneExtras::default())
    }

    /// Draws `snapshot` like [`Renderer::render`], with the environment, lights,
    /// sprites, ribbons and highlights of `extras` (see [`SceneExtras`]).
    ///
    /// More than [`MAX_HIGHLIGHTS`](crate::MAX_HIGHLIGHTS) highlights give an
    /// `RE6018` warning and the later ones are not drawn; an entity in two
    /// highlights shows the first.
    pub fn render_with(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        snapshot: &RenderSnapshot,
        extras: &SceneExtras,
    ) -> Result<FrameReport, Diagnostic> {
        let start = Instant::now();
        let submitted = self.draw_time.submit;
        let report = self.draw_frame(device, queue, target, snapshot, extras);
        let waited = self.draw_time.submit - submitted;
        self.draw_time.encode += start.elapsed().saturating_sub(waited);
        report
    }

    fn draw_frame(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        snapshot: &RenderSnapshot,
        extras: &SceneExtras,
    ) -> Result<FrameReport, Diagnostic> {
        let texture = target.texture();
        self.check_target(texture)?;
        let size = (texture.width(), texture.height());
        let view = prepare::View::new(snapshot, size)?;
        let mut report = FrameReport::default();
        // The frame's lists are taken for the frame and put back after it.
        let mut frame = std::mem::take(&mut self.frame);
        self.prepare(
            device,
            queue,
            &mut frame,
            &view,
            size,
            snapshot,
            extras,
            &mut report,
        );
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("re_render frame"),
        });
        self.record_shadows(&mut encoder, &frame, &mut report);
        self.record_scene(&mut encoder, &frame, &mut report);
        self.record_post(
            device,
            queue,
            &mut encoder,
            &frame,
            snapshot,
            extras,
            target,
            &mut report,
        );
        let commands = encoder.finish();
        let submitting = Instant::now();
        queue.submit([commands]);
        self.draw_time.submit += submitting.elapsed();
        frame.clear();
        self.frame = frame;
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

    /// The sun's shadow at world points `points` on surfaces facing `normal`,
    /// as the last frame drew it: the same lookup the lit passes make, so tests
    /// and tools can check shadows without reading pixels.
    pub fn probe_sun_shadow(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        points: &[Vec3],
        normal: Vec3,
    ) -> Result<Vec<ShadowProbe>, Diagnostic> {
        let width = points.len() as u32;
        if points.is_empty() || width > device.limits().max_texture_dimension_2d {
            return Err(Diagnostic::error(
                codes::INVALID_TARGET,
                format!(
                    "{} points cannot be probed: 1 to {} at a time",
                    points.len(),
                    device.limits().max_texture_dimension_2d
                ),
            ));
        }
        let pipeline = self.probe.get_or_insert_with(|| {
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("probe.wgsl"),
                source: wgpu::ShaderSource::Wgsl(self.built["probe.wgsl"].as_str().into()),
            });
            probe_pipeline(device, &self.layouts.frame_only, &module)
        });
        let vertices: Vec<f32> = points
            .iter()
            .enumerate()
            .flat_map(|(i, p)| {
                let x = (2 * i + 1) as f32 / width as f32 - 1.0;
                [p.x, p.y, p.z, normal.x, normal.y, normal.z, x]
            })
            .collect();
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("re_render shadow probe"),
            size: (vertices.len() * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        queue.write_buffer(&buffer, 0, bytemuck::cast_slice(&vertices));
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("re_render shadow probe"),
            size: wgpu::Extent3d {
                width,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: PROBE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("re_render shadow probe"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("re_render shadow probe"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &self.shadow.frame_bind_group, &[]);
            pass.set_vertex_buffer(0, buffer.slice(..));
            pass.draw(0..width, 0..1);
        }
        queue.submit([encoder.finish()]);
        let bytes = capture::read_texture(device, queue, &texture, wgpu::TextureAspect::All)?;
        Ok(bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|px| {
                let channel = |i: usize| f16_to_f32(u16::from_le_bytes([px[2 * i], px[2 * i + 1]]));
                let cascade = channel(1).round();
                ShadowProbe {
                    visibility: channel(0),
                    cascade: (cascade >= 1.0).then(|| cascade as u8 - 1),
                    blend: channel(2),
                }
            })
            .collect())
    }

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
        let start = Instant::now();
        let painted = self.ui.paint(device, queue, target, ui);
        let waited = *painted.as_ref().unwrap_or(&Duration::ZERO);
        self.draw_time.submit += waited;
        self.draw_time.encode += start.elapsed().saturating_sub(waited);
        painted.map(|_| ())
    }

    /// The time spent drawing since the last call (then zero): PerfReport's
    /// `render_submit` and `queue_submit` (ADR-0038).
    pub fn take_draw_time(&mut self) -> DrawTime {
        std::mem::take(&mut self.draw_time)
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

pub(crate) fn stale(code: re_core::Code, kind: &str, index: u32, generation: u32) -> Diagnostic {
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

/// An IEEE half-precision float's value.
fn f16_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exponent = i32::from((h >> 10) & 0x1f);
    let mantissa = f32::from(h & 0x3ff);
    sign * match exponent {
        0 => mantissa * (1.0 / 16_777_216.0),
        31 if mantissa == 0.0 => f32::INFINITY,
        31 => f32::NAN,
        e => (1.0 + mantissa / 1024.0) * pow2(e - 15),
    }
}

/// 2^e for small `e`, exactly.
fn pow2(e: i32) -> f32 {
    if e >= 0 {
        (1u32 << e) as f32
    } else {
        1.0 / (1u32 << -e) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use block_compression::{BC7Settings, CompressionVariant, decode, encode};

    /// `width` x `height` RGBA8 in which every channel varies, alpha included.
    fn pattern(width: u32, height: u32) -> Vec<u8> {
        (0..width * height)
            .flat_map(|i| {
                let (x, y) = (i % width, i / width);
                [
                    (x * 37 % 256) as u8,
                    (y * 59 % 256) as u8,
                    ((x + y) * 23 % 256) as u8,
                    (255 - (x * y * 13) % 128) as u8,
                ]
            })
            .collect()
    }

    /// `rgba` as blocks; the sizes are multiples of 4.
    fn compress(variant: CompressionVariant, width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let mut blocks = vec![0; variant.blocks_byte_size(width, height)];
        encode::compress_rgba8(variant, rgba, &mut blocks, width, height, width * 4);
        blocks
    }

    const BC7: CompressionVariant = CompressionVariant::BC7(BC7Settings::alpha_very_fast());

    #[test]
    fn bc7_blocks_decode_like_the_encoders_own_decoder() {
        let blocks = compress(BC7, 8, 12, &pattern(8, 12));
        let mut want = vec![0; 8 * 12 * 4];
        decode::decompress_blocks_as_rgba8(BC7, 8, 12, &blocks, &mut want);
        assert_eq!(decode_blocks(TextureFormat::Bc7Srgb, 8, 12, &blocks), want);
        assert_eq!(decode_blocks(TextureFormat::Bc7Unorm, 8, 12, &blocks), want);
    }

    #[test]
    fn bc5_blocks_decode_to_red_and_green() {
        let bc5 = CompressionVariant::BC5;
        let blocks = compress(bc5, 8, 8, &pattern(8, 8));
        let mut want = vec![0; 8 * 8 * 4];
        decode::decompress_blocks_as_rgba8(bc5, 8, 8, &blocks, &mut want);
        let got = decode_blocks(TextureFormat::Bc5Unorm, 8, 8, &blocks);
        // The decoders round the values between the endpoints differently
        // (bcdec_rs adds one before dividing, block_compression truncates), so
        // a channel may be one level apart.
        for (got, want) in got.as_chunks::<4>().0.iter().zip(want.as_chunks::<4>().0) {
            let near = |c: usize| got[c].abs_diff(want[c]) <= 1;
            assert!(near(0) && near(1), "{got:?} decoded where {want:?} was");
            assert_eq!(got[2..], [0, 255]);
        }
    }

    #[test]
    fn levels_smaller_than_a_block_are_its_top_left_corner() {
        let block = compress(BC7, 4, 4, &pattern(4, 4));
        let whole = decode_blocks(TextureFormat::Bc7Unorm, 4, 4, &block);
        for (w, h) in [(2, 2), (1, 1), (4, 2), (2, 1), (1, 4)] {
            let corner: Vec<u8> = (0..h)
                .flat_map(|y| &whole[y * 16..y * 16 + w * 4])
                .copied()
                .collect();
            assert_eq!(
                decode_blocks(TextureFormat::Bc7Unorm, w as u32, h as u32, &block),
                corner,
                "{w}x{h}"
            );
        }
    }

    /// Without BC formats, every level of a block-compressed texture is
    /// decoded to RGBA8 at upload (ADR-0047).
    #[test]
    fn block_compressed_textures_are_decoded_without_bc_formats() {
        let backends = crate::backends_from_env().expect("RE_BACKENDS");
        let gpu = crate::Gpu::headless(backends).unwrap_or_else(|d| panic!("{}", d.render_human()));
        let mut renderer = Renderer::new(&gpu, crate::Capture::FORMAT);
        renderer.bc_native = false;
        let block = compress(BC7, 4, 4, &pattern(4, 4));
        let data = TextureData {
            width: 8,
            height: 8,
            format: TextureFormat::Bc7Srgb,
            pixels: compress(BC7, 8, 8, &pattern(8, 8)),
            levels: vec![block.clone(), block.clone(), block],
        };
        renderer.upload_texture(&data).unwrap();
        assert_eq!(
            renderer.stats().textures.bytes,
            (8 * 8 + 4 * 4 + 2 * 2 + 1) * 4,
            "RGBA8 levels, not blocks"
        );
        assert_eq!(gpu.errors().count(), 0, "{:?}", gpu.errors().take());
    }

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

    #[test]
    fn half_floats_decode() {
        for (bits, value) in [
            (0x0000, 0.0),
            (0x3c00, 1.0),
            (0x4000, 2.0),
            (0x3800, 0.5),
            (0xc400, -4.0),
            (0x3555, 0.333_251_95),
            (0x0001, 1.0 / 16_777_216.0),
        ] {
            assert_eq!(f16_to_f32(bits), value, "{bits:#06x}");
        }
    }
}
