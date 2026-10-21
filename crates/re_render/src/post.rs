//! The HDR scene targets and the post chain (ADR-0038).
//!
//! The scene pass renders linear colour into an `Rgba16Float` target (resolved
//! from an MSAA one when MSAA is on); alpha holds coverage. The chain then runs
//! sun rays, bloom and blur on `Rgba16Float` textures, and a final pass grades,
//! tone maps and encodes into the caller's target. A step whose setting is off
//! records no pass.

use std::collections::HashMap;
use std::mem::size_of;
use std::num::NonZeroU64;

use glam::Vec2;
use re_types::render::{PostSettings, ToneMapping};

use crate::renderer::stores_gamma;
use crate::shader;

/// The scene's colour format on every backend.
pub(crate) const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Bloom never uses more levels than this.
const MAX_BLOOM_LEVELS: usize = 8;
/// Passes in one chain at most: sun rays, 2 x 8 bloom, 2 blur and the final one.
const MAX_PASSES: usize = 1 + 2 * MAX_BLOOM_LEVELS + 2 + 1;
/// Target sets kept for different sizes (a window and an editor viewport, say).
const TARGET_CACHE: usize = 4;
/// Taps on each side of the centre in one blur pass, at most.
const MAX_BLUR_TAPS: f32 = 24.0;

/// `struct Post` in post.wgsl.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, bytemuck::Pod, bytemuck::Zeroable)]
struct PostUniform {
    texel: [f32; 4],
    params: [f32; 4],
    grading: [f32; 4],
    tint: [f32; 4],
    background: [f32; 4],
}

/// What the chain needs to know about the frame besides the settings.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PostFrame {
    /// Where the sun is on screen, in UV (may be outside 0..1); `None` when it is
    /// behind the camera or there is no sun.
    pub sun_uv: Option<Vec2>,
    /// The clear colour (linear RGB and alpha), or `None` with a sky. The scene
    /// is drawn over transparent black, and the chain puts this behind it.
    pub clear: Option<[f32; 4]>,
}

/// The scene's targets and the chain's textures for one size and sample count.
pub(crate) struct HdrTargets {
    size: (u32, u32),
    pub depth: wgpu::TextureView,
    /// The multisampled colour target, resolved into `scene`.
    pub msaa: Option<wgpu::TextureView>,
    pub scene: wgpu::TextureView,
    ping: Option<wgpu::TextureView>,
    bloom: Vec<((u32, u32), wgpu::TextureView)>,
    blur: Option<[wgpu::TextureView; 2]>,
}

fn texture(
    device: &wgpu::Device,
    label: &str,
    (width, height): (u32, u32),
    format: wgpu::TextureFormat,
    samples: u32,
    usage: wgpu::TextureUsages,
) -> wgpu::TextureView {
    device
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
            usage,
            view_formats: &[],
        })
        .create_view(&wgpu::TextureViewDescriptor::default())
}

const SAMPLED: wgpu::TextureUsages =
    wgpu::TextureUsages::RENDER_ATTACHMENT.union(wgpu::TextureUsages::TEXTURE_BINDING);

impl HdrTargets {
    fn new(device: &wgpu::Device, size: (u32, u32), samples: u32) -> HdrTargets {
        // MSAA textures carry RENDER_ATTACHMENT only: with more usages, GL
        // resolves them to zeros (ADR-0038).
        let attachment = wgpu::TextureUsages::RENDER_ATTACHMENT;
        HdrTargets {
            size,
            depth: texture(
                device,
                "re_render depth",
                size,
                DEPTH_FORMAT,
                samples,
                attachment,
            ),
            msaa: (samples > 1).then(|| {
                texture(
                    device,
                    "re_render msaa colour",
                    size,
                    HDR_FORMAT,
                    samples,
                    attachment,
                )
            }),
            scene: texture(device, "re_render scene", size, HDR_FORMAT, 1, SAMPLED),
            ping: None,
            bloom: Vec::new(),
            blur: None,
        }
    }

