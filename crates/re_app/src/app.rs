//! [`App`]: a project opened for running and editing, and the frame loop of
//! ADR-0009.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use re_command::{
    CommandOutcome, CommandRegistry, EditTarget, EngineReceiver, EngineRequest, History, Origin,
    PolicyCheck,
};
use re_core::{Diagnostic, FixedClock, Location, Phase, TaskPool};
use re_input::{ActionMap, RawInput, Replay};
use re_physics::{Physics, PhysicsConfig};
use re_reflect::Value;
use re_scene::{HudText, SaveOptions, World, entity_from_id};
use re_types::{
    ActionFrame, AudioCmd, BodyPose, ContactEvent, PhysicsCmd, RenderSnapshot, ReplayDoc,
};
use serde::Serialize;

use crate::codes;
use crate::config::AppConfig;
use crate::host::{Host, Pending};
use crate::perf::{PerfCounts, PerfRecorder, PerfReport};
use crate::sync::audio::{self, AudioSync};
use crate::sync::physics::{self, PhysicsSync, ShapeLibrary};
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
    /// The world's [`state_hash`](re_scene::World::state_hash) after the last frame, as hex.
    pub state_hash: String,
    /// Everything reported during the run (subsystem errors, failed replay
    /// commands, script errors).
    pub diagnostics: Vec<Diagnostic>,
    /// Frame timings and counts.
    pub perf: PerfReport,
    /// Screenshots asked for during the run.
    pub screenshots: Vec<ScreenshotRequest>,
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
    config: AppConfig,
    world: World,
    history: History,
    commands: Arc<CommandRegistry<App>>,
    clock: FixedClock,
    frame: u64,
    alpha: f32,

    action_map: ActionMap<String>,
    raw: RawInput,
    actions: ActionFrame,
    replay: Option<Replay>,

    physics: Physics,
    physics_sync: PhysicsSync,
    shapes: ShapeLibrary,
    physics_cmds: Vec<PhysicsCmd>,
    poses: Vec<BodyPose>,
    contacts: Vec<ContactEvent>,

    render_assets: RenderAssets,
    snapshot: RenderSnapshot,

    audio_sync: AudioSync,
    audio_cmds: Vec<AudioCmd>,

    pending: Pending,
    screenshots: Vec<ScreenshotRequest>,
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
        let world = load_world(project, &config)?;
        let physics = Physics::new(physics_config(&config), TaskPool::global())?;
        let mut commands = CommandRegistry::new();
        re_command::register_builtin_commands(&mut commands);
        crate::commands::register(&mut commands);
        let clock = FixedClock::new(config.sim.fixed_hz, config.sim.max_steps_per_frame);
        let mut app = App {
            project: project.to_path_buf(),
            world,
            history: History::default(),
            commands: Arc::new(commands),
            clock,
            frame: 0,
            alpha: 0.0,
            action_map,
            raw: RawInput::default(),
            actions: ActionFrame::default(),
            replay: None,
            physics,
            physics_sync: PhysicsSync::new(),
            shapes: ShapeLibrary::default(),
            physics_cmds: Vec::new(),
            poses: Vec::new(),
            contacts: Vec::new(),
            render_assets: RenderAssets::default(),
            snapshot: RenderSnapshot::default(),
            audio_sync: AudioSync::new(),
            audio_cmds: Vec::new(),
            pending: Pending::default(),
            screenshots: Vec::new(),
            perf: PerfRecorder::new(),
            diagnostics: Vec::new(),
            verify_stages: Vec::new(),
            config,
        };
        app.edit_frame();
        Ok(app)
    }

    /// Opens the project at `project` with its `real.toml` and the given runtime
    /// options left at their defaults (headless, edits not persisted).
    pub fn open_project(project: &Path) -> Result<App, Diagnostic> {
        App::open(project, AppConfig::load(project)?)
    }

    /// The project directory.
    pub fn project(&self) -> &Path {
        &self.project
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

    /// The physics world.
    pub fn physics(&self) -> &Physics {
        &self.physics
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
        &self.audio_cmds
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
            physics_cmds: &mut self.physics_cmds,
            audio_cmds: &mut self.audio_cmds,
            pending: &mut self.pending,
        }
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

    /// Writes the World's authored entities back to the entry scene file.
    ///
    /// # Errors
    ///
    /// `RE0518` when the file cannot be written.
    pub fn save_scene(&mut self) -> Result<(), Diagnostic> {
        let doc = self.world.save(SaveOptions::default());
        let text = re_scene::scene::to_json(&doc);
        let rel_path = self.config.project.entry_scene.clone();
        let path = self.project.join(&rel_path);
        std::fs::write(&path, text).map_err(|e| {
            Diagnostic::error(
                codes::SCENE_SAVE_FAILED,
                format!("cannot write the scene back to {rel_path}: {e}"),
            )
            .at(rel(&rel_path))
        })
    }

    /// The work an edit needs outside the simulation: regenerate procedural
    /// sources whose inputs changed, then propagate transforms.
    pub fn edit_frame(&mut self) {
        let report = self.world.regenerate_changed();
        self.diagnostics.extend(report.diagnostics.iter().cloned());
        self.world.propagate();
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
        let frames = frames.or(replay.as_ref().map(|r| r.frames)).unwrap_or(60);
        if let Some(doc) = replay {
            self.play_replay(doc)?;
        }
        let dt = self.clock.dt();
        let start = self.frame;
        for _ in 0..frames {
            self.frame(dt);
        }
        let mut diagnostics = self.take_diagnostics();
        diagnostics.retain(|d| d.severity != re_core::Severity::Note || d.is_error());
        Ok(RunReport {
            frames: self.frame - start,
            state_hash: self.world.state_hash().to_hex().to_string(),
            diagnostics,
            perf: self.perf_report(),
            screenshots: std::mem::take(&mut self.screenshots),
        })
    }

    /// Runs one frame of play (ADR-0009): Input, PreRender, the fixed steps
    /// (PreAnimation, PreSimulation, Physics, PostSimulation), Sync, Render and
    /// EndFrame. `frame_time` is the wall time since the last frame (a headless
    /// run passes one step).
    pub fn frame(&mut self, frame_time: Duration) {
        let frame_start = Instant::now();
        let f = self.frame;

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
            }
            None => self.actions = self.action_map.frame(&self.raw, f),
        }
        self.raw.end_frame();
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

        // PreRender: variable-step script callbacks run here once scripting is
        // integrated.

        // Fixed steps.
        let steps = self.clock.advance(frame_time);
        self.alpha = steps.alpha;
        self.contacts.clear();
        for _ in 0..steps.count {
            self.fixed_step();
        }

        // Sync.
        let t = Instant::now();
        self.world.propagate();
        let inputs = RenderInputs {
            assets: &self.render_assets,
            physics: &self.physics_sync,
            alpha: self.alpha,
            clear: self.config.render.clear,
        };
        self.snapshot = render::build(&mut self.world, &inputs);
        audio::build(&mut self.world, &mut self.audio_sync, &mut self.audio_cmds);
        self.perf.add(Phase::Sync, t.elapsed());

        // Render: drawing is done by the windowed or headless driver that owns
        // the renderer, from `snapshot()`.

        // EndFrame.
        let t = Instant::now();
        self.end_frame(f);
        self.perf.add(Phase::EndFrame, t.elapsed());

        self.frame += 1;
        self.perf.end_frame(frame_start.elapsed());
    }

    fn fixed_step(&mut self) {
        // PreAnimation and PreSimulation: animation sampling and fixed-update
        // script callbacks run here once they are integrated.

        // Physics.
        let t = Instant::now();
        self.physics_sync.begin_step();
        physics::push(
            &mut self.world,
            &mut self.physics_sync,
            &self.shapes,
            &mut self.physics_cmds,
            &mut self.diagnostics,
        );
        self.physics.apply(&mut self.physics_cmds);
        let step = Instant::now();
        if let Err(d) = self.physics.step(self.clock.dt_secs()) {
            self.diagnostics.push(d);
        }
        self.perf.physics_step(step.elapsed());
        self.physics.poses_into(&mut self.poses);
        self.contacts.extend_from_slice(self.physics.events());
        self.diagnostics.extend(self.physics.take_diagnostics());
        self.perf.add(Phase::Physics, t.elapsed());

        // PostSimulation.
        let t = Instant::now();
        physics::pull(&mut self.world, &mut self.physics_sync, &self.poses);
        self.perf.add(Phase::PostSimulation, t.elapsed());
    }

    fn end_frame(&mut self, f: u64) {
        let pending = std::mem::take(&mut self.pending);
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
        let report = self.world.regenerate_changed();
        self.diagnostics.extend(report.diagnostics.iter().cloned());
        for path in pending.screenshots {
            self.screenshots.push(ScreenshotRequest {
                frame: f,
                golden: None,
                path: Some(path),
            });
        }
        if pending.restart {
            self.restart();
        }
    }

    /// Reloads the entry scene into a fresh World (what `world.restart()` asks
    /// for): physics, audio and the undo history start over; the frame counter
    /// and the replay keep going.
    pub fn restart(&mut self) {
        match load_world(&self.project, &self.config) {
            Ok(world) => {
                self.world = world;
                self.history = History::default();
                match Physics::new(physics_config(&self.config), TaskPool::global()) {
                    Ok(p) => self.physics = p,
                    Err(d) => self.diagnostics.push(d),
                }
                self.physics_sync = PhysicsSync::new();
                self.physics_cmds.clear();
                self.audio_cmds.push(AudioCmd::StopAll);
                self.audio_sync = AudioSync::new();
                self.clock.reset();
                self.edit_frame();
            }
            Err(d) => self.diagnostics.push(d),
        }
    }
}

/// A fresh World with the entry scene loaded and its procedural sources run.
fn load_world(project: &Path, config: &AppConfig) -> Result<World, Diagnostic> {
    let rel_path = &config.project.entry_scene;
    let path = project.join(rel_path);
    let fail = |msg: String| Diagnostic::error(codes::ENTRY_SCENE_FAILED, msg).at(rel(rel_path));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| fail(format!("cannot read the entry scene {rel_path}: {e}")))?;
    let doc = re_scene::scene::from_json(&text).map_err(|e| {
        fail(format!("the entry scene {rel_path} does not parse")).with_related(e.to_diagnostic())
    })?;
    let mut world = World::new();
    world.load(&doc).map_err(|e| {
        fail(format!("the entry scene {rel_path} does not load")).with_related(e.to_diagnostic())
    })?;
    world.propagate_all();
    Ok(world)
}
