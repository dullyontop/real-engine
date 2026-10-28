//! A native game's script runtime, its debugger (ADR-0045), and the errors
//! and output its scripts report.

use re_core::{Diagnostic, Phase};

use re_script::{ContextKind, RuntimeConfig, ScriptError, ScriptRuntime};

use crate::host::{Host, PlayHost};

use super::App;

impl App {
    /// The script runtime, created on first use (so editing never starts a VM).
    fn script_runtime(&mut self) -> Option<&mut ScriptRuntime> {
        if self.scripts.is_none() {
            if self.scripts_failed {
                return None;
            }
            let config = RuntimeConfig::with_files(self.files.clone())
                .with_limits(
                    self.config.scripts.cpu_budget_ms.max(0.0).ceil() as u64,
                    self.config.scripts.memory_limit_mb as usize,
                )
                .with_seed(self.seed);
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
    /// and again whenever a VM is replaced; `None` detaches them. Scripts
    /// already running reach their breakpoints, and a script thread stopped
    /// at one is parked while the game runs on. Breakpoint changes and
    /// commands for a parked script are carried out at the start of each
    /// frame.
    pub fn set_debugger(&mut self, debugger: Option<re_script::debug::Debugger>) {
        if let (Some(old), Some(scripts)) = (&self.debugger, &self.scripts) {
            old.detach(scripts.context(ContextKind::Game).lua());
        }
        if let (Some(old), Some(c)) = (&self.debugger, &self.compat) {
            for vm in [ContextKind::Server, ContextKind::Client]
                .into_iter()
                .filter_map(|kind| c.vm(kind))
            {
                old.detach(vm.context().lua());
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

    /// Serves an attached debugger between frames, in each VM it can be
    /// attached to: the game's, or a compat game's server and client.
    pub(super) fn service_debugger(&self) {
        if self.debugger.is_some()
            && let Some(scripts) = &self.scripts
        {
            re_script::debug::service(scripts.context(ContextKind::Game).lua());
        }
        if self.debugger.is_some()
            && let Some(c) = &self.compat
        {
            // An online server has no client VM, and a client no server VM.
            for vm in [ContextKind::Server, ContextKind::Client]
                .into_iter()
                .filter_map(|kind| c.vm(kind))
            {
                re_script::debug::service(vm.context().lua());
            }
        }
    }

    pub(super) fn attach_debugger(&mut self) {
        let (Some(debugger), Some(scripts)) = (&self.debugger, &self.scripts) else {
            return;
        };
        // The scheduler parks a stopped script thread, so the game keeps
        // running, and the scripts already running reach their breakpoints.
        if let Err(e) = scripts.context(ContextKind::Game).attach_debugger(debugger) {
            self.diagnostics
                .push(ScriptError::from_mlua(ContextKind::Game.name(), &e).to_diagnostic());
        }
    }

    /// Attaches the compat server and client VMs to the debugger, with the
    /// scripts they already run.
    pub(super) fn attach_compat_debugger(&mut self) {
        let (Some(debugger), Some(c)) = (&self.debugger, &self.compat) else {
            return;
        };
        for kind in [ContextKind::Server, ContextKind::Client] {
            let Some(vm) = c.vm(kind) else {
                continue;
            };
            if let Err(e) = vm.attach_debugger(debugger) {
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
                bus_levels: &self.script_levels,
                audio_graph: self.audio.graph(),
                pending: &mut self.pending,
                skins: &mut self.skin_sync,
                time_scale: &mut self.time_scale,
            },
            game: Some(crate::scenes::Game {
                scenes: &mut self.scenes,
                saves: &mut self.saves,
            }),
        };
        let out = f(scripts, &mut host);
        self.collect_script_output();
        Some(out)
    }

    pub(super) fn run_scripts(&mut self, phase: Phase, dt: f64) {
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

    /// Reports script diagnostics, one per code and place until the scripts
    /// restart or reload (a failing callback fails every step, possibly with a
    /// different message each time); repeats only go to the log.
    pub(super) fn report_script_errors(&mut self, diags: Vec<Diagnostic>) {
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
}
