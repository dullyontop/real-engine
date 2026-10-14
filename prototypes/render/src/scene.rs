//! A depth-tested, lit, indexed scene (cube on a checkered ground plane) rendered
//! with a WGSL shader. Camera/light uniforms are built with glam; per-object
//! uniforms live in one buffer addressed with dynamic offsets.

use std::borrow::Cow;
use std::num::NonZeroU64;

use bytemuck::{Pod, Zeroable};
use glam::{Mat4, Quat, Vec3, Vec4};
use wgpu::util::DeviceExt;

pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Offscreen color format: sRGB, so the shader writes linear values and the
/// stored bytes are what a PNG expects.
pub const OFFSCREEN_COLOR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Clear color (linear).
pub const CLEAR_COLOR: wgpu::Color = wgpu::Color {
    r: 0.02,
    g: 0.03,
    b: 0.06,
    a: 1.0,
};

#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Vertex {
    pub position: [f32; 3],
    pub normal: [f32; 3],
    pub color: [f32; 3],
}

/// Must match `struct Globals` in scene.wgsl (128 bytes, 16-byte aligned members).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct Globals {
    pub view_proj: Mat4,
    pub camera_pos: Vec4,
    pub light_dir: Vec4,
    pub light_color: Vec4,
    pub point_light: Vec4,
}

/// Must match `struct Object` in scene.wgsl (144 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Pod, Zeroable)]
pub struct ObjectUniform {
    pub model: Mat4,
    pub normal_matrix: Mat4,
    pub tint: Vec4,
}

#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
}

impl Mesh {
    /// Append a quad centred at `c` spanning `±u`, `±v`. Counter-clockwise when
    /// viewed from the side `u × v` points to (which is the face normal).
    pub fn push_quad(&mut self, c: Vec3, u: Vec3, v: Vec3, color: [f32; 3]) {
        let n = u.cross(v).normalize();
        let base = self.vertices.len() as u32;
        for p in [c - u - v, c + u - v, c + u + v, c - u + v] {
            self.vertices.push(Vertex {
                position: p.to_array(),
                normal: n.to_array(),
                color,
            });
        }
        self.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
}

/// Unit cube centred on the origin: 24 vertices (flat normals), 36 indices.
pub fn cube_mesh(color: [f32; 3]) -> Mesh {
    let mut m = Mesh::default();
    let h = 0.5;
    // (normal, u, v) with u × v = normal
    let faces = [
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
    ];
    for (n, u, v) in faces {
        m.push_quad(n * h, u * h, v * h, color);
    }
    m
}

/// Ground plane at y = 0, `2*half` wide, facing +Y.
pub fn plane_mesh(half: f32, color: [f32; 3]) -> Mesh {
    let mut m = Mesh::default();
    m.push_quad(Vec3::ZERO, Vec3::X * half, Vec3::NEG_Z * half, color);
    m
}

#[derive(Debug, Clone, Copy)]
pub struct Camera {
    pub eye: Vec3,
    pub target: Vec3,
    pub fov_y_radians: f32,
    pub near: f32,
    pub far: f32,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            eye: Vec3::new(2.6, 2.2, 3.4),
            target: Vec3::new(0.0, 0.5, 0.0),
            fov_y_radians: 45f32.to_radians(),
            near: 0.1,
            far: 100.0,
        }
    }
}

impl Camera {
    /// wgpu uses the WebGPU clip-space convention on every backend (including
    /// Vulkan): NDC depth 0..1 and NDC Y up. In glam 0.34 that is
    /// `camera::rh::proj::directx` (the `vulkan` module is Y-down and would flip
    /// the image). glam 0.34 removed `Mat4::perspective_rh` / `Mat4::look_at_rh`.
    pub fn view_proj(&self, aspect: f32) -> Mat4 {
        let proj = glam::camera::rh::proj::directx::perspective(
            self.fov_y_radians,
            aspect,
            self.near,
            self.far,
        );
        let view = glam::camera::rh::view::look_at_mat4(self.eye, self.target, Vec3::Y);
        proj * view
    }
}

struct GpuMesh {
    vertex_buffer: wgpu::Buffer,
    index_buffer: wgpu::Buffer,
    index_count: u32,
    object_slot: u32,
}

