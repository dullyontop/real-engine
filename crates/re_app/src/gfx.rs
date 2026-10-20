//! [`Graphics`]: a GPU device and the renderer, the uploads that keep the
//! renderer in step with the World, the HUD, and captures.
//!
//! The windowed runner builds one for its window surface; headless runs build
//! one only when a frame must be captured (`--screenshot`, the `screenshot`
//! command, golden checks).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use re_core::time::Instant;
use re_core::{Diagnostic, Location};
use re_render::{Capture, Gpu, Image, Renderer, UiFrame, egui, wgpu};
use re_scene::World;
use re_types::{MaterialDesc, RenderSnapshot, TextureHandle, TextureRef};

use crate::codes;
use crate::compat_view::CompatView;
use crate::hud::{self, HudLine};
use crate::media::{DERIVED, LoadedMaterial, Media};
use crate::sync::render::{self, RenderAssets};

/// The wgpu backends for a project: `RE_BACKENDS` when set, else the first of
/// `real.toml [render] backends` that parses (all of them combined), else the
/// platform's primary API.
pub fn backends(preference: &[String]) -> Result<wgpu::Backends, Diagnostic> {
    if std::env::var("RE_BACKENDS").is_ok_and(|v| !v.trim().is_empty()) {
        return re_render::backends_from_env();
    }
    let mut out = wgpu::Backends::empty();
    for name in preference {
        if let Some(b) = re_render::parse_backends(name) {
            out |= b;
        }
    }
    Ok(if out.is_empty() {
        wgpu::Backends::PRIMARY
    } else {
        out
    })
}

/// The lower-case name of a backend, as golden paths use it (`{backend}`).
pub fn backend_name(backend: wgpu::Backend) -> &'static str {
    match backend {
        wgpu::Backend::Vulkan => "vulkan",
        wgpu::Backend::Metal => "metal",
        wgpu::Backend::Dx12 => "dx12",
        wgpu::Backend::Gl => "gl",
        wgpu::Backend::BrowserWebGpu => "webgpu",
        _ => "noop",
    }
}

/// The GPU side of an [`App`](crate::App).
pub struct Graphics {
    renderer: Renderer,
    egui: egui::Context,
    textures: HashMap<String, TextureHandle>,
    started: Instant,
    keep_derived: bool,
    // Last, so the device outlives the renderer's resources.
    gpu: Gpu,
}

impl Graphics {
    /// Graphics with no window, drawing into offscreen captures.
    ///
    /// # Errors
    ///
    /// The renderer's error when no adapter or device is available.
    pub fn headless(backends: wgpu::Backends) -> Result<Graphics, Diagnostic> {
        let gpu = Gpu::headless(backends)?;
        Ok(Graphics::new(gpu, Capture::FORMAT))
    }

    /// Graphics on `gpu` whose main target has `format` (a window surface's).
    pub fn new(gpu: Gpu, format: wgpu::TextureFormat) -> Graphics {
        let renderer = Renderer::new(&gpu, format);
        Graphics::from_parts(gpu, renderer)
    }

    /// Graphics around a renderer the caller made on `gpu` (the editor's, which
    /// also paints its UI), so the app uploads its meshes and materials there.
    pub fn from_parts(gpu: Gpu, renderer: Renderer) -> Graphics {
        Graphics {
            renderer,
            egui: egui::Context::default(),
            textures: HashMap::new(),
            started: Instant::now(),
            keep_derived: false,
            gpu,
        }
    }

    /// The device and the renderer back (to drop the renderer first, or move
    /// them to another app).
    pub fn into_parts(self) -> (Gpu, Renderer) {
        (self.gpu, self.renderer)
    }

    /// The device.
    pub fn gpu(&self) -> &Gpu {
        &self.gpu
    }

    /// The renderer.
    pub fn renderer(&self) -> &Renderer {
        &self.renderer
    }

    /// The renderer, mutably.
    pub fn renderer_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    /// While true, `derived:` meshes nothing names are kept (set while the
    /// graphics are lent to a `play.run` copy, whose world is not the live one).
    pub fn set_keep_derived(&mut self, keep: bool) {
        self.keep_derived = keep;
    }

