//! The 3D view (WP15): parts, materials, lights, the sky and fog, the
//! camera, particles, beams, trails and highlights, as data for a renderer.
//!
//! The host calls [`build`] once a frame, after the frame's scripts and
//! simulation, with the viewport. It returns a [`RenderFrame`]: the camera
//! and lights, one [`FrameItem`] per drawn part or merged static cell, the
//! environment (ambient light, fog, sky, post effects), the effects, and
//! the meshes, materials and textures to upload or release since the last
//! frame. Nothing here talks to the GPU: the host maps the frame onto its
//! renderer (`re_render`'s `RenderSnapshot` and `SceneExtras`), keeping a
//! table from [`MeshKey`], [`MaterialKey`] and texture names to its
//! handles.
//!
//! Each `ViewportFrame`'s parts come as a [`ViewportScene`] of their own.
//!
//! Anchored parts that `re_compat::physics` groups into a static cell
//! (ADR-0031) are drawn as one mesh per cell and material, with their colour
//! in the vertices; transparent, `Neon`, `ForceField` and highlighted parts
//! are always drawn on their own.

mod camera;
mod effects;
mod lighting;
mod looks;
mod mesh;
mod models;
mod parts;
mod viewport;

use std::collections::HashSet;

use glam::{Mat4, Quat, Vec2, Vec3};
use mlua::Lua;
use re_core::Diagnostic;
use re_scene::{Entity, World};
use re_types::EntityId;
use re_types::mesh::MeshData;
use re_types::render::{Camera, Light, MaterialDesc, PostSettings, TextureData};

pub use lighting::{PostEffects, sun_direction};
pub use looks::{MaterialKey, TEXTURE_SIZE};
pub use mesh::UV_STUDS;
pub use models::{
    LoadModelFn, Model, ModelMesh, ModelSettings, SkinnedFrameItem, configure_models,
};
pub use viewport::ViewportScene;

use crate::class::Accessor;
use crate::gui::props::{FxMap, Scope};
use crate::instance::tree;
use crate::physics::CellId;
use crate::runtime::{Bindings, state, with_world};
use crate::scene::CompatConfig;

/// The `Enum.Material` items, in the order [`MaterialKey::material`]
/// numbers them.
pub const MATERIALS: &[&str] = &[
    "Plastic",
    "SmoothPlastic",
    "Neon",
    "Wood",
    "WoodPlanks",
    "Marble",
    "Basalt",
    "Slate",
    "CrackedLava",
    "Concrete",
    "Limestone",
    "Granite",
    "Pavement",
    "Brick",
    "Pebble",
    "Cobblestone",
    "Rock",
    "Sandstone",
    "CorrodedMetal",
    "DiamondPlate",
    "Foil",
    "Metal",
    "Grass",
    "LeafyGrass",
    "Sand",
    "Fabric",
    "Snow",
    "Mud",
    "Ground",
    "Asphalt",
    "Salt",
    "Ice",
    "Glacier",
    "Glass",
    "ForceField",
    "Air",
    "Water",
    "Cardboard",
    "Carpet",
    "CeramicTiles",
    "ClayRoofTiles",
    "RoofShingles",
    "Leather",
    "Plaster",
    "Rubber",
];

/// The number of [`MATERIALS`] item `name`, `Plastic`'s for an unknown one.
pub(crate) fn material_index(name: &str) -> u8 {
    MATERIALS.iter().position(|m| *m == name).unwrap_or(0) as u8
}

/// Frames a mesh or material stays uploaded after its last use.
pub const RELEASE_AFTER_FRAMES: u64 = 300;

/// A mesh the host uploads once and draws by this key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MeshKey(pub u64);

/// One thing to draw: a mesh with a material at a world transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FrameItem {
    /// The part, or for a merged cell its first member.
    pub entity: EntityId,
    /// The mesh, uploaded by a frame's [`RenderFrame::new_meshes`].
    pub mesh: MeshKey,
    /// The material, created by a frame's [`RenderFrame::new_materials`].
    pub material: MaterialKey,
    /// Model-to-world transform.
    pub world: Mat4,
    /// `CastShadow`.
    pub cast_shadow: bool,
}

