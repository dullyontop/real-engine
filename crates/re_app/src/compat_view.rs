//! The window side of a compat game: the local player's 3D view
//! (`re_compat::render`) and 2D UI (`re_compat::gui`), on the app's renderer.
//!
//! Each frame the client VM builds a [`RenderFrame`]: what to draw this
//! frame, plus the meshes, materials and textures to create or release. The
//! view keeps a CPU copy of each until it is released, so a frame built while
//! the app has no GPU side (or has lent it to a `play.run` copy) still draws
//! once one is there. The UI is one egui pass with the client's `ScreenGui`s
//! and the HUD lines, kept until it is drawn.

use std::collections::HashMap;

use re_compat::render::{MaterialKey, MeshKey, RenderFrame};
use re_core::Diagnostic;
use re_render::{Renderer, SceneExtras, egui};
use re_types::mesh::MeshData;
use re_types::render::{
    DrawFlags, DrawItem, MaterialDesc, MaterialHandle, MeshHandle, RenderSnapshot, TextureData,
    TextureHandle, TextureRef,
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
    meshes: HashMap<MeshKey, Kept<MeshData, MeshHandle>>,
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
}

impl CompatView {
    /// Takes a frame from the client VM: keeps its new resources, marks its
    /// released ones and remembers what it draws.
    pub fn take(&mut self, mut frame: RenderFrame) {
        for (name, data) in frame.new_textures.drain(..) {
            self.textures.insert(name, Kept::new(data));
        }
        for (key, data) in frame.new_meshes.drain(..) {
            self.meshes.insert(key, Kept::new(data));
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
    /// frame to draw: the snapshot (with the exposure and colour grading in
    /// its post settings) and the extras. `None` before the first frame.
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
                m.handle = Some(r.upload_mesh(&m.data)?);
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
        let items = f
            .items
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
            .collect();
        let mut snapshot = RenderSnapshot {
            camera: f.camera,
            lights: f.lights.clone(),
            items,
            clear: f.clear,
            ..RenderSnapshot::default()
        };
        snapshot.post.exposure = f.exposure;
        snapshot.post.grading = f.grading.map(|g| re_types::render::ColorGrading {
            brightness: g.brightness,
            contrast: g.contrast,
            saturation: g.saturation,
            tint: g.tint,
        });
        Ok(Some((snapshot, extras(f))))
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
