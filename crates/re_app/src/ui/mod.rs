//! Native game UI (ADR-0058): the host between the World's UI components
//! and the UI core, `re_ui`. At Sync it builds the frame's tree from the
//! components ([`build`]) and lays it out at the run's view, which gives the
//! values scripts read and input routes by. When the app has graphics, the
//! pass that draws the HUD ([`hud`]) lays the tree out again at the size it
//! draws and paints it: world roots far to near, a compat game's GUI, screen
//! roots by order, then the HUD.
//!
//! The host's egui context lays out and paints every UI the game draws (the
//! graphics adopt it), so text laid out at Sync uses the same font atlas. A
//! `UiImage` is an egui texture the host makes from the image file (in a
//! packaged game, from the form the build stored it in), which the UI pass
//! uploads like the font atlas; a run without graphics loads none. egui
//! takes its textures with premultiplied alpha, which the renderer's
//! textures do not have, so images are not uploaded through the renderer.

pub mod build;
pub mod hud;

use std::collections::{HashMap, HashSet};

use glam::{Vec2, Vec3};
use re_assets::AssetServer;
use re_core::files::{FileError, ProjectFiles};
use re_core::{Code, DiagInfo, Diagnostic, Location};
use re_render::egui;
use re_render::egui::epaint::ClippedShape;
use re_scene::{Entity, StableId, World, entity_id};
use re_types::render::{Camera as RenderCamera, TextureFormat};
use re_ui::theme::State;
use re_ui::{Camera, Item, Layout, Looks, Painter, Root, Theme, ThemeFile, Visit};

pub use build::{Built, HostRoot, Space};
pub use re_ui::theme;

use crate::codes;

const N: usize = re_ui::DIAGNOSTICS.len();
const CATALOG: [DiagInfo; N] = {
    let first = re_ui::DIAGNOSTICS[0];
    let mut out = [DiagInfo {
        code: Code::new(first.0),
        title: first.1,
        page: first.2,
    }; N];
    let mut i = 1;
    while i < N {
        let (code, title, page) = re_ui::DIAGNOSTICS[i];
        out[i] = DiagInfo {
            code: Code::new(code),
            title,
            page,
        };
        i += 1;
    }
    out
};

/// The UI core's diagnostics (`RE7700`–`RE7799`), as `real explain` lists
/// them: re_ui reaches no engine crate, so its catalogue is plain text.
pub const DIAGNOSTICS: &[DiagInfo] = &CATALOG;

/// Where the UI is drawn: a view of `size_px` physical pixels at
/// `pixels_per_point`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    /// The view's size in physical pixels.
    pub size_px: [u32; 2],
    /// Physical pixels per point.
    pub pixels_per_point: f32,
}

impl View {
    /// The view's size in points, the unit the UI is laid out in.
    pub fn points(self) -> Vec2 {
        Vec2::new(self.size_px[0] as f32, self.size_px[1] as f32) / self.pixels_per_point.max(1e-3)
    }
}

/// What painting the frame's UI takes: the tree built at Sync and the camera
/// that places its world roots. [`UiHost::take_frame`] lends it to a pass the
/// host does not run (a compat game's).
#[derive(Clone, Debug, Default)]
pub struct Frame {
    built: Built,
    camera: Option<Camera>,
}

impl Frame {
    /// The tree.
    pub fn built(&self) -> &Built {
        &self.built
    }

    /// Lays every root out in `ctx`'s current pass, in a view of `points`:
    /// the drawn items of each root (by its index in the built roots, in
    /// drawing order: world roots far to near, then screen roots), and the
    /// visits.
    fn lay_out(
        &mut self,
        ctx: &egui::Context,
        points: Vec2,
    ) -> (Vec<(usize, Vec<Item>)>, Vec<Visit>) {
        let mut placed: Vec<(usize, Root, f32)> = Vec::new();
        for (i, r) in self.built.roots.iter().enumerate() {
            match r.space {
                Space::Screen { reference_height } => {
                    let k = if reference_height > 0.0 && points.y > 0.0 {
                        points.y / reference_height
                    } else {
                        1.0
                    };
                    self.built.tree.node_mut(r.node).scale = Some(k);
                    placed.push((i, root(r, Vec2::ZERO, points / k), f32::NEG_INFINITY));
                }
                Space::World {
                    at,
                    canvas,
                    height,
                    max_distance,
                } => {
                    let Some(cam) = self.camera else { continue };
                    if canvas.x <= 0.0 || canvas.y <= 0.0 || height <= 0.0 {
                        continue;
                    }
                    if max_distance > 0.0 && (at - cam.position).length() > max_distance {
                        continue;
                    }
                    let Some((center, depth)) = cam.project(at, points) else {
                        continue;
                    };
                    let k = cam.focal(points) * height / depth / canvas.y;
                    self.built.tree.node_mut(r.node).scale = Some(k);
                    placed.push((i, root(r, center - canvas * k * 0.5, canvas), depth));
                }
            }
        }
        // World roots far to near, then the screen roots in their order.
        placed.sort_by(|a, b| b.2.total_cmp(&a.2));
        let mut layout = Layout::new(&self.built.tree, ctx);
        let laid = placed
            .iter()
            .map(|(i, r, _)| (*i, layout.root(r)))
            .collect();
        (laid, layout.take_visits())
    }