/// Distance fog, in studs, linear RGB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fog {
    /// Where it starts.
    pub start: f32,
    /// Where it is complete.
    pub end: f32,
    /// Its colour.
    pub color: [f32; 3],
}

/// The sky behind everything, linear RGB.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sky {
    /// Straight up.
    pub zenith: [f32; 3],
    /// At the horizon.
    pub horizon: [f32; 3],
    /// Below the horizon.
    pub ground: [f32; 3],
    /// The direction towards the sun and its disc's colour, when shown.
    pub sun: Option<(Vec3, [f32; 3])>,
}

/// The colour correction of the frame (`ColorCorrectionEffect`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorGrading {
    /// Added to every channel.
    pub brightness: f32,
    /// 0 keeps the contrast; 1 doubles it.
    pub contrast: f32,
    /// 0 keeps the saturation; -1 is grey.
    pub saturation: f32,
    /// Multiplies the colour, linear RGB.
    pub tint: [f32; 3],
}

impl Default for ColorGrading {
    fn default() -> Self {
        ColorGrading {
            brightness: 0.0,
            contrast: 0.0,
            saturation: 0.0,
            tint: [1.0; 3],
        }
    }
}

/// A cone of light (`SpotLight`, `SurfaceLight`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpotLight {
    /// World position.
    pub position: Vec3,
    /// The direction it shines, normalised.
    pub direction: Vec3,
    /// Linear RGB.
    pub color: [f32; 3],
    /// Lumens, as for a point light.
    pub intensity: f32,
    /// Where it reaches zero.
    pub range: f32,
    /// Half the cone's angle, in radians.
    pub half_angle: f32,
}

/// How a sprite's quad fades.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SpriteShape {
    /// A round spot fading to its rim.
    #[default]
    Soft,
    /// The whole quad.
    Square,
}

/// One camera-facing quad (a particle).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sprite {
    /// The centre.
    pub position: Vec3,
    /// The side, in studs.
    pub size: f32,
    /// Rotation about the view axis, in radians.
    pub rotation: f32,
    /// Linear RGB and opacity.
    pub color: [f32; 4],
    /// 0 blends; 1 adds (`LightEmission`).
    pub emission: f32,
    /// How it fades.
    pub shape: SpriteShape,
}

/// One point of a [`Ribbon`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RibbonPoint {
    /// World position.
    pub position: Vec3,
    /// Width here, in studs.
    pub width: f32,
    /// Linear RGB and opacity here.
    pub color: [f32; 4],
}

/// A strip through points (a beam or a trail).
#[derive(Clone, Debug, PartialEq)]
pub struct Ribbon {
    /// The points, in order.
    pub points: Vec<RibbonPoint>,
    /// `None` faces the camera; `Some(n)` keeps the strip perpendicular to
    /// `n`.
    pub normal: Option<Vec3>,
    /// 0 blends; 1 adds.
    pub emission: f32,
}

/// A `Highlight`: a fill and outline over some parts' items.
#[derive(Clone, Debug, PartialEq)]
pub struct Highlight {
    /// The highlighted parts (each drawn as its own [`FrameItem`]).
    pub entities: Vec<EntityId>,
    /// Linear RGB and opacity of the fill.
    pub fill: [f32; 4],
    /// Linear RGB and opacity of the outline.
    pub outline: [f32; 4],
    /// `DepthMode` `AlwaysOnTop`.
    pub always_on_top: bool,
}

/// How many effect pieces a frame drew and left out.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EffectStats {
    /// Particles plus beam and trail segments drawn.
    pub drawn: u32,
    /// The ones over the budget, left out (`RE9351`).
    pub dropped: u32,
}

/// How much work a frame's build did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BuildStats {
    /// Parts looked at again: every part on a full pass, only the changed
    /// ones otherwise (zero on an idle frame).
    pub parts_visited: u32,
    /// True when the frame rebuilt every part (the first frame).
    pub full: bool,
}

