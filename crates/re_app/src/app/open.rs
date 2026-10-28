//! Opening a project into an [`App`], and the copy of it that `play.run` plays
//! (ADR-0021).

use std::path::Path;
use std::sync::Arc;

use re_command::{CommandRegistry, History};
use re_core::files::ProjectFiles;
use re_core::{Diagnostic, FixedClock, Location, TaskPool};
use re_input::{ActionMap, RawInput};
use re_physics::{Physics, PhysicsConfig};
use re_scene::World;
use re_types::{ActionFrame, RenderSnapshot, ReplayDoc};

use re_audio::Audio;
use re_render::Image;

use crate::codes;
use crate::config::AppConfig;
use crate::host::Pending;
use crate::media::Media;
use crate::perf::PerfRecorder;
use crate::procedural;
use crate::scenes::{self, Scenes};
use crate::sync::anim::AnimSync;
use crate::sync::anim_skin::SkinSync;
use crate::sync::audio::AudioSync;
use crate::sync::physics::{self, PhysicsSync, ShapeLibrary};
use crate::sync::ragdoll::RagdollSync;
use crate::sync::render::RenderAssets;

use super::physics_step::load_collider_sources;
use super::{App, RunReport, script_levels};

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
        let (world, problems) = scenes::fresh_world(&files, &config, &config.project.entry_scene)?;
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
        scenes::ensure_streamer(&mut world, &config.stream)?;
        let compat_config = crate::compat::compat_config(&config)?;
        let scenes = Scenes::new(config.scene_list()?, &config.project.entry_scene);
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
            kept_file_changes: None,
            anim_sync: AnimSync::new(),
            skin_sync: SkinSync::new(),
            ragdoll_sync: RagdollSync::new(),
            ground: None,
            graph_inputs,
            scenes,
            start_world: None,
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
    /// game that already started copies the scene the run began in instead,
    /// since its World is split between the server and the client.
    ///
    /// The copy is [`crate::scenes::fork_world`] with a fresh physics world
    /// built from it and given the live bodies' motion
    /// ([`Physics::motion_state`]), so it carries on from the live state, with
    /// the same scenes loaded and entities kept; contacts start fresh, and
    /// nothing the live run asked to load is loading in the copy. Audio goes
    /// to a silent mixer.
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
        let (world, problems) = if restarted {
            let (mut world, problems) =
                scenes::fresh_world(&self.files, &self.config, self.scenes.start_path())?;
            if let Some(store) = self.world.ecs.get_resource::<re_proc::GraphStore>() {
                world.ecs.insert_resource(store.clone());
            }
            (world, problems)
        } else {
            // Streaming carries on from the live state too.
            (scenes::fork_world(&self.world), Vec::new())
        };
        let mut copy = App::with_world(
            self.files.clone(),
            self.config.clone(),
            world,
            self.action_map.clone(),
            Arc::clone(&self.commands),
        )?;
        copy.alpha = self.alpha;
        copy.shaders = self.shaders.clone();
        if restarted {
            copy.scenes.begin_at(self.scenes.start_path());
        } else {
            copy.scenes = self.scenes.fork();
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
}

pub(super) fn physics_config(config: &AppConfig) -> PhysicsConfig {
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
