//! The scene pass into the HDR target: the sky, the batches with the
//! depth-tested world quads among the blended ones, the instance sets' runs
//! after the opaque and after the blended batches, the sprites and ribbons,
//! and the debug lines. Also [`Renderer::draw_batches`], which every pass that
//! draws the frame's meshes records its batches with.

use std::mem::size_of;
use std::ops::Range;

use super::prepare::{Batch, PreparedFrame, Slot};
use super::{FrameReport, Renderer, SKINNED};
use crate::extras::EffectVertex;
use crate::frame::{InstanceRaw, LineVertex};
use crate::instances;
use crate::post::DEPTH_CLEAR;

/// What [`Renderer::draw_batches`] drew.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Drawn {
    pub(crate) draw_calls: u32,
    pub(crate) instances: u32,
    pub(crate) triangles: u64,
}

impl Renderer {
    /// Draws `batches` of `frame` into `pass`, with `pipeline`'s pipeline for
    /// each batch's variant. A skinned batch binds the joint palette at
    /// `palette_group`; with `material_group`, each batch binds its material
    /// there. Only what changes from one batch to the next is set, so the
    /// pass's other bind groups stay as the caller set them. (`pipeline` is a
    /// `dyn`, so every pass shares one copy of this code.)
    pub(crate) fn draw_batches<'p>(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        frame: &PreparedFrame,
        batches: &[Batch],
        palette_group: u32,
        material_group: Option<u32>,
        pipeline: &dyn Fn(usize) -> &'p wgpu::RenderPipeline,
    ) -> Drawn {
        let stride = size_of::<InstanceRaw>() as u64;
        let mut drawn = Drawn::default();
        // The variant, material and mesh currently bound.
        let mut current: (Option<usize>, Option<Slot>, Option<Slot>) = (None, None, None);
        for batch in batches {
            let skinned = batch.variant & SKINNED != 0;
            if current.0 != Some(batch.variant) {
                pass.set_pipeline(pipeline(batch.variant));
                current.0 = Some(batch.variant);
                if let Some(palette) = frame.palette.as_ref().filter(|_| skinned) {
                    pass.set_bind_group(palette_group, palette, &[]);
                }
            }
            if let Some(group) = material_group
                && current.1 != Some(batch.material)
            {
                let material = self
                    .materials
                    .get(batch.material.0, batch.material.1)
                    .expect("resolved when prepared");
                pass.set_bind_group(group, &material.bind_group, &[]);
                current.1 = Some(batch.material);
            }
            let mesh = self
                .meshes
                .get(batch.mesh.0, batch.mesh.1)
                .expect("resolved when prepared");
            if current.2 != Some(batch.mesh) {
                pass.set_vertex_buffer(0, mesh.vertices.slice(..));
                pass.set_index_buffer(mesh.indices.slice(..), wgpu::IndexFormat::Uint32);
                current.2 = Some(batch.mesh);
            }
            if let Some(skin) = mesh.skin.as_ref().filter(|_| skinned) {
                pass.set_vertex_buffer(2, skin.buffer.slice(..));
            }
            let start = u64::from(batch.first_instance) * stride;
            pass.set_vertex_buffer(
                1,
                self.instances
                    .buffer
                    .slice(start..start + u64::from(batch.instances) * stride),
            );
            pass.draw_indexed(0..mesh.index_count, 0, 0..batch.instances);
            drawn.draw_calls += 1;
            drawn.instances += batch.instances;
            drawn.triangles += u64::from(mesh.index_count / 3) * u64::from(batch.instances);
        }
        drawn
    }

    /// Records the scene pass. The HDR target is cleared to transparent black;
    /// the post chain puts the clear colour behind it, where nothing covers it.
    pub(super) fn record_scene(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &PreparedFrame,
        report: &mut FrameReport,
    ) {
        let pipelines = &self.pipelines[&frame.samples];
        let targets = self.post.active_targets();
        let (view, resolve_target) = match &targets.msaa {
            Some(msaa) => (msaa, Some(&targets.scene)),
            None => (&targets.scene, None),
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("re_render scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
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
                    load: wgpu::LoadOp::Clear(DEPTH_CLEAR),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        pass.set_bind_group(0, &self.shadow.frame_bind_group, &[]);
        if frame.sky {
            pass.set_pipeline(&pipelines.sky);
            pass.draw(0..3, 0..1);
        }
        // The opaque batches, then the blended ones in runs between the
        // depth-tested world quads, which sort among the blended ones only.
        // Each run binds what it needs again, since a quad changes the
        // pipeline and group 1.
        let draw_run =
            |pass: &mut wgpu::RenderPass<'_>, run: Range<usize>, report: &mut FrameReport| {
                let drawn =
                    self.draw_batches(pass, frame, &frame.batches[run], 2, Some(1), &|variant| {
                        &pipelines.forward[variant]
                    });
                report.stats.draw_calls += drawn.draw_calls;
                report.stats.triangles += drawn.triangles;
            };
        // The instance sets' runs (ADR-0052): materials at group 1 as the
        // items', each (set, part) entry at group 2.
        let draw_sets =
            |pass: &mut wgpu::RenderPass<'_>, runs: &[instances::Run], report: &mut FrameReport| {
                let drawn = self.sets.draw(
                    pass,
                    &frame.sets,
                    runs,
                    &self.meshes,
                    Some((1, &self.materials)),
                    2,
                    &|variant| &pipelines.sets[variant],
                );
                report.stats.draw_calls += drawn.draw_calls;
                report.stats.triangles += drawn.triangles;
            };
        draw_run(&mut pass, 0..frame.opaque_batches, report);
        draw_sets(
            &mut pass,
            &frame.sets.runs[..frame.sets.opaque_runs],
            report,
        );
        // Here the opaque scene is drawn and nothing blended yet.
        let mut first = frame.opaque_batches;
        for &(before, n) in &frame.tested_quads {
            if before > first {
                draw_run(&mut pass, first..before, report);
                first = before;
            }
            pass.set_pipeline(&pipelines.quads);
            pass.set_bind_group(
                1,
                &frame.quad_groups[n],
                &[(n as u64 * frame.quad_slot) as u32],
            );
            pass.draw(0..6, 0..1);
        }
        draw_run(&mut pass, first..frame.batches.len(), report);
        draw_sets(
            &mut pass,
            &frame.sets.runs[frame.sets.opaque_runs..],
            report,
        );
        let quads = frame.effect_quads;
        if quads > 0 {
            pass.set_pipeline(&pipelines.effects);
            pass.set_vertex_buffer(
                0,
                self.effect_vertices
                    .buffer
                    .slice(..(quads * 4 * size_of::<EffectVertex>()) as u64),
            );
            pass.set_index_buffer(
                self.effect_indices.buffer.slice(..(quads * 6 * 4) as u64),
                wgpu::IndexFormat::Uint32,
            );
            pass.draw_indexed(0..(quads * 6) as u32, 0, 0..1);
            report.stats.draw_calls += 1;
        }
        if frame.line_vertices > 0 {
            pass.set_pipeline(&pipelines.lines);
            pass.set_vertex_buffer(
                0,
                self.lines
                    .buffer
                    .slice(..(frame.line_vertices * size_of::<LineVertex>()) as u64),
            );
            pass.draw(0..frame.line_vertices as u32, 0..1);
        }
    }
}