/// What the host gives a frame.
#[derive(Clone, Copy, Debug)]
pub struct RenderInput {
    /// The window's size in pixels.
    pub viewport: Vec2,
    /// The height of the top bar (the GUI inset), in pixels.
    pub top_inset: f32,
    /// The frame's time in seconds (particles and trails advance by it).
    pub dt: f32,
}

impl Default for RenderInput {
    fn default() -> Self {
        RenderInput {
            viewport: Vec2::new(1280.0, 720.0),
            top_inset: 0.0,
            dt: 1.0 / 60.0,
        }
    }
}

/// One frame of the 3D view.
#[derive(Clone, Debug)]
pub struct RenderFrame {
    /// `Workspace.CurrentCamera`.
    pub camera: Camera,
    /// The same camera for the UI step's billboards
    /// ([`GuiFrame::camera`](crate::gui::GuiFrame)).
    pub gui_camera: crate::gui::GuiCamera,
    /// The sun (or moon) and the point lights.
    pub lights: Vec<Light>,
    /// Spot and surface lights.
    pub spot_lights: Vec<SpotLight>,
    /// What to draw.
    pub items: Vec<FrameItem>,
    /// Skinned `MeshPart`s to draw.
    pub skinned: Vec<SkinnedFrameItem>,
    /// The joint matrices [`RenderFrame::skinned`] items point into.
    pub joint_palette: Vec<Mat4>,
    /// The colour behind the sky.
    pub clear: [f32; 4],
    /// Multiplies every light (`ExposureCompensation`).
    pub exposure: f32,
    /// Light from above and from below, linear RGB.
    pub ambient: ([f32; 3], [f32; 3]),
    /// Distance fog.
    pub fog: Option<Fog>,
    /// The sky.
    pub sky: Sky,
    /// Colour correction, bloom, blur and sun rays.
    pub post: PostEffects,
    /// Particles.
    pub sprites: Vec<Sprite>,
    /// Beams and trails.
    pub ribbons: Vec<Ribbon>,
    /// Highlights.
    pub highlights: Vec<Highlight>,
    /// What each `ViewportFrame` shows, for the host to draw into the
    /// texture the UI shows it with
    /// ([`GuiOutput::viewports`](crate::gui::GuiOutput)).
    pub viewports: Vec<ViewportScene>,
    /// Effect pieces drawn and dropped.
    pub effects: EffectStats,
    /// Meshes to upload before drawing.
    pub new_meshes: Vec<(MeshKey, MeshData)>,
    /// Skinned meshes to upload before drawing, with their weights.
    pub new_skinned_meshes: Vec<(MeshKey, MeshData, re_types::mesh::SkinWeights)>,
    /// Meshes no longer drawn, to release.
    pub released_meshes: Vec<MeshKey>,
    /// Textures to upload, by name; materials name them as
    /// `TextureRef::Asset`.
    pub new_textures: Vec<(String, TextureData)>,
    /// Materials to create before drawing.
    pub new_materials: Vec<(MaterialKey, MaterialDesc)>,
    /// Materials no longer drawn, to release.
    pub released_materials: Vec<MaterialKey>,
    /// Warnings (`RE9351`, once per budget overflow streak).
    pub diagnostics: Vec<Diagnostic>,
    /// The work the build did.
    pub stats: BuildStats,
}

impl RenderFrame {
    /// `base` (the project's post settings) with this frame's exposure and
    /// post effects: each kind of effect the place has replaces the base
    /// setting of that kind, and a kind it lacks leaves the base as it is.
    pub fn post_settings(&self, base: PostSettings) -> PostSettings {
        let mut post = base;
        post.exposure = self.exposure;
        let fx = &self.post;
        if let Some(g) = fx.grading {
            post.grading = Some(re_types::render::ColorGrading {
                brightness: g.brightness,
                contrast: g.contrast,
                saturation: g.saturation,
                tint: g.tint,
            });
        }
        if let Some(b) = fx.bloom {
            post.bloom = b;
        }
        if fx.blur_radius > 0.0 {
            post.blur_radius = fx.blur_radius;
        }
        if let Some(r) = fx.sun_rays {
            post.sun_rays = r;
        }
        post
    }
}