/// Owns the pipeline, buffers and bind groups for the spike scene.
pub struct SceneRenderer {
    pipeline: wgpu::RenderPipeline,
    globals_buffer: wgpu::Buffer,
    globals_bind_group: wgpu::BindGroup,
    objects_buffer: wgpu::Buffer,
    objects_bind_group: wgpu::BindGroup,
    object_stride: u64,
    meshes: Vec<GpuMesh>,
    pub camera: Camera,
    pub color_format: wgpu::TextureFormat,
}

pub const MAX_OBJECTS: u64 = 8;

impl SceneRenderer {
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("scene.wgsl"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("scene.wgsl"))),
        });

        let globals_size = std::mem::size_of::<Globals>() as u64;
        let object_size = std::mem::size_of::<ObjectUniform>() as u64;
        // Dynamic uniform offsets must be multiples of this limit (256 on most adapters).
        let align = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let object_stride = object_size.div_ceil(align) * align;

        let globals_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("globals-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(globals_size),
                },
                count: None,
            }],
        });
        let objects_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("objects-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: NonZeroU64::new(object_size),
                },
                count: None,
            }],
        });

        let globals_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("globals"),
            size: globals_size,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let objects_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("objects"),
            size: object_stride * MAX_OBJECTS,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let globals_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("globals-bg"),
            layout: &globals_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: globals_buffer.as_entire_binding(),
            }],
        });
        let objects_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("objects-bg"),
            layout: &objects_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                // A window of one ObjectUniform; the dynamic offset selects the slot.
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &objects_buffer,
                    offset: 0,
                    size: NonZeroU64::new(object_size),
                }),
            }],
        });

        // wgpu 29+: bind group layouts are Option<&BindGroupLayout> (gaps allowed).
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("scene-pipeline-layout"),
            bind_group_layouts: &[Some(&globals_layout), Some(&objects_layout)],
            immediate_size: 0,
        });

        let vertex_layout = wgpu::VertexBufferLayout {
            array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32x3],
        };

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("scene-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                // wgpu 30: vertex buffer slots are Option<VertexBufferLayout>.
                buffers: &[Some(vertex_layout)],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: Some(wgpu::Face::Back),
                ..Default::default()
            },
            // wgpu 29+: depth_write_enabled / depth_compare are Option.
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: wgpu::StencilState::default(),
                bias: wgpu::DepthBiasState::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });

        let mut renderer = Self {
            pipeline,
            globals_buffer,
            globals_bind_group,
            objects_buffer,
            objects_bind_group,
            object_stride,
            meshes: Vec::new(),
            camera: Camera::default(),
            color_format,
        };
        // Draw order is deliberate: the cube first, then the ground plane. Without
        // a working depth test the plane would overwrite the cube.
        renderer.add_mesh(device, &cube_mesh([0.95, 0.42, 0.12]), 0);
        renderer.add_mesh(device, &plane_mesh(4.0, [0.55, 0.6, 0.55]), 1);
        renderer
    }

    fn add_mesh(&mut self, device: &wgpu::Device, mesh: &Mesh, object_slot: u32) {
        let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("vertices"),
            contents: bytemuck::cast_slice(&mesh.vertices),
            usage: wgpu::BufferUsages::VERTEX,
        });
        let index_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("indices"),
            contents: bytemuck::cast_slice(&mesh.indices),
            usage: wgpu::BufferUsages::INDEX,
        });
        self.meshes.push(GpuMesh {
            vertex_buffer,
            index_buffer,
            index_count: mesh.indices.len() as u32,
            object_slot,
        });
    }

    /// Upload camera, light and per-object uniforms. `cube_angle` rotates the cube.
    pub fn update(&self, queue: &wgpu::Queue, aspect: f32, cube_angle: f32) {
        let globals = Globals {
            view_proj: self.camera.view_proj(aspect),
            camera_pos: self.camera.eye.extend(1.0),
            light_dir: Vec3::new(-0.45, 1.0, 0.65).normalize().extend(0.0),
            light_color: Vec4::new(1.0, 0.96, 0.9, 0.55),
            point_light: Vec4::new(-1.2, 1.4, 1.6, 2.5),
        };
        queue.write_buffer(&self.globals_buffer, 0, bytemuck::bytes_of(&globals));

        let cube_model = Mat4::from_rotation_translation(
            Quat::from_rotation_y(cube_angle),
            Vec3::new(0.0, 0.5, 0.0),
        );
        let objects = [
            ObjectUniform {
                model: cube_model,
                normal_matrix: cube_model.inverse().transpose(),
                tint: Vec4::new(1.0, 1.0, 1.0, 0.0),
            },
            ObjectUniform {
                model: Mat4::IDENTITY,
                normal_matrix: Mat4::IDENTITY,
                tint: Vec4::new(1.0, 1.0, 1.0, 1.0), // w = 1: procedural checker
            },
        ];
        for (i, obj) in objects.iter().enumerate() {
            queue.write_buffer(
                &self.objects_buffer,
                i as u64 * self.object_stride,
                bytemuck::bytes_of(obj),
            );
        }
    }

    /// Record the draws into an already-begun render pass.
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.globals_bind_group, &[]);
        for mesh in &self.meshes {
            let offset = (u64::from(mesh.object_slot) * self.object_stride) as u32;
            pass.set_bind_group(1, &self.objects_bind_group, &[offset]);
            pass.set_vertex_buffer(0, mesh.vertex_buffer.slice(..));
            pass.set_index_buffer(mesh.index_buffer.slice(..), wgpu::IndexFormat::Uint32);
            pass.draw_indexed(0..mesh.index_count, 0, 0..1);
        }
    }

    /// Begin a render pass that clears color + depth, draw the scene, end the pass.
    pub fn encode(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        color_view: &wgpu::TextureView,
        depth_view: &wgpu::TextureView,
        timestamp_writes: Option<wgpu::RenderPassTimestampWrites<'_>>,
    ) {
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("scene-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: color_view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(CLEAR_COLOR),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        self.draw(&mut pass);
    }
}