    /// The UI's shapes for `ctx`'s current pass over `view`: the world
    /// roots', and the screen roots'.
    pub fn shapes(
        &mut self,
        ctx: &egui::Context,
        view: View,
    ) -> (Vec<ClippedShape>, Vec<ClippedShape>) {
        if self.built.roots.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let points = view.points();
        let (laid, _) = self.lay_out(ctx, points);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(points.x, points.y));
        let looks = Looks::default();
        let mut world = Painter::new(&self.built.tree, screen);
        let mut over = Painter::new(&self.built.tree, screen);
        for (i, items) in &laid {
            let painter = match self.built.roots[*i].space {
                Space::World { .. } => &mut world,
                Space::Screen { .. } => &mut over,
            };
            for it in items {
                painter.item(it, &looks);
            }
        }
        (world.finish(), over.finish())
    }
}

/// The native UI of a running app.
pub struct UiHost {
    ctx: egui::Context,
    /// Whether the context has run a pass since its fonts were installed.
    warm: bool,
    files: ProjectFiles,
    /// Reads the images a packaged game stores processed; made when one is
    /// first needed.
    assets: Option<AssetServer>,
    /// Development runs report fields a style hides (`RE7702`).
    dev: bool,
    /// The project's theme file (`[ui] theme`), if it names one.
    theme_path: Option<String>,
    theme: Theme,
    root_themes: HashMap<String, Theme>,
    /// `UiRoot.theme` files that failed, reported once.
    bad_themes: HashSet<String>,
    states: HashMap<Entity, State>,
    images: HashMap<String, egui::TextureId>,
    /// Keeps the image textures alive.
    textures: HashMap<String, egui::TextureHandle>,
    /// Image paths that failed, reported once.
    bad_images: HashSet<String>,
    frame: Frame,
    visits: Vec<Visit>,
    /// Texture updates of the host's own passes, for the next pass painted.
    pending: egui::TexturesDelta,
    painted: bool,
    /// The time of the last pass, so the host's own passes never go back.
    time: f64,
    warned_styles: HashSet<String>,
    warned_fields: HashSet<(Entity, &'static str, &'static str)>,
}

impl UiHost {
    /// A host for a project read from `files`, with the project's theme
    /// file `theme` (`[ui] theme`) over the engine's. `dev` is true for a
    /// development run.
    ///
    /// Returns `RE7700` when the project's theme cannot be used; the engine's
    /// theme is used instead.
    pub fn new(files: ProjectFiles, theme: Option<&str>, dev: bool) -> (UiHost, Vec<Diagnostic>) {
        let ctx = egui::Context::default();
        re_ui::fonts::install(&ctx);
        let mut diagnostics = Vec::new();
        let theme_path = theme.filter(|t| !t.is_empty()).map(str::to_owned);
        let theme = match &theme_path {
            Some(path) => load_theme(&files, path).unwrap_or_else(|d| {
                diagnostics.extend(d);
                Theme::engine().clone()
            }),
            None => Theme::engine().clone(),
        };
        let host = UiHost {
            ctx,
            warm: false,
            files,
            assets: None,
            dev,
            theme_path,
            theme,
            root_themes: HashMap::new(),
            bad_themes: HashSet::new(),
            states: HashMap::new(),
            images: HashMap::new(),
            textures: HashMap::new(),
            bad_images: HashSet::new(),
            frame: Frame::default(),
            visits: Vec::new(),
            pending: egui::TexturesDelta::default(),
            painted: false,
            time: 0.0,
            warned_styles: HashSet::new(),
            warned_fields: HashSet::new(),
        };
        (host, diagnostics)
    }

