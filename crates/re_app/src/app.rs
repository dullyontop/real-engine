//! [`App`]: a project opened for running and editing, and the frame loop of
//! ADR-0009.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use re_command::{
    CommandOutcome, CommandRegistry, EditTarget, EngineReceiver, EngineRequest, History, Origin,
    PolicyCheck,
};
use re_core::files::{ProjectFiles, Writable};
use re_core::time::Instant;
use re_core::{Diagnostic, FixedClock, Location, Phase, TaskPool};
use re_input::{ActionMap, RawInput, Replay};
use re_physics::{Physics, PhysicsConfig};
use re_reflect::Value;
use re_scene::{HudText, SaveOptions, World, entity_from_id};
use re_types::{
    ActionFrame, AudioCmd, BodyPose, BusLevel, ContactEvent, PhysicsCmd, RenderSnapshot, ReplayDoc,
};
use serde::Serialize;

use re_assets::AssetWatcher;
use re_audio::Audio;
use re_render::Image;
use re_script::{ContextKind, RuntimeConfig, ScriptError, ScriptRuntime};

use crate::codes;
use crate::config::AppConfig;
use crate::gfx::{self, Graphics};
use crate::host::{Host, Pending, PlayHost};
use crate::hud::{self, HudLine};
use crate::media::Media;
use crate::perf::{PerfCounts, PerfRecorder, PerfReport};
use crate::procedural;
use crate::sync::anim::{self, AnimSync};
use crate::sync::anim_skin::{self, SkinSync};
use crate::sync::audio::{self, AudioSync};
use crate::sync::physics::{self, PhysicsSync, ShapeLibrary};
use crate::sync::ragdoll::{self, RagdollSync};
use crate::sync::render::{self, RenderAssets, RenderInputs};
use crate::verify::VerifyStage;

/// A screenshot a run was asked to take: by a replay check (`golden` set) or by
/// a script (`path` set).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ScreenshotRequest {
    /// The frame after which it is taken.
    pub frame: u64,
    /// The golden image to compare with (replay checks), `{backend}` unexpanded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub golden: Option<String>,
    /// Where to write it, relative to the project (script requests).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// What [`App::run_headless`] returns.
#[derive(Clone, Debug, Serialize)]
pub struct RunReport {
    /// Frames run.
    pub frames: u64,
    /// The game's [`state_hash`](App::state_hash) after the last frame, as hex.
    pub state_hash: String,
    /// Everything reported during the run (subsystem errors, failed replay
    /// commands, script errors).
    pub diagnostics: Vec<Diagnostic>,
    /// Frame timings and counts.
    pub perf: PerfReport,
    /// Screenshots asked for during the run.
    pub screenshots: Vec<ScreenshotRequest>,
    /// What scripts printed (`print` and `warn`), one line each.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub output: Vec<String>,
    /// For a compat project: uncaught script errors, stuck yields and
    /// unsupported members ([`crate::compat::CompatRunInfo`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compat: Option<crate::compat::CompatRunInfo>,
}

impl RunReport {
    /// True when no error was reported.
    pub fn ok(&self) -> bool {
        !self.diagnostics.iter().any(Diagnostic::is_error)
    }
}

/// A project opened for running and editing: its World, undo history, command
/// registry, physics and the sync state that connects them.
///
/// `App` is the [`EditTarget`] of the engine's commands, so the CLI, the MCP
/// server and the editor all edit through it ([`App::dispatch`], [`App::serve`]).
/// Playing is a separate mode: [`App::frame`] runs the phases of ADR-0009 and
/// [`App::run_headless`] drives them from a replay.
pub struct App {
    project: PathBuf,
    /// Every read of a project file goes through these (ADR-0037).
    files: ProjectFiles,
    config: AppConfig,
    world: World,
    history: History,
    commands: Arc<CommandRegistry<App>>,
    clock: FixedClock,
    /// Fixed steps taken since the scene was loaded: world streaming's tick.
    stream_tick: u64,
    /// How world streaming treats a cell generated too late for its tick.
    stream_lateness: re_proc::stream::Lateness,
    frame: u64,
    alpha: f32,

    action_map: ActionMap<String>,
    raw: RawInput,
    /// This frame's raw input, as scripts see it (compat `UserInputService`).
    raw_frame: re_types::input::RawInputFrame,
    /// The cursor settings scripts asked for, until the window takes them.
    mouse_behavior: Option<(re_types::input::MouseLock, bool)>,
    actions: ActionFrame,
    replay: Option<Replay>,

    physics: Physics,
    physics_sync: PhysicsSync,
    shapes: ShapeLibrary,
    physics_cmds: Vec<PhysicsCmd>,
    poses: Vec<BodyPose>,
    wheel_poses: Vec<re_types::WheelPose>,
    contacts: Vec<ContactEvent>,

    media: Media,
    graphics: Option<Graphics>,
    render_assets: RenderAssets,
    snapshot: RenderSnapshot,
    hud: Vec<HudLine>,
    watcher: Option<AssetWatcher>,
    changed_scripts: Vec<String>,

    anim_sync: AnimSync,
    skin_sync: SkinSync,
    ragdoll_sync: RagdollSync,

    /// The `derived:` heightfields colliders used at the last step: when they
    /// change during play, characters are put back on the new ground.
    ground: Option<std::collections::BTreeSet<String>>,
    /// What each procedural source's graph was last loaded from, to load the
    /// graph again when a source's `graph` or `builder` is edited.
    graph_inputs: procedural::GraphInputs,
    /// How many times the World was reloaded by [`App::restart`].
    restarts: u64,
    /// Characters [`App::link_new_characters`] already tried to link.
    link_tried: std::collections::BTreeSet<re_types::EntityId>,

    scripts: Option<ScriptRuntime>,
    scripts_failed: bool,
    /// The script debugger the game context is attached to (ADR-0045).
    debugger: Option<re_script::debug::Debugger>,
    /// `[compat]`, when the project is a compat project; its scripts then run
    /// in [`CompatPlay`](crate::compat::CompatPlay) instead of `scripts`.
    compat_config: Option<re_compat::CompatConfig>,
    compat: Option<crate::compat::CompatPlay>,
    compat_failed: bool,
    /// Whether the compat data services keep their data in the run's data folder
    /// ([`App::persist_data`]).
    persist_data: bool,
    /// A compat game's 3D view and UI on the renderer.
    compat_view: crate::compat_view::CompatView,
    /// The size (physical pixels) and scale of the last window draw.
    view_size: [u32; 2],
    view_ppp: f32,
    script_output: Vec<String>,
    /// Script errors already reported (a failing `on_step` fails every step).
    script_errors: std::collections::HashSet<String>,

    audio: Audio,
    audio_sync: AudioSync,
    audio_cmds: Vec<AudioCmd>,
    last_audio_cmds: Vec<AudioCmd>,
    /// Every bus's level over the last frame (ADR-0041), for scripts.
    bus_levels: Vec<BusLevel>,

    pending: Pending,
    screenshots: Vec<ScreenshotRequest>,
    captures: Vec<(ScreenshotRequest, Image)>,
    perf: PerfRecorder,
    diagnostics: Vec<Diagnostic>,
    verify_stages: Vec<Arc<dyn VerifyStage + Send + Sync>>,
}

impl EditTarget for App {
    fn world(&mut self) -> &mut World {
        &mut self.world
    }
    fn history(&mut self) -> &mut History {
        &mut self.history
    }
}

/// Gives `world` its streaming manager with `[stream]`, so `stream.load` works
/// before the first step.
fn ensure_streamer(world: &mut World, config: &AppConfig) -> Result<(), Diagnostic> {
    if !world.ecs.contains_resource::<re_proc::stream::Streamer>() {
        let streamer = re_proc::stream::Streamer::new(config.stream.clone())?;
        world.ecs.insert_resource(streamer);
    }
    Ok(())
}

fn physics_config(config: &AppConfig) -> PhysicsConfig {
    PhysicsConfig {
        gravity: config.sim.gravity.into(),
        ..PhysicsConfig::default()
    }
}

fn rel(path: &str) -> Location {
    Location::file(path.replace('\\', "/"))
}

impl App {
    /// Opens the project at `project` with `config` (usually
    /// [`AppConfig::load`]): builds the World, loads the entry scene, runs the
    /// procedural sources, and registers the engine's commands (the built-ins of
    /// `re_command` and the runtime commands of `re_app`).
    ///
    /// # Errors
    ///
    /// `RE0502` when the entry scene cannot be read or loaded (with the scene's
    /// own diagnostic as `related`), `RE0501` when the `[input]` bindings are
    /// invalid, or the physics world's error.
    pub fn open(project: &Path, config: AppConfig) -> Result<App, Diagnostic> {
        App::open_files(crate::package::dev_files(project), config)
    }

    /// Opens the project read through `files` (ADR-0037), as [`App::open`]
    /// does for a directory; the engine layer
    /// ([`crate::package::engine_layer`]) is put first unless `files` already
    /// starts with an engine layer. Without a project directory
    /// ([`ProjectFiles::project_dir`]) the app writes no project file:
    /// `save_scene`, asset imports and hot reload are refused.
    ///
    /// # Errors
    ///
    /// As [`App::open`].
    pub fn open_files(files: ProjectFiles, config: AppConfig) -> Result<App, Diagnostic> {
        // Every run reads the engine's own files first.
        let files = match files.layers().first().map(|l| l.kind()) {
            Some(re_core::files::LayerKind::Engine) => files,
            _ => crate::package::with_engine_layer(&files),
        };
        let action_map = config.input.to_action_map().map_err(|ds| {
            let mut d = Diagnostic::error(
                codes::CONFIG_INVALID,
                "the [input] section of real.toml has invalid bindings",
            )
            .at(Location::file(crate::config::CONFIG_FILE));
            for other in ds {
                d = d.with_related(other);
            }
            d
        })?;
        let (world, problems) = load_world(&files, &config)?;
        let mut commands = CommandRegistry::new();
        re_command::register_builtin_commands(&mut commands);
        crate::commands::register(&mut commands);
        re_proc::register_commands(&mut commands);
        let mut app = App::with_world(files, config, world, action_map, Arc::new(commands))?;
        app.diagnostics.extend(problems);
        app.edit_frame();
        Ok(app)
    }

    fn with_world(
        files: ProjectFiles,
        config: AppConfig,
        world: World,
        action_map: ActionMap<String>,
        commands: Arc<CommandRegistry<App>>,
    ) -> Result<App, Diagnostic> {
        let mut world = world;
        ensure_streamer(&mut world, &config)?;
        let compat_config = crate::compat::compat_config(&config)?;
        let graph_inputs = procedural::inputs(&mut world);
        let physics = Physics::new(physics_config(&config), TaskPool::global())?;
        let clock = FixedClock::new(config.sim.fixed_hz, config.sim.max_steps_per_frame);
        let mut audio = headless_audio(&files);
        let audio_problems = audio.set_graph(&config.audio);
        let bus_levels = audio.bus_levels();
        Ok(App {
            project: files
                .project_dir()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            media: Media::with_files(files.clone()),
            audio,
            files,
            world,
            history: History::default(),
            commands,
            clock,
            stream_tick: 0,
            stream_lateness: re_proc::stream::Lateness::Wait,
            frame: 0,
            alpha: 0.0,
            action_map,
            raw: RawInput::default(),
            raw_frame: re_types::input::RawInputFrame::default(),
            mouse_behavior: None,
            actions: ActionFrame::default(),
            replay: None,
            physics,
            physics_sync: PhysicsSync::new(),
            shapes: ShapeLibrary::default(),
            physics_cmds: Vec::new(),
            poses: Vec::new(),
            wheel_poses: Vec::new(),
            contacts: Vec::new(),
            graphics: None,
            render_assets: RenderAssets::default(),
            snapshot: RenderSnapshot::default(),
            hud: Vec::new(),
            watcher: None,
            changed_scripts: Vec::new(),
            anim_sync: AnimSync::new(),
            skin_sync: SkinSync::new(),
            ragdoll_sync: RagdollSync::new(),
            ground: None,
            graph_inputs,
            restarts: 0,
            link_tried: Default::default(),
            scripts: None,
            scripts_failed: false,
            debugger: None,
            compat_config,
            compat: None,
            compat_failed: false,
            persist_data: false,
            compat_view: crate::compat_view::CompatView::new(&config.render),
            view_size: [config.render.width, config.render.height],
            view_ppp: 1.0,
            script_output: Vec::new(),
            script_errors: Default::default(),
            audio_sync: AudioSync::new(),
            audio_cmds: Vec::new(),
            last_audio_cmds: Vec::new(),
            bus_levels,
            pending: Pending::default(),
            screenshots: Vec::new(),
            captures: Vec::new(),
            perf: PerfRecorder::new(),
            diagnostics: audio_problems,
            verify_stages: Vec::new(),
            config,
        })
    }

