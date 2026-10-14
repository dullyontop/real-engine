//! egui 0.36 through egui-wgpu 0.36 into an arbitrary texture view (headless or
//! surface). Also the demo "editor" UI used by the spike binaries.

use std::time::{Duration, Instant};

/// Editor-ish demo state.
#[derive(Debug, Clone)]
pub struct DemoState {
    pub cube_yaw_deg: f32,
    pub show_grid: bool,
    pub seed: u32,
    pub selected: usize,
    pub frame: u64,
    pub adapter_line: String,
}

impl Default for DemoState {
    fn default() -> Self {
        Self {
            cube_yaw_deg: 30.0,
            show_grid: true,
            seed: 1234,
            selected: 1,
            frame: 0,
            adapter_line: String::new(),
        }
    }
}

/// Build the demo editor UI: outliner (left), inspector (right), console
/// (bottom) and a central viewport showing an engine-rendered texture.
pub fn demo_ui(
    ui: &mut egui::Ui,
    state: &mut DemoState,
    viewport: Option<egui::load::SizedTexture>,
) {
    egui::Panel::top("menu").show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.strong("Real Engine");
            ui.separator();
            for item in ["File", "Edit", "Generate", "Agents", "Help"] {
                let _ = ui.button(item);
            }
        });
    });
    egui::Panel::left("outliner")
        .default_size(170.0)
        .show(ui, |ui| {
            ui.heading("Outliner");
            ui.separator();
            for (i, name) in ["Camera", "Cube", "Ground", "Sun"].iter().enumerate() {
                if ui.selectable_label(state.selected == i, *name).clicked() {
                    state.selected = i;
                }
            }
        });
    egui::Panel::right("inspector")
        .default_size(200.0)
        .show(ui, |ui| {
            ui.heading("Inspector");
            ui.separator();
            ui.add(egui::Slider::new(&mut state.cube_yaw_deg, 0.0..=360.0).text("yaw"));
            ui.checkbox(&mut state.show_grid, "Checker ground");
            ui.horizontal(|ui| {
                ui.label("Seed");
                ui.add(egui::DragValue::new(&mut state.seed));
            });
            let _ = ui.button("Regenerate");
            ui.add(egui::ProgressBar::new(0.62).text("bake 62%"));
        });
    egui::Panel::bottom("console")
        .default_size(70.0)
        .show(ui, |ui| {
            ui.monospace(format!("frame {}  |  {}", state.frame, state.adapter_line));
            ui.monospace("mcp: listening (stub)");
        });
    egui::CentralPanel::default().show(ui, |ui| {
        ui.label("Viewport");
        if let Some(tex) = viewport {
            let avail = ui.available_size();
            let scale = (avail.x / tex.size.x).min(avail.y / tex.size.y).min(1.0);
            ui.image(egui::load::SizedTexture::new(tex.id, tex.size * scale));
        } else {
            ui.colored_label(egui::Color32::LIGHT_RED, "no viewport texture");
        }
    });
}

/// egui context + wgpu renderer that can draw into any texture view.
pub struct GuiRenderer {
    pub ctx: egui::Context,
    pub renderer: egui_wgpu::Renderer,
    pub format: wgpu::TextureFormat,
    start: Instant,
}

#[derive(Debug, Clone, Copy)]
pub struct GuiFrameStats {
    pub ui_build: Duration,
    pub tessellate: Duration,
    pub primitives: usize,
    pub vertices: usize,
    pub indices: usize,
    pub textures_set: usize,
    pub textures_freed: usize,
}

impl GuiRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let ctx = egui::Context::default();
        let renderer =
            egui_wgpu::Renderer::new(device, format, egui_wgpu::RendererOptions::default());
        Self {
            ctx,
            renderer,
            format,
            start: Instant::now(),
        }
    }

    /// Raw input for a headless frame of `width`x`height` physical pixels.
    pub fn headless_input(
        &self,
        width: u32,
        height: u32,
        pixels_per_point: f32,
        max_texture_side: u32,
    ) -> egui::RawInput {
        let mut raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(
                    width as f32 / pixels_per_point,
                    height as f32 / pixels_per_point,
                ),
            )),
            max_texture_side: Some(max_texture_side as usize),
            time: Some(self.start.elapsed().as_secs_f64()),
            focused: true,
            ..Default::default()
        };
        raw.viewports
            .entry(raw.viewport_id)
            .or_default()
            .native_pixels_per_point = Some(pixels_per_point);
        raw
    }

    /// Run the UI, upload textures/buffers, and record one render pass into `view`.
    /// `load` decides whether the pass clears (standalone GUI) or loads (overlay).
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        size_in_pixels: [u32; 2],
        raw_input: egui::RawInput,
        load: wgpu::LoadOp<wgpu::Color>,
        run_ui: impl FnMut(&mut egui::Ui),
    ) -> (
        Vec<wgpu::CommandBuffer>,
        egui::PlatformOutput,
        GuiFrameStats,
    ) {
        let t0 = Instant::now();
        let mut full_output = self.ctx.run_ui(raw_input, run_ui);
        let ui_build = t0.elapsed();

        let t1 = Instant::now();
        let ppp = full_output.pixels_per_point;
        let primitives = self.ctx.tessellate(full_output.shapes, ppp);
        let tessellate = t1.elapsed();

        let (mut vertices, mut indices) = (0usize, 0usize);
        for p in &primitives {
            if let egui::epaint::Primitive::Mesh(m) = &p.primitive {
                vertices += m.vertices.len();
                indices += m.indices.len();
            }
        }

        // egui 0.36: each texture id maps to a SmallVec of deltas (apply in order), and
        // TexturesDelta debug-asserts on drop that it was drained: consume it with drain().
        let mut textures_set = 0usize;
        for (id, deltas) in full_output.textures_delta.set.drain() {
            for delta in &deltas {
                self.renderer.update_texture(device, queue, id, delta);
                textures_set += 1;
            }
        }
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels,
            pixels_per_point: ppp,
        };
        let user_cmds = self
            .renderer
            .update_buffers(device, queue, encoder, &primitives, &screen);
        {
            // egui-wgpu wants RenderPass<'static>: call forget_lifetime().
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("egui-pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load,
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, &primitives, &screen);
        }
        let mut textures_freed = 0usize;
        for id in full_output.textures_delta.free.drain() {
            self.renderer.free_texture(&id);
            textures_freed += 1;
        }
        let stats = GuiFrameStats {
            ui_build,
            tessellate,
            primitives: primitives.len(),
            vertices,
            indices,
            textures_set,
            textures_freed,
        };
        (user_cmds, full_output.platform_output, stats)
    }
}