    /// The egui context the UI (and the HUD, and a compat game's GUI) is laid
    /// out and painted with.
    pub fn ctx(&self) -> &egui::Context {
        &self.ctx
    }

    /// Starts painting for a new painter (the app's new graphics): the host
    /// moves to a new context, which the painter adopts, so the first pass
    /// uploads the whole font atlas and the images load again into it.
    /// Returns the context.
    pub fn attach(&mut self) -> egui::Context {
        self.pending.clear();
        self.ctx = egui::Context::default();
        re_ui::fonts::install(&self.ctx);
        self.warm = false;
        self.images.clear();
        self.textures.clear();
        self.painted = true;
        self.ctx.clone()
    }

    /// Stops painting (the app's graphics went away): the host's passes drop
    /// their texture updates from now on.
    pub fn detach(&mut self) {
        self.pending.clear();
        self.painted = false;
    }

    /// The tree built at the last Sync.
    pub fn built(&self) -> &Built {
        &self.frame.built
    }

    /// The layout values of the last Sync: one visit per drawn node, each
    /// root's container first.
    pub fn visits(&self) -> &[Visit] {
        &self.visits
    }

    /// The visit of `entity`'s node at the last Sync.
    pub fn visit(&self, entity: Entity) -> Option<&Visit> {
        self.visits
            .iter()
            .find(|v| self.frame.built.entity(v.node) == Some(entity))
    }

    /// Puts a node in a state for the next builds (input routing sets these;
    /// tests too).
    pub fn set_state(&mut self, entity: Entity, state: State) {
        if state == State::default() {
            self.states.remove(&entity);
        } else {
            self.states.insert(entity, state);
        }
    }

    /// Whether the last Sync found anything to draw.
    pub fn has_roots(&self) -> bool {
        !self.frame.built.roots.is_empty()
    }

    /// Lends the frame's tree to a pass the host does not run; give it back
    /// with [`UiHost::put_frame`].
    pub fn take_frame(&mut self) -> Frame {
        std::mem::take(&mut self.frame)
    }

    /// Gives back the frame [`UiHost::take_frame`] lent.
    pub fn put_frame(&mut self, frame: Frame) {
        self.frame = frame;
    }

    /// Sync: builds the frame's tree from `world` and lays it out at `view`,
    /// with world roots placed by `camera` (relative to the render `origin`).
    /// Style names a theme lacks (`RE7701`) and, in development runs, fields a
    /// style hides (`RE7702`) are reported once each.
    pub fn sync(
        &mut self,
        world: &mut World,
        camera: &RenderCamera,
        origin: Vec3,
        view: View,
        diagnostics: &mut Vec<Diagnostic>,
    ) {
        self.load_root_themes(world, diagnostics);
        if self.painted {
            self.load_images(world, diagnostics);
        }
        let mut findings = build::Findings::default();
        let inputs = build::Inputs {
            theme: &self.theme,
            root_themes: &self.root_themes,
            states: &self.states,
            images: &self.images,
            origin,
            check_hidden: self.dev,
        };
        self.frame.built = build::build(world, &inputs, &mut findings);
        self.report(world, findings, diagnostics);
        let (_, rotation, position) = camera.world.to_scale_rotation_translation();
        self.frame.camera = Some(Camera {
            position,
            rotation,
            fov_y: camera.fov_y_radians,
        });
        if self.frame.built.roots.is_empty() {
            self.visits.clear();
            return;
        }
        self.warm_up(view);
        let mut visits = Vec::new();
        self.pass(view, |frame, ctx| {
            visits = frame.lay_out(ctx, view.points()).1;
        });
        self.visits = visits;
    }

    /// Runs the first pass of the context, after which its fonts are the
    /// bundled ones.
    fn warm_up(&mut self, view: View) {
        if !self.warm {
            self.warm = true;
            self.pass(view, |_, _| {});
        }
    }

    /// Runs one egui pass of the host's context at `view`, keeping its
    /// texture updates for the next pass painted.
    fn pass(&mut self, view: View, mut f: impl FnMut(&mut Frame, &egui::Context)) {
        let ctx = self.ctx.clone();
        let frame = &mut self.frame;
        let mut output = ctx.run_ui(raw_input(view, self.time), |ui| f(frame, ui.ctx()));
        if self.painted {
            self.pending.append(output.textures_delta);
        } else {
            // Nothing will upload them (egui asserts that they are not
            // dropped unhandled).
            output.textures_delta.clear();
        }
    }

