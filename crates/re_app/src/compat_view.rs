//! The window side of a compat game: the local player's 3D view
//! (`re_compat::render`) and 2D UI (`re_compat::gui`), on the app's renderer.
//!
//! Each frame the client VM builds a [`RenderFrame`]: what to draw this
//! frame, plus the meshes, materials and textures to create or release. The
//! view keeps a CPU copy of each until it is released, so a frame built while
//! the app has no GPU side (or has lent it to a `play.run` copy) still draws
//! once one is there. The UI is one egui pass with the client's `ScreenGui`s
//! and the HUD lines, kept until it is drawn.
//!
//! `SurfaceGui`s and `ViewportFrame`s render to textures (ADR-0048 §2): each
//! has an offscreen capture, a surface's painted with its UI shapes when
//! [`SurfaceRedraws`] plans it and shown on its part as a world quad, a
//! viewport's drawn with its parts when they or its size changed and shown in
//! the UI as an egui texture.

use std::collections::HashMap;

use re_compat::gui::{MAX_VIEWPORT, SurfaceDraw, SurfaceRedraws, ViewportDraw};
use re_compat::render::{FrameItem, MaterialKey, MeshKey, RenderFrame, ViewportScene};
use re_core::Diagnostic;
use re_render::{Capture, Renderer, SceneExtras, UiFrame, egui, wgpu};
use re_scene::Entity;
use re_types::mesh::{MeshData, SkinWeights};
use re_types::render::{
    DrawFlags, DrawItem, MaterialDesc, MaterialHandle, MeshHandle, PostSettings, RenderSnapshot,
    SkinnedDrawItem, SunShadows, TextureData, TextureHandle, TextureRef, WorldQuad,
};

/// A resource the view keeps: its data, and its handle once uploaded.
struct Kept<D, H> {
    data: D,
    handle: Option<H>,
}

impl<D, H> Kept<D, H> {
    fn new(data: D) -> Self {
        Kept { data, handle: None }
    }
}

/// The compat game's view; see the module docs.
#[derive(Default)]
pub struct CompatView {
    /// Meshes, with skin weights for a skinned `MeshPart`'s.
    meshes: HashMap<MeshKey, Kept<(MeshData, Option<SkinWeights>), MeshHandle>>,
    materials: HashMap<MaterialKey, Kept<MaterialDesc, MaterialHandle>>,
    textures: HashMap<String, Kept<TextureData, TextureHandle>>,
    /// Handles of released resources, freed at the next draw.
    dead_meshes: Vec<MeshHandle>,
    dead_materials: Vec<MaterialHandle>,
    /// The last frame built (its uploads and releases taken out).
    frame: Option<RenderFrame>,
    /// The last UI pass, not drawn yet.
    ui: Option<egui::FullOutput>,
    /// True once the bundled fonts are in the egui context.
    pub(crate) fonts: bool,
    /// The project's `[render.post]` and `[render.shadows]`, under the
    /// place's own exposure and grading.
    post: PostSettings,
    shadows: SunShadows,
    /// The surfaces and viewports of the last UI pass.
    surfaces: Vec<SurfaceDraw>,
    viewports: Vec<ViewportDraw>,
    /// The surfaces' textures, and what each holds.
    surface_targets: HashMap<Entity, SurfaceTarget>,
    redraws: SurfaceRedraws,
    /// `[compat.gui] surface_redraws_per_frame`.
    pub(crate) surface_budget: usize,
    /// The viewports' textures.
    viewport_targets: HashMap<Entity, ViewportTarget>,
}

/// A surface's texture.
struct SurfaceTarget {
    capture: Capture,
    texture: TextureHandle,
}

/// A viewport's texture, and the scene it last drew.
struct ViewportTarget {
    capture: Capture,
    id: egui::TextureId,
    drawn: Option<ViewportScene>,
}

impl CompatView {
    /// A view whose frames start from the project's `[render]` post and
    /// shadow settings.
    pub fn new(render: &crate::config::RenderConfig) -> CompatView {
        let mut view = CompatView::default();
        view.post = render.post;
        view.shadows = render.shadows;
        view
    }

