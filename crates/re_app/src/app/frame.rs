//! The frame loop of ADR-0009: [`App::frame`] and its phases, and headless
//! runs driven by replays.

use std::time::Duration;

use re_command::Origin;
use re_core::time::Instant;
use re_core::{Diagnostic, Location, Phase};
use re_input::Replay;
use re_reflect::Value;
use re_scene::{HudText, entity_from_id};
use re_types::{PhysicsCmd, ReplayDoc};

use re_script::ContextKind;

use crate::codes;
use crate::hud;
use crate::sync::anim;
use crate::sync::anim_skin;
use crate::sync::audio;
use crate::sync::physics;
use crate::sync::ragdoll;
use crate::sync::render::{self, RenderInputs};

use super::{App, RunReport, ScreenshotRequest, script_levels};

/// A frame [`App::try_frame`] began: when, which, and what its second half
/// needs.
pub(super) struct HalfFrame {
    start: Instant,
    f: u64,
    frame_time: Duration,
    steps: u32,
}

impl App {
    /// Plays `replay` from the next frame: its actions replace live input, its
    /// commands are dispatched at their frames and its checks are recorded as
    /// screenshot requests.
    ///
    /// # Errors
    ///
    /// `RE0507` when the replay does not match the project's actions (with the
    /// input crate's diagnostics as `related`).
    pub fn play_replay(&mut self, doc: ReplayDoc) -> Result<(), Diagnostic> {
        let seed = doc.seed;
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
        // The run starts from the replay's seed: scripts that already run
        // start their `math.random` again from it.
        self.seed = seed;
        if let Some(scripts) = &mut self.scripts
            && let Err(e) = scripts.reseed(seed)
        {
            self.diagnostics.push(e.to_diagnostic());
        }
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
            if self.stopped {
                break;
            }
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
    /// run passes one step). A network client waits in it for its server
    /// (ADR-0050 §7); [`App::try_frame`] does not.
    pub fn frame(&mut self, frame_time: Duration) {
        if let Some(half) = self.half.take() {
            // A frame `try_frame` began finishes first.
            self.finish_frame(half);
        }
        let half = self.begin_frame(frame_time);
        self.finish_frame(half);
    }

    /// [`App::frame`] for a page, whose frames cannot wait: runs the frame
    /// unless a network client would wait for its server. Before its first
    /// frame it waits for the server's welcome; in lockstep, the client half
    /// of each frame waits for the server's frame. Returns false while it
    /// waits, having run nothing or only the frame's first half (`frame_time`
    /// is then the frame's); call it again from the page's event loop, which
    /// delivers the server's messages in between.
    pub fn try_frame(&mut self, frame_time: Duration) -> bool {
        if self.half.is_none() {
            if !self.net_ready() {
                return false;
            }
            self.half = Some(self.begin_frame(frame_time));
        }
        if !self.net_ready() {
            return false;
        }
        let half = self.half.take().expect("begun above");
        self.finish_frame(half);
        true
    }

    /// A frame up to its fixed steps: Input, PreRender (where a compat game
    /// starts, and a network client joins) and the fixed steps.
    fn begin_frame(&mut self, frame_time: Duration) -> HalfFrame {
        let frame_start = Instant::now();
        let f = self.frame;
        self.service_debugger();
        self.input_phase(f);
        self.pre_render_phase(frame_time);

        // Fixed steps: as many as the frame time, scaled by `time.set_scale`,
        // holds.
        let sim_time = if self.time_scale == 1.0 {
            frame_time
        } else {
            frame_time.mul_f64(self.time_scale)
        };
        let steps = self.clock.advance(sim_time);
        self.alpha = steps.alpha;
        self.contacts.clear();
        for _ in 0..steps.count {
            self.fixed_step();
        }
        HalfFrame {
            start: frame_start,
            f,
            frame_time,
            steps: steps.count,
        }
    }

    /// The rest of a frame [`App::begin_frame`] began: a compat game's client
    /// half (which a lockstep network client waits in), Sync, Render and
    /// EndFrame.
    fn finish_frame(&mut self, half: HalfFrame) {
        let HalfFrame {
            start: frame_start,
            f,
            frame_time,
            steps,
        } = half;
        if self.compat.is_some() {
            self.compat_client_frame(frame_time.as_secs_f64(), steps);
        }
        self.sync_phase(f);
        self.render_phase(frame_time);

        // EndFrame.
        let t = Instant::now();
        self.end_frame(f);
        self.perf.add(Phase::EndFrame, t.elapsed());

        self.frame += 1;
        self.perf.end_frame(frame_start.elapsed());
    }

    /// One fixed step of the simulation: PreAnimation, PreSimulation, Physics
    /// and PostSimulation.
    fn fixed_step(&mut self) {
        self.pre_animation_phase();
        self.pre_simulation_phase();
        self.physics_phase();
        self.post_simulation_phase();
    }

    /// Input: the frame's actions and raw input, from the replay (which also
    /// asks for its screenshots) or the devices, then changed files and the
    /// replay's commands.
    fn input_phase(&mut self, f: u64) {
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
    }

    /// PreRender: variable-step script callbacks (in a compat project, the
    /// server context's, after its remotes are delivered).
    fn pre_render_phase(&mut self, frame_time: Duration) {
        let t = Instant::now();
        self.ensure_compat();
        if self.compat.is_some() {
            let dt = frame_time.as_secs_f64();
            self.net_receive_server();
            self.compat_call(ContextKind::Server, |vm, h| {
                let mut d = re_compat::net::pump(vm, h);
                d.extend(vm.pre_render(h, dt));
                d
            });
        } else {
            self.run_scripts(Phase::PreRender, frame_time.as_secs_f64());
        }
        self.perf.add(Phase::PreRender, t.elapsed());
    }

    /// PreAnimation: animation players and skins, root motion as character
    /// moves, and in a compat project its scripts' `PreAnimation`.
    fn pre_animation_phase(&mut self) {
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
    }

    /// PreSimulation: the streaming step, new characters linked, then
    /// fixed-update script callbacks; what they queue reaches this step's
    /// physics.
    fn pre_simulation_phase(&mut self) {
        let t = Instant::now();
        self.stream_step();
        self.link_new_characters();
        self.run_scripts(Phase::PreSimulation, self.clock.dt_secs().into());
        self.perf.add(Phase::PreSimulation, t.elapsed());
    }

    /// PostSimulation: poses back into the World, then contact callbacks
    /// and post-physics script updates.
    fn post_simulation_phase(&mut self) {
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

    /// Sync: assets uploaded, and the World propagated and read into the
    /// render snapshot, the HUD and the audio commands. A network client sends
    /// its frame here.
    fn sync_phase(&mut self, f: u64) {
        let t = Instant::now();
        self.world.propagate();
        if self.graphics.is_some()
            && (!self.pending.screenshots.is_empty()
                || self.screenshots.iter().any(|r| r.frame == f))
        {
            self.complete_terrain_view();
        }
        self.upload_assets();
        let inputs = RenderInputs {
            assets: &self.render_assets,
            physics: &self.physics_sync,
            skins: &self.skin_sync,
            alpha: self.alpha,
            clear: self.config.render.clear,
            post: self.config.render.post,
            shadows: self.config.render.shadows,
            origin: self.physics.origin(),
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
        // A network client sends its frame once its world is propagated, as
        // the in-process pass reads the client world after the Sync phase:
        // the compat simulation moves welded parts without change detection,
        // and the propagation is what marks them moved.
        self.net_send_client();
        if self.compat.is_some() && self.graphics.is_some() {
            self.compat_render(self.view_size, self.view_ppp, 1, true);
        }
        audio::build(&mut self.world, &mut self.audio_sync, &mut self.audio_cmds);
        self.perf.add(Phase::Sync, t.elapsed());
    }

    /// Render: the windowed driver draws `snapshot()` and the HUD after
    /// `frame` returns; here the audio commands reach the mixer. Without a
    /// device nothing else renders the mixer, so the frame's audio renders
    /// here, and sounds end and free their voices as on a device; a
    /// dedicated server has no one to hear it.
    fn render_phase(&mut self, frame_time: Duration) {
        let t = Instant::now();
        self.diagnostics.extend(self.audio.apply(&self.audio_cmds));
        if !self.is_dedicated_server() {
            self.audio.advance(frame_time);
        }
        self.last_audio_cmds = std::mem::take(&mut self.audio_cmds);
        self.bus_levels = self.audio.bus_levels();
        self.script_levels = script_levels(&self.audio, &self.bus_levels);
        if let Some(gfx) = &self.graphics {
            self.diagnostics.extend(gfx.take_errors());
        }
        self.perf.add(Phase::Render, t.elapsed());
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
}