    /// The backend's lower-case name (`vulkan`, `gl`, `metal`, `dx12`).
    pub fn backend(&self) -> &'static str {
        backend_name(self.gpu.backend())
    }

    /// Uploads the meshes and materials the World's entities name that are not
    /// on the GPU yet, and releases `derived:` meshes nothing names any more.
    /// Each asset that cannot be loaded is reported once (`RE0520`).
    pub fn sync_assets(
        &mut self,
        world: &mut World,
        media: &mut Media,
        assets: &mut RenderAssets,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        if assets.default_material.is_none() {
            match self.renderer.create_material(&MaterialDesc {
                base_color: [0.8, 0.8, 0.8, 1.0],
                roughness: 0.8,
                ..MaterialDesc::default()
            }) {
                Ok(h) => assets.default_material = Some(h),
                Err(d) => diagnostics.push(d),
            }
        }
        let (meshes, materials) = render::referenced_assets(world);
        let wanted: HashSet<&str> = meshes.iter().map(String::as_str).collect();
        let unused: Vec<String> = assets
            .mesh_assets()
            .filter(|a| !self.keep_derived && a.starts_with(DERIVED) && !wanted.contains(a))
            .map(str::to_string)
            .collect();
        for asset in unused {
            self.release_mesh(assets, &asset, diagnostics);
            media.remove_derived_mesh(&asset);
        }
        for asset in &meshes {
            if assets.mesh(asset).is_some() {
                continue;
            }
            let loaded = match media.mesh(asset) {
                Ok(l) => l,
                Err(d) => {
                    report(media, asset, d, diagnostics);
                    continue;
                }
            };
            match self.renderer.upload_mesh(&loaded.mesh) {
                Ok(h) => assets.insert_mesh(asset.clone(), h),
                Err(d) => {
                    report(media, asset, d, diagnostics);
                    continue;
                }
            }
            if let Some(material) = loaded.material {
                match self.create_material(&material) {
                    Ok(h) => assets.insert_mesh_material(asset.clone(), h),
                    Err(d) => report(media, asset, d, diagnostics),
                }
            }
        }
        for asset in &materials {
            if assets.material(asset).is_some() {
                continue;
            }
            let made = media.material(asset).and_then(|m| self.create_material(&m));
            match made {
                Ok(h) => assets.insert_material(asset.clone(), h),
                Err(d) => report(media, asset, d, diagnostics),
            }
        }
    }

    /// Forgets and releases the GPU copies of `stale` assets (changed on disk),
    /// so the next [`Graphics::sync_assets`] uploads them again.
    pub fn invalidate(
        &mut self,
        assets: &mut RenderAssets,
        stale: &[String],
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        for asset in stale {
            self.release_mesh(assets, asset, diagnostics);
            if let Some(h) = assets.remove_material(asset)
                && let Err(d) = self.renderer.release_material(h)
            {
                diagnostics.push(d);
            }
        }
        if !stale.is_empty() {
            // Textures are shared between materials; upload them afresh.
            for (_, h) in self.textures.drain() {
                let _ = self.renderer.release_texture(h);
            }
        }
    }

    fn release_mesh(
        &mut self,
        assets: &mut RenderAssets,
        asset: &str,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        if let Some(h) = assets.remove_mesh(asset)
            && let Err(d) = self.renderer.release_mesh(h)
        {
            diagnostics.push(d);
        }
        if let Some(h) = assets.remove_mesh_material(asset)
            && let Err(d) = self.renderer.release_material(h)
        {
            diagnostics.push(d);
        }
    }

    fn create_material(
        &mut self,
        material: &LoadedMaterial,
    ) -> Result<re_types::MaterialHandle, Diagnostic> {
        let mut desc = material.desc.clone();
        for (slot, id, data) in &material.textures {
            let handle = match self.textures.get(id) {
                Some(h) => *h,
                None => {
                    let h = self.renderer.upload_texture(data)?;
                    self.textures.insert(id.clone(), h);
                    h
                }
            };
            let target = match slot {
                0 => &mut desc.base_color_texture,
                1 => &mut desc.metallic_roughness_texture,
                _ => &mut desc.normal_texture,
            };
            *target = Some(TextureRef::Handle(handle));
        }
        self.renderer.create_material(&desc)
    }

    /// Draws `snapshot` and the HUD into `target` (`size_px` physical pixels).
    ///
    /// # Errors
    ///
    /// The renderer's errors.
    pub fn draw(
        &mut self,
        target: &wgpu::TextureView,
        size_px: [u32; 2],
        pixels_per_point: f32,
        snapshot: &RenderSnapshot,
        hud_lines: &[HudLine],
    ) -> Result<(), Diagnostic> {
        self.renderer
            .render(&self.gpu.device, &self.gpu.queue, target, snapshot)?;
        if hud_lines.is_empty() {
            return Ok(());
        }
        let time = self.started.elapsed().as_secs_f64();
        let mut output = hud::run(&self.egui, hud_lines, size_px, pixels_per_point, time);
        let ui = UiFrame::from_output(&self.egui, &mut output, size_px);
        self.renderer
            .paint_ui(&self.gpu.device, &self.gpu.queue, target, &ui)
    }

    /// Draws a compat game's view (its 3D frame and its UI pass, or the
    /// HUD without one) into `target` (`size_px` physical pixels).
    ///
    /// # Errors
    ///
    /// The renderer's errors.
    pub fn draw_compat(
        &mut self,
        target: &wgpu::TextureView,
        size_px: [u32; 2],
        pixels_per_point: f32,
        view: &mut CompatView,
        hud_lines: &[HudLine],
    ) -> Result<(), Diagnostic> {
        let (device, queue) = (&self.gpu.device, &self.gpu.queue);
        match view.prepare(&mut self.renderer)? {
            Some((snapshot, extras)) => {
                self.renderer
                    .render_with(device, queue, target, &snapshot, &extras)?;
            }
            None => {
                self.renderer
                    .render(device, queue, target, &RenderSnapshot::default())?;
            }
        }
        let mut output = match view.take_ui() {
            Some(o) => o,
            None if hud_lines.is_empty() => return Ok(()),
            None => {
                let time = self.started.elapsed().as_secs_f64();
                hud::run(&self.egui, hud_lines, size_px, pixels_per_point, time)
            }
        };
        let ui = UiFrame::from_output(&self.egui, &mut output, size_px);
        self.renderer.paint_ui(device, queue, target, &ui)
    }

    /// Draws a compat game's view offscreen and reads the image back.
    ///
    /// # Errors
    ///
    /// The renderer's errors.
    pub fn capture_compat(
        &mut self,
        size: [u32; 2],
        view: &mut CompatView,
        hud_lines: &[HudLine],
    ) -> Result<Image, Diagnostic> {
        let capture = Capture::new(&self.gpu.device, size[0], size[1])?;
        self.draw_compat(capture.view(), size, 1.0, view, hud_lines)?;
        capture.read(&self.gpu.device, &self.gpu.queue)
    }

    /// The egui context the HUD and a compat game's UI are laid out with.
    pub fn egui(&self) -> &egui::Context {
        &self.egui
    }

    /// Draws `snapshot` and the HUD offscreen and reads the image back.
    ///
    /// # Errors
    ///
    /// The renderer's errors.
    pub fn capture(
        &mut self,
        size: [u32; 2],
        snapshot: &RenderSnapshot,
        hud_lines: &[HudLine],
    ) -> Result<Image, Diagnostic> {
        let capture = Capture::new(&self.gpu.device, size[0], size[1])?;
        self.draw(capture.view(), size, 1.0, snapshot, hud_lines)?;
        capture.read(&self.gpu.device, &self.gpu.queue)
    }

    /// GPU errors reported since the last call.
    pub fn take_errors(&self) -> Vec<Diagnostic> {
        self.gpu.errors().take()
    }
}

fn report(media: &mut Media, asset: &str, d: Diagnostic, diagnostics: &mut Vec<Diagnostic>) {
    if !media.first_report(asset) {
        return;
    }
    if d.code == codes::ASSET_UNAVAILABLE {
        diagnostics.push(d);
    } else {
        diagnostics.push(
            Diagnostic::error(
                codes::ASSET_UNAVAILABLE,
                format!("`{asset}` cannot be loaded"),
            )
            .at(Location::file(asset))
            .with_related(d),
        );
    }
}

/// Writes `image` as a PNG at `path`, creating its directory.
///
/// # Errors
///
/// `RE0516` when the file cannot be written.
pub fn save_png(image: &Image, path: &Path) -> Result<(), Diagnostic> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        std::fs::create_dir_all(dir).map_err(|e| {
            Diagnostic::error(
                codes::SCREENSHOT_FAILED,
                format!("cannot create {}: {e}", dir.display()),
            )
        })?;
    }
    image.save_png(path).map_err(|d| {
        Diagnostic::error(
            codes::SCREENSHOT_FAILED,
            format!("cannot write the screenshot {}", path.display()),
        )
        .with_related(d)
    })
}