    /// Takes a frame from the client VM: keeps its new resources, marks its
    /// released ones and remembers what it draws.
    pub fn take(&mut self, mut frame: RenderFrame) {
        for (name, data) in frame.new_textures.drain(..) {
            self.textures.insert(name, Kept::new(data));
        }
        for (key, data) in frame.new_meshes.drain(..) {
            self.meshes.insert(key, Kept::new((data, None)));
        }
        for (key, data, weights) in frame.new_skinned_meshes.drain(..) {
            self.meshes.insert(key, Kept::new((data, Some(weights))));
        }
        for (key, desc) in frame.new_materials.drain(..) {
            self.materials.insert(key, Kept::new(desc));
        }
        for key in frame.released_meshes.drain(..) {
            if let Some(h) = self.meshes.remove(&key).and_then(|k| k.handle) {
                self.dead_meshes.push(h);
            }
        }
        for key in frame.released_materials.drain(..) {
            if let Some(h) = self.materials.remove(&key).and_then(|k| k.handle) {
                self.dead_materials.push(h);
            }
        }
        self.frame = Some(frame);
    }

    /// Keeps the surfaces and viewports of a UI pass.
    pub fn set_textured(&mut self, surfaces: Vec<SurfaceDraw>, viewports: Vec<ViewportDraw>) {
        self.surfaces = surfaces;
        self.viewports = viewports;
    }

    /// The texture of each viewport, for the UI pass to show
    /// ([`GuiFrame::images`](re_compat::gui::GuiFrame)).
    pub fn images(&self) -> HashMap<Entity, egui::TextureId> {
        self.viewport_targets
            .iter()
            .map(|(e, t)| (*e, t.id))
            .collect()
    }

    /// Gives each `ViewportFrame` of the last frame a texture (a pixel
    /// until its size is known), so the next UI pass can show it, and frees
    /// those of viewports gone from it.
    ///
    /// # Errors
    ///
    /// A capture cannot be made.
    pub fn prepare_images(
        &mut self,
        device: &wgpu::Device,
        r: &mut Renderer,
    ) -> Result<(), Diagnostic> {
        let Some(f) = &self.frame else {
            return Ok(());
        };
        let gone: Vec<Entity> = self
            .viewport_targets
            .keys()
            .filter(|e| !f.viewports.iter().any(|v| v.entity == **e))
            .copied()
            .collect();
        for e in gone {
            if let Some(t) = self.viewport_targets.remove(&e) {
                r.free_ui_texture(t.id);
            }
        }
        for v in &f.viewports {
            if let std::collections::hash_map::Entry::Vacant(slot) =
                self.viewport_targets.entry(v.entity)
            {
                let capture = viewport_capture(device, [1, 1])?;
                let id = r.register_ui_texture(capture.view());
                slot.insert(ViewportTarget {
                    capture,
                    id,
                    drawn: None,
                });
            }
        }
        Ok(())
    }

    /// Keeps a UI pass to draw; the texture changes of a pass that was not
    /// drawn (the font atlas) go first.
    pub fn set_ui(&mut self, mut output: egui::FullOutput) {
        if let Some(old) = self.ui.take() {
            let mut delta = old.textures_delta;
            delta.append(std::mem::take(&mut output.textures_delta));
            output.textures_delta = delta;
        }
        self.ui = Some(output);
    }

    /// The UI pass to draw, if there is one.
    pub fn take_ui(&mut self) -> Option<egui::FullOutput> {
        self.ui.take()
    }

    /// A copy of the UI pass not drawn yet, without its texture changes: it
    /// draws the same pass again after those were applied.
    pub fn ui_again(&self) -> Option<egui::FullOutput> {
        self.ui.as_ref().map(|o| {
            let mut again = o.clone();
            again.textures_delta.clear();
            again
        })
    }

    /// Takes the texture changes not drawn yet (the font atlas) as a pass
    /// with nothing to paint, leaving the pass itself. egui reports each change once, so they must
    /// reach the renderer whatever happens to the pass.
    pub fn take_textures(&mut self) -> Option<egui::FullOutput> {
        let o = self.ui.as_mut()?;
        if o.textures_delta.is_empty() {
            return None;
        }
        Some(egui::FullOutput {
            textures_delta: std::mem::take(&mut o.textures_delta),
            pixels_per_point: o.pixels_per_point,
            ..Default::default()
        })
    }

