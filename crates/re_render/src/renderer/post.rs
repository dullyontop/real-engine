//! What follows the scene pass: the post chain from the HDR target into the
//! caller's target (in `crate::post`), then the highlights and the on-top world
//! quads over the finished image, in display colours.

use re_types::render::RenderSnapshot;

use super::prepare::PreparedFrame;
use super::{FrameReport, HighlightUniform, OUTLINE_PX, Renderer, SKINNED};
use crate::extras::{MAX_HIGHLIGHTS, SceneExtras};
use crate::post::{DEPTH_CLEAR, PostFrame};

impl Renderer {
    /// Records the post chain into `target`, then the highlights and the
    /// on-top world quads over it.
    #[expect(
        clippy::too_many_arguments,
        reason = "the frame's inputs, borrowed apart"
    )]
    pub(super) fn record_post(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        frame: &PreparedFrame,
        snapshot: &RenderSnapshot,
        extras: &SceneExtras,
        target: &wgpu::TextureView,
        report: &mut FrameReport,
    ) {
        report.stats.post_passes = self.post.record(
            device,
            queue,
            encoder,
            &snapshot.post,
            PostFrame {
                sun_uv: frame.sun_uv,
                clear: (!frame.sky).then_some(snapshot.clear),
            },
            target,
        );
        if !frame.highlight_batches.is_empty() {
            report.stats.highlights = frame.highlights as u32;
            self.draw_highlights(
                queue,
                encoder,
                target,
                frame,
                &extras.highlights[..frame.highlights],
            );
        }
        if !frame.top_quads.is_empty() {
            let format = target.texture().format();
            if !self.top_quad_pipelines.contains_key(&format) {
                let pipeline = self.quad_pipeline(format, 1, false);
                self.top_quad_pipelines.insert(format, pipeline);
            }
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("re_render quads on top"),
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
            pass.set_pipeline(&self.top_quad_pipelines[&format]);
            pass.set_bind_group(0, &self.shadow.frame_bind_group, &[]);
            for &n in &frame.top_quads {
                pass.set_bind_group(
                    1,
                    &frame.quad_groups[n],
                    &[(n as u64 * frame.quad_slot) as u32],
                );
                pass.draw(0..6, 0..1);
            }
        }
    }

    /// The highlight mask (a depth pass of the opaque scene, the occluded
    /// highlights, then the on-top ones over a cleared depth) and its
    /// composite over `target`.
    fn draw_highlights(
        &mut self,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::TextureView,
        frame: &PreparedFrame,
        highlights: &[crate::extras::Highlight],
    ) {
        let mut uniform = HighlightUniform {
            colors: [[0.0; 4]; 2 * MAX_HIGHLIGHTS],
            params: [OUTLINE_PX, 0.0, 0.0, 0.0],
        };
        let clean = |c: [f32; 4]| {
            c.map(|v| {
                if v.is_finite() {
                    v.clamp(0.0, 1.0)
                } else {
                    0.0
                }
            })
        };
        for (i, h) in highlights.iter().enumerate() {
            uniform.colors[2 * i] = clean(h.fill);
            uniform.colors[2 * i + 1] = clean(h.outline);
        }
        queue.write_buffer(&self.highlight_buffer, 0, bytemuck::bytes_of(&uniform));
        let format = target.texture().format();
        self.highlight_pipeline(format);
        self.mask_targets(frame.size.0, frame.size.1);
        let targets = self.mask_targets.as_ref().expect("created above");
        // Mask pipeline `n` (0 depth only, 1 numbering) for a batch's variant.
        let (skinning, plain) = (self.skinning.as_ref(), &self.mask_pipelines);
        let mask = |n: usize| {
            move |variant: usize| match skinning.filter(|_| variant & SKINNED != 0) {
                Some(skinning) => &skinning.mask_pipelines[n],
                None => &plain[n],
            }
        };
        let (occluded, on_top) = frame.highlight_batches.split_at(frame.on_top_from);
        for (top, highlighted) in [(false, occluded), (true, on_top)] {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("re_render highlight mask"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.mask,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if top {
                            wgpu::LoadOp::Load
                        } else {
                            wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                        },
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth,
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(DEPTH_CLEAR),
                        store: wgpu::StoreOp::Discard,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, &self.mask_frame_group, &[]);
            if !top {
                let opaque = &frame.batches[..frame.opaque_batches];
                self.draw_batches(&mut pass, frame, opaque, 1, None, &mask(0));
                // The opaque instance sets hide occluded highlights too.
                self.sets.draw(
                    &mut pass,
                    &frame.sets,
                    &frame.sets.runs[..frame.sets.opaque_runs],
                    &self.meshes,
                    None,
                    1,
                    &|_| &self.sets.mask_pipelines[0],
                );
            }
            self.draw_batches(&mut pass, frame, highlighted, 1, None, &mask(1));
        }
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("re_render highlight"),
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
        pass.set_pipeline(&self.highlight_pipelines[&format]);
        pass.set_bind_group(0, &targets.bind_group, &[]);
        pass.draw(0..3, 0..1);
    }
}