    /// Plays a copy of the World for `frames` frames (ADR-0021's `play.run`),
    /// then its `steps` ([`App::play_steps`], whose results it returns).
    /// Scripts run in fresh VMs and start again from their entry points, on top
    /// of the copied World state (in-VM state cannot be copied). A `[compat]`
    /// game that already started copies the entry scene instead, since its
    /// World is split between the server and the client.
    ///
    /// The copy is [`World::fork`] with a fresh physics world built from it and
    /// given the live bodies' motion ([`Physics::motion_state`]), so it carries on
    /// from the live state; contacts start fresh. Audio goes to a silent mixer.
    /// With `screenshot`, the last frame is captured (the live app's GPU side is
    /// lent to the copy). Nothing from the copy reaches the live World.
    ///
    /// # Errors
    ///
    /// The replay's error, and `RE0516` when a screenshot cannot be taken.
    pub fn play_copy(
        &mut self,
        frames: u64,
        replay: Option<ReplayDoc>,
        steps: &[crate::play::PlayStep],
        screenshot: bool,
    ) -> Result<(RunReport, Vec<serde_json::Value>, Option<Image>), Diagnostic> {
        // A compat game that already started has split its World into the
        // server's and the client's; the copy starts again from the scene.
        let restarted = self.compat.is_some();
        let (mut world, problems) = if restarted {
            load_world(&self.files, &self.config)?
        } else {
            (self.world.fork(), Vec::new())
        };
        if let Some(store) = self.world.ecs.get_resource::<re_proc::GraphStore>() {
            world.ecs.insert_resource(store.clone());
        }
        // Streaming carries on from the live state too.
        if !restarted
            && let Some(streamer) = self.world.ecs.get_resource::<re_proc::stream::Streamer>()
        {
            world
                .ecs
                .insert_resource(streamer.forked(&self.world, &world));
        }
        let mut copy = App::with_world(
            self.files.clone(),
            self.config.clone(),
            world,
            self.action_map.clone(),
            Arc::clone(&self.commands),
        )?;
        copy.alpha = self.alpha;
        if !restarted {
            copy.stream_tick = self.stream_tick;
        }
        copy.diagnostics.extend(problems);
        load_collider_sources(&mut copy.world, &mut copy.shapes, &mut self.media);
        physics::push(
            &mut copy.world,
            &mut copy.physics_sync,
            &copy.shapes,
            &mut copy.physics_cmds,
            &mut copy.diagnostics,
        );
        copy.physics.apply(&mut copy.physics_cmds);
        if !restarted {
            copy.physics
                .apply_motion_state(&self.physics.motion_state());
        }
        // Lend the GPU side and the loaded media, so nothing is loaded twice.
        // Font atlas changes not drawn yet go with it, older first.
        std::mem::swap(&mut copy.media, &mut self.media);
        if let Some(pending) = self.compat_view.take_textures() {
            copy.compat_view.set_ui(pending);
        }
        copy.graphics = self.graphics.take();
        if let Some(g) = &mut copy.graphics {
            g.set_keep_derived(true);
        }
        copy.render_assets = std::mem::take(&mut self.render_assets);
        let run = copy.run_headless(Some(frames), replay).map(|mut r| {
            let results = copy.play_steps(steps, &mut r);
            (r, results)
        });
        let image = match (&run, screenshot) {
            (Ok(_), true) => Some(copy.screenshot(None)),
            _ => None,
        };
        std::mem::swap(&mut copy.media, &mut self.media);
        if let Some(g) = &mut copy.graphics {
            copy.compat_view.release_all(g.renderer_mut());
        }
        if let Some(pending) = copy.compat_view.take_textures() {
            self.compat_view.set_ui(pending);
        }
        self.graphics = copy.graphics.take();
        if let Some(g) = &mut self.graphics {
            g.set_keep_derived(false);
        }
        self.render_assets = std::mem::take(&mut copy.render_assets);
        let (run, results) = run?;
        Ok((run, results, image.transpose()?))
    }

    /// Opens the project at `project` with its `real.toml` and the given runtime
    /// options left at their defaults (headless, edits not persisted).
    pub fn open_project(project: &Path) -> Result<App, Diagnostic> {
        App::open(project, AppConfig::load(project)?)
    }

    /// The project directory (empty when the app reads no directory, as a
    /// packaged game does).
    pub fn project(&self) -> &Path {
        &self.project
    }

    /// The project's files (ADR-0037).
    pub fn files(&self) -> &ProjectFiles {
        &self.files
    }

    /// Where an output a runtime command names (a screenshot path) is written:
    /// relative to the project directory in a development run, else to the
    /// run's output folder ([`Writable::Out`]).
    ///
    /// # Errors
    ///
    /// `RE0516` when the run has nowhere on disk to write.
    pub fn output_path(&self, rel: impl AsRef<Path>) -> Result<PathBuf, Diagnostic> {
        let rel = rel.as_ref();
        if let Some(dir) = self.files.project_dir() {
            return Ok(dir.join(rel));
        }
        let rel = rel.strip_prefix(".real/out").unwrap_or(rel);
        self.files
            .writable(Writable::Out)
            .path()
            .map(|dir| dir.join(rel))
            .ok_or_else(|| {
                Diagnostic::error(
                    codes::SCREENSHOT_FAILED,
                    format!(
                        "cannot write {}: this run has no output folder on disk",
                        rel.display()
                    ),
                )
            })
    }

    /// The configuration the project was opened with.
    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    /// The World.
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The World, for direct changes (no undo; prefer [`App::dispatch`]).
    pub fn world_mut(&mut self) -> &mut World {
        &mut self.world
    }

    /// The undo history.
    pub fn history(&self) -> &History {
        &self.history
    }

    /// The command registry.
    pub fn commands(&self) -> &CommandRegistry<App> {
        &self.commands
    }

    /// The command registry, to register more commands (`re_agent`'s, `re_cli`'s
    /// `explain`). Call it between dispatches.
    ///
    /// # Panics
    ///
    /// Panics if called from inside a command handler.
    pub fn commands_mut(&mut self) -> &mut CommandRegistry<App> {
        Arc::get_mut(&mut self.commands)
            .expect("commands_mut called while a command is being dispatched")
    }

    /// Adds a verify stage that the `verify` command runs after `compile`
    /// (`re_cli` adds the static scan this way).
    pub fn add_verify_stage(&mut self, stage: Arc<dyn VerifyStage + Send + Sync>) {
        self.verify_stages.push(stage);
    }

    /// The extra verify stages added with [`App::add_verify_stage`].
    pub fn verify_stages(&self) -> Vec<Arc<dyn VerifyStage + Send + Sync>> {
        self.verify_stages.clone()
    }

    /// How many times the World was reloaded (`world.restart()`, `R` in the
    /// demo) since the app opened.
    pub fn restarts(&self) -> u64 {
        self.restarts
    }

    /// The physics world.
    pub fn physics(&self) -> &Physics {
        &self.physics
    }

    /// A hash of the game state, for determinism checks: the world's
    /// [`state_hash`](re_scene::World::state_hash), and with any ragdolls also their
    /// physics state, which the scene does not hold (the skin shows it), and with
    /// any animation graphs also their parameters, states and times (ADR-0039).
    /// A game without ragdolls or graphs hashes as its world.
    pub fn state_hash(&self) -> blake3::Hash {
        let world = self.world.state_hash();
        let ragdolls = self.physics.ragdoll_state_bytes();
        let base = if ragdolls.is_empty() {
            world
        } else {
            let mut h = blake3::Hasher::new();
            h.update(b"real-engine/state-hash+ragdolls/1");
            h.update(world.as_bytes());
            h.update(&ragdolls);
            h.finalize()
        };
        if !self.skin_sync.has_graphs() {
            return base;
        }
        let mut bytes = Vec::new();
        self.skin_sync.write_state(&mut bytes);
        let mut h = blake3::Hasher::new();
        h.update(b"real-engine/state-hash/anim/1");
        h.update(base.as_bytes());
        h.update(&(bytes.len() as u64).to_le_bytes());
        h.update(&bytes);
        h.finalize()
    }

    /// Which entity has which body or character.
    pub fn physics_sync(&self) -> &PhysicsSync {
        &self.physics_sync
    }

    /// The meshes and heightfields collider sources can name.
    pub fn shapes_mut(&mut self) -> &mut ShapeLibrary {
        &mut self.shapes
    }

    /// Renderer handles for mesh and material assets.
    pub fn render_assets_mut(&mut self) -> &mut RenderAssets {
        &mut self.render_assets
    }

    /// The render snapshot built by the last frame's Sync phase.
    pub fn snapshot(&self) -> &RenderSnapshot {
        &self.snapshot
    }

    /// The audio commands of the last frame.
    pub fn audio_commands(&self) -> &[AudioCmd] {
        &self.last_audio_cmds
    }

    /// The audio output (headless until a windowed driver sets a device).
    pub fn audio_mut(&mut self) -> &mut Audio {
        &mut self.audio
    }

    /// Replaces the audio output (the windowed runner opens the device) and
    /// builds the project's mixer graph on it.
    pub fn set_audio(&mut self, mut audio: Audio) {
        audio.set_files(self.files.clone());
        self.diagnostics.extend(audio.set_graph(&self.config.audio));
        self.bus_levels = audio.bus_levels();
        self.audio = audio;
        self.audio_sync = AudioSync::new();
    }

    /// Every mixer bus's and send's level over the last frame, after its
    /// effects and volume (ADR-0041).
    pub fn bus_levels(&self) -> &[BusLevel] {
        &self.bus_levels
    }

    /// Meshes, materials and clips by asset string.
    pub fn media_mut(&mut self) -> &mut Media {
        &mut self.media
    }

    /// The GPU side, when there is one.
    pub fn graphics(&self) -> Option<&Graphics> {
        self.graphics.as_ref()
    }

    /// The GPU side, mutably.
    pub fn graphics_mut(&mut self) -> Option<&mut Graphics> {
        self.graphics.as_mut()
    }

    /// Gives the app a GPU side (the windowed runner's, for its surface).
    pub fn set_graphics(&mut self, graphics: Graphics) {
        self.render_assets = RenderAssets::default();
        self.graphics = Some(graphics);
    }

    /// Takes the GPU side away (meshes are uploaded again by the next owner).
    pub fn take_graphics(&mut self) -> Option<Graphics> {
        self.render_assets = RenderAssets::default();
        self.graphics.take()
    }