/// What the view needs from the project's configuration.
#[derive(Clone, Copy, Debug)]
pub struct RenderSettings {
    /// Particles plus beam and trail segments per frame.
    pub effect_budget: u32,
    /// The seed of the particles' randomness.
    pub seed: u64,
}

impl Default for RenderSettings {
    fn default() -> Self {
        RenderSettings::from_config(&CompatConfig::default())
    }
}

impl RenderSettings {
    /// The settings of a project: `[compat] effect_budget` and `seed`.
    pub fn from_config(config: &CompatConfig) -> Self {
        RenderSettings {
            effect_budget: config.effect_budget(),
            seed: config.seed,
        }
    }
}

/// The key of a part's own mesh: its shape and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct ShapeKey(u8, [u32; 3]);

/// What a merged cell is drawn with.
#[derive(Clone, Debug, Default)]
struct CellRender {
    batches: Vec<(MeshKey, MaterialKey, EntityId, bool)>,
    /// The members drawn in the batches.
    merged: HashSet<Entity>,
}

/// What the view keeps from frame to frame, in the VM's app data.
#[derive(Default)]
pub(crate) struct Hub {
    settings: RenderSettings,
    frame: u64,
    next_mesh: u64,
    shapes: FxMap<ShapeKey, (MeshKey, u64)>,
    cells: FxMap<CellId, CellRender>,
    materials: FxMap<MaterialKey, u64>,
    textures: HashSet<String>,
    /// The last frame's viewport and inset, for the camera's methods.
    pub(crate) viewport: Vec2,
    pub(crate) inset: f32,
    pub(crate) effects: effects::State,
    /// The parts as the last frame drew them.
    parts: parts::Retained,
    /// Every how many frames the build is checked against a full rebuild
    /// (0: never). `REAL_RENDER_VERIFY=1` sets 60 in debug builds.
    verify_every: u64,
    /// A content hash of each uploaded mesh, for the check.
    mesh_sigs: FxMap<MeshKey, u64>,
    /// `MeshPart` models.
    models: models::Models,
}

/// The variable that turns on the check against a full rebuild every 60th
/// frame, in debug builds.
pub const VERIFY_ENV: &str = "REAL_RENDER_VERIFY";

fn new_hub() -> Hub {
    let verify_every =
        if cfg!(debug_assertions) && std::env::var(VERIFY_ENV).is_ok_and(|v| v == "1") {
            60
        } else {
            0
        };
    Hub {
        viewport: RenderInput::default().viewport,
        verify_every,
        ..Hub::default()
    }
}

pub(crate) fn hub_mut<R>(lua: &Lua, f: impl FnOnce(&mut Hub) -> R) -> R {
    if lua.app_data_ref::<Hub>().is_none() {
        lua.set_app_data(new_hub());
    }
    let mut h = lua.app_data_mut::<Hub>().expect("installed above");
    f(&mut h)
}

/// Checks every `every`-th frame's parts against a full rebuild (0 turns the
/// check off); [`build`] fails on a difference. For tests: the check rebuilds
/// every part.
pub fn verify_every(lua: &Lua, every: u64) {
    hub_mut(lua, |h| {
        h.verify_every = every;
    });
}

/// A hash of a mesh's content.
fn mesh_signature(data: &MeshData) -> u64 {
    use std::hash::Hasher;
    let mut h = crate::gui::props::Fx::default();
    for v in &data.vertices {
        for f in v.pos.iter().chain(&v.normal).chain(&v.uv).chain(&v.color) {
            h.write_u32(f.to_bits());
        }
    }
    for i in &data.indices {
        h.write_u32(*i);
    }
    h.finish()
}

/// Installs the view's settings for this VM.
pub fn configure(lua: &Lua, settings: RenderSettings) {
    hub_mut(lua, |h| {
        h.settings = settings;
        h.effects.reseed(settings.seed);
    });
}

impl Hub {
    fn new_mesh(&mut self, out: &mut RenderFrame, data: MeshData) -> MeshKey {
        self.next_mesh += 1;
        let key = MeshKey(self.next_mesh);
        self.mesh_sigs.insert(key, mesh_signature(&data));
        out.new_meshes.push((key, data));
        key
    }

