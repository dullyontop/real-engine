//! [`MaterialPreview`]: the Material panel's sphere (ADR-0064 §3), drawn
//! with the material the panel shows by the game view's renderer into a
//! 256-pixel offscreen capture that the panel shows.
//!
//! It is drawn again only when what it shows changes, at most once a frame:
//! the material (its open document's content, else its file's, with the
//! value under the pointer while a widget is dragged), the turn a drag on it
//! gives, or the light. The open scene's suns light it, with the scene's
//! exposure and tone mapping; a fixed light does when the scene has no sun.
//! Its sphere, material and textures are its own uploads to the app's
//! renderer, not the World's assets, and a texture is loaded as the game
//! view loads it. It draws before the game view, so the renderer's last
//! frame (its statistics, the shadow probes) stays the game view's.

use std::collections::HashMap;

use glam::{Mat4, Vec3};
use re_app::App;
use re_core::Diagnostic;
use re_render::{Capture, Image, Renderer, SceneExtras};
use re_types::render::PostSettings;
use re_types::{
    Camera, DrawFlags, DrawItem, EntityId, Light, LightKind, MaterialDesc, MaterialHandle,
    MeshHandle, RenderSnapshot, TextureData, TextureHandle, TextureRef,
};
use serde_json::Value as Json;

use crate::view::{VIEW_FORMAT, no_graphics};
use crate::{Editor, Panel};

/// The capture's width and height in pixels.
pub const SIZE: u32 = 256;

/// The clear colour behind the sphere, linear RGBA.
const BACKGROUND: [f32; 4] = [0.03, 0.03, 0.035, 1.0];

/// The Material panel's preview sphere. The [`GameView`](crate::GameView)
/// keeps it, since it needs both the app's renderer (to draw) and the UI
/// renderer (to show the capture in egui).
#[derive(Debug, Default)]
pub struct MaterialPreview {
    /// The capture and its egui id, once the panel has been shown.
    target: Option<(Capture, egui::TextureId)>,
    /// The sphere, uploaded once.
    sphere: Option<MeshHandle>,
    /// The material drawn last.
    material: Option<MaterialHandle>,
    /// The uploaded textures by asset id, with the asset server's version of
    /// each: a texture changed on disk is uploaded again.
    textures: HashMap<String, (u32, TextureHandle)>,
    /// What the capture shows.
    drawn: Option<Drawn>,
    /// Why the last material could not be drawn.
    error: Option<String>,
    /// How many times the sphere was drawn.
    draws: u64,
}

/// What a drawing of the preview depends on.
#[derive(Clone, Debug, PartialEq)]
struct Shown {
    path: String,
    value: Json,
    turn: f32,
    lights: Vec<Light>,
    post: PostSettings,
}

/// What the capture shows: what it was drawn from, and the asset server's
/// version of each texture file it used.
#[derive(Clone, Debug)]
struct Drawn {
    shown: Shown,
    textures: Vec<(String, u32)>,
}

/// A texture of the material to draw: its slot, asset id, the asset server's
/// version of its file, and its pixels unless that version is uploaded.
type Texture = (usize, String, u32, Option<TextureData>);

impl MaterialPreview {
    /// How many times the sphere has been drawn.
    pub fn draws(&self) -> u64 {
        self.draws
    }

    /// Why the shown material could not be drawn, if it could not.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The capture's pixels, once it has been drawn.
    ///
    /// # Errors
    ///
    /// `RE0516` when the app has no GPU side, and readback errors.
    pub fn read(&self, app: &App) -> Result<Option<Image>, Diagnostic> {
        let Some((capture, _)) = &self.target else {
            return Ok(None);
        };
        if self.drawn.is_none() {
            return Ok(None);
        }
        let gpu = app.graphics().ok_or_else(no_graphics)?.gpu();
        capture.read(&gpu.device, &gpu.queue).map(Some)
    }