    /// The GPU side, opening a headless device on first use
    /// (`real.toml [render] backends`, or `RE_BACKENDS`).
    ///
    /// # Errors
    ///
    /// `RE0516` when no GPU device can be opened.
    pub fn ensure_graphics(&mut self) -> Result<&mut Graphics, Diagnostic> {
        if self.graphics.is_none() {
            let fail = |d: Diagnostic| {
                Diagnostic::error(
                    codes::SCREENSHOT_FAILED,
                    "no GPU device to draw frames with",
                )
                .with_hint("install a Vulkan or GL driver (lavapipe and llvmpipe work), or set RE_BACKENDS")
                .with_related(d)
            };
            let backends = gfx::backends(&self.config.render.backends).map_err(fail)?;
            let graphics = Graphics::headless(backends).map_err(fail)?;
            self.set_graphics(graphics);
        }
        Ok(self.graphics.as_mut().expect("set above"))
    }

    /// Draws the current frame (the last Sync's snapshot and HUD) offscreen at
    /// the configured size and returns it; with `path`, also writes it as a PNG
    /// (relative paths are under the project).
    ///
    /// # Errors
    ///
    /// `RE0516` when no GPU device can be opened or the file cannot be written,
    /// and the renderer's errors.
    pub fn screenshot(&mut self, path: Option<&Path>) -> Result<Image, Diagnostic> {
        self.ensure_graphics()?;
        let gfx = self.graphics.as_mut().expect("ensured");
        gfx.sync_assets(
            &mut self.world,
            &mut self.media,
            &mut self.render_assets,
            &mut self.diagnostics,
        );
        let inputs = RenderInputs {
            assets: &self.render_assets,
            physics: &self.physics_sync,
            skins: &self.skin_sync,
            alpha: self.alpha,
            clear: self.config.render.clear,
            post: self.config.render.post,
            shadows: self.config.render.shadows,
        };
        self.snapshot = render::build(&mut self.world, &inputs);
        self.hud = hud::collect(&mut self.world);
        let size = [self.config.render.width, self.config.render.height];
        let image = if self.compat.is_some() {
            self.capture_compat(size)?
        } else {
            gfx.capture(size, &self.snapshot, &self.hud)?
        };
        let gfx = self.graphics.as_mut().expect("ensured");
        self.diagnostics.extend(gfx.take_errors());
        if let Some(path) = path {
            gfx::save_png(&image, &self.output_path(path)?)?;
        }
        Ok(image)
    }

    /// Draws the last frame (snapshot and HUD) into `target`, `size_px`
    /// physical pixels at `pixels_per_point`, with the app's GPU side.
    ///
    /// # Errors
    ///
    /// `RE0516` when the app has no GPU side, and the renderer's errors.
    pub fn draw_into(
        &mut self,
        target: &re_render::wgpu::TextureView,
        size_px: [u32; 2],
        pixels_per_point: f32,
    ) -> Result<(), Diagnostic> {
        let gfx = self.graphics.as_mut().ok_or_else(|| {
            Diagnostic::error(
                codes::SCREENSHOT_FAILED,
                "the app has no GPU side to draw with",
            )
        })?;
        if self.compat.is_some() {
            self.view_size = size_px;
            self.view_ppp = pixels_per_point;
            return gfx.draw_compat(
                target,
                size_px,
                pixels_per_point,
                &mut self.compat_view,
                &self.hud,
            );
        }
        gfx.draw(target, size_px, pixels_per_point, &self.snapshot, &self.hud)
    }

    /// The HUD lines of the last frame.
    pub fn hud_lines(&self) -> &[HudLine] {
        &self.hud
    }

    /// Images taken for replay checks since the last call, with their requests.
    pub fn take_captures(&mut self) -> Vec<(ScreenshotRequest, Image)> {
        std::mem::take(&mut self.captures)
    }

    /// Watches the project's files: changed meshes, materials and textures are
    /// re-imported and uploaded again between frames, and changed scripts are
    /// collected for [`App::take_changed_scripts`].
    ///
    /// # Errors
    ///
    /// The watcher's error.
    pub fn enable_hot_reload(&mut self) -> Result<(), Diagnostic> {
        // Only a project directory changes under a running game.
        let Some(dir) = self.files.project_dir() else {
            return Ok(());
        };
        self.watcher = Some(AssetWatcher::new(dir, AssetWatcher::DEFAULT_DEBOUNCE)?);
        Ok(())
    }

    /// Sets how world streaming treats a cell whose generation is not done
    /// at its apply tick (ADR-0042). [`Lateness::Wait`] (the default) waits
    /// for it, so `real verify`, replays and `play.run` load the same cells at
    /// the same ticks on every machine. Only a live run, which must not stall
    /// a frame, sets [`Lateness::Late`]: the cell applies when it is ready,
    /// reported as `RE4305`.
    ///
    /// [`Lateness::Wait`]: re_proc::stream::Lateness::Wait
    /// [`Lateness::Late`]: re_proc::stream::Lateness::Late
    pub fn set_stream_lateness(&mut self, lateness: re_proc::stream::Lateness) {
        self.stream_lateness = lateness;
    }

    /// Makes a compat game's `DataStoreService` and `MemoryStoreService`
    /// keep their data in the run's data folder (`.real/data/` on desktop,
    /// IndexedDB in a browser) with the wall clock, as `real run` does
    /// (ADR-0048). Without it, each run starts from
    /// `data/seed/` on a fixed clock so it repeats. Call it before the first
    /// frame.
    pub fn persist_data(&mut self) {
        self.persist_data = true;
    }

