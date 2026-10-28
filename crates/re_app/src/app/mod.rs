//! [`App`]: a project opened for running and editing, and the frame loop of
//! ADR-0009. The struct and its accessors are here; its other methods are
//! grouped by concern:
//!
//! - `open`: opening a project, and the copy `play.run` plays;
//! - `scenes`: scene changes at EndFrame, `world.restart()`, and the scene
//!   edit mode has open;
//! - `frame`: the frame loop, one method per phase, and headless runs;
//! - `physics_step`: the Physics phase and what it needs;
//! - `stream`: world streaming in the frame loop;
//! - `scripts`: a native game's scripts and the debugger;
//! - `compat_play`: a compat game's server and client contexts;
//! - `net`: online play;
//! - `edit`: edit mode, commands and saving;
//! - `files`: hot reload, and open documents changed on disk;
//! - `view`: graphics, screenshots and drawing into a window;
//! - `play_steps`: the steps of a `play.run`.

mod compat_play;
mod edit;
mod files;
mod frame;
mod net;
mod open;
mod physics_step;
mod play_steps;
mod scenes;
mod scripts;
mod stream;
mod view;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use re_command::{CommandRegistry, EditTarget, History};
use re_core::files::{ProjectFiles, Writable};
use re_core::{Diagnostic, FixedClock, Location};
use re_input::{ActionMap, RawInput, Replay};
use re_physics::Physics;
use re_scene::World;
use re_types::{
    ActionFrame, AudioCmd, BodyPose, BusLevel, ContactEvent, PhysicsCmd, RenderSnapshot,
};
use serde::Serialize;

use re_assets::AssetWatcher;
use re_audio::Audio;
use re_render::Image;
use re_script::ScriptRuntime;

use crate::codes;
use crate::config::AppConfig;
use crate::gfx::Graphics;
use crate::host::{Host, Pending};
use crate::hud::HudLine;
use crate::media::Media;
use crate::perf::{PerfCounts, PerfRecorder, PerfReport};
use crate::procedural;
use crate::sync::anim::AnimSync;
use crate::sync::anim_skin::SkinSync;
use crate::sync::audio::AudioSync;
use crate::sync::physics::{PhysicsSync, ShapeLibrary};
use crate::sync::ragdoll::RagdollSync;
use crate::sync::render::RenderAssets;
use crate::verify::VerifyStage;

use self::frame::HalfFrame;

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
    /// The scene changes the run made, as a replay's `scenes` list holds them
    /// (ADR-0059 §6).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scenes: Vec<re_types::SceneChange>,
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
    /// The floating origin physics last refused, reported once.
    origin_refused: Option<glam::Vec3>,
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
    /// The run's seed: `[scripts] seed`, or the seed of the replay being
    /// played. Native scripts' `math.random` starts from it, and so do the
    /// ids the World makes during play.
    seed: i64,
    /// Whether the World's id generator has the session stream of `seed`
    /// (ADR-0036 amendment): unset when the App is made, and cleared by
    /// `play_replay` and when a fresh World takes the old one's place
    /// (`restart`, `open_scene`), so the next played frame seeds it.
    ids_seeded: bool,
    /// How fast simulation time runs against frame time (`time.set_scale`):
    /// the frame time the clock advances by is multiplied by it.
    time_scale: f64,

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
    /// The project's and the engine directory's shader files (ADR-0038).
    shaders: crate::shaders::ShaderFiles,
    /// Watches the engine shader directory ([`App::watch_engine_shaders`]).
    shader_watcher: Option<AssetWatcher>,
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
    /// The run's scenes (ADR-0059): the list, the loaded and kept ones, the
    /// loads not applied yet, and the log of the changes applied.
    scenes: crate::scenes::Scenes,
    /// What a restart forks instead of loading the scene the run began in
    /// ([`App::set_start_world`]).
    start_world: Option<self::scenes::StartWorld>,
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
    /// Online play ([`App::set_net`]).
    net: Option<crate::net::NetPlay>,
    /// The run should end: a network client was kicked or lost its server.
    stopped: bool,
    /// A frame [`App::try_frame`] began and has not finished.
    half: Option<HalfFrame>,
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
    /// Every bus's level over the last frame (ADR-0041).
    bus_levels: Vec<BusLevel>,
    /// What scripts read for `bus_levels` (see [`script_levels`]).
    script_levels: Vec<BusLevel>,

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

fn rel(path: &str) -> Location {
    Location::file(path.replace('\\', "/"))
}

impl App {
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
    /// any bound animation graph (playing or paused) also its parameters,
    /// states and times (ADR-0039). A game without ragdolls or graphs hashes as
    /// its world.
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

    /// The render origin the [`snapshot`](Self::snapshot) is relative to:
    /// world streaming's floating origin (ADR-0042), zero without one.
    pub fn render_origin(&self) -> glam::Vec3 {
        self.physics.origin()
    }

    /// The render snapshot built by the last frame's Sync phase, relative to
    /// the [`render_origin`](Self::render_origin).
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
        self.script_levels = script_levels(&audio, &self.bus_levels);
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

    /// Sets how world streaming treats a cell whose generation is not done
    /// at its apply tick (ADR-0042). [`Lateness::Wait`] (the default) waits
    /// for it, so `real verify`, replays and `play.run` load the same cells at
    /// the same ticks on every machine; a frame that takes a screenshot also
    /// waits for the whole streamed terrain view, so the image is the same
    /// too. Only a live run, which must not stall a frame, sets
    /// [`Lateness::Late`]: the cell applies when it is ready, reported as
    /// `RE4305`.
    ///
    /// [`Lateness::Wait`]: re_proc::stream::Lateness::Wait
    /// [`Lateness::Late`]: re_proc::stream::Lateness::Late
    pub fn set_stream_lateness(&mut self, lateness: re_proc::stream::Lateness) {
        self.stream_lateness = lateness;
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
            // The game's entities, which all have stable ids: `Entities::len`
            // counts every index ever allocated, so it only grows, and bevy
            // keeps entities of its own (resources, observers).
            entities: self.world.ids().len() as u64,
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
            bus_levels: &self.script_levels,
            audio_graph: self.audio.graph(),
            pending: &mut self.pending,
            skins: &mut self.skin_sync,
            time_scale: &mut self.time_scale,
        }
    }

    /// The cursor settings scripts asked for since the last call
    /// (`UserInputService.MouseBehavior` and `MouseIconEnabled`); the window
    /// applies them.
    pub fn take_mouse_behavior(&mut self) -> Option<(re_types::input::MouseLock, bool)> {
        self.mouse_behavior.take()
    }
}

/// The bus levels scripts read (`audio.levels`): `levels` on a device, and
/// silence on the headless backend, as before it rendered. What a script writes
/// from `on_frame` is in the state hash, and kira's effects use the platform's
/// maths, so a replay that read a rendered mix could hash differently on
/// another platform.
fn script_levels(audio: &Audio, levels: &[BusLevel]) -> Vec<BusLevel> {
    if !audio.is_headless() {
        return levels.to_vec();
    }
    let silence = re_audio::linear_to_decibels(0.0).0;
    levels
        .iter()
        .map(|l| BusLevel {
            bus: l.bus.clone(),
            peak_db: silence,
            rms_db: silence,
        })
        .collect()
}
