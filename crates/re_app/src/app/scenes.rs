//! Scenes in the frame loop (ADR-0059): the scene changes scripts ask for,
//! applied at EndFrame; `world.restart()`, the full-reset path back to the
//! scene the run began in; and the scene that edit mode has open.

use re_command::History;
use re_core::{Diagnostic, TaskPool};
use re_input::Replay;
use re_physics::Physics;
use re_proc::stream::Lateness;
use re_scene::World;
use re_types::AudioCmd;

use crate::procedural;
use crate::scenes::{self, Scenes};
use crate::sync::anim::AnimSync;
use crate::sync::anim_skin::SkinSync;
use crate::sync::audio::AudioSync;
use crate::sync::physics::PhysicsSync;
use crate::sync::ragdoll::RagdollSync;

use super::App;
use super::open::physics_config;

/// The World each restart forks in place of a fresh load
/// ([`App::set_start_world`]), with the streaming tick and the physics
/// origin the app had when it was set.
pub(super) struct StartWorld {
    world: World,
    stream_tick: u64,
    origin: glam::Vec3,
}

impl App {
    /// The run's scenes (ADR-0059): the list, the main scene and the added
    /// ones, the kept entities, and the log of the changes applied.
    pub fn scenes(&self) -> &Scenes {
        &self.scenes
    }

    /// The project path of the scene open for editing, which
    /// [`App::save_scene`] writes to (ADR-0059 §9): the main scene, which is
    /// the entry scene until another is opened ([`App::open_scene`]) or
    /// switched to.
    pub fn scene_path(&self) -> &str {
        self.scenes.main_path()
    }

    /// Makes each `world.restart()` begin from `world` instead of loading
    /// the scene the run began in from disk: an editor play session's start
    /// (ADR-0059 §5), which the session makes from its play world before the
    /// first step with [`crate::scenes::fork_world`]. The app keeps it, and
    /// each restart plays a fork of it, at the streaming tick and physics
    /// origin the app has now.
    pub fn set_start_world(&mut self, world: World) {
        self.start_world = Some(StartWorld {
            world,
            stream_tick: self.stream_tick,
            origin: self.physics.origin(),
        });
    }

    /// Opens the scene at project path `path` for editing (ADR-0059 §9): a
    /// fresh World with the scene loaded, as when the project opens, with
    /// everything else reset as by `world.restart()`, and the open documents
    /// (project files, not scene content) carried over. The undo history is
    /// the caller's: it is left as it is. The scene becomes the one the run
    /// began in, which [`App::scene_path`] names and a restart goes back to.
    /// Returns the problems making its procedural sources' graphs.
    ///
    /// # Errors
    ///
    /// When the scene cannot be read, parsed or loaded (`RE0502` for the
    /// entry scene, else `RE0570`); nothing changes then.
    pub fn open_scene(&mut self, path: &str) -> Result<Vec<Diagnostic>, Diagnostic> {
        let (mut world, problems) = scenes::fresh_world(&self.files, &self.config, path)?;
        if let Some(docs) = self.world.ecs.remove_resource::<re_scene::OpenDocs>() {
            world.ecs.insert_resource(docs);
        }
        self.start_world = None;
        self.scenes.begin_at(path);
        self.reset_to(world, Vec::new());
        self.edit_frame();
        Ok(problems)
    }

    /// Reloads the scene the run began in into a fresh World (what
    /// `world.restart()` asks for, ADR-0059 §5), or forks the start world
    /// ([`App::set_start_world`]): physics, audio, the scripts' VMs and the
    /// undo history start over, no scene stays added and no entity kept, and
    /// the frame counter and the replay keep going. Called between frames,
    /// it is logged as made at the end of the last frame.
    pub fn restart(&mut self) {
        self.restart_at(self.frame.saturating_sub(1));
    }

    /// [`App::restart`] at the end of frame `frame`.
    pub(super) fn restart_at(&mut self, frame: u64) {
        let fresh = match &self.start_world {
            Some(start) => Ok((scenes::fork_world(&start.world), Vec::new())),
            None => scenes::fresh_world(&self.files, &self.config, self.scenes.start_path()),
        };
        let (world, problems) = match fresh {
            Ok(fresh) => fresh,
            Err(d) => {
                self.diagnostics.push(d);
                return;
            }
        };
        self.reset_to(world, problems);
        self.restarts += 1;
        self.history = History::default();
        if let Some(start) = &self.start_world {
            self.stream_tick = start.stream_tick;
            if let Err(d) = self.physics.set_origin(start.origin) {
                self.diagnostics.push(d);
            }
        }
        let problems = self.scenes.restarted(frame);
        self.diagnostics.extend(problems);
        self.edit_frame();
    }

    /// Puts `world` in place of the World with everything that follows it
    /// started over: physics and its sync, audio, animation, a compat game's
    /// contexts and any network session, the scripts' VMs, the fixed clock,
    /// the time scale and the streaming tick.
    fn reset_to(&mut self, world: World, problems: Vec<Diagnostic>) {
        self.world = world;
        if let Err(d) = scenes::ensure_streamer(&mut self.world, &self.config.stream) {
            self.diagnostics.push(d);
        }
        self.diagnostics.extend(problems);
        self.ground = None;
        self.link_tried.clear();
        self.graph_inputs = procedural::inputs(&mut self.world);
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
            None => self.compat_view = crate::compat_view::CompatView::new(&self.config.render),
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
        // The fresh World makes the first run's ids again.
        self.ids_seeded = false;
        self.stream_tick = 0;
    }

    /// EndFrame's scene work, after the frame's screenshots: the restart a
    /// script asked for (`restart`), or else the scene changes that are
    /// ready (ADR-0059 §4); then the script threads waiting on loads that
    /// ended are woken, and at the end of a replay the rest of its scene
    /// changes are checked.
    pub(super) fn scenes_end_frame(&mut self, f: u64, restart: bool) {
        if restart {
            self.restart_at(f);
        } else {
            // Assets warm only for a run that draws and does not wait.
            let warm = (self.stream_lateness == Lateness::Late && self.graphics.is_some())
                .then_some((&mut self.media, &mut self.audio));
            let applied = self.scenes.end_frame(&mut scenes::EndFrame {
                world: &mut self.world,
                files: &self.files,
                stream: &self.config.stream,
                frame: f,
                lateness: self.stream_lateness,
                warm,
            });
            self.diagnostics.extend(applied.diagnostics);
            if applied.changed {
                // What the change loaded generates now, as play does
                // (ADR-0017), and the derived assets that only what it took
                // away drew with go.
                self.regenerate_changed();
                let unused = scenes::unreferenced(&mut self.world, &applied.released);
                procedural::release(&mut self.world, &mut self.media, &mut self.shapes, &unused);
                // A scene stands where it was authored: the next step takes
                // the ground as it finds it, and puts no character on it.
                self.ground = None;
                if applied.switched {
                    self.history = History::default();
                }
            }
        }
        let ended = self.scenes.take_ended();
        if let Some(scripts) = &self.scripts
            && let Err(e) = re_script::wake_scene_loads(scripts, &ended)
        {
            self.diagnostics.push(e.to_diagnostic());
        }
        if self.replay.as_ref().is_some_and(Replay::is_finished)
            && let Some(d) = self.scenes.replay_ended()
        {
            self.diagnostics.push(d);
        }
    }
}