    /// True once a frame was built.
    pub fn has_frame(&self) -> bool {
        self.frame.is_some()
    }

    /// The last frame built.
    pub fn frame(&self) -> Option<&RenderFrame> {
        self.frame.as_ref()
    }

    /// Draws `frame` again (its resources were taken when it was built).
    pub fn restore_frame(&mut self, frame: RenderFrame) {
        self.frame = Some(frame);
    }

    /// Frees what was released, uploads what is missing and returns the
    /// frame to draw: the snapshot (the project's post and shadow settings,
    /// with the place's exposure and colour grading over them) and the
    /// extras. `None` before the first frame.
    ///
    /// # Errors
    ///
    /// The renderer's errors.
    pub fn prepare(
        &mut self,
        r: &mut Renderer,
    ) -> Result<Option<(RenderSnapshot, SceneExtras)>, Diagnostic> {
        for h in self.dead_meshes.drain(..) {
            r.release_mesh(h)?;
        }
        for h in self.dead_materials.drain(..) {
            r.release_material(h)?;
        }
        let Some(f) = &self.frame else {
            return Ok(None);
        };
        for t in self.textures.values_mut() {
            if t.handle.is_none() {
                t.handle = Some(r.upload_texture(&t.data)?);
            }
        }
        for m in self.meshes.values_mut() {
            if m.handle.is_none() {
                m.handle = Some(match &m.data {
                    (data, Some(weights)) => r.upload_skinned_mesh(data, weights)?,
                    (data, None) => r.upload_mesh(data)?,
                });
            }
        }
        for m in self.materials.values_mut() {
            if m.handle.is_none() {
                let mut desc = m.data.clone();
                if let Some(TextureRef::Asset(name)) = &desc.base_color_texture {
                    desc.base_color_texture = self
                        .textures
                        .get(name)
                        .and_then(|t| t.handle)
                        .map(TextureRef::Handle);
                }
                m.handle = Some(r.create_material(&desc)?);
            }
        }
        let items = self.draw_items(&f.items);
        let skinned = self.skinned_items(f);
        let snapshot = RenderSnapshot {
            camera: f.camera,
            lights: f.lights.clone(),
            items,
            skinned,
            joint_palette: f.joint_palette.clone(),
            clear: f.clear,
            post: f.post_settings(self.post),
            shadows: self.shadows,
            ..RenderSnapshot::default()
        };
        Ok(Some((snapshot, extras(f))))
    }

    /// The renderer's items of `items`, skipping any whose mesh or material
    /// is not uploaded.
    fn draw_items(&self, items: &[FrameItem]) -> Vec<DrawItem> {
        items
            .iter()
            .filter_map(|i| {
                Some(DrawItem {
                    entity: i.entity,
                    mesh: self.meshes.get(&i.mesh)?.handle?,
                    material: self.materials.get(&i.material)?.handle?,
                    world: i.world,
                    flags: if i.cast_shadow {
                        DrawFlags::CAST_SHADOWS | DrawFlags::RECEIVE_SHADOWS
                    } else {
                        DrawFlags::RECEIVE_SHADOWS
                    },
                })
            })
            .collect()
    }

    /// The renderer's skinned items of a frame, skipping any whose mesh or
    /// material is not uploaded.
    fn skinned_items(&self, f: &RenderFrame) -> Vec<SkinnedDrawItem> {
        f.skinned
            .iter()
            .filter_map(|i| {
                Some(SkinnedDrawItem {
                    entity: i.entity,
                    mesh: self.meshes.get(&i.mesh)?.handle?,
                    material: self.materials.get(&i.material)?.handle?,
                    world: i.world,
                    flags: if i.cast_shadow {
                        DrawFlags::CAST_SHADOWS | DrawFlags::RECEIVE_SHADOWS
                    } else {
                        DrawFlags::RECEIVE_SHADOWS
                    },
                    palette: i.palette,
                    joints: i.joints,
                    bounds: i.bounds,
                })
            })
            .collect()
    }