    /// Runs the passes that draw the UI and the HUD `lines` over `view` at
    /// `time`, and gives the output to paint, the host's earlier texture
    /// updates first. egui lays a new HUD area out invisibly on its first
    /// pass, so it runs twice and paints the second.
    pub fn run(&mut self, lines: &[hud::HudLine], view: View, time: f64) -> egui::FullOutput {
        self.warm_up(view);
        self.time = self.time.max(time);
        let input = raw_input(view, self.time);
        let ctx = self.ctx.clone();
        let frame = &mut self.frame;
        let mut pass = |ui: &mut egui::Ui| {
            let (world, screen) = frame.shapes(ui.ctx(), view);
            paint(ui.ctx(), world.into_iter().chain(screen).collect());
            hud::draw(ui.ctx(), lines);
        };
        let first = ctx.run_ui(input.clone(), &mut pass);
        let mut second = ctx.run_ui(input, &mut pass);
        let mut delta = std::mem::take(&mut self.pending);
        delta.append(first.textures_delta);
        delta.append(std::mem::take(&mut second.textures_delta));
        second.textures_delta = delta;
        second
    }

    /// Texture updates the host's own passes made since the last call: a
    /// caller painting another pass of the host's context sends them first.
    pub fn take_pending(&mut self) -> egui::TexturesDelta {
        std::mem::take(&mut self.pending)
    }

    /// Makes textures of the images `world`'s `UiImage`s name that have none
    /// yet. A file that cannot be read is `RE0520` and one that does not
    /// decode `RE7003`, once each.
    fn load_images(&mut self, world: &mut World, diagnostics: &mut Vec<Diagnostic>) {
        let paths: Vec<String> = world
            .ecs
            .query::<&re_scene::UiImage>()
            .iter(&world.ecs)
            .map(|i| i.image.clone())
            .filter(|p| !p.is_empty())
            .collect();
        for path in paths {
            if self.images.contains_key(&path) || self.bad_images.contains(&path) {
                continue;
            }
            match self.image(&path) {
                Ok(pixels) => {
                    let handle = self
                        .ctx
                        .load_texture(&path, pixels, egui::TextureOptions::LINEAR);
                    self.images.insert(path.clone(), handle.id());
                    self.textures.insert(path, handle);
                }
                Err(d) => {
                    diagnostics.push(d);
                    self.bad_images.insert(path);
                }
            }
        }
    }

    /// The image file at `path` as egui pixels: decoded from the source, or,
    /// where a packaged game stores it as the importer's output (ADR-0037),
    /// read through an asset server, with block-compressed pixels decoded.
    fn image(&mut self, path: &str) -> Result<egui::ColorImage, Diagnostic> {
        let texture = match self.files.read_source(path) {
            Ok(bytes) => re_assets::decode_image(&bytes, TextureFormat::Rgba8Srgb, path, path)?,
            Err(FileError::WrongForm { .. }) => {
                let assets = self
                    .assets
                    .get_or_insert_with(|| AssetServer::with_files(self.files.clone()));
                let handle = assets.load_texture(path)?;
                let texture = assets.texture(handle).cloned();
                assets.release(handle);
                texture.expect("a texture just loaded")
            }
            Err(e) => {
                return Err(Diagnostic::error(
                    codes::ASSET_UNAVAILABLE,
                    format!("`{path}` cannot be loaded: {e}"),
                )
                .at(Location::file(path)));
            }
        };
        let (w, h) = (texture.width, texture.height);
        let rgba = if texture.format.is_compressed() {
            re_render::decode_blocks(texture.format, w, h, &texture.pixels)
        } else {
            texture.pixels
        };
        Ok(egui::ColorImage::from_rgba_unmultiplied(
            [w as usize, h as usize],
            &rgba,
        ))
    }

