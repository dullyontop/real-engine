//! egui painting (ADR-0010).
//!
//! The caller runs egui and tessellates its shapes (tessellation needs the
//! context's font atlas), then hands the result to `Renderer::paint_ui` as a
//! [`UiFrame`]. egui-wgpu paints into a gamma-space `Rgba8Unorm` overlay, which is
//! what egui is designed for, and a small pass composites the overlay onto the
//! target. For an sRGB target that pass converts gamma to linear itself, so the
//! UI looks the same on every backend, including GL, which has no view formats to
//! alias an sRGB texture as `Unorm`.

use std::collections::HashMap;
use std::time::Duration;

use re_core::Diagnostic;
use re_core::time::Instant;

use crate::{codes, shader};

/// The overlay's format: gamma-space RGBA, as egui prefers.
const OVERLAY_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// One frame of tessellated egui output.
///
/// Painting applies `textures_delta`; dropping a `UiFrame` discards whatever it
/// still holds (egui asserts in debug builds when a `TexturesDelta` is dropped
/// unapplied, and a `UiFrame` clears it first). Every frame that egui produced must
/// be painted, or its texture updates (the font atlas, for one) are lost.
pub struct UiFrame {
    /// The tessellated shapes, from `egui::Context::tessellate`.
    pub primitives: Vec<egui::ClippedPrimitive>,
    /// Texture uploads and frees from `egui::FullOutput::textures_delta`.
    pub textures_delta: egui::TexturesDelta,
    /// The target size in physical pixels.
    pub size_px: [u32; 2],
    /// Physical pixels per egui point.
    pub pixels_per_point: f32,
}

impl UiFrame {
    /// Tessellates `output` with `ctx` (the context that produced it) for a target
    /// of `size_px` physical pixels. The rest of `output` (platform output,
    /// viewport commands) stays with the caller.
    pub fn from_output(
        ctx: &egui::Context,
        output: &mut egui::FullOutput,
        size_px: [u32; 2],
    ) -> UiFrame {
        let pixels_per_point = output.pixels_per_point;
        let shapes = std::mem::take(&mut output.shapes);
        UiFrame {
            primitives: ctx.tessellate(shapes, pixels_per_point),
            textures_delta: std::mem::take(&mut output.textures_delta),
            size_px,
            pixels_per_point,
        }
    }
}

impl Drop for UiFrame {
    fn drop(&mut self) {
        self.textures_delta.clear();
    }
}

impl std::fmt::Debug for UiFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UiFrame")
            .field("primitives", &self.primitives.len())
            .field("textures_delta", &self.textures_delta)
            .field("size_px", &self.size_px)
            .field("pixels_per_point", &self.pixels_per_point)
            .finish()
    }
}

/// The composite shader and its pipelines, as [`UiPainter::replace_blit`]
/// found them.
pub(crate) struct UiBlit {
    shader: wgpu::ShaderModule,
    blits: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
}

struct Overlay {
    size: [u32; 2],
    view: wgpu::TextureView,
    bind_group: wgpu::BindGroup,
}

/// The egui painter: one egui-wgpu renderer, the overlay and the composite pass.
pub(crate) struct UiPainter {
    egui: egui_wgpu::Renderer,
    overlay: Option<Overlay>,
    blit_layout: wgpu::BindGroupLayout,
    blit_pipeline_layout: wgpu::PipelineLayout,
    blit_shader: wgpu::ShaderModule,
    blits: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
}