    fn new_skinned_mesh(
        &mut self,
        out: &mut RenderFrame,
        data: MeshData,
        weights: re_types::mesh::SkinWeights,
    ) -> MeshKey {
        self.next_mesh += 1;
        let key = MeshKey(self.next_mesh);
        out.new_skinned_meshes.push((key, data, weights));
        key
    }

    /// Notes a material as used this frame, sending it (and its texture)
    /// the first time.
    fn use_material(&mut self, out: &mut RenderFrame, key: MaterialKey) {
        let frame = self.frame;
        if let Some(last) = self.materials.get_mut(&key) {
            *last = frame;
            return;
        }
        self.materials.insert(key, frame);
        let name = MATERIALS[usize::from(key.material).min(MATERIALS.len() - 1)];
        let desc = looks::desc(&key, name);
        if desc.base_color_texture.is_some()
            && self.textures.insert(name.to_string())
            && let Some(t) = looks::texture(name)
        {
            out.new_textures.push((looks::texture_name(name), t));
        }
        out.new_materials.push((key, desc));
    }

    /// Releases what has not been drawn for [`RELEASE_AFTER_FRAMES`].
    fn collect(&mut self, out: &mut RenderFrame) {
        let frame = self.frame;
        let old = |last: u64| frame.saturating_sub(last) > RELEASE_AFTER_FRAMES;
        self.shapes.retain(|_, (key, last)| {
            let keep = !old(*last);
            if !keep {
                out.released_meshes.push(*key);
            }
            keep
        });
        self.models.collect(frame, RELEASE_AFTER_FRAMES, out);
        self.materials.retain(|key, last| {
            let keep = !old(*last);
            if !keep {
                out.released_materials.push(*key);
            }
            keep
        });
    }
}

/// The world frame of an instance with one: a part's pose and size, or an
/// attachment's world frame (size zero). `None` otherwise.
pub(crate) fn frame_of(s: &Scope, e: Entity) -> Option<(Vec3, Quat, Vec3)> {
    if s.is_a(e, "BasePart") {
        let pose = crate::physics::world_pose(s.w, e)?;
        return Some((pose.pos, pose.rot, s.props(e).vec3("Size")));
    }
    if s.is_a(e, "Attachment") {
        let parent = tree::parent(s.w, e)?;
        let (pos, rot, _) = frame_of(s, parent)?;
        let local = s
            .props(e)
            .get("CFrame")
            .and_then(|v| crate::physics::pose_of(&v))
            .unwrap_or(re_physics::Pose::IDENTITY);
        return Some((pos + rot * local.pos, rot * local.rot, Vec3::ZERO));
    }
    None
}

/// The entities whose own component is `class`'s.
pub(crate) fn instances_of(w: &mut World, class: &str) -> Vec<Entity> {
    let Some(key) = w.registry.try_key(&crate::class::own_component_name(class)) else {
        return Vec::new();
    };
    let id = w.registry.component_id(key);
    let mut q = re_scene::ecs::query::QueryBuilder::<Entity>::new(&mut w.ecs)
        .with_id(id)
        .build();
    q.iter(&w.ecs).collect()
}

/// The builtin-property accessors of the view's classes.
pub fn accessors() -> Vec<(&'static str, &'static str, Accessor)> {
    lighting::accessors()
}

/// Registers the view's accessors, methods and the `Ray` global.
pub fn register(b: &mut Bindings) {
    for (class, prop, a) in accessors() {
        b.accessor(class, prop, a);
    }
    lighting::register(b);
    camera::register(b);
    effects::register(b);
}

/// Builds the frame of the 3D view: reads the world after the frame's
/// scripts and simulation, advances particles and trails by `input.dt`,
/// and writes `Camera.ViewportSize` when the window's size changed.
///
/// # Errors
/// When the VM has no world bound or the camera write fails.
pub fn build(lua: &Lua, input: &RenderInput) -> mlua::Result<RenderFrame> {
    let st = state(lua)?;
    let classes = st.setup.classes.clone();
    let game = st.game;
    let mut hub = lua.remove_app_data::<Hub>().unwrap_or_else(new_hub);
    let out = with_world(lua, |w| build_in(w, &classes, game, &mut hub, input));
    lua.set_app_data(hub);
    out?
}

