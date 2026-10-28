//! Opening a project into an [`App`], the copy of it that `play.run` plays
//! (ADR-0021), and `world.restart()`.

use std::path::Path;
use std::sync::Arc;

use re_command::{CommandRegistry, History};
use re_core::files::ProjectFiles;
use re_core::{Diagnostic, FixedClock, Location, TaskPool};
use re_input::{ActionMap, RawInput};
use re_physics::{Physics, PhysicsConfig};
use re_scene::World;
use re_types::{ActionFrame, AudioCmd, RenderSnapshot, ReplayDoc};

use re_audio::Audio;
use re_render::Image;

use crate::codes;
use crate::config::AppConfig;
use crate::host::Pending;
use crate::media::Media;
use crate::perf::PerfRecorder;
use crate::procedural;
use crate::sync::anim::AnimSync;
use crate::sync::anim_skin::SkinSync;
use crate::sync::audio::AudioSync;
use crate::sync::physics::{self, PhysicsSync, ShapeLibrary};
use crate::sync::ragdoll::RagdollSync;
use crate::sync::render::RenderAssets;

use super::physics_step::load_collider_sources;
use super::{App, RunReport, rel, script_levels};

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
        let (shaders, problems) = crate::shaders::ShaderFiles::load(&app.files);
        app.shaders = shaders;
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
        let script_levels = script_levels(&audio, &bus_levels);
        Ok(App {
            project: files
                .project_dir()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
            media: Media::with_files(files.clone()).with_assets_config(config.assets),
            audio,
            files,
            world,
            history: History::default(),
            commands,
            clock,
            stream_tick: 0,
            stream_lateness: re_proc::stream::Lateness::Wait,
            origin_refused: None,
            frame: 0,
            alpha: 0.0,
            action_map,
            raw: RawInput::default(),
            raw_frame: re_types::input::RawInputFrame::default(),
            mouse_behavior: None,
            actions: ActionFrame::default(),
            replay: None,
            seed: config.scripts.seed,
            ids_seeded: false,
            time_scale: 1.0,
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
            shaders: Default::default(),
            shader_watcher: None,
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
            net: None,
            stopped: false,
            half: None,
            view_size: [config.render.width, config.render.height],
            view_ppp: 1.0,
            script_output: Vec::new(),
            script_errors: Default::default(),
            audio_sync: AudioSync::new(),
            audio_cmds: Vec::new(),
            last_audio_cmds: Vec::new(),
            bus_levels,
            script_levels,
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
        copy.shaders = self.shaders.clone();
        if !restarted {
            copy.stream_tick = self.stream_tick;
        }
        copy.diagnostics.extend(problems);
        if !restarted && let Err(d) = copy.physics.set_origin(self.physics.origin()) {
            copy.diagnostics.push(d);
        }
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
        let mut run = copy.run_headless(Some(frames), replay).map(|mut r| {
            let results = copy.play_steps(steps, &mut r);
            (r, results)
        });
        let image = match (&run, screenshot) {
            (Ok(_), true) => Some(copy.screenshot(None)),
            _ => None,
        };
        // The screenshot's own problems (a terrain chunk that failed as the
        // view completed, a mesh it could not upload) belong to the run.
        if let Ok((r, _)) = &mut run {
            let mut problems = copy.take_diagnostics();
            problems.retain(|d| d.severity != re_core::Severity::Note || d.is_error());
            r.diagnostics.extend(problems);
        }
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
                self.origin_refused = None;
                self.audio_cmds.push(AudioCmd::StopAll);
                self.audio_sync = AudioSync::new();
                self.anim_sync = AnimSync::new();
                self.skin_sync = SkinSync::new();
                self.ragdoll_sync = RagdollSync::new();
                self.script_errors.clear();
                // The compat contexts start again on the next frame, offline:
                // a network session does not survive a restart.
                if let Some(net) = &mut self.net {
                    net.close();
                    self.stopped = true;
                }
                self.net = None;
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
                self.time_scale = 1.0;
                // The reloaded World makes the first run's ids again.
                self.ids_seeded = false;
                self.stream_tick = 0;
                self.edit_frame();
            }
            Err(d) => self.diagnostics.push(d),
        }
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