    /// Files changed on disk (in a development run): the theme files among
    /// `paths` that are in use load again now, and images and themes that
    /// failed at the next Sync. A theme that no longer loads is `RE7700`,
    /// and the last good one stays (ADR-0058 part 7).
    pub fn files_changed(&mut self, paths: &[String]) -> Vec<Diagnostic> {
        let mut diagnostics = Vec::new();
        for path in paths {
            self.images.remove(path);
            self.textures.remove(path);
            self.bad_images.remove(path);
            self.bad_themes.remove(path);
            let used = self.theme_path.as_ref() == Some(path);
            if !used && !self.root_themes.contains_key(path) {
                continue;
            }
            match load_theme(&self.files, path) {
                Ok(theme) if used => {
                    if let Some(own) = self.root_themes.get_mut(path) {
                        *own = theme.clone();
                    }
                    self.theme = theme;
                }
                Ok(theme) => {
                    self.root_themes.insert(path.clone(), theme);
                }
                Err(d) => diagnostics.extend(d),
            }
        }
        diagnostics
    }

    /// Loads the theme files roots name that are not loaded yet.
    fn load_root_themes(&mut self, world: &mut World, diagnostics: &mut Vec<Diagnostic>) {
        let names: Vec<String> = world
            .ecs
            .query::<&re_scene::UiRoot>()
            .iter(&world.ecs)
            .map(|r| r.theme.clone())
            .filter(|t| !t.is_empty())
            .collect();
        for name in names {
            if self.root_themes.contains_key(&name) || self.bad_themes.contains(&name) {
                continue;
            }
            match load_theme(&self.files, &name) {
                Ok(t) => {
                    self.root_themes.insert(name, t);
                }
                Err(d) => {
                    diagnostics.extend(d);
                    self.bad_themes.insert(name);
                }
            }
        }
    }

    fn report(&mut self, world: &World, f: build::Findings, diagnostics: &mut Vec<Diagnostic>) {
        let at = |e: Entity| {
            let id = world
                .ecs
                .get::<StableId>(e)
                .map_or_else(|| entity_id(e).to_string(), |s| s.0.clone());
            Location::entity(id)
        };
        for (e, name) in f.missing {
            if self.warned_styles.insert(name.clone()) {
                diagnostics.push(
                    Diagnostic::warning(
                        Code::new(re_ui::codes::STYLE_MISSING),
                        format!("no style `{name}` in the theme; the node keeps its own look"),
                    )
                    .at(at(e)),
                );
            }
        }
        for (e, component, field) in f.hidden {
            if self.warned_fields.insert((e, component, field)) {
                diagnostics.push(
                    Diagnostic::warning(
                        Code::new(re_ui::codes::STYLE_HIDES_FIELD),
                        format!(
                            "`{component}.{field}` is set, but the node's style sets it too, so the value does not show"
                        ),
                    )
                    .at(at(e)),
                );
            }
        }
    }
}

/// Reads and resolves the theme file `path` over the engine's (`RE7700`).
impl Drop for UiHost {
    fn drop(&mut self) {
        // egui asserts that texture updates are not dropped unhandled.
        self.pending.clear();
    }
}

fn load_theme(files: &ProjectFiles, path: &str) -> Result<Theme, Vec<Diagnostic>> {
    let fail = |message: String| {
        Diagnostic::error(Code::new(re_ui::codes::THEME_INVALID), message).at(Location::file(path))
    };
    let text = files
        .read_str(path)
        .map_err(|e| vec![fail(format!("cannot read the theme `{path}`: {e}"))])?;
    let file = ThemeFile::parse(&text).map_err(|e| vec![fail(e.message)])?;
    Theme::new(&[ThemeFile::engine(), &file])
        .map_err(|errors| errors.into_iter().map(|e| fail(e.message)).collect())
}

fn root(r: &HostRoot, origin: Vec2, size: Vec2) -> Root {
    Root {
        node: r.node,
        origin,
        abs_origin: Vec2::ZERO,
        size,
        global_z: false,
        clip: None,
        input: r.input,
    }
}

/// egui's input for a pass over `view` at `time`.
pub fn raw_input(view: View, time: f64) -> egui::RawInput {
    hud::raw_input(view.size_px, view.pixels_per_point, time)
}

/// Paints UI shapes on a background layer of the context's current pass,
/// in order, under egui's own areas (the HUD).
pub fn paint(ctx: &egui::Context, shapes: Vec<ClippedShape>) {
    let layer = egui::LayerId::new(egui::Order::Background, egui::Id::new("re_app.ui"));
    let painter = ctx.layer_painter(layer);
    for s in shapes {
        painter.with_clip_rect(s.clip_rect).add(s.shape);
    }
}