    /// Makes the capture once the Material panel has been drawn, and hands
    /// its egui id and the last error to `editor`.
    ///
    /// # Errors
    ///
    /// `RE0516` when the app has no GPU side, `RE6005` when the texture cannot
    /// be created.
    pub(crate) fn sync(
        &mut self,
        app: &App,
        ui: &mut Renderer,
        editor: &mut Editor,
    ) -> Result<(), Diagnostic> {
        if self.target.is_none() && editor.rects().get(Panel::Material).is_some() {
            let device = &app.graphics().ok_or_else(no_graphics)?.gpu().device;
            let capture = Capture::with_format(device, SIZE, SIZE, VIEW_FORMAT)?;
            let id = ui.register_ui_texture(capture.view());
            self.target = Some((capture, id));
        }
        let shown = self.drawn.is_some().then_some(()).and(self.target.as_ref());
        editor.set_material_preview(shown.map(|(_, id)| *id), self.error.clone());
        Ok(())
    }

    /// Draws the sphere if the Material panel was drawn this frame and what
    /// it shows changed. An error is kept for the panel to show, and the
    /// frame goes on.
    pub(crate) fn render(&mut self, app: &mut App, editor: &Editor) {
        if self.target.is_none() || editor.rects().get(Panel::Material).is_none() {
            return;
        }
        let Some((path, value, turn)) = editor.material_preview_value(app) else {
            return;
        };
        let (lights, post) = light(app.snapshot());
        let shown = Shown {
            path,
            value,
            turn,
            lights,
            post,
        };
        if let Some(drawn) = &self.drawn
            && drawn.shown == shown
            && drawn
                .textures
                .iter()
                .all(|(file, v)| texture_version(app, file) == Some(*v))
        {
            return;
        }
        // Until what it shows changes, a material that cannot be drawn is
        // not tried again.
        let mut drawn = Drawn {
            shown,
            textures: Vec::new(),
        };
        let result = self
            .load(app, &mut drawn)
            .and_then(|(desc, textures)| self.draw(app, &desc, &textures, &drawn.shown));
        match result {
            Ok(()) => {
                self.error = None;
                self.draws += 1;
            }
            Err(d) => self.error = Some(d.message),
        }
        self.drawn = Some(drawn);
    }

    /// The material `drawn` shows, with its textures: the pixels of those not
    /// uploaded at the version the asset server has. Records each texture
    /// file's version in `drawn`.
    fn load(
        &self,
        app: &mut App,
        drawn: &mut Drawn,
    ) -> Result<(MaterialDesc, Vec<Texture>), Diagnostic> {
        let shown = &drawn.shown;
        let preview = re_app::documents::material::preview(app.files(), &shown.path, &shown.value)?;
        let server = app.media_mut().server();
        let mut textures = Vec::new();
        for (slot, id, file) in &preview.textures {
            let h = server.load_texture(file)?;
            let version = server.info(h.into()).map_or(0, |i| i.version);
            let uploaded = self.textures.get(id).is_some_and(|(v, _)| *v == version);
            let data = if uploaded {
                None
            } else {
                server.texture(h).cloned()
            };
            drawn.textures.push((file.clone(), version));
            textures.push((*slot, id.clone(), version, data));
        }
        Ok((preview.desc, textures))
    }