fn build_in(
    w: &mut World,
    classes: &crate::ClassRegistry,
    game: Entity,
    hub: &mut Hub,
    input: &RenderInput,
) -> mlua::Result<RenderFrame> {
    hub.frame += 1;
    hub.inset = input.top_inset;
    let workspace = crate::services::core::find_service(w, game, "Workspace");
    let lighting = crate::services::core::find_service(w, game, "Lighting");
    let camera_entity = workspace.and_then(|ws| {
        let s = Scope::new(w, classes);
        s.props(ws).entity("CurrentCamera")
    });
    if let Some(cam) = camera_entity
        && hub.viewport != input.viewport
    {
        hub.viewport = input.viewport;
        let v = crate::gui::props::vec2_value(input.viewport);
        let _ = crate::tween::set_own(w, cam, "Camera", "ViewportSize", v);
    }
    // The parts to look at: all of them on the first frame, then what the
    // world's change journal names.
    // A journal that overflowed (no build drained it for a long time) names
    // nothing: look at everything once.
    let recorded = w.records_changes();
    w.record_changes();
    let journal = w.drain_changes();
    let full = !hub.parts.ready || !recorded || journal.overflowed;
    let dirty: HashSet<Entity> = if full {
        instances_of(w, "BasePart").into_iter().collect()
    } else {
        dirty_of(w, &journal)
    };
    let check = hub.verify_every > 0 && hub.frame.is_multiple_of(hub.verify_every);
    let all_parts = if check {
        instances_of(w, "BasePart").into_iter().collect()
    } else {
        HashSet::new()
    };
    let light_list = instances_of(w, "Light");
    let mut mesh_parts = instances_of(w, "MeshPart");
    mesh_parts.sort();
    let viewport_list = instances_of(w, "ViewportFrame");
    let effect_lists = effects::Lists::find(w);

    let s = Scope::new(w, classes);
    let no_cells = std::collections::BTreeMap::new();
    let cells =
        s.w.ecs
            .get_resource::<crate::physics::StaticCells>()
            .map_or(&no_cells, |c| c.cells());
    let cam = camera::read(&s, camera_entity);
    let env = lighting::environment(&s, lighting, camera_entity);
    let mut out = RenderFrame {
        camera: cam.camera(input.viewport),
        gui_camera: cam.gui(),
        lights: env.sun.into_iter().collect(),
        spot_lights: Vec::new(),
        items: Vec::new(),
        skinned: Vec::new(),
        joint_palette: Vec::new(),
        clear: [
            env.sky.horizon[0],
            env.sky.horizon[1],
            env.sky.horizon[2],
            1.0,
        ],
        exposure: env.exposure,
        ambient: env.ambient,
        fog: env.fog,
        sky: env.sky,
        post: env.effects,
        sprites: Vec::new(),
        ribbons: Vec::new(),
        highlights: Vec::new(),
        viewports: Vec::new(),
        effects: EffectStats::default(),
        new_meshes: Vec::new(),
        new_skinned_meshes: Vec::new(),
        released_meshes: Vec::new(),
        new_textures: Vec::new(),
        new_materials: Vec::new(),
        released_materials: Vec::new(),
        diagnostics: Vec::new(),
        stats: BuildStats {
            parts_visited: 0,
            full,
        },
    };
    let in_workspace =
        |e: Entity| workspace.is_some_and(|ws| tree::ancestors(s.w, e).contains(&ws));
    let lights: Vec<Entity> = light_list
        .into_iter()
        .filter(|&e| in_workspace(e))
        .collect();
    lighting::lights(&s, &lights, &mut out.lights, &mut out.spot_lights);

    let highlights = effects::highlights(&s, &effect_lists, &in_workspace, &mut out);
    let first_item = out.items.len();
    out.stats.parts_visited =
        parts::draw(&s, hub, &dirty, cells, &in_workspace, &highlights, &mut out);
    if check {
        verify(
            &s,
            hub,
            &all_parts,
            cells,
            &in_workspace,
            &highlights,
            &out,
            first_item,
        )?;
    }
    models::draw(&s, hub, &mesh_parts, &in_workspace, &mut out);
    effects::draw(
        &s,
        hub,
        &effect_lists,
        &in_workspace,
        &cam,
        input.dt,
        &mut out,
    );
    viewport::draw(&s, hub, &viewport_list, &mut out);
    hub.collect(&mut out);
    for k in &out.released_meshes {
        hub.mesh_sigs.remove(k);
    }
    Ok(out)
}

