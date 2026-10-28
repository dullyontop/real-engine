//! The shadow pass: one depth-only pass per cascade of the sun's shadow map,
//! each drawing the casters its fit took (ADR-0038).

use super::prepare::PreparedFrame;
use super::{FrameReport, Renderer, SKINNED};
use crate::timing::Group;

impl Renderer {
    /// Records the frame's cascades into the shadow map's layers.
    pub(super) fn record_shadows(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        frame: &PreparedFrame,
        report: &mut FrameReport,
    ) {
        let Some(cascade_group) = &frame.cascade_group else {
            return;
        };
        for (n, batches) in frame.cascades.iter().enumerate() {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("re_render shadow"),
                color_attachments: &[],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &self.shadow.layer_views[n],
                    depth_ops: Some(wgpu::Operations {
                        load: wgpu::LoadOp::Clear(1.0),
                        store: wgpu::StoreOp::Store,
                    }),
                    stencil_ops: None,
                }),
                timestamp_writes: self.timer.as_ref().and_then(|t| t.writes(Group::Shadow)),
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_bind_group(0, cascade_group, &[(n as u64 * frame.cascade_slot) as u32]);
            let drawn = self.draw_batches(
                &mut pass,
                frame,
                &frame.cascade_batches[batches.clone()],
                1,
                None,
                &|variant| match self.skinning.as_ref().filter(|_| variant & SKINNED != 0) {
                    Some(skinning) => &skinning.shadow_pipeline,
                    None => &self.shadow_pipeline,
                },
            );
            report.stats.shadow_casters += drawn.instances;
            report.stats.shadow_draw_calls += drawn.draw_calls;
        }
    }
}