    /// Uploads what is missing and draws the sphere with `desc`.
    fn draw(
        &mut self,
        app: &mut App,
        desc: &MaterialDesc,
        textures: &[Texture],
        shown: &Shown,
    ) -> Result<(), Diagnostic> {
        let Some((capture, _)) = &self.target else {
            return Ok(());
        };
        let gfx = app.graphics_mut().ok_or_else(no_graphics)?;
        let (device, queue) = (gfx.gpu().device.clone(), gfx.gpu().queue.clone());
        let renderer = gfx.renderer_mut();
        let sphere = match self.sphere {
            Some(h) => h,
            None => {
                let mesh = re_app::media::builtin_mesh("sphere").expect("a built-in sphere");
                *self.sphere.insert(renderer.upload_mesh(&mesh)?)
            }
        };
        let mut desc = desc.clone();
        for (slot, id, version, data) in textures {
            let handle = match (self.textures.get(id), data) {
                (Some((v, h)), _) if v == version => *h,
                (_, Some(data)) => {
                    let h = renderer.upload_texture(data)?;
                    if let Some((_, old)) = self.textures.insert(id.clone(), (*version, h)) {
                        // The material drawn last may still name it.
                        if let Some(m) = self.material.take() {
                            let _ = renderer.release_material(m);
                        }
                        let _ = renderer.release_texture(old);
                    }
                    h
                }
                (_, None) => continue,
            };
            let target = match slot {
                0 => &mut desc.base_color_texture,
                1 => &mut desc.metallic_roughness_texture,
                _ => &mut desc.normal_texture,
            };
            *target = Some(TextureRef::Handle(handle));
        }
        // A texture the asset server could not give is drawn without.
        for t in [
            &mut desc.base_color_texture,
            &mut desc.metallic_roughness_texture,
            &mut desc.normal_texture,
        ] {
            if matches!(t, Some(TextureRef::Asset(_))) {
                *t = None;
            }
        }
        let material = renderer.create_material(&desc)?;
        if let Some(old) = self.material.replace(material) {
            let _ = renderer.release_material(old);
        }
        // Only the textures of the material drawn now stay uploaded.
        let used: Vec<TextureHandle> = desc
            .textures()
            .into_iter()
            .filter_map(|t| match t {
                Some(TextureRef::Handle(h)) => Some(*h),
                _ => None,
            })
            .collect();
        self.textures.retain(|_, (_, h)| {
            let keep = used.contains(h);
            if !keep {
                let _ = renderer.release_texture(*h);
            }
            keep
        });
        let snapshot = RenderSnapshot {
            camera: Camera {
                world: Mat4::from_translation(Vec3::new(0.0, 0.0, 1.9)),
                fov_y_radians: 35f32.to_radians(),
                near: 0.05,
                far: 10.0,
            },
            lights: shown.lights.clone(),
            items: vec![DrawItem {
                entity: EntityId(0),
                mesh: sphere,
                material,
                world: Mat4::from_rotation_y(shown.turn),
                flags: DrawFlags::NONE,
            }],
            clear: BACKGROUND,
            post: shown.post,
            ..RenderSnapshot::default()
        };
        renderer.render_with(
            &device,
            &queue,
            capture.view(),
            &snapshot,
            &SceneExtras::default(),
        )?;
        Ok(())
    }

    /// Forgets the capture's egui id (before the UI renderer is dropped).
    pub(crate) fn release(&mut self, ui: &mut Renderer) {
        if let Some((_, id)) = self.target.take() {
            ui.free_ui_texture(id);
        }
    }
}

/// The asset server's version of the texture file `file`, while it is
/// loaded.
fn texture_version(app: &mut App, file: &str) -> Option<u32> {
    let server = app.media_mut().server();
    let loaded = server.loaded(file)?;
    server.info(loaded).map(|i| i.version)
}

/// The light the preview is drawn in: the scene's suns, without their
/// shadows, with the scene's exposure and tone mapping; else a fixed light
/// from above, to the left and in front, at the default exposure.
fn light(scene: &RenderSnapshot) -> (Vec<Light>, PostSettings) {
    let suns: Vec<Light> = scene
        .lights
        .iter()
        .filter(|l| l.kind == LightKind::Directional)
        .map(|l| Light {
            shadows: false,
            ..*l
        })
        .collect();
    if !suns.is_empty() {
        return (suns, scene.post);
    }
    let fixed = Light {
        kind: LightKind::Directional,
        direction: Vec3::new(0.5, -0.6, -0.6).normalize(),
        intensity: 1.2,
        ..Light::default()
    };
    (vec![fixed], PostSettings::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scene_sun_lights_the_preview_and_a_fixed_light_stands_in() {
        let mut scene = RenderSnapshot::default();
        let (lights, _) = light(&scene);
        assert_eq!(lights.len(), 1);
        assert_eq!(lights[0].kind, LightKind::Directional);

        scene.lights = vec![
            Light::default(),
            Light {
                kind: LightKind::Directional,
                intensity: 100_000.0,
                shadows: true,
                ..Light::default()
            },
        ];
        scene.post.exposure = 1e-5;
        let (lights, post) = light(&scene);
        assert_eq!(lights.len(), 1, "point lights are the scene's own");
        assert_eq!(lights[0].intensity, 100_000.0);
        assert!(!lights[0].shadows);
        assert_eq!(post.exposure, 1e-5);
    }
}