    fn ping(&mut self, device: &wgpu::Device) -> wgpu::TextureView {
        self.ping
            .get_or_insert_with(|| {
                texture(device, "re_render post", self.size, HDR_FORMAT, 1, SAMPLED)
            })
            .clone()
    }

    fn half(&self) -> (u32, u32) {
        ((self.size.0 / 2).max(1), (self.size.1 / 2).max(1))
    }

    fn bloom(
        &mut self,
        device: &wgpu::Device,
        levels: usize,
    ) -> &[((u32, u32), wgpu::TextureView)] {
        while self.bloom.len() < levels {
            let size = match self.bloom.last() {
                None => self.half(),
                Some(((w, h), _)) => ((w / 2).max(1), (h / 2).max(1)),
            };
            let view = texture(device, "re_render bloom", size, HDR_FORMAT, 1, SAMPLED);
            self.bloom.push((size, view));
        }
        &self.bloom[..levels]
    }

    fn blur(&mut self, device: &wgpu::Device) -> [wgpu::TextureView; 2] {
        let half = self.half();
        self.blur
            .get_or_insert_with(|| {
                [0, 1].map(|_| texture(device, "re_render blur", half, HDR_FORMAT, 1, SAMPLED))
            })
            .clone()
    }
}

/// Bloom levels for a target `height` px high: the chain stops at the level
/// whose texel covers `radius` (a fraction of the height), between 1 and 8
/// levels, and before a level would be smaller than 2 px.
pub(crate) fn bloom_levels(radius: f32, (width, height): (u32, u32)) -> usize {
    let px = (radius.max(0.0) * height as f32).max(1.0);
    // ceil(log2(px)) in integers, so no platform maths (ADR-0036).
    let wanted = ((px.min(65536.0).ceil() as u32)
        .next_power_of_two()
        .trailing_zeros() as usize)
        .max(1);
    // Level k (1-based) is size / 2^k.
    let fits = (1..=MAX_BLOOM_LEVELS)
        .take_while(|k| (width.min(height) >> k) >= 2)
        .count()
        .max(1);
    wanted.min(fits).min(MAX_BLOOM_LEVELS)
}

/// How one blur pass samples: the step between taps in half-size texels, the
/// Gaussian's sigma in steps, and the taps on each side.
pub(crate) fn blur_taps(radius: f32, height: u32) -> (f32, f32, f32) {
    // The radius is about two sigma, at half size.
    let sigma = (radius * height as f32 / 4.0).max(0.5);
    let step = (2.0 * sigma / MAX_BLUR_TAPS).max(1.0);
    let sigma = sigma / step;
    (step, sigma, (2.0 * sigma).ceil().min(MAX_BLUR_TAPS))
}

/// The pipelines, sampler and targets of the chain.
pub(crate) struct PostChain {
    shader: wgpu::ShaderModule,
    layout: wgpu::BindGroupLayout,
    pipeline_layout: wgpu::PipelineLayout,
    /// Passes into `Rgba16Float` targets, by entry point.
    hdr_pipelines: HashMap<&'static str, wgpu::RenderPipeline>,
    /// The final pass, by output format.
    final_pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    slot: u64,
    targets: HashMap<((u32, u32), u32), HdrTargets>,
    /// The targets the last scene pass used.
    active: ((u32, u32), u32),
}

struct Step {
    entry: &'static str,
    target: wgpu::TextureView,
    /// Add onto the target instead of replacing it.
    add: bool,
    /// The final pass, into the caller's target.
    output: bool,
    /// What the target is cleared to first, unless `add`.
    clear: wgpu::Color,
    source: wgpu::TextureView,
    extra: wgpu::TextureView,
    uniform: PostUniform,
}