    /// The world quads of the surfaces that have a texture.
    pub fn quads(&self) -> Vec<WorldQuad> {
        self.surfaces
            .iter()
            .filter_map(|s| {
                let t = self.surface_targets.get(&s.entity)?;
                Some(WorldQuad {
                    entity: re_scene::entity_id(s.entity),
                    world: s.world,
                    size: s.size,
                    texture: t.texture,
                    depth: s.depth,
                    light_influence: s.light_influence,
                    brightness: s.brightness,
                })
            })
            .collect()
    }

    /// Draws the viewports whose scene or size changed, then paints the
    /// surfaces [`SurfaceRedraws`] plans (with `ui`'s texture changes, the
    /// font atlas, applied first), before the frame that shows them.
    /// `pixels_per_point` is the screen UI's. Call it after
    /// [`prepare`](Self::prepare), which uploads the viewports' meshes.
    ///
    /// # Errors
    ///
    /// The renderer's errors.
    pub fn draw_textured(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        r: &mut Renderer,
        ctx: &egui::Context,
        ui: Option<&mut egui::FullOutput>,
        pixels_per_point: f32,
    ) -> Result<(), Diagnostic> {
        let Some(f) = &self.frame else {
            return Ok(());
        };
        // Viewports first: a surface may show one.
        let mut repaint = Vec::new();
        for v in &self.viewports {
            let Some(scene) = f.viewports.iter().find(|s| s.entity == v.entity) else {
                continue;
            };
            let Some(t) = self.viewport_targets.get(&v.entity) else {
                continue;
            };
            let ppp = match v.surface {
                Some(s) => self
                    .surfaces
                    .iter()
                    .find(|d| d.entity == s)
                    .map_or(1.0, |d| d.pixels_per_point),
                None => pixels_per_point,
            };
            let px = (v.size * ppp)
                .ceil()
                .clamp(glam::Vec2::ONE, glam::Vec2::splat(MAX_VIEWPORT));
            let px = [px.x as u32, px.y as u32];
            let resized = [t.capture.width(), t.capture.height()] != px;
            if !resized && t.drawn.as_ref() == Some(scene) {
                continue;
            }
            let snapshot = RenderSnapshot {
                camera: scene.camera.unwrap_or_default(),
                lights: vec![scene.light],
                items: if scene.camera.is_some() {
                    self.draw_items(&scene.items)
                } else {
                    Vec::new()
                },
                clear: [0.0; 4],
                // The main frame's cascades, so the shadow map is not
                // reallocated between the two.
                shadows: self.shadows,
                ..RenderSnapshot::default()
            };
            let extras = SceneExtras {
                ambient: Some(re_render::Ambient {
                    sky: scene.ambient,
                    ground: scene.ambient,
                }),
                ..SceneExtras::default()
            };
            let t = self
                .viewport_targets
                .get_mut(&v.entity)
                .expect("looked up above");
            if resized {
                t.capture = viewport_capture(device, px)?;
                r.update_ui_texture(t.id, t.capture.view());
            }
            r.render_with(device, queue, t.capture.view(), &snapshot, &extras)?;
            t.drawn = Some(scene.clone());
            if let Some(s) = v.surface {
                repaint.push(s);
            }
        }
        for s in repaint {
            self.redraws.invalidate(s);
        }

        let plan = self.redraws.plan(&self.surfaces, self.surface_budget);
        for e in &plan.gone {
            if let Some(t) = self.surface_targets.remove(e) {
                r.release_texture(t.texture)?;
            }
        }
        let mut ui = ui;
        let mut delta = ui.as_mut().map(|o| std::mem::take(&mut o.textures_delta));
        for &i in &plan.redraw {
            let s = &self.surfaces[i];
            let fits = self
                .surface_targets
                .get(&s.entity)
                .is_some_and(|t| [t.capture.width(), t.capture.height()] == s.texture_px);
            if !fits {
                if let Some(old) = self.surface_targets.remove(&s.entity) {
                    r.release_texture(old.texture)?;
                }
                let capture = Capture::new(device, s.texture_px[0], s.texture_px[1])?;
                let texture = r.capture_texture(&capture);
                self.surface_targets
                    .insert(s.entity, SurfaceTarget { capture, texture });
            }
            let t = &self.surface_targets[&s.entity];
            // Cleared to transparent, then the UI over it.
            let clear = RenderSnapshot {
                clear: [0.0; 4],
                shadows: self.shadows,
                ..RenderSnapshot::default()
            };
            r.render(device, queue, t.capture.view(), &clear)?;
            let frame = UiFrame {
                primitives: ctx.tessellate(s.shapes.clone(), s.pixels_per_point),
                textures_delta: delta.take().unwrap_or_default(),
                size_px: s.texture_px,
                pixels_per_point: s.pixels_per_point,
            };
            r.paint_ui(device, queue, t.capture.view(), &frame)?;
        }
        // Texture changes no surface took go back to the UI pass.
        if let (Some(d), Some(o)) = (delta, ui) {
            o.textures_delta = d;
        }
        Ok(())
    }