/// Offscreen color + depth targets.
pub struct OffscreenTarget {
    pub color: wgpu::Texture,
    pub color_view: wgpu::TextureView,
    pub depth: wgpu::Texture,
    pub depth_view: wgpu::TextureView,
    pub width: u32,
    pub height: u32,
}

impl OffscreenTarget {
    /// Color + depth targets without view-format aliases (works on every backend).
    pub fn new(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        color_format: wgpu::TextureFormat,
    ) -> Self {
        Self::with_options(device, width, height, color_format, false)
    }

    /// Like [`Self::new`], but when `non_srgb_alias` is true the color texture also
    /// allows a view without the sRGB suffix. That needs
    /// `DownlevelFlags::VIEW_FORMATS`, which the GL backend does not have.
    pub fn with_options(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        color_format: wgpu::TextureFormat,
        non_srgb_alias: bool,
    ) -> Self {
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        // Allow a non-sRGB view of an sRGB texture: egui samples "gamma" textures
        // (Rgba8Unorm), while the scene renders through the sRGB view.
        let linear_alias = color_format.remove_srgb_suffix();
        let view_formats: &[wgpu::TextureFormat] = if non_srgb_alias && linear_alias != color_format
        {
            std::slice::from_ref(&linear_alias)
        } else {
            &[]
        };
        let color = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-color"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: color_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats,
        });
        let depth = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen-depth"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let color_view = color.create_view(&wgpu::TextureViewDescriptor::default());
        let depth_view = depth.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            color,
            color_view,
            depth,
            depth_view,
            width,
            height,
        }
    }

    pub fn aspect(&self) -> f32 {
        self.width as f32 / self.height as f32
    }

    /// A view of the color texture reinterpreted without the sRGB suffix
    /// (e.g. Rgba8UnormSrgb -> Rgba8Unorm), for egui's `register_native_texture`.
    pub fn non_srgb_color_view(&self) -> wgpu::TextureView {
        self.color.create_view(&wgpu::TextureViewDescriptor {
            label: Some("offscreen-color-unorm-view"),
            format: Some(self.color.format().remove_srgb_suffix()),
            ..Default::default()
        })
    }
}

/// Create only a depth texture view (used by the windowed path for the surface size).
pub fn create_depth_view(device: &wgpu::Device, width: u32, height: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("depth"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEPTH_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}