impl PostChain {
    pub(crate) fn new(device: &wgpu::Device) -> PostChain {
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("re_render post"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: NonZeroU64::new(size_of::<PostUniform>() as u64),
                    },
                    count: None,
                },
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("re_render post"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let align = u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let slot = (size_of::<PostUniform>() as u64).next_multiple_of(align);
        PostChain {
            shader: shader::module(device, "post.wgsl"),
            layout,
            pipeline_layout,
            hdr_pipelines: HashMap::new(),
            final_pipelines: HashMap::new(),
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("re_render post"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                ..Default::default()
            }),
            uniforms: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("re_render post"),
                size: slot * MAX_PASSES as u64,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            slot,
            targets: HashMap::new(),
            active: ((0, 0), 0),
        }
    }

    /// The scene targets for `size` and `samples`, created on first use.
    pub(crate) fn targets(
        &mut self,
        device: &wgpu::Device,
        size: (u32, u32),
        samples: u32,
    ) -> &HdrTargets {
        let key = (size, samples);
        if !self.targets.contains_key(&key) {
            if self.targets.len() >= TARGET_CACHE {
                self.targets.clear();
            }
            self.targets
                .insert(key, HdrTargets::new(device, size, samples));
        }
        self.active = key;
        &self.targets[&key]
    }

    fn pipeline(
        &self,
        device: &wgpu::Device,
        entry: &str,
        format: wgpu::TextureFormat,
        blend: Option<wgpu::BlendState>,
    ) -> wgpu::RenderPipeline {
        device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(entry),
            layout: Some(&self.pipeline_layout),
            vertex: wgpu::VertexState {
                module: &self.shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &self.shader,
                entry_point: Some(entry),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        })
    }

    /// Records the chain from the resolved scene into `output`; returns the
    /// number of passes recorded.
    pub(crate) fn record(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        settings: &PostSettings,
        frame: PostFrame,
        output: &wgpu::TextureView,
    ) -> u32 {
        let finite = |v: f32| if v.is_finite() { v } else { 0.0 };
        let targets = self
            .targets
            .get_mut(&self.active)
            .expect("the scene pass ran");
        let size = targets.size;
        let texel = |(w, h): (u32, u32)| [1.0 / w as f32, 1.0 / h as f32];
        let mut steps: Vec<Step> = Vec::new();
        let mut current = targets.scene.clone();
        // The clear colour goes behind the scene in the first pass that reads it.
        let clear = frame
            .clear
            .map(|c| c.map(|v| if v.is_finite() { v } else { 0.0 }));
        let behind = clear.map_or([0.0; 4], |[r, g, b, _]| [r, g, b, 0.0]);
        // `current` is still the scene itself (no step replaced it yet).
        let mut raw = true;

        let rays = finite(settings.sun_rays.intensity);
        if let (true, Some(sun)) = (rays > 0.0, frame.sun_uv.filter(|s| s.is_finite())) {
            let ping = targets.ping(device);
            let [x, y] = texel(size);
            steps.push(Step {
                entry: "fs_sun_rays",
                target: ping.clone(),
                add: false,
                output: false,
                clear: wgpu::Color::TRANSPARENT,
                source: current.clone(),
                extra: current.clone(),
                uniform: PostUniform {
                    texel: [x, y, x, y],
                    params: [sun.x, sun.y, rays, finite(settings.sun_rays.spread)],
                    background: behind,
                    ..Default::default()
                },
            });
            current = ping;
            raw = false;
        }

        let bloom_intensity = finite(settings.bloom.intensity);
        let mut bloom = None;
        if bloom_intensity > 0.0 {
            let levels = bloom_levels(finite(settings.bloom.radius), size);
            let chain = targets.bloom(device, levels).to_vec();
            let mut source = (size, current.clone());
            for (k, (level_size, view)) in chain.iter().enumerate() {
                let [x, y] = texel(source.0);
                let [zx, zy] = texel(*level_size);
                steps.push(Step {
                    entry: if k == 0 {
                        "fs_bloom_prefilter"
                    } else {
                        "fs_bloom_down"
                    },
                    target: view.clone(),
                    add: false,
                    output: false,
                    clear: wgpu::Color::TRANSPARENT,
                    source: source.1.clone(),
                    extra: source.1.clone(),
                    uniform: PostUniform {
                        texel: [x, y, zx, zy],
                        params: [
                            finite(settings.bloom.threshold).max(0.0),
                            finite(settings.bloom.knee).clamp(0.0, 1.0),
                            0.0,
                            0.0,
                        ],
                        background: if k == 0 && raw { behind } else { [0.0; 4] },
                        ..Default::default()
                    },
                });
                source = (*level_size, view.clone());
            }
            for k in (1..chain.len()).rev() {
                let [x, y] = texel(chain[k].0);
                let [zx, zy] = texel(chain[k - 1].0);
                steps.push(Step {
                    entry: "fs_bloom_up",
                    target: chain[k - 1].1.clone(),
                    add: true,
                    output: false,
                    clear: wgpu::Color::TRANSPARENT,
                    source: chain[k].1.clone(),
                    extra: chain[k].1.clone(),
                    uniform: PostUniform {
                        texel: [x, y, zx, zy],
                        ..Default::default()
                    },
                });
            }
            // Each level holds about the thresholded light once; the sum of all
            // of them is scaled back to that.
            bloom = Some((chain[0].1.clone(), bloom_intensity / chain.len() as f32));
        }

        let blur_radius = finite(settings.blur_radius);
        let mut blurred = None;
        if blur_radius > 0.0 {
            let [a, b] = targets.blur(device);
            let half = targets.half();
            let (step, sigma, taps) = blur_taps(blur_radius, size.1);
            let [x, y] = texel(half);
            let (entry, extra, intensity) = match &bloom {
                Some((view, intensity)) => ("fs_blur_bloom", view.clone(), *intensity),
                None => ("fs_blur", current.clone(), 0.0),
            };
            steps.push(Step {
                entry,
                target: a.clone(),
                add: false,
                output: false,
                clear: wgpu::Color::TRANSPARENT,
                source: current.clone(),
                extra,
                uniform: PostUniform {
                    texel: [x, y, x, y],
                    params: [step, 0.0, sigma, taps],
                    tint: [0.0, 0.0, 0.0, intensity],
                    background: if raw { behind } else { [0.0; 4] },
                    ..Default::default()
                },
            });
            steps.push(Step {
                entry: "fs_blur",
                target: b.clone(),
                add: false,
                output: false,
                clear: wgpu::Color::TRANSPARENT,
                source: a.clone(),
                extra: a,
                uniform: PostUniform {
                    texel: [x, y, x, y],
                    params: [0.0, step, sigma, taps],
                    ..Default::default()
                },
            });
            blurred = Some(b);
        }

        let grading = settings.grading.filter(|g| {
            [g.brightness, g.contrast, g.saturation]
                .iter()
                .chain(&g.tint)
                .all(|v| v.is_finite())
        });
        let operator = match settings.tone_mapping {
            ToneMapping::None => 0.0,
            ToneMapping::PbrNeutral => 1.0,
            ToneMapping::Aces => 2.0,
            ToneMapping::AgX => 3.0,
        };
        let (extra, bloom_mix, blur_flag) = match (&blurred, &bloom) {
            (Some(b), _) => (b.clone(), 0.0, 1.0),
            (None, Some((view, intensity))) => (view.clone(), *intensity, 0.0),
            (None, None) => (current.clone(), 0.0, 0.0),
        };
        let format = output.texture().format();
        let [x, y] = texel(size);
        // With no step that changes the background, the final pass leaves the
        // pixels nothing drew into to the target's clear colour: the GPU's own
        // sRGB encoding of a shader output can differ from it by a level.
        let untouched = clear.is_some()
            && raw
            && bloom.is_none()
            && blurred.is_none()
            && grading.is_none()
            && settings.tone_mapping == ToneMapping::None;
        // A gamma target stores the clear colour encoded.
        let output_clear = clear.map_or(wgpu::Color::TRANSPARENT, |mut c| {
            if stores_gamma(format) {
                for v in &mut c[..3] {
                    *v = f32::from(crate::capture::linear_to_srgb_u8(*v)) / 255.0;
                }
            }
            let [r, g, b, a] = c.map(f64::from);
            wgpu::Color { r, g, b, a }
        });
        steps.push(Step {
            entry: if stores_gamma(format) {
                "fs_final_gamma"
            } else {
                "fs_final_linear"
            },
            target: output.clone(),
            add: false,
            output: true,
            clear: output_clear,
            source: current,
            extra,
            uniform: PostUniform {
                texel: [x, y, x, y],
                params: [
                    bloom_mix,
                    blur_flag,
                    clear.map_or(1.0, |c| c[3].clamp(0.0, 1.0)),
                    operator,
                ],
                grading: grading
                    .map_or([0.0; 4], |g| [g.brightness, g.contrast, g.saturation, 1.0]),
                tint: grading.map_or([1.0; 4], |g| [g.tint[0], g.tint[1], g.tint[2], 0.0]),
                background: if raw {
                    [
                        behind[0],
                        behind[1],
                        behind[2],
                        f32::from(u8::from(untouched)),
                    ]
                } else {
                    [0.0; 4]
                },
            },
        });

        let mut bytes = vec![0u8; self.slot as usize * steps.len()];
        for (i, step) in steps.iter().enumerate() {
            let at = i * self.slot as usize;
            bytes[at..at + size_of::<PostUniform>()]
                .copy_from_slice(bytemuck::bytes_of(&step.uniform));
        }
        queue.write_buffer(&self.uniforms, 0, &bytes);

        for step in &steps {
            if step.output {
                if !self.final_pipelines.contains_key(&format) {
                    let p = self.pipeline(device, step.entry, format, None);
                    self.final_pipelines.insert(format, p);
                }
            } else if !self.hdr_pipelines.contains_key(step.entry) {
                let blend = step.add.then_some(wgpu::BlendState {
                    color: wgpu::BlendComponent {
                        src_factor: wgpu::BlendFactor::One,
                        dst_factor: wgpu::BlendFactor::One,
                        operation: wgpu::BlendOperation::Add,
                    },
                    alpha: wgpu::BlendComponent::REPLACE,
                });
                let p = self.pipeline(device, step.entry, HDR_FORMAT, blend);
                self.hdr_pipelines.insert(step.entry, p);
            }
        }
        for (i, step) in steps.iter().enumerate() {
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("re_render post"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.uniforms,
                            offset: 0,
                            size: NonZeroU64::new(size_of::<PostUniform>() as u64),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&step.source),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&step.extra),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.sampler),
                    },
                ],
            });
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(step.entry),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &step.target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if step.add {
                            wgpu::LoadOp::Load
                        } else {
                            wgpu::LoadOp::Clear(step.clear)
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let pipeline = if step.output {
                &self.final_pipelines[&format]
            } else {
                &self.hdr_pipelines[step.entry]
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bind_group, &[(i as u64 * self.slot) as u32]);
            pass.draw(0..3, 0..1);
        }
        steps.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bloom_stops_where_a_texel_covers_the_radius() {
        // 5% of 720 px is 36 px: 2^6 = 64 covers it.
        assert_eq!(bloom_levels(0.05, (1280, 720)), 6);
        assert_eq!(bloom_levels(0.0, (1280, 720)), 1);
        assert_eq!(bloom_levels(10.0, (1280, 720)), MAX_BLOOM_LEVELS);
        // A 16 px target has room for 3 levels (8, 4, 2 px).
        assert_eq!(bloom_levels(1.0, (16, 16)), 3);
        assert_eq!(bloom_levels(1.0, (1, 1)), 1);
    }

    #[test]
    fn wide_blurs_spread_their_taps() {
        let (step, sigma, taps) = blur_taps(0.01, 720);
        assert_eq!(step, 1.0);
        assert!((sigma - 1.8).abs() < 1e-4 && taps == 4.0, "{sigma} {taps}");
        let (step, sigma, taps) = blur_taps(0.5, 720);
        assert!(step > 1.0 && taps <= MAX_BLUR_TAPS, "{step} {taps}");
        assert!((sigma * step - 90.0).abs() < 1e-3);
    }
}