    /// Frees everything on the GPU and forgets it (a restart, whose new VM
    /// numbers its meshes from the start, or the end of a `play.run` copy).
    pub fn release_all(&mut self, r: &mut Renderer) {
        self.dead_meshes
            .extend(self.meshes.drain().filter_map(|(_, k)| k.handle));
        self.dead_materials
            .extend(self.materials.drain().filter_map(|(_, k)| k.handle));
        for h in self.dead_meshes.drain(..) {
            let _ = r.release_mesh(h);
        }
        for h in self.dead_materials.drain(..) {
            let _ = r.release_material(h);
        }
        for (_, t) in self.textures.drain() {
            if let Some(h) = t.handle {
                let _ = r.release_texture(h);
            }
        }
        for (_, t) in self.surface_targets.drain() {
            let _ = r.release_texture(t.texture);
        }
        for (_, t) in self.viewport_targets.drain() {
            r.free_ui_texture(t.id);
        }
        self.redraws.clear();
        self.surfaces.clear();
        self.viewports.clear();
        self.frame = None;
        // The texture changes are kept: the egui context outlives the view.
        let kept = self.take_textures();
        self.ui = kept;
    }
}

impl Drop for CompatView {
    fn drop(&mut self) {
        // Nothing is left to draw them on: the GPU side goes too.
        if let Some(o) = &mut self.ui {
            o.textures_delta.clear();
        }
    }
}

/// A viewport's texture: not sRGB, so egui samples the gamma-encoded
/// colour it expects.
fn viewport_capture(device: &wgpu::Device, px: [u32; 2]) -> Result<Capture, Diagnostic> {
    Capture::with_format(device, px[0], px[1], wgpu::TextureFormat::Rgba8Unorm)
}

/// The renderer's extras of a frame.
fn extras(f: &RenderFrame) -> SceneExtras {
    use re_compat::render::SpriteShape;
    use re_render as rr;
    SceneExtras {
        ambient: Some(rr::Ambient {
            sky: f.ambient.0,
            ground: f.ambient.1,
        }),
        fog: f.fog.map(|g| rr::Fog {
            start: g.start,
            end: g.end,
            color: g.color,
        }),
        sky: Some(rr::Sky {
            zenith: f.sky.zenith,
            horizon: f.sky.horizon,
            ground: f.sky.ground,
            sun: f.sky.sun,
        }),
        spot_lights: f
            .spot_lights
            .iter()
            .map(|s| rr::SpotLight {
                position: s.position,
                direction: s.direction,
                color: s.color,
                intensity: s.intensity,
                range: s.range,
                half_angle: s.half_angle,
            })
            .collect(),
        sprites: f
            .sprites
            .iter()
            .map(|s| rr::Sprite {
                position: s.position,
                size: s.size,
                rotation: s.rotation,
                color: s.color,
                emission: s.emission,
                shape: match s.shape {
                    SpriteShape::Soft => rr::SpriteShape::Soft,
                    SpriteShape::Square => rr::SpriteShape::Square,
                },
            })
            .collect(),
        ribbons: f
            .ribbons
            .iter()
            .map(|r| rr::Ribbon {
                points: r
                    .points
                    .iter()
                    .map(|p| rr::RibbonPoint {
                        position: p.position,
                        width: p.width,
                        color: p.color,
                    })
                    .collect(),
                normal: r.normal,
                emission: r.emission,
            })
            .collect(),
        highlights: f
            .highlights
            .iter()
            .map(|h| rr::Highlight {
                entities: h.entities.clone(),
                fill: h.fill,
                outline: h.outline,
                always_on_top: h.always_on_top,
            })
            .collect(),
    }
}
