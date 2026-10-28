//! A compat game (`[compat]` in `real.toml`): its server and client
//! contexts, started on the first frame, and the calls into them.

use re_core::Diagnostic;
use re_scene::World;

use re_script::ContextKind;

use crate::host::{Host, PlayHost};

use super::App;

impl App {
    /// Makes a compat game's `DataStoreService` and `MemoryStoreService`
    /// keep their data in the run's data folder (`.real/data/` on desktop,
    /// IndexedDB in a browser) with the wall clock, as `real run` does
    /// (ADR-0048). Without it, each run starts from
    /// `data/seed/` on a fixed clock so it repeats. Call it before the first
    /// frame.
    pub fn persist_data(&mut self) {
        self.persist_data = true;
    }

    /// Starts the compat contexts on the first frame of a compat project:
    /// splits the World, starts both sides' scripts and joins the
    /// `[compat] players`. A failure is reported once; the game then runs
    /// without scripts.
    pub(super) fn ensure_compat(&mut self) {
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
        let role = self
            .net
            .as_ref()
            .map_or(crate::compat::Role::InProcess, |n| n.role());
        match crate::compat::CompatPlay::new(
            &self.files,
            &self.config,
            &cfg,
            &mut self.world,
            clips,
            self.persist_data,
            role,
        ) {
            Ok(play) => self.compat = Some(play),
            Err(d) => {
                self.compat_failed = true;
                if self.net.is_some() {
                    // An online run with no game stops rather than serving
                    // (or playing) nothing.
                    self.net_failed(d);
                } else {
                    self.diagnostics.push(d);
                }
                return;
            }
        }
        // Attached before the scripts start, so their first lines can stop.
        self.attach_compat_debugger();
        if self.net.is_some() {
            self.start_online(&cfg);
            return;
        }
        for kind in [ContextKind::Server, ContextKind::Client] {
            self.compat_call(kind, |vm, h| crate::compat::start_scripts(vm, h));
        }
        for _ in 0..cfg.players {
            if let Err(d) = self.join_player() {
                self.diagnostics.push(d);
            }
        }
    }

    /// Calls `f` with a compat context's VM and a host over its world, then
    /// keeps and reports the errors and collects the output.
    pub(super) fn compat_call(
        &mut self,
        kind: ContextKind,
        f: impl FnOnce(&re_compat::CompatVm, &mut PlayHost<'_>) -> Vec<Diagnostic>,
    ) {
        let Some(c) = self.compat.as_mut() else {
            return;
        };
        let (world, vm) = match kind {
            ContextKind::Client => (&mut c.client, c.client_vm.as_ref()),
            _ => (&mut self.world, c.server_vm.as_ref()),
        };
        let Some(vm) = vm else {
            return;
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
                bus_levels: &self.script_levels,
                audio_graph: self.audio.graph(),
                pending: &mut self.pending,
                skins: &mut self.skin_sync,
                time_scale: &mut self.time_scale,
                drawn: &self.drawn,
            },
        };
        let diags = f(vm, &mut host);
        let diags = c.keep(kind, diags);
        let output = c.take_output();
        self.report_script_errors(diags);
        self.script_output.extend(output);
    }

    /// The client half of a compat frame, after the server's fixed steps:
    /// replication, the client's deliveries and phases (one round of fixed
    /// phases per server step, with character and part physics, tweens and
    /// sounds),
    /// then the sound commands for the mixer.
    pub(super) fn compat_client_frame(&mut self, frame_dt: f64, steps: u32) {
        let role = self.net.as_ref().map(|n| n.role());
        if let Some(c) = &mut self.compat
            && c.server_vm.is_some()
            && c.client_vm.is_some()
        {
            c.replicate(&mut self.world);
        }
        if role.is_some() {
            // The simulated time this frame ran, for the server's limits.
            let dt = f64::from(steps.max(1)) * f64::from(self.clock.dt_secs());
            self.net_replicate(dt);
            self.net_receive_client();
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
            .and_then(|c| c.client_vm.as_ref())
            .map(|vm| crate::compat::client_sounds(vm, None))
            .unwrap_or_default();
        self.audio_cmds.extend(sounds);
    }

    /// Restarts a running compat game's changed scripts, and the scripts
    /// that required a changed module (ADR-0048 §3), in both contexts. Only
    /// while files are watched ([`App::enable_hot_reload`], so `real run` and
    /// the editor): `real verify`, replays and `play.run` read files once.
    /// A changed source that does not compile keeps the old run (`RE9420`).
    pub(super) fn reload_compat(&mut self, paths: &[String]) {
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

    /// A compat game's client world (the local player's, which is drawn),
    /// once the game started; `None` on a dedicated server.
    pub fn compat_client(&self) -> Option<&World> {
        self.compat
            .as_ref()
            .filter(|c| c.client_vm.is_some())
            .map(|c| &c.client)
    }

    /// True for a compat game with no client side (`real run --server`).
    pub(super) fn is_dedicated_server(&self) -> bool {
        self.compat.is_some() && self.compat_client().is_none()
    }

    /// Joins the next compat player (`Player<n>`); returns its `Player`.
    pub(super) fn join_player(&mut self) -> Result<re_scene::Entity, Diagnostic> {
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
}