/// The entities a journal says may draw differently: each one written, and
/// with every descendant those that moved or changed parent (their pose and
/// whether they are in `Workspace` follow their ancestors).
fn dirty_of(w: &World, journal: &re_scene::ChangeJournal) -> HashSet<Entity> {
    let transform = w.registry.try_key("Transform");
    let mut out = HashSet::new();
    let mut roots = Vec::new();
    for &(e, key) in &journal.fields {
        if Some(key) == transform {
            roots.push(e);
        } else {
            out.insert(e);
        }
    }
    roots.extend(&journal.structure);
    roots.extend(&journal.moved);
    let mut seen = HashSet::new();
    for e in roots {
        if seen.insert(e) {
            out.insert(e);
            for d in re_scene::hierarchy::subtree(&w.ecs, e) {
                out.insert(d);
            }
        }
    }
    out
}

/// The parts' items of a frame without the hub's mesh numbering: the mesh as
/// its content's hash.
fn normalized(
    hub: &Hub,
    items: &[FrameItem],
) -> Vec<(EntityId, u64, MaterialKey, [u32; 16], bool)> {
    items
        .iter()
        .map(|i| {
            (
                i.entity,
                hub.mesh_sigs.get(&i.mesh).copied().unwrap_or(u64::MAX),
                i.material,
                i.world.to_cols_array().map(f32::to_bits),
                i.cast_shadow,
            )
        })
        .collect()
}

/// Rebuilds every part from scratch and fails when the frame's parts differ
/// ([`VERIFY_ENV`], [`verify_every`]).
#[expect(
    clippy::too_many_arguments,
    reason = "the frame's state that the rebuild is compared with"
)]
fn verify(
    s: &Scope,
    hub: &Hub,
    all_parts: &HashSet<Entity>,
    cells: &std::collections::BTreeMap<CellId, crate::physics::Cell>,
    in_workspace: &dyn Fn(Entity) -> bool,
    highlights: &HashSet<Entity>,
    out: &RenderFrame,
    first_item: usize,
) -> mlua::Result<()> {
    let mut scratch = Hub {
        settings: hub.settings,
        models: models::Models::with(hub.models.settings.clone()),
        frame: hub.frame,
        verify_every: 1,
        ..Hub::default()
    };
    let mut tmp = out.clone();
    tmp.items.clear();
    parts::draw(
        s,
        &mut scratch,
        all_parts,
        cells,
        in_workspace,
        highlights,
        &mut tmp,
    );
    let want = normalized(&scratch, &tmp.items);
    let got = normalized(hub, &out.items[first_item..]);
    if want != got {
        let at = want.iter().zip(&got).position(|(a, b)| a != b);
        return Err(mlua::Error::runtime(format!(
            "the incremental 3D view differs from a full rebuild on frame {}: {} part items against {} rebuilt, first difference at {:?} ({:?} against {:?}); a write skipped the change journal",
            hub.frame,
            got.len(),
            want.len(),
            at,
            at.and_then(|i| got.get(i)),
            at.and_then(|i| want.get(i)),
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn materials_are_the_enum_items() {
        for m in MATERIALS {
            assert!(
                crate::enums::EnumItem::lookup("Material", m).is_some(),
                "{m} is not an Enum.Material item"
            );
        }
        assert_eq!(material_index("Neon"), 2);
        assert_eq!(material_index("Nope"), 0);
    }
}