    /// Scripts under `scripts/` that changed on disk since the last call.
    pub fn take_changed_scripts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.changed_scripts)
    }

    /// Animation playback state.
    pub fn anim_sync(&self) -> &AnimSync {
        &self.anim_sync
    }

    /// Skeleton poses of the world's skinned meshes.
    pub fn skin_sync(&self) -> &SkinSync {
        &self.skin_sync
    }

    /// Skeleton poses and animation graphs, to set graph parameters.
    pub fn skin_sync_mut(&mut self) -> &mut SkinSync {
        &mut self.skin_sync
    }

    /// The ragdolls built for `Ragdoll` components, with their blend weights.
    pub fn ragdoll_sync(&self) -> &RagdollSync {
        &self.ragdoll_sync
    }

    /// The contact events of the last frame's physics steps.
    pub fn contacts(&self) -> &[ContactEvent] {
        &self.contacts
    }

    /// This frame's actions.
    pub fn actions(&self) -> &ActionFrame {
        &self.actions
    }

    /// The raw input state, for a windowed driver to feed device events into.
    pub fn raw_input_mut(&mut self) -> &mut RawInput {
        &mut self.raw
    }

    /// Frames run since the project was opened.
    pub fn frame_count(&self) -> u64 {
        self.frame
    }

    /// The fixed simulation step.
    pub fn step(&self) -> Duration {
        self.clock.dt()
    }

    /// The diagnostics reported since the last [`App::take_diagnostics`].
    pub fn pending_diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Takes the diagnostics reported since the last call.
    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// The timing samples recorded so far.
    pub fn perf(&self) -> &PerfRecorder {
        &self.perf
    }

    /// Forgets the timing samples (after warm-up frames).
    pub fn reset_perf(&mut self) {
        self.perf.clear();
    }

    /// The PerfReport of the frames run so far.
    pub fn perf_report(&mut self) -> PerfReport {
        let counts = PerfCounts {
            entities: self.world.ecs.entities().len() as u64,
            draw_items: self.snapshot.items.len() as u64,
            bodies: u64::from(self.physics.body_count()),
            characters: self.physics.character_count() as u64,
        };
        self.perf.report(counts, None)
    }

    /// A [`Host`] over this app's state, for the script runtime. The World is
    /// not part of it: the script runtime takes it separately.
    pub fn host(&mut self) -> Host<'_> {
        Host {
            physics: &self.physics,
            links: &self.physics_sync,
            actions: &self.actions,
            raw: &self.raw_frame,
            physics_cmds: &mut self.physics_cmds,
            audio_cmds: &mut self.audio_cmds,
            bus_levels: &self.bus_levels,
            audio_graph: self.audio.graph(),
            pending: &mut self.pending,
            skins: &mut self.skin_sync,
        }
    }

    /// The cursor settings scripts asked for since the last call
    /// (`UserInputService.MouseBehavior` and `MouseIconEnabled`); the window
    /// applies them.
    pub fn take_mouse_behavior(&mut self) -> Option<(re_types::input::MouseLock, bool)> {
        self.mouse_behavior.take()
    }

    // ---- editing --------------------------------------------------------

    /// Dispatches a command through the registry and `policy`, as `origin`.
    ///
    /// After a command that changed the world (not a dry run), the edit frame
    /// runs (procedural sources regenerate, transforms propagate) and, when
    /// [`AppConfig::persist_edits`] is set, the scene is written back to its
    /// file; a failed write is a warning (`RE0518`) in the outcome.
    pub fn dispatch(
        &mut self,
        name: &str,
        args: serde_json::Value,
        dry_run: bool,
        policy: &dyn PolicyCheck,
        origin: Origin,
    ) -> CommandOutcome {
        let dry = dry_run || args.get("dry_run").and_then(serde_json::Value::as_bool) == Some(true);
        let registry = Arc::clone(&self.commands);
        let mut outcome = registry.dispatch_as(self, name, args, dry_run, policy, origin);
        drop(registry);
        if outcome.ok && !dry && !outcome.changes.is_empty() {
            self.edit_frame();
            if self.config.persist_edits
                && let Err(d) = self.save_scene()
            {
                outcome.diagnostics.push(Diagnostic {
                    severity: re_core::Severity::Warning,
                    ..d
                });
            }
        }
        outcome
    }

    /// Answers engine requests on the current thread until every
    /// [`EngineHandle`](re_command::EngineHandle) is dropped: each command goes
    /// through [`App::dispatch`] with `policy`.
    ///
    /// This is edit mode: nothing simulates, so the scene an agent or a person
    /// edits is the scene on disk. Running the game is `play.run` (on a copy) or
    /// `real run`.
    pub fn serve(&mut self, receiver: EngineReceiver, policy: &dyn PolicyCheck) {
        while let Some(request) = receiver.recv() {
            self.answer(request, policy);
        }
    }

    /// Answers the requests already waiting, without blocking (for a frame loop
    /// that also serves, such as the editor's). Returns how many were answered,
    /// or `None` once every handle is dropped.
    pub fn serve_pending(
        &mut self,
        receiver: &EngineReceiver,
        policy: &dyn PolicyCheck,
    ) -> Option<usize> {
        let mut served = 0;
        loop {
            match receiver.try_recv() {
                Ok(Some(request)) => {
                    self.answer(request, policy);
                    served += 1;
                }
                Ok(None) => return Some(served),
                Err(()) if served > 0 => return Some(served),
                Err(()) => return None,
            }
        }
    }

    fn answer(&mut self, request: EngineRequest, policy: &dyn PolicyCheck) {
        match request {
            EngineRequest::Command {
                name,
                args,
                dry_run,
                origin,
                reply,
            } => {
                let outcome = self.dispatch(&name, args, dry_run, policy, origin);
                let _ = reply.send(outcome);
            }
            EngineRequest::ListSpecs { reply } => {
                let _ = reply.send(self.commands.specs().to_vec());
            }
        }
    }

    /// Writes the World's authored entities back to the entry scene file,
    /// and each open document with unsaved changes back to its file
    /// (ADR-0044), which clears its dirty flag; the procedural sources that
    /// read a saved file reload their stored graph from it, so it stays the
    /// file's content once the document is closed.
    ///
    /// # Errors
    ///
    /// `RE0518` when a file cannot be written.
    pub fn save_scene(&mut self) -> Result<(), Diagnostic> {
        let doc = self.world.save(SaveOptions::default());
        let text = re_scene::scene::to_json(&doc);
        let rel_path = self.config.project.entry_scene.clone();
        // Only a development run writes project files (ADR-0037).
        let Some(dir) = self.files.project_dir().map(Path::to_path_buf) else {
            return Err(Diagnostic::error(
                codes::PACKAGED_WRITE,
                format!(
                    "cannot write the scene back to {rel_path}: this run has no project directory"
                ),
            ));
        };
        let failed = |what: &str, rel_path: &str, e: std::io::Error| {
            Diagnostic::error(
                codes::SCENE_SAVE_FAILED,
                format!("cannot write {what} back to {rel_path}: {e}"),
            )
            .at(rel(rel_path))
        };
        std::fs::write(dir.join(&rel_path), text).map_err(|e| failed("the scene", &rel_path, e))?;
        let saved = self.world.unsaved_docs();
        for path in &saved {
            let Some(text) = self.world.doc_text(path) else {
                continue;
            };
            std::fs::write(dir.join(path), text)
                .map_err(|e| failed("the open document", path, e))?;
            self.world.mark_doc_saved(path);
        }
        if !saved.is_empty() {
            let problems = procedural::load_graphs(&mut self.world, &self.files, Some(&saved));
            self.diagnostics.extend(problems);
        }
        Ok(())
    }

    /// The work an edit needs outside the simulation: regenerate procedural
    /// sources whose inputs changed, then propagate transforms. With a GPU side
    /// (an editor's), also upload new assets and rebuild the snapshot and HUD,
    /// so [`App::draw_into`] draws the World as it is.
    pub fn edit_frame(&mut self) {
        self.regenerate_changed();
        self.world.propagate();
        if self.graphics.is_some() {
            // An editor's view: the World as it is, with its assets on the GPU.
            self.upload_assets();
            let inputs = RenderInputs {
                assets: &self.render_assets,
                physics: &self.physics_sync,
                skins: &self.skin_sync,
                alpha: self.alpha,
                clear: self.config.render.clear,
                post: self.config.render.post,
                shadows: self.config.render.shadows,
            };
            self.snapshot = render::build(&mut self.world, &inputs);
            self.hud = hud::collect(&mut self.world);
        }
    }

    /// Regenerates the procedural sources whose inputs changed and forgets the
    /// derived assets nothing uses any more (the GPU copies go at the next
    /// upload).
    fn regenerate_changed(&mut self) {
        let problems =
            procedural::refresh_graphs(&mut self.world, &self.files, &mut self.graph_inputs);
        self.diagnostics.extend(problems);
        let report = self.world.regenerate_changed();
        self.diagnostics.extend(report.diagnostics.iter().cloned());
        procedural::release(
            &mut self.world,
            &mut self.media,
            &mut self.shapes,
            &report.released_assets,
        );
    }

    /// Uploads the meshes and materials the World names that are not on the GPU
    /// yet (with the app's [`Graphics`]; nothing without one), recording their
    /// handles in the app's `RenderAssets`. `frame` and `edit_frame` call it.
    pub fn upload_assets(&mut self) {
        if let Some(gfx) = &mut self.graphics {
            procedural::sync_derived(&mut self.world, &mut self.media, &mut self.shapes);
            gfx.sync_assets(
                &mut self.world,
                &mut self.media,
                &mut self.render_assets,
                &mut self.diagnostics,
            );
        }
    }

    // ---- playing --------------------------------------------------------

    /// Plays `replay` from the next frame: its actions replace live input, its
    /// commands are dispatched at their frames and its checks are recorded as
    /// screenshot requests.
    ///
    /// # Errors
    ///
    /// `RE0507` when the replay does not match the project's actions (with the
    /// input crate's diagnostics as `related`).
    pub fn play_replay(&mut self, doc: ReplayDoc) -> Result<(), Diagnostic> {
        let replay = Replay::new(doc, &self.action_map.layout()).map_err(|ds| {
            let mut d = Diagnostic::error(
                codes::REPLAY_FAILED,
                "the replay does not match this project's actions",
            );
            for r in ds {
                d = d.with_related(r);
            }
            d
        })?;
        self.replay = Some(replay);
        Ok(())
    }

    /// Runs `frames` frames headless, at a fixed frame time of one simulation
    /// step, optionally driven by `replay` (whose frame count is used when
    /// `frames` is `None`).
    ///
    /// # Errors
    ///
    /// The replay's error ([`App::play_replay`]); everything that goes wrong
    /// during the frames is in the report's diagnostics instead.
    pub fn run_headless(
        &mut self,
        frames: Option<u64>,
        replay: Option<ReplayDoc>,
    ) -> Result<RunReport, Diagnostic> {
        self.run_headless_observed(frames, replay, |_| {})
    }

    /// [`App::run_headless`], calling `observe` after every frame (verify
    /// watches regenerations this way).
    ///
    /// # Errors
    ///
    /// As [`App::run_headless`].
    pub fn run_headless_observed(
        &mut self,
        frames: Option<u64>,
        replay: Option<ReplayDoc>,
        mut observe: impl FnMut(&mut App),
    ) -> Result<RunReport, Diagnostic> {
        let frames = frames.or(replay.as_ref().map(|r| r.frames)).unwrap_or(60);
        if let Some(doc) = replay {
            self.play_replay(doc)?;
        }
        let dt = self.clock.dt();
        let start = self.frame;
        for _ in 0..frames {
            self.frame(dt);
            observe(self);
        }
        let mut diagnostics = self.take_diagnostics();
        diagnostics.retain(|d| d.severity != re_core::Severity::Note || d.is_error());
        Ok(RunReport {
            frames: self.frame - start,
            state_hash: self.state_hash().to_hex().to_string(),
            diagnostics,
            perf: self.perf_report(),
            screenshots: std::mem::take(&mut self.screenshots),
            output: std::mem::take(&mut self.script_output),
            compat: self.compat.as_ref().map(|c| c.info()),
        })
    }

    /// Runs one frame of play (ADR-0009): Input, PreRender, the fixed steps
    /// (PreAnimation, PreSimulation, Physics, PostSimulation), Sync, Render and
    /// EndFrame. `frame_time` is the wall time since the last frame (a headless
    /// run passes one step).
    pub fn frame(&mut self, frame_time: Duration) {
        let frame_start = Instant::now();
        let f = self.frame;
        if self.debugger.is_some()
            && let Some(scripts) = &self.scripts
        {
            re_script::debug::service(scripts.context(ContextKind::Game).lua());
        }
        if self.debugger.is_some()
            && let Some(c) = &self.compat
        {
            for kind in [ContextKind::Server, ContextKind::Client] {
                re_script::debug::service(c.vm(kind).context().lua());
            }
        }

        // Input.
        let t = Instant::now();
        let mut replay_cmds = Vec::new();
        match &mut self.replay {
            Some(replay) => {
                replay_cmds = replay.commands_at(f).cloned().collect();
                for check in replay.checks_at(f) {
                    if let Some(golden) = &check.screenshot {
                        self.screenshots.push(ScreenshotRequest {
                            frame: f,
                            golden: Some(golden.clone()),
                            path: None,
                        });
                    }
                }
                let mut actions = replay.next_frame().unwrap_or_default();
                actions.frame = f;
                self.actions = actions;
                self.raw_frame = replay.raw_frame().clone();
                self.raw_frame.frame = f;
            }
            None => {
                self.actions = self.action_map.frame(&self.raw, f);
                self.raw_frame = self.raw.raw_frame(f);
            }
        }
        self.raw.end_frame();
        self.poll_files();
        for cmd in replay_cmds {
            let outcome = self.dispatch(
                &cmd.name,
                cmd.args.clone(),
                false,
                &re_command::AllowAll,
                Origin::User,
            );
            if !outcome.ok {
                let mut d = Diagnostic::error(
                    codes::REPLAY_COMMAND_FAILED,
                    format!("replay command {:?} at frame {f} failed", cmd.name),
                )
                .at(Location::command(cmd.name.clone()));
                for r in outcome.diagnostics {
                    d = d.with_related(r);
                }
                self.diagnostics.push(d);
            }
        }
        self.perf.add(Phase::Input, t.elapsed());

        // PreRender: variable-step script callbacks (in a compat project,
        // the server context's, after its remotes are delivered).
        let t = Instant::now();
        self.ensure_compat();
        if self.compat.is_some() {
            let dt = frame_time.as_secs_f64();
            self.compat_call(ContextKind::Server, |vm, h| {
                let mut d = re_compat::net::pump(vm, h);
                d.extend(vm.pre_render(h, dt));
                d
            });
        } else {
            self.run_scripts(Phase::PreRender, frame_time.as_secs_f64());
        }
        self.perf.add(Phase::PreRender, t.elapsed());

        // Fixed steps.
        let steps = self.clock.advance(frame_time);
        self.alpha = steps.alpha;
        self.contacts.clear();
        for _ in 0..steps.count {
            self.fixed_step();
        }
        if self.compat.is_some() {
            self.compat_client_frame(frame_time.as_secs_f64(), steps.count);
        }

        // Sync.
        let t = Instant::now();
        self.world.propagate();
        self.upload_assets();
        let inputs = RenderInputs {
            assets: &self.render_assets,
            physics: &self.physics_sync,
            skins: &self.skin_sync,
            alpha: self.alpha,
            clear: self.config.render.clear,
            post: self.config.render.post,
            shadows: self.config.render.shadows,
        };
        // A compat game shows the local player's world.
        let shown = match &mut self.compat {
            Some(c) => {
                c.client.propagate();
                &mut c.client
            }
            None => &mut self.world,
        };
        self.snapshot = render::build(shown, &inputs);
        self.hud = hud::collect(shown);
        if self.compat.is_some() && self.graphics.is_some() {
            self.compat_render(self.view_size, self.view_ppp, 1, true);
        }
        audio::build(&mut self.world, &mut self.audio_sync, &mut self.audio_cmds);
        self.perf.add(Phase::Sync, t.elapsed());

        // Render: the windowed driver draws `snapshot()` and the HUD after
        // `frame` returns; here the audio commands reach the mixer.
        let t = Instant::now();
        self.diagnostics.extend(self.audio.apply(&self.audio_cmds));
        self.last_audio_cmds = std::mem::take(&mut self.audio_cmds);
        self.bus_levels = self.audio.bus_levels();
        if let Some(gfx) = &self.graphics {
            self.diagnostics.extend(gfx.take_errors());
        }
        self.perf.add(Phase::Render, t.elapsed());

        // EndFrame.
        let t = Instant::now();
        self.end_frame(f);
        self.perf.add(Phase::EndFrame, t.elapsed());

        self.frame += 1;
        self.perf.end_frame(frame_start.elapsed());
    }

    fn fixed_step(&mut self) {
        // PreAnimation.
        let t = Instant::now();
        anim::step(
            &mut self.world,
            &mut self.anim_sync,
            &mut self.media,
            self.clock.dt_secs(),
            &mut self.diagnostics,
        );
        let motions = anim_skin::step(
            &mut self.world,
            &mut self.skin_sync,
            &mut self.media,
            self.clock.dt_secs(),
            &mut self.diagnostics,
        );
        if !motions.is_empty() {
            let links = &self.physics_sync;
            let moves = anim_skin::apply_root_motion(
                &mut self.world,
                &motions,
                self.clock.dt_secs(),
                &|id| links.kind(id) == Some(physics::LinkKind::Character),
            );
            self.physics_cmds
                .extend(moves.into_iter().map(|(id, desired_velocity)| {
                    PhysicsCmd::MoveCharacter {
                        id,
                        desired_velocity,
                        jump: false,
                    }
                }));
        }
        if self.compat.is_some() {
            // The compat clock advances here, then `PreAnimation` fires.
            self.run_scripts(Phase::PreAnimation, self.clock.dt_secs().into());
        }
        self.perf.add(Phase::PreAnimation, t.elapsed());

        // PreSimulation: fixed-update script callbacks; what they queue reaches
        // this step's physics.
        let t = Instant::now();
        self.stream_step();
        self.link_new_characters();
        self.run_scripts(Phase::PreSimulation, self.clock.dt_secs().into());
        self.perf.add(Phase::PreSimulation, t.elapsed());

        // Physics.
        let t = Instant::now();
        self.physics_sync.begin_step();
        self.load_collider_sources();
        physics::push(
            &mut self.world,
            &mut self.physics_sync,
            &self.shapes,
            &mut self.physics_cmds,
            &mut self.diagnostics,
        );
        ragdoll::push(
            &mut self.world,
            &self.physics,
            &self.skin_sync,
            &mut self.ragdoll_sync,
            &mut self.physics_cmds,
            &mut self.diagnostics,
        );
        self.physics.apply(&mut self.physics_cmds);
        let ground = procedural::ground(&mut self.world);
        if self.ground.as_ref() != Some(&ground) {
            if let Some(old) = self.ground.take()
                && !ground.is_empty()
            {
                // The terrain changed under the characters: put them on it.
                self.reground(&old, &ground);
                physics::push(
                    &mut self.world,
                    &mut self.physics_sync,
                    &self.shapes,
                    &mut self.physics_cmds,
                    &mut self.diagnostics,
                );
                self.physics.apply(&mut self.physics_cmds);
            }
            self.ground = Some(ground);
        }
        let step = Instant::now();
        if let Err(d) = self.physics.step(self.clock.dt_secs()) {
            self.diagnostics.push(d);
        }
        // The server's parts (WP13) and characters (WP14), with their touch
        // events.
        if self.compat.is_some() {
            let dt: f64 = self.clock.dt_secs().into();
            self.compat_call(ContextKind::Server, |vm, h| vm.physics(h, dt));
        }
        self.perf.physics_step(step.elapsed());
        self.physics.poses_into(&mut self.poses);
        self.physics.vehicle_poses_into(&mut self.wheel_poses);
        self.contacts.extend_from_slice(self.physics.events());
        self.diagnostics.extend(self.physics.take_diagnostics());
        self.perf.add(Phase::Physics, t.elapsed());

        // PostSimulation: poses back into the World, then contact callbacks
        // and post-physics script updates.
        let t = Instant::now();
        physics::pull(&mut self.world, &mut self.physics_sync, &self.poses);
        physics::pull_wheels(&mut self.world, &mut self.physics_sync, &self.wheel_poses);
        if let Some(scripts) = &mut self.scripts {
            scripts.push_contacts(self.physics.events());
        }
        self.run_scripts(Phase::PostSimulation, self.clock.dt_secs().into());
        // The animated/simulated blend (ADR-0040), after scripts saw the step.
        ragdoll::pull(
            &self.physics,
            &mut self.skin_sync,
            &mut self.ragdoll_sync,
            self.clock.dt_secs(),
        );
        // Bone attachments follow the final pose, ragdolls included.
        anim_skin::attach(&mut self.world, &mut self.skin_sync, &mut self.diagnostics);
        self.perf.add(Phase::PostSimulation, t.elapsed());
    }

    /// World streaming (ADR-0042): loads and unloads the cells of `Streamed`
    /// procedural sources around the focuses, before scripts' fixed update
    /// sees this tick's stream events. A world with no streamed source pays a
    /// query.
    fn stream_step(&mut self) {
        let streams = self
            .world
            .ecs
            .get_resource::<re_proc::stream::Streamer>()
            .is_some_and(|s| !s.is_idle())
            || self
                .world
                .ecs
                .query_filtered::<(), re_scene::ecs::query::With<re_scene::Streamed>>()
                .iter(&self.world.ecs)
                .next()
                .is_some();
        if streams {
            if let Some(mut s) = self
                .world
                .ecs
                .get_resource_mut::<re_proc::stream::Streamer>()
                && s.lateness() != self.stream_lateness
            {
                s.set_lateness(self.stream_lateness);
            }
            // The terrain view is only made for an app that draws.
            if let Some(mut s) = self
                .world
                .ecs
                .get_resource_mut::<re_proc::stream::Streamer>()
                && s.view() != self.graphics.is_some()
            {
                s.set_view(self.graphics.is_some());
            }
            let report =
                re_proc::stream::step(&mut self.world, self.stream_tick, &self.config.stream);
            self.diagnostics.extend(report.diagnostics);
        }
        self.stream_tick += 1;
    }

    /// Gives characters and vehicles that appeared since the last physics push
    /// (the first step, a spawn, a restart) their physics character or vehicle
    /// now, so scripts can move them in the step they appear.
    fn link_new_characters(&mut self) {
        let mut unlinked: Vec<re_types::EntityId> = self
            .world
            .ecs
            .query_filtered::<re_scene::Entity, re_scene::ecs::query::With<re_scene::CharacterController>>()
            .iter(&self.world.ecs)
            .map(re_scene::entity_id)
            .filter(|&id| self.physics_sync.kind(id) != Some(physics::LinkKind::Character))
            .collect();
        unlinked.extend(
            self.world
                .ecs
                .query_filtered::<re_scene::Entity, re_scene::ecs::query::With<re_scene::Vehicle>>()
                .iter(&self.world.ecs)
                .map(re_scene::entity_id)
                .filter(|&id| !self.physics_sync.is_vehicle(id)),
        );
        // A character or vehicle that cannot be linked (reported by `push`) is tried
        // once here, not on every step.
        let fresh = unlinked.iter().any(|id| !self.link_tried.contains(id));
        self.link_tried = unlinked.into_iter().collect();
        if fresh {
            self.load_collider_sources();
            physics::push(
                &mut self.world,
                &mut self.physics_sync,
                &self.shapes,
                &mut self.physics_cmds,
                &mut self.diagnostics,
            );
            self.physics.apply(&mut self.physics_cmds);
        }
    }

    /// Puts characters back on the ground after the terrain changed: a ray
    /// down from above every terrain at the character's x and z finds the
    /// first terrain (a derived heightfield in `new`, passing through anything
    /// else on the way); when that terrain is a new one, the character's centre
    /// goes half its height (plus a skin) above the hit. Characters over a
    /// terrain that did not change stay where they are.
    fn reground(
        &mut self,
        old: &std::collections::BTreeSet<String>,
        new: &std::collections::BTreeSet<String>,
    ) {
        const SKIN: f32 = 0.02;
        const MARGIN: f32 = 10.0;
        // Scripts and earlier steps this frame moved things since Sync.
        self.world.propagate();
        let Some((bottom, top)) = procedural::ground_span(&mut self.world) else {
            return;
        };
        let characters: Vec<(re_scene::Entity, f32)> = self
            .world
            .ecs
            .query::<(re_scene::Entity, &re_scene::CharacterController)>()
            .iter(&self.world.ecs)
            .map(|(e, c)| (e, c.height))
            .collect();
        for (entity, height) in characters {
            let Some(gt) = self.world.ecs.get::<re_scene::GlobalTransform>(entity) else {
                continue;
            };
            let pos = glam::Vec3::from(gt.0.translation);
            match self.terrain_below(entity, pos, bottom - MARGIN, top + MARGIN, new) {
                Some((source, _)) if old.contains(&source) => {}
                Some((_, ground)) => {
                    let target = glam::Vec3::new(pos.x, ground + height * 0.5 + SKIN, pos.z);
                    let local = match re_scene::hierarchy::parent(&self.world.ecs, entity)
                        .and_then(|p| self.world.ecs.get::<re_scene::GlobalTransform>(p))
                    {
                        Some(parent) => parent.0.inverse().transform_point3(target),
                        None => target,
                    };
                    if let Some(mut tr) = self.world.ecs.get_mut::<re_scene::Transform>(entity) {
                        tr.translation = local;
                    }
                }
                None => {
                    let name = self.world.id_of(entity).unwrap_or("?").to_string();
                    self.diagnostics.push(
                        Diagnostic::warning(
                            codes::REGROUND_FAILED,
                            format!("no ground below character {name:?} after the terrain changed"),
                        )
                        .at(Location::entity(name)),
                    );
                }
            }
        }
        self.world.propagate();
    }

    /// The first terrain (a derived heightfield collider in `ground`) straight
    /// below `pos`, between the heights `top` and `bottom`: its source and the
    /// height of the hit.
    fn terrain_below(
        &self,
        character: re_scene::Entity,
        pos: glam::Vec3,
        bottom: f32,
        top: f32,
        ground: &std::collections::BTreeSet<String>,
    ) -> Option<(String, f32)> {
        const MAX_SKIPS: usize = 64;
        let mut origin = glam::Vec3::new(pos.x, top.max(pos.y), pos.z);
        let mut exclude = re_scene::entity_id(character);
        for _ in 0..MAX_SKIPS {
            let length = origin.y - bottom;
            if length <= 0.0 {
                return None;
            }
            let filter = re_physics::QueryFilter {
                exclude: Some(exclude),
                sensors: false,
                ..re_physics::QueryFilter::default()
            };
            let hit = self
                .physics
                .raycast(origin, glam::Vec3::NEG_Y, length, &filter)
                .ok()??;
            let terrain = entity_from_id(hit.entity)
                .and_then(|e| self.world.ecs.get::<re_scene::Collider>(e))
                .filter(|c| {
                    c.shape == re_scene::ColliderShape::Heightfield && ground.contains(&c.source)
                })
                .map(|c| c.source.clone());
            if let Some(source) = terrain {
                return Some((source, hit.point.y));
            }
            // Something else (a crate, a roof, another character): look past it.
            exclude = hit.entity;
            origin = glam::Vec3::new(pos.x, hit.point.y - 1e-3, pos.z);
        }
        None
    }

    fn end_frame(&mut self, f: u64) {
        let pending = std::mem::take(&mut self.pending);
        if pending.mouse.is_some() {
            self.mouse_behavior = pending.mouse;
        }
        for (entity, text) in pending.hud {
            let ok = entity_from_id(entity)
                .and_then(|e| self.world.ecs.get_mut::<HudText>(e))
                .map(|mut h| {
                    if h.text != text {
                        h.text = text;
                    }
                })
                .is_some();
            if !ok {
                self.diagnostics.push(Diagnostic::error(
                    codes::HOST_NO_SUCH_THING,
                    format!("entity {} has no HudText to set", entity.0),
                ));
            }
        }
        for (source, seed) in pending.regenerate {
            if let Err(e) = self
                .world
                .set(&source, "Procedural", "seed", Value::Int(seed))
            {
                self.diagnostics.push(
                    Diagnostic::error(
                        codes::HOST_NO_SUCH_THING,
                        format!("cannot regenerate {source:?}"),
                    )
                    .at(Location::entity(source.clone()))
                    .with_related(e.to_diagnostic()),
                );
            }
        }
        self.regenerate_changed();
        for path in pending.screenshots {
            self.screenshots.push(ScreenshotRequest {
                frame: f,
                golden: None,
                path: Some(path),
            });
        }
        self.take_frame_screenshots(f);
        if pending.restart {
            self.restart();
        }
    }

    /// Takes this frame's requested screenshots when the app has a GPU side:
    /// script requests are written to their path, replay checks are kept for
    /// [`App::take_captures`].
    /// Builds the local player's view for drawing: the client's 3D frame
    /// and `passes` egui passes of its UI (the `PlayerGui`) and the HUD, for
    /// a target of `size_px` physical pixels at `pixels_per_point`.
    fn compat_render(
        &mut self,
        size_px: [u32; 2],
        pixels_per_point: f32,
        passes: u32,
        build: bool,
    ) {
        let Some(gfx) = &self.graphics else {
            return;
        };
        let ctx = gfx.egui().clone();
        if !self.compat_view.fonts {
            re_compat::gui::install_fonts(&ctx);
            self.compat_view.fonts = true;
        }
        let Some(c) = &self.compat else {
            return;
        };
        let roots: Vec<_> = re_compat::players::local_player_in(&c.client)
            .and_then(|p| re_compat::instance::tree::find_child(&c.client, p, "PlayerGui", false))
            .into_iter()
            .collect();
        let input = re_compat::render::RenderInput {
            viewport: glam::Vec2::new(size_px[0] as f32, size_px[1] as f32),
            top_inset: 0.0,
            dt: self.clock.dt_secs(),
        };
        let time = self.clock.tick() as f64 * f64::from(self.clock.dt_secs());
        let raw = hud::raw_input(size_px, pixels_per_point, time);
        let hud_lines = self.hud.clone();
        if let Some(cfg) = &self.compat_config {
            self.compat_view.surface_budget = cfg.gui.surface_redraws_per_frame as usize;
        }
        // Building advances effects and the camera's viewport, so it happens
        // once a frame unless the size changes.
        if build {
            let mut built = None;
            self.compat_call(ContextKind::Client, |vm, h| {
                vm.run(h, |lua| {
                    built = Some(re_compat::render::build(lua, &input)?);
                    Ok(())
                })
            });
            if let Some(mut frame) = built {
                self.diagnostics.append(&mut frame.diagnostics);
                self.compat_view.take(frame);
            }
        }
        let camera = self.compat_view.frame().map(|f| f.gui_camera);
        // The viewports need their textures before the UI pass shows them.
        if let Some(gfx) = self.graphics.as_mut()
            && let Err(d) = gfx.compat_images(&mut self.compat_view)
        {
            self.diagnostics.push(d);
        }
        let gui = re_compat::gui::GuiFrame {
            viewport: input.viewport / pixels_per_point,
            top_inset: 0.0,
            roots,
            camera,
            input: re_compat::gui::GuiInput::default(),
            images: self.compat_view.images(),
        };
        let mut ui = Vec::new();
        let mut textured = None;
        self.compat_call(ContextKind::Client, |vm, h| {
            vm.run(h, |lua| {
                for _ in 0..passes {
                    let mut failed = None;
                    let out = ctx.run_ui(raw.clone(), |ui| {
                        match re_compat::gui::step(lua, ui.ctx(), &gui) {
                            Ok(o) => {
                                re_compat::gui::paint(ui.ctx(), o.shapes);
                                hud::draw(ui.ctx(), &hud_lines);
                                textured = Some((o.surfaces, o.viewports, o.diagnostics));
                            }
                            Err(e) => failed = Some(e),
                        }
                    });
                    if let Some(e) = failed {
                        return Err(e);
                    }
                    ui.push(out);
                }
                Ok(())
            })
        });
        if let Some((surfaces, viewports, mut diagnostics)) = textured {
            self.diagnostics.append(&mut diagnostics);
            self.compat_view.set_textured(surfaces, viewports);
        }
        for out in ui {
            self.compat_view.set_ui(out);
        }
    }

    /// Draws a compat game's view offscreen at `size`. This frame's view is
    /// reused when it was built at that size; otherwise one is built for the
    /// capture and the window's own is put back afterwards.
    fn capture_compat(&mut self, size: [u32; 2]) -> Result<Image, Diagnostic> {
        let same = size == self.view_size && self.compat_view.has_frame();
        let again = if same && self.view_ppp == 1.0 {
            self.compat_view.ui_again()
        } else {
            None
        };
        let window = if same {
            None
        } else {
            self.compat_view.frame().cloned()
        };
        if again.is_none() {
            // Two UI passes: egui lays text out with the fonts of the last.
            self.compat_render(size, 1.0, 2, !same);
        }
        let gfx = self.graphics.as_mut().expect("checked by the callers");
        let image = gfx.capture_compat(size, &mut self.compat_view, &self.hud);
        if let Some(ui) = again {
            self.compat_view.set_ui(ui);
        } else if self.compat_view.has_frame() && (window.is_some() || same) {
            if let Some(f) = window {
                self.compat_view.restore_frame(f);
            }
            self.compat_render(self.view_size, self.view_ppp, 1, false);
        }
        image
    }

    fn take_frame_screenshots(&mut self, f: u64) {
        let due: Vec<ScreenshotRequest> = self
            .screenshots
            .iter()
            .filter(|r| r.frame == f)
            .cloned()
            .collect();
        if due.is_empty() || self.graphics.is_none() {
            return;
        }
        let size = [self.config.render.width, self.config.render.height];
        let captured = if self.compat.is_some() {
            self.capture_compat(size)
        } else {
            let gfx = self.graphics.as_mut().expect("checked");
            gfx.capture(size, &self.snapshot, &self.hud)
        };
        let image = match captured {
            Ok(i) => i,
            Err(d) => {
                self.diagnostics.push(d);
                return;
            }
        };
        for request in due {
            match &request.path {
                Some(path) => {
                    if let Err(d) = self
                        .output_path(path)
                        .and_then(|p| gfx::save_png(&image, &p))
                    {
                        self.diagnostics.push(d);
                    }
                }
                None => self.captures.push((request, image.clone())),
            }
        }
    }

    fn load_collider_sources(&mut self) {
        load_collider_sources(&mut self.world, &mut self.shapes, &mut self.media);
    }

    fn poll_files(&mut self) {
        let Some(watcher) = &self.watcher else {
            return;
        };
        let changes = watcher.poll();
        if !changes.is_empty() {
            self.apply_file_changes(&changes);
        }
    }

    /// Applies file changes (from a watcher, or made by a command): re-imports
    /// what changed, uploads it again and forgets collider shapes built from
    /// it. Returns the problems re-imports reported (the last good version of
    /// each stays loaded); they are also in the app's diagnostics.
    pub fn apply_file_changes(&mut self, changes: &[re_assets::AssetChange]) -> Vec<Diagnostic> {
        let (report, stale) = self.media.apply_changes(changes);
        let mut problems = report.problems.clone();
        for key in &stale {
            self.shapes.remove(key);
        }
        self.diagnostics.extend(report.problems);
        self.reload_compat(&report.scripts);
        for path in report.scripts {
            self.reload_script(&path);
            self.changed_scripts.push(path);
        }
        if let Some(gfx) = &mut self.graphics {
            gfx.invalidate(&mut self.render_assets, &stale, &mut self.diagnostics);
        }
        self.anim_sync.invalidate(&stale);
        self.skin_sync.invalidate(&stale);
        // A changed graph file or builder script rebuilds that source's graph
        // (the new graph's hash regenerates it); a changed prefab regenerates
        // every source, since prefab contents are not part of a fingerprint.
        let paths: Vec<String> = changes
            .iter()
            .flat_map(|c| match c {
                re_assets::AssetChange::Created(p)
                | re_assets::AssetChange::Modified(p)
                | re_assets::AssetChange::Removed(p) => vec![p.clone()],
                re_assets::AssetChange::Renamed { from, to } => vec![from.clone(), to.clone()],
            })
            .collect();
        if paths.iter().any(|p| procedural::is_generator_script(p)) {
            let declared = procedural::declare_generators(
                &mut self.world,
                &self.files,
                self.config.proc.limits(),
            );
            self.diagnostics.extend(declared.iter().cloned());
            problems.extend(declared);
        }
        for p in paths.iter().filter(|p| p.ends_with(".heightfield")) {
            procedural::heightfield_changed(&mut self.world, p);
        }
        let doc_problems = self.reload_open_docs(&paths);
        self.diagnostics.extend(doc_problems.iter().cloned());
        problems.extend(doc_problems);
        let graphs: Vec<String> = paths
            .iter()
            .filter(|p| procedural::is_graph_input(&mut self.world, p))
            .cloned()
            .collect();
        if !graphs.is_empty() {
            let graph_problems =
                procedural::load_graphs(&mut self.world, &self.files, Some(&graphs));
            self.diagnostics.extend(graph_problems.iter().cloned());
            problems.extend(graph_problems);
        }
        if paths.iter().any(|p| p.ends_with(".prefab.json")) {
            let sources: Vec<re_scene::Entity> = self
                .world
                .ecs
                .query_filtered::<re_scene::Entity, re_scene::ecs::query::With<re_scene::Procedural>>()
                .iter(&self.world.ecs)
                .collect();
            for e in sources {
                let report = self.world.regenerate(e);
                self.diagnostics.extend(report.diagnostics.iter().cloned());
                procedural::release(
                    &mut self.world,
                    &mut self.media,
                    &mut self.shapes,
                    &report.released_assets,
                );
            }
        }
        problems
    }

    /// Reloads the open documents (ADR-0044) among the changed project files
    /// `paths`. A document without unsaved edits is reopened from its file
    /// (a new epoch, so its undo history is dropped, and a new revision, so
    /// views rebuild), unless the file holds exactly the open content (as
    /// after a save). A document with unsaved edits is kept, and so is one
    /// whose file is gone or invalid; each gives an `RE0528` warning.
    fn reload_open_docs(&mut self, paths: &[String]) -> Vec<Diagnostic> {
        let mut problems = Vec::new();
        for path in paths {
            let Some(doc) = self.world.doc(path) else {
                continue;
            };
            let path = re_scene::doc_path(path).unwrap_or_else(|_| path.clone());
            let warn = |message: String| {
                Diagnostic::warning(codes::OPEN_DOC_CHANGED_ON_DISK, message).at(rel(&path))
            };
            let read = std::fs::read_to_string(self.project.join(&path))
                .map_err(|e| e.to_string())
                .and_then(|t| {
                    serde_json::from_str::<serde_json::Value>(&t).map_err(|e| e.to_string())
                });
            match open_doc_reload(doc.dirty, &doc.value, read, |v| doc.matches_saved(v)) {
                DocReload::Keep => {}
                DocReload::Conflict => problems.push(warn(format!(
                    "{path} changed on disk while it has unsaved edits; saving will overwrite the file"
                ))),
                DocReload::Unreadable(e) => problems.push(warn(format!(
                    "{path} changed on disk but cannot be reloaded ({e}); the open document is kept"
                ))),
                DocReload::Reopen(value) => {
                    if let Err(e) = self.world.open_doc(&path, value) {
                        problems.push(warn(format!(
                            "{path} changed on disk but cannot be reloaded ({e}); the open document is kept"
                        )));
                    }
                }
            }
        }
        problems
    }

    /// The script runtime, created on first use (so editing never starts a VM).
    fn script_runtime(&mut self) -> Option<&mut ScriptRuntime> {
        if self.scripts.is_none() {
            if self.scripts_failed {
                return None;
            }
            let config = RuntimeConfig::with_files(self.files.clone()).with_limits(
                self.config.scripts.cpu_budget_ms.max(0.0).ceil() as u64,
                self.config.scripts.memory_limit_mb as usize,
            );
            match ScriptRuntime::new(config) {
                Ok(r) => {
                    self.scripts = Some(r);
                    self.attach_debugger();
                }
                Err(e) => {
                    // Reported once; the game runs on without scripts.
                    self.scripts_failed = true;
                    self.diagnostics.push(e.to_diagnostic());
                    return None;
                }
            }
        }
        self.scripts.as_mut()
    }

    /// The script runtime, once the game has run a frame.
    pub fn scripts(&self) -> Option<&ScriptRuntime> {
        self.scripts.as_ref()
    }

    /// Attaches the game context's VM, and a compat project's server and
    /// client VMs, to `debugger` (ADR-0045), now if the scripts are running
    /// and again whenever a VM is replaced; `None` detaches them. Breakpoint changes and commands for a parked script are
    /// carried out at the start of each frame.
    pub fn set_debugger(&mut self, debugger: Option<re_script::debug::Debugger>) {
        if let (Some(old), Some(scripts)) = (&self.debugger, &self.scripts) {
            old.detach(scripts.context(ContextKind::Game).lua());
        }
        if let (Some(old), Some(c)) = (&self.debugger, &self.compat) {
            for kind in [ContextKind::Server, ContextKind::Client] {
                old.detach(c.vm(kind).context().lua());
            }
        }
        self.debugger = debugger;
        self.attach_debugger();
        self.attach_compat_debugger();
    }

    /// The debugger set with [`App::set_debugger`].
    pub fn debugger(&self) -> Option<&re_script::debug::Debugger> {
        self.debugger.as_ref()
    }

    fn attach_debugger(&mut self) {
        let (Some(debugger), Some(scripts)) = (&self.debugger, &self.scripts) else {
            return;
        };
        let lua = scripts.context(ContextKind::Game).lua();
        if let Err(e) = debugger.attach(lua, ContextKind::Game.name(), None) {
            self.diagnostics
                .push(ScriptError::from_mlua(ContextKind::Game.name(), &e).to_diagnostic());
        }
    }

    /// Attaches the compat server and client VMs to the debugger, with the
    /// scripts they already run.
    fn attach_compat_debugger(&mut self) {
        let (Some(debugger), Some(c)) = (&self.debugger, &self.compat) else {
            return;
        };
        for kind in [ContextKind::Server, ContextKind::Client] {
            if let Err(e) = c.vm(kind).attach_debugger(debugger) {
                self.diagnostics
                    .push(ScriptError::from_mlua(kind.name(), &e).to_diagnostic());
            }
        }
    }

    /// Calls `f` with the script runtime (created on first use) and a host over
    /// the running game, then collects what the scripts printed. `None` when the
    /// runtime could not be created.
    fn with_scripts<R>(
        &mut self,
        f: impl FnOnce(&mut ScriptRuntime, &mut PlayHost<'_>) -> R,
    ) -> Option<R> {
        self.script_runtime()?;
        let scripts = self.scripts.as_mut().expect("created above");
        let mut host = PlayHost {
            world: &mut self.world,
            services: Host {
                physics: &self.physics,
                links: &self.physics_sync,
                actions: &self.actions,
                raw: &self.raw_frame,
                physics_cmds: &mut self.physics_cmds,
                audio_cmds: &mut self.audio_cmds,
                bus_levels: &self.bus_levels,
                audio_graph: self.audio.graph(),
                pending: &mut self.pending,
                skins: &mut self.skin_sync,
            },
        };
        let out = f(scripts, &mut host);
        self.collect_script_output();
        Some(out)
    }

    fn run_scripts(&mut self, phase: Phase, dt: f64) {
        if self.compat_config.is_some() {
            // The server context's fixed-step phases; PreRender is in `frame`.
            if self.compat.is_some() {
                self.compat_call(ContextKind::Server, |vm, h| match phase {
                    Phase::PreAnimation => vm.pre_animation(h, dt),
                    Phase::PreSimulation => vm.pre_simulation(h, dt),
                    Phase::PostSimulation => {
                        // Tweens and sounds step after the physics (WP15);
                        // the server plays no sound of its own.
                        let mut d = vm.media(h, dt);
                        re_compat::sound::take_commands(vm.context().lua());
                        d.extend(vm.post_simulation(h, dt));
                        d
                    }
                    _ => Vec::new(),
                });
            }
            return;
        }
        if let Some(diags) = self.with_scripts(|s, host| s.run_phase(host, phase, dt)) {
            self.report_script_errors(diags);
        }
    }

    /// Starts the compat contexts on the first frame of a compat project:
    /// splits the World, starts both sides' scripts and joins the
    /// `[compat] players`. A failure is reported once; the game then runs
    /// without scripts.
    fn ensure_compat(&mut self) {
        if self.compat.is_some() || self.compat_failed {
            return;
        }
        let Some(cfg) = self.compat_config.clone() else {
            return;
        };
        let mut clips = std::collections::HashMap::new();
        for path in cfg.assets.values() {
            if let Ok(secs) = self.audio.clip_seconds(path) {
                clips.insert(path.clone(), secs);
            }
        }
        match crate::compat::CompatPlay::new(
            &self.files,
            &self.config,
            &cfg,
            &mut self.world,
            clips,
            self.persist_data,
        ) {
            Ok(play) => self.compat = Some(play),
            Err(d) => {
                self.compat_failed = true;
                self.diagnostics.push(d);
                return;
            }
        }
        // Attached before the scripts start, so their first lines can stop.
        self.attach_compat_debugger();
        for kind in [ContextKind::Server, ContextKind::Client] {
            self.compat_call(kind, |vm, h| crate::compat::start_scripts(vm, h));
        }
        for _ in 0..cfg.players {
            if let Err(d) = self.join_player() {
                self.diagnostics.push(d);
            }
        }
    }

    /// Joins the next compat player (`Player<n>`); returns its `Player`.
    fn join_player(&mut self) -> Result<re_scene::Entity, Diagnostic> {
        let Some(c) = self.compat.as_mut() else {
            return Err(crate::play::step_failed(
                "joining a player needs a [compat] project",
            ));
        };
        let n = c.players + 1;
        let mut joined = Err(crate::play::step_failed("the player did not join"));
        self.compat_call(ContextKind::Server, |vm, h| {
            let (j, d) = crate::compat::add_player(vm, h, n);
            joined = j;
            d
        });
        if joined.is_ok()
            && let Some(c) = self.compat.as_mut()
        {
            c.players = n;
        }
        joined
    }

    /// Runs the steps of a `play.run` (after its first frames), in order, and
    /// returns one JSON result per step (`{ "step", "ok", ... }`). Frames,
    /// diagnostics, output and the final state hash are added to `report`. A
    /// step that fails reports its diagnostic and the play goes on.
    pub fn play_steps(
        &mut self,
        steps: &[crate::play::PlayStep],
        report: &mut RunReport,
    ) -> Vec<serde_json::Value> {
        use crate::play::PlayStep;
        use serde_json::json;
        let mut results = Vec::new();
        for step in steps {
            let result = match step {
                PlayStep::Frames(n) => match self.run_headless(Some(*n), None) {
                    Ok(r) => {
                        report.frames += r.frames;
                        report.diagnostics.extend(r.diagnostics);
                        report.screenshots.extend(r.screenshots);
                        report.output.extend(r.output);
                        Ok(json!({ "step": "frames", "frames": r.frames }))
                    }
                    Err(d) => Err(("frames", d)),
                },
                PlayStep::AddPlayer => match self.join_player() {
                    Ok(p) => {
                        let world = &self.world;
                        Ok(json!({
                            "step": "add_player",
                            "player": world.name_of(p),
                            "id": world.id_of(p),
                        }))
                    }
                    Err(d) => Err(("add_player", d)),
                },
                PlayStep::Say(say) => self.play_say(say).map_err(|d| ("say", d)),
                PlayStep::Invoke(inv) => self.play_invoke(inv).map_err(|d| ("invoke", d)),
                PlayStep::DumpTree(dump) => self.play_dump(dump).map_err(|d| ("dump_tree", d)),
            };
            results.push(match result {
                Ok(mut v) => {
                    v["ok"] = json!(true);
                    v
                }
                Err((name, d)) => {
                    let v = json!({ "step": name, "ok": false, "error": d });
                    self.diagnostics.push(d);
                    v
                }
            });
        }
        let mut diagnostics = self.take_diagnostics();
        diagnostics.retain(|d| d.severity != re_core::Severity::Note || d.is_error());
        report.diagnostics.extend(diagnostics);
        report
            .output
            .extend(std::mem::take(&mut self.script_output));
        report.state_hash = self.state_hash().to_hex().to_string();
        report.compat = self.compat.as_ref().map(|c| c.info());
        results
    }

    fn play_say(&mut self, say: &crate::play::SayStep) -> Result<serde_json::Value, Diagnostic> {
        if self.compat.is_none() {
            return Err(crate::play::step_failed("`say` needs a [compat] project"));
        }
        let name = format!("Player{}", say.player);
        let player = crate::play::instance_at(&self.world, &format!("Players.{name}"))?;
        let mut said = Ok(());
        self.compat_call(ContextKind::Server, |vm, h| {
            let (r, d) = re_compat::players::say(vm, h, player, &say.text);
            said = r;
            d
        });
        said?;
        Ok(serde_json::json!({ "step": "say", "player": name }))
    }

    fn play_invoke(
        &mut self,
        inv: &crate::play::InvokeStep,
    ) -> Result<serde_json::Value, Diagnostic> {
        let Some(c) = &self.compat else {
            return Err(crate::play::step_failed(
                "`invoke` needs a [compat] project",
            ));
        };
        let (kind, world) = if inv.client {
            (ContextKind::Client, &c.client)
        } else {
            (ContextKind::Server, &self.world)
        };
        let entity = crate::play::instance_at(world, &inv.path)?;
        let args = serde_json::Value::Array(inv.args.clone()).to_string();
        let mut out = Err(crate::play::step_failed("the call did not run"));
        self.compat_call(kind, |vm, h| {
            let (r, d) = re_compat::net::invoke_bindable(vm, h, entity, &args);
            out = r;
            d
        });
        let text = out?;
        let results: serde_json::Value =
            serde_json::from_str(&text).unwrap_or(serde_json::Value::String(text));
        Ok(serde_json::json!({ "step": "invoke", "path": inv.path, "results": results }))
    }

    fn play_dump(&self, dump: &crate::play::DumpStep) -> Result<serde_json::Value, Diagnostic> {
        let world = match (&self.compat, dump.client) {
            (Some(c), true) => &c.client,
            (None, true) => {
                return Err(crate::play::step_failed(
                    "a client dump needs a [compat] project",
                ));
            }
            _ => &self.world,
        };
        let start = crate::play::instance_at(world, dump.path.as_deref().unwrap_or(""))?;
        let root = world.id_of(start).unwrap_or_default().to_string();
        let tree = re_compat::dump::dump_tree(world, &crate::play::dump_args(dump, root))?;
        let mut v = serde_json::json!(tree);
        v["step"] = serde_json::json!("dump_tree");
        Ok(v)
    }

    /// Calls `f` with a compat context's VM and a host over its world, then
    /// keeps and reports the errors and collects the output.
    fn compat_call(
        &mut self,
        kind: ContextKind,
        f: impl FnOnce(&re_compat::CompatVm, &mut PlayHost<'_>) -> Vec<Diagnostic>,
    ) {
        let Some(c) = self.compat.as_mut() else {
            return;
        };
        let (world, vm) = match kind {
            ContextKind::Client => (&mut c.client, &c.client_vm),
            _ => (&mut self.world, &c.server_vm),
        };
        let mut host = PlayHost {
            world,
            services: Host {
                physics: &self.physics,
                links: &self.physics_sync,
                actions: &self.actions,
                raw: &self.raw_frame,
                physics_cmds: &mut self.physics_cmds,
                audio_cmds: &mut self.audio_cmds,
                bus_levels: &self.bus_levels,
                audio_graph: self.audio.graph(),
                pending: &mut self.pending,
                skins: &mut self.skin_sync,
            },
        };
        let diags = f(vm, &mut host);
        let diags = c.keep(kind, diags);
        let output = c.take_output();
        self.report_script_errors(diags);
        for line in output {
            let text = format!("[{}] {}", line.context.name(), line.text);
            if line.warn {
                tracing::warn!(target: "script", "{text}");
            } else {
                tracing::info!(target: "script", "{text}");
            }
            self.script_output.push(text);
        }
    }

    /// The client half of a compat frame, after the server's fixed steps:
    /// replication, the client's deliveries and phases (one round of fixed
    /// phases per server step, with character and part physics, tweens and
    /// sounds),
    /// then the sound commands for the mixer.
    fn compat_client_frame(&mut self, frame_dt: f64, steps: u32) {
        if let Some(c) = &mut self.compat {
            c.replicate(&mut self.world);
        }
        let step_dt: f64 = self.clock.dt_secs().into();
        self.compat_call(ContextKind::Client, |vm, h| {
            let mut d = re_compat::net::pump(vm, h);
            d.extend(vm.pre_render(h, frame_dt));
            for _ in 0..steps {
                d.extend(vm.pre_animation(h, step_dt));
                d.extend(vm.pre_simulation(h, step_dt));
                d.extend(vm.physics(h, step_dt));
                d.extend(vm.media(h, step_dt));
                d.extend(vm.post_simulation(h, step_dt));
            }
            d
        });
        let sounds = self
            .compat
            .as_ref()
            .map(|c| crate::compat::client_sounds(&c.client_vm, None))
            .unwrap_or_default();
        self.audio_cmds.extend(sounds);
    }

    /// Reports script diagnostics, one per code and place until the scripts
    /// restart or reload (a failing callback fails every step, possibly with a
    /// different message each time); repeats only go to the log.
    fn report_script_errors(&mut self, diags: Vec<Diagnostic>) {
        for d in diags {
            let key = format!("{}|{:?}", d.code, d.location);
            if self.script_errors.insert(key) {
                self.diagnostics.push(d);
            } else {
                tracing::debug!(target: "script", "repeated: {}", d.message);
            }
        }
    }

    fn collect_script_output(&mut self) {
        let Some(scripts) = &mut self.scripts else {
            return;
        };
        for line in scripts.take_output() {
            let text = format!("[{}] {}", line.context.name(), line.text);
            if line.warn {
                tracing::warn!(target: "script", "{text}");
            } else {
                tracing::info!(target: "script", "{text}");
            }
            self.script_output.push(text);
        }
    }

    /// Restarts a running compat game's changed scripts, and the scripts
    /// that required a changed module (ADR-0048 §3), in both contexts. Only
    /// while files are watched ([`App::enable_hot_reload`], so `real run` and
    /// the editor): `real verify`, replays and `play.run` read files once.
    /// A changed source that does not compile keeps the old run (`RE9420`).
    fn reload_compat(&mut self, paths: &[String]) {
        if paths.is_empty() || self.compat.is_none() || self.watcher.is_none() {
            return;
        }
        let mut reports: Vec<Diagnostic> = Vec::new();
        for kind in [ContextKind::Server, ContextKind::Client] {
            self.compat_call(kind, |vm, h| {
                let (r, errors): (Vec<_>, Vec<_>) = vm
                    .reload(h, paths)
                    .into_iter()
                    .partition(|d| d.code == re_compat::codes::RELOAD_COMPILE);
                // Both contexts compile the file; one report is enough.
                for d in r {
                    if !reports.contains(&d) {
                        reports.push(d);
                    }
                }
                errors
            });
        }
        for d in &reports {
            tracing::warn!(target: "script", "{}", d.message);
        }
        self.diagnostics.extend(reports);
    }

    /// Reloads a changed script (hot reload): modules are patched or replaced,
    /// entry scripts restart. A `[compat]` project's scripts restart instead
    /// (`reload_compat`).
    pub fn reload_script(&mut self, path: &str) {
        if self.scripts.is_none() {
            // Nothing runs yet; the new version is what starts.
            return;
        }
        self.script_errors.clear();
        if let Some(report) = self.with_scripts(|s, host| s.reload(host, path)) {
            self.report_script_errors(report.diagnostics);
        }
    }

    /// Runs a Luau snippet to completion in the game or tools context, against
    /// the running game (no `task.wait`), and returns its first result as JSON.
    ///
    /// # Errors
    ///
    /// The script's diagnostics.
    pub fn exec_script(
        &mut self,
        kind: ContextKind,
        name: &str,
        source: &str,
    ) -> Result<serde_json::Value, Vec<Diagnostic>> {
        match self.with_scripts(|s, host| s.exec(host, kind, name, source)) {
            Some(out) => out,
            None => Err(vec![Diagnostic::error(
                re_script::codes::SERVICE_UNAVAILABLE,
                "the script runtime could not be started (see the earlier error)",
            )]),
        }
    }

    /// Reloads the entry scene into a fresh World (what `world.restart()` asks
    /// for): physics, audio and the undo history start over; the frame counter
    /// and the replay keep going.
    pub fn restart(&mut self) {
        match load_world(&self.files, &self.config) {
            Ok((world, problems)) => {
                self.world = world;
                if let Err(d) = ensure_streamer(&mut self.world, &self.config) {
                    self.diagnostics.push(d);
                }
                self.diagnostics.extend(problems);
                self.ground = None;
                self.link_tried.clear();
                self.graph_inputs = procedural::inputs(&mut self.world);
                self.restarts += 1;
                self.history = History::default();
                match Physics::new(physics_config(&self.config), TaskPool::global()) {
                    Ok(p) => self.physics = p,
                    Err(d) => self.diagnostics.push(d),
                }
                self.physics_sync = PhysicsSync::new();
                self.physics_cmds.clear();
                self.audio_cmds.push(AudioCmd::StopAll);
                self.audio_sync = AudioSync::new();
                self.anim_sync = AnimSync::new();
                self.skin_sync = SkinSync::new();
                self.ragdoll_sync = RagdollSync::new();
                self.script_errors.clear();
                // The compat contexts start again on the next frame.
                self.compat = None;
                self.compat_failed = false;
                // The new client VM numbers its meshes from the start.
                match &mut self.graphics {
                    Some(g) => self.compat_view.release_all(g.renderer_mut()),
                    None => {
                        self.compat_view = crate::compat_view::CompatView::new(&self.config.render)
                    }
                }
                if self.compat_config.is_some() {
                    // A cursor the old scripts held or hid is let go.
                    self.mouse_behavior = Some((re_types::input::MouseLock::Free, true));
                }
                if let Some(scripts) = &mut self.scripts
                    && let Err(e) = scripts.reset()
                {
                    self.diagnostics.push(e.to_diagnostic());
                }
                // The fresh game VM is attached in place of the old one.
                self.attach_debugger();
                self.clock.reset();
                self.stream_tick = 0;
                self.edit_frame();
            }
            Err(d) => self.diagnostics.push(d),
        }
    }
}

/// Puts the meshes that `Convex` and `Mesh` colliders name (asset paths and
/// built-ins) into the shape library, loading each once.
fn load_collider_sources(world: &mut World, shapes: &mut ShapeLibrary, media: &mut Media) {
    use re_scene::{Collider, ColliderShape};
    procedural::sync_derived(world, media, shapes);
    let wanted: Vec<String> = world
        .ecs
        .query::<&Collider>()
        .iter(&world.ecs)
        .filter(|c| matches!(c.shape, ColliderShape::Convex | ColliderShape::Mesh))
        .map(|c| c.source.clone())
        .filter(|s| !s.is_empty() && !s.starts_with(crate::media::DERIVED))
        .collect();
    for source in wanted {
        if shapes.mesh(&source).is_some() {
            continue;
        }
        // A source that cannot be loaded stays missing; push reports it.
        if let Ok(loaded) = media.mesh(&source) {
            shapes.insert_mesh(source, loaded.mesh);
        }
    }
}

fn headless_audio(files: &ProjectFiles) -> Audio {
    let mut audio = Audio::headless();
    audio.set_files(files.clone());
    audio
}

/// An empty World with the components the project's content may use: in a
/// compat project, the class layer's components too.
pub(crate) fn content_world(config: &AppConfig) -> Result<World, Diagnostic> {
    let mut world = World::new();
    if config.compat.is_some() {
        // A compat scene's instances carry the class layer's components.
        let setup = crate::compat::setup()?;
        setup.classes.register_components(&mut world).map_err(|e| {
            Diagnostic::error(
                codes::ENTRY_SCENE_FAILED,
                format!("the compat classes do not register: {e}"),
            )
        })?;
    }
    Ok(world)
}

/// A fresh World with the entry scene loaded and the graphs of its procedural
/// sources in its store (they generate on the first `regenerate_changed`), with
/// the problems making those graphs.
fn load_world(
    files: &ProjectFiles,
    config: &AppConfig,
) -> Result<(World, Vec<Diagnostic>), Diagnostic> {
    let rel_path = &config.project.entry_scene;
    let fail = |msg: String| Diagnostic::error(codes::ENTRY_SCENE_FAILED, msg).at(rel(rel_path));
    let text = files
        .read_str(rel_path)
        .map_err(|e| fail(format!("cannot read the entry scene {rel_path}: {e}")))?;
    let doc = re_scene::scene::from_json(&text).map_err(|e| {
        fail(format!("the entry scene {rel_path} does not parse")).with_related(e.to_diagnostic())
    })?;
    let mut world = content_world(config).map_err(|e| fail(e.message.clone()))?;
    procedural::install(&mut world, files);
    let mut problems = procedural::declare_generators(&mut world, files, config.proc.limits());
    world.load(&doc).map_err(|e| {
        fail(format!("the entry scene {rel_path} does not load")).with_related(e.to_diagnostic())
    })?;
    world.propagate_all();
    problems.extend(procedural::load_graphs(&mut world, files, None));
    Ok((world, problems))
}

/// What to do with an open document whose file changed on disk.
#[derive(Debug, PartialEq)]
enum DocReload {
    /// The file holds the open content, or the content last saved (our own
    /// save, seen after later edits).
    Keep,
    /// Reopen the document with the file's content.
    Reopen(serde_json::Value),
    /// The document has unsaved edits: keep them and warn.
    Conflict,
    /// The file is gone or not JSON: keep the document and warn.
    Unreadable(String),
}

/// Decides what a change on disk does to an open document: `dirty` and
/// `open` are the document's state, `file` what reading the file gave, and
/// `saved` tells whether a content is what the document was last saved as.
fn open_doc_reload(
    dirty: bool,
    open: &serde_json::Value,
    file: Result<serde_json::Value, String>,
    saved: impl Fn(&serde_json::Value) -> bool,
) -> DocReload {
    match file {
        Ok(value) if value == *open || saved(&value) => DocReload::Keep,
        _ if dirty => DocReload::Conflict,
        Ok(value) => DocReload::Reopen(value),
        Err(e) => DocReload::Unreadable(e),
    }
}

#[cfg(test)]
mod doc_reload_tests {
    use super::{DocReload, open_doc_reload};
    use serde_json::json;

    #[test]
    fn a_change_on_disk_reopens_clean_documents_only() {
        let open = json!({"a": 1});
        assert_eq!(
            open_doc_reload(false, &open, Ok(json!({"a": 1})), |_| false),
            DocReload::Keep
        );
        assert_eq!(
            open_doc_reload(true, &open, Ok(json!({"a": 1})), |_| false),
            DocReload::Keep
        );
        assert_eq!(
            open_doc_reload(false, &open, Ok(json!({"a": 2})), |_| false),
            DocReload::Reopen(json!({"a": 2}))
        );
        assert_eq!(
            open_doc_reload(true, &open, Ok(json!({"a": 2})), |_| false),
            DocReload::Conflict
        );
        assert_eq!(
            open_doc_reload(true, &open, Err("gone".into()), |_| false),
            DocReload::Conflict
        );
        assert_eq!(
            open_doc_reload(false, &open, Err("gone".into()), |_| false),
            DocReload::Unreadable("gone".into())
        );
    }

    /// A save followed by an edit before the watcher reports the save: the
    /// file holds the saved content, which is not a conflict.
    #[test]
    fn our_own_save_seen_after_an_edit_is_kept() {
        let saved = json!({"a": 1});
        let edited = json!({"a": 2});
        assert_eq!(
            open_doc_reload(true, &edited, Ok(saved.clone()), |v| *v == saved),
            DocReload::Keep
        );
        assert_eq!(
            open_doc_reload(true, &edited, Ok(json!({"a": 3})), |v| *v == saved),
            DocReload::Conflict
        );
    }
}