impl UiPainter {
    pub fn new(device: &wgpu::Device) -> UiPainter {
        let egui = egui_wgpu::Renderer::new(
            device,
            OVERLAY_FORMAT,
            egui_wgpu::RendererOptions {
                msaa_samples: 1,
                depth_stencil_format: None,
                dithering: true,
                predictable_texture_filtering: false,
            },
        );
        let blit_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("re_render ui overlay"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let blit_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("re_render ui blit"),
            bind_group_layouts: &[Some(&blit_layout)],
            immediate_size: 0,
        });
        UiPainter {
            egui,
            overlay: None,
            blit_layout,
            blit_pipeline_layout,
            blit_shader: shader::module(device, "ui_blit.wgsl"),
            blits: HashMap::new(),
        }
    }

    pub fn egui(&mut self) -> &mut egui_wgpu::Renderer {
        &mut self.egui
    }

    fn overlay(&mut self, device: &wgpu::Device, size: [u32; 2]) -> &Overlay {
        if self.overlay.as_ref().is_none_or(|o| o.size != size) {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("re_render ui overlay"),
                size: wgpu::Extent3d {
                    width: size[0],
                    height: size[1],
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: OVERLAY_FORMAT,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("re_render ui overlay"),
                layout: &self.blit_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                }],
            });
            self.overlay = Some(Overlay {
                size,
                view,
                bind_group,
            });
        }
        self.overlay.as_ref().expect("created above")
    }

    fn blit_pipeline(
        &mut self,
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
    ) -> wgpu::RenderPipeline {
        self.blits
            .entry(format)
            .or_insert_with(|| {
                device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("re_render ui blit"),
                    layout: Some(&self.blit_pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &self.blit_shader,
                        entry_point: Some("vs_fullscreen"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    fragment: Some(wgpu::FragmentState {
                        module: &self.blit_shader,
                        entry_point: Some(if crate::renderer::stores_gamma(format) {
                            "fs_blit_gamma"
                        } else {
                            "fs_blit_linear"
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
            })
            .clone()
    }

    /// Swaps in `shader` (a new `ui_blit.wgsl`, ADR-0038) and builds the
    /// composite with it for the formats painted into so far and for a linear
    /// and a gamma target, so the device checks both entry points now. Returns
    /// what it replaced, for [`UiPainter::restore_blit`].
    pub fn replace_blit(&mut self, device: &wgpu::Device, shader: wgpu::ShaderModule) -> UiBlit {
        let formats = crate::renderer::with_checks(self.blits.keys());
        let old = UiBlit {
            shader: std::mem::replace(&mut self.blit_shader, shader),
            blits: std::mem::take(&mut self.blits),
        };
        for format in formats {
            self.blit_pipeline(device, format);
        }
        old
    }

    /// Puts back what [`UiPainter::replace_blit`] replaced.
    pub fn restore_blit(&mut self, old: UiBlit) {
        self.blit_shader = old.shader;
        self.blits = old.blits;
    }

    /// Paints `frame` over `target` (which keeps its contents) and returns the
    /// time spent in `Queue::submit`.
    pub fn paint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        target: &wgpu::TextureView,
        frame: &UiFrame,
    ) -> Result<Duration, Diagnostic> {
        let texture = target.texture();
        let size = [texture.width(), texture.height()];
        if !texture
            .usage()
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
            || texture.sample_count() != 1
        {
            return Err(Diagnostic::error(
                codes::INVALID_TARGET,
                "cannot paint the UI into this view: it must be a single-sample texture with \
                 RENDER_ATTACHMENT usage",
            ));
        }
        if frame.size_px != size {
            return Err(Diagnostic::error(
                codes::INVALID_TARGET,
                format!(
                    "the UI frame is {}x{} pixels but the target is {}x{}",
                    frame.size_px[0], frame.size_px[1], size[0], size[1]
                ),
            )
            .with_hint("tessellate the UI for the size of the view it is painted into"));
        }
        if !(frame.pixels_per_point.is_finite() && frame.pixels_per_point > 0.0) {
            return Err(Diagnostic::error(
                codes::INVALID_TARGET,
                format!(
                    "pixels_per_point must be positive, not {}",
                    frame.pixels_per_point
                ),
            ));
        }
        for (id, deltas) in &frame.textures_delta.set {
            for delta in deltas {
                self.egui.update_texture(device, queue, *id, delta);
            }
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size,
            pixels_per_point: frame.pixels_per_point,
        };
        let blit = self.blit_pipeline(device, target.texture().format());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("re_render ui"),
        });
        let user_commands =
            self.egui
                .update_buffers(device, queue, &mut encoder, &frame.primitives, &screen);
        self.overlay(device, size);
        let overlay = self.overlay.as_ref().expect("created above");
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("re_render ui paint"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &overlay.view,
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
                })
                .forget_lifetime();
            self.egui.render(&mut pass, &frame.primitives, &screen);
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("re_render ui composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&blit);
            pass.set_bind_group(0, &overlay.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        let commands = encoder.finish();
        let submitting = Instant::now();
        queue.submit(user_commands.into_iter().chain([commands]));
        let waited = submitting.elapsed();
        for id in &frame.textures_delta.free {
            self.egui.free_texture(id);
        }
        Ok(waited)
    }
}
