//! [`ScriptRuntime`]: the `game` and `tools` contexts and the per-phase entry
//! point (ADR-0009, ADR-0011). **Frozen** at WP3's exit; `re_app` drives it.
//!
//! Every entry point takes the [`ScriptHost`] for the duration of the call and
//! returns the diagnostics of the errors raised while it ran (scripts that fail
//! are reported, never fatal to the runtime).
//!
//! What runs in each phase ([`ScriptRuntime::run_phase`]), in both contexts:
//!
//! | Phase | Work |
//! |---|---|
//! | `PreRender` | start newly attached `ScriptRef`s; frame counter and `time.delta`; `input.on_pressed`/`on_released`; `time.on_frame(dt)` |
//! | `PreSimulation` | start newly attached `ScriptRef`s; advance `time.now` by the fixed step; `time.on_step(dt)` |
//! | `PostSimulation` | contact callbacks (from [`ScriptRuntime::push_contacts`]); threads whose `task.wait` is due; `time.on_post_step(dt)` |
//! | others | nothing |
//!
//! Deferred threads (`task.defer`) run at the end of each phase's work.

use std::path::{Path, PathBuf};
use std::time::Duration;

use mlua::{Lua, MultiValue, Value};
use re_core::{Diagnostic, Location, Phase};
use re_scene::{Entity, ScriptRef, World};
use re_types::graph::GraphDoc;
use re_types::physics::ContactEvent;

use crate::api::{self, json};
use crate::diag::ScriptError;
use crate::host::{self, ScriptHost};
use crate::modules::{self, Loaded, Reload};
use crate::scheduler::{self, Owner, Signal};
use crate::vm::{self, ContextKind, OutputLine};
use crate::{ErrorKind, codes};

/// How the runtime's contexts are set up (`real.toml [scripts]`).
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeConfig {
    /// The project root; `require` and script paths resolve under its `scripts/`.
    pub project_root: PathBuf,
    /// The CPU time one resume of a script thread may use (`cpu_budget_ms`).
    pub cpu_budget: Duration,
    /// The memory each context's VM may allocate, in bytes (`memory_limit_mb`);
    /// 0 means unlimited.
    pub memory_limit: usize,
}

impl RuntimeConfig {
    /// The default `cpu_budget_ms`.
    pub const DEFAULT_CPU_BUDGET_MS: u64 = 4;
    /// The default `memory_limit_mb`.
    pub const DEFAULT_MEMORY_LIMIT_MB: usize = 256;

    /// The defaults for a project at `project_root`.
    pub fn new(project_root: impl Into<PathBuf>) -> RuntimeConfig {
        RuntimeConfig {
            project_root: project_root.into(),
            cpu_budget: Duration::from_millis(Self::DEFAULT_CPU_BUDGET_MS),
            memory_limit: Self::DEFAULT_MEMORY_LIMIT_MB * 1024 * 1024,
        }
    }

    /// The values of `real.toml [scripts]`.
    pub fn with_limits(mut self, cpu_budget_ms: u64, memory_limit_mb: usize) -> RuntimeConfig {
        self.cpu_budget = Duration::from_millis(cpu_budget_ms);
        self.memory_limit = memory_limit_mb * 1024 * 1024;
        self
    }
}

/// One context: a VM with its own globals, modules and scheduler.
pub struct Context {
    kind: ContextKind,
    lua: Lua,
    loaded: Loaded,
}

impl Context {
    fn new(config: &RuntimeConfig, kind: ContextKind) -> Result<Context, ScriptError> {
        let loaded = Loaded::default();
        let lua = vm::create(
            &config.project_root,
            kind,
            config.cpu_budget,
            config.memory_limit,
            loaded.clone(),
        )
        .map_err(|e| ScriptError::from_mlua(kind.name(), &e))?;
        Ok(Context { kind, lua, loaded })
    }

    /// Which context this is.
    pub fn kind(&self) -> ContextKind {
        self.kind
    }

    /// Bytes the VM currently uses.
    pub fn memory_used(&self) -> usize {
        self.lua.used_memory()
    }

    /// Threads suspended in `task.wait` or `task.delay`.
    pub fn sleeping_threads(&self) -> usize {
        scheduler::with(&self.lua, |s| s.sleeping()).unwrap_or(0)
    }

    /// Connected callbacks (`time.on_step`, `physics.on_touch`, ...).
    pub fn connections(&self) -> usize {
        scheduler::with(&self.lua, |s| s.connections()).unwrap_or(0)
    }

    /// Entity handles currently alive in the VM. Runs a full collection first, so
    /// handles no script holds are not counted.
    pub fn live_handles(&self) -> usize {
        let _ = self.lua.gc_collect();
        let _ = self.lua.gc_collect();
        api::entity::cached_handles(&self.lua).unwrap_or(0)
    }

    /// The modules loaded with `require`, as project paths.
    pub fn loaded_modules(&self) -> Vec<String> {
        self.loaded.borrow().iter().cloned().collect()
    }

    fn errors(&self) -> Vec<Diagnostic> {
        let errors = scheduler::with(&self.lua, |s| s.take_errors()).unwrap_or_default();
        errors.iter().map(report).collect()
    }
}

fn report(e: &ScriptError) -> Diagnostic {
    tracing::warn!(target: "re_script", "{e}");
    e.to_diagnostic()
}

fn infra(kind: ContextKind, err: &mlua::Error) -> Diagnostic {
    report(&ScriptError::from_mlua(kind.name(), err))
}

/// What [`ScriptRuntime::reload`] did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReloadReport {
    /// Modules reloaded, per context.
    pub modules: Vec<(ContextKind, Reload)>,
    /// Entry scripts started again (one per entity, or one for a script started
    /// without an entity). Their old callbacks were disconnected first; threads
    /// already waiting keep running the old code.
    pub restarted: usize,
    /// Errors: a compile error (nothing was reloaded) or errors raised while the
    /// new version ran.
    pub diagnostics: Vec<Diagnostic>,
}

/// The script runtime: the `game` and `tools` contexts.
pub struct ScriptRuntime {
    config: RuntimeConfig,
    game: Context,
    tools: Context,
    /// Entry scripts started: `ScriptRef`s and [`ScriptRuntime::run_script`] runs.
    entries: Vec<Entry>,
    next_owner: Owner,
    contacts: Vec<ContactEvent>,
}

/// One entry-script run. Its owner id tags every thread and callback it starts.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    kind: ContextKind,
    path: String,
    entity: Option<Entity>,
    /// Started by a `ScriptRef` (rather than `run_script`).
    attached: bool,
    owner: Owner,
}

impl ScriptRuntime {
    /// Creates both contexts.
    pub fn new(config: RuntimeConfig) -> Result<ScriptRuntime, ScriptError> {
        Ok(ScriptRuntime {
            game: Context::new(&config, ContextKind::Game)?,
            tools: Context::new(&config, ContextKind::Tools)?,
            config,
            entries: Vec::new(),
            next_owner: 1,
            contacts: Vec::new(),
        })
    }

    /// The configuration.
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// A context.
    pub fn context(&self, kind: ContextKind) -> &Context {
        match kind {
            ContextKind::Game => &self.game,
            ContextKind::Tools => &self.tools,
        }
    }

    /// Throws both VMs away and starts fresh ones (for `world.restart()`, after
    /// `re_app` reloaded the scene). Attached scripts start again on the next phase.
    pub fn reset(&mut self) -> Result<(), ScriptError> {
        self.game = Context::new(&self.config, ContextKind::Game)?;
        self.tools = Context::new(&self.config, ContextKind::Tools)?;
        self.entries.clear();
        self.contacts.clear();
        Ok(())
    }

    /// Queues the physics step's contact events for the next `PostSimulation`.
    pub fn push_contacts(&mut self, events: &[ContactEvent]) {
        self.contacts.extend_from_slice(events);
    }

    /// Drains what `print` and `warn` wrote, game context first.
    pub fn take_output(&mut self) -> Vec<OutputLine> {
        let mut out = Vec::new();
        for ctx in [&self.game, &self.tools] {
            if let Some(mut o) = ctx.lua.app_data_mut::<vm::Output>() {
                out.append(&mut o.lines);
            }
        }
        out
    }

    /// Runs the scripts' work for `phase`; `dt` is the frame time for
    /// `PreRender` and the fixed step for the fixed-step phases, in seconds.
    pub fn run_phase(
        &mut self,
        host: &mut dyn ScriptHost,
        phase: Phase,
        dt: f64,
    ) -> Vec<Diagnostic> {
        let mut diags = Vec::new();
        if matches!(phase, Phase::PreRender | Phase::PreSimulation) {
            diags.extend(self.start_attached(host));
        }
        match phase {
            Phase::PreRender => {
                let (pressed, released): (Vec<String>, Vec<String>) = {
                    let actions = host.services().actions();
                    (
                        actions
                            .buttons
                            .iter()
                            .filter(|(_, b)| b.pressed)
                            .map(|(a, _)| a.clone())
                            .collect(),
                        actions
                            .buttons
                            .iter()
                            .filter(|(_, b)| b.released)
                            .map(|(a, _)| a.clone())
                            .collect(),
                    )
                };
                for ctx in [&self.game, &self.tools] {
                    diags.extend(run_in(ctx, host, |lua| {
                        scheduler::with(lua, |s| {
                            s.frame += 1;
                            s.frame_dt = dt;
                        })?;
                        for a in &pressed {
                            scheduler::fire(
                                lua,
                                |s| *s == Signal::Pressed(a.clone()),
                                MultiValue::new(),
                            )?;
                        }
                        for a in &released {
                            scheduler::fire(
                                lua,
                                |s| *s == Signal::Released(a.clone()),
                                MultiValue::new(),
                            )?;
                        }
                        fire_dt(lua, Signal::Frame, dt)
                    }));
                }
            }
            Phase::PreSimulation => {
                for ctx in [&self.game, &self.tools] {
                    diags.extend(run_in(ctx, host, |lua| {
                        scheduler::with(lua, |s| s.now += dt)?;
                        fire_dt(lua, Signal::Step, dt)
                    }));
                }
            }
            Phase::PostSimulation => {
                let contacts = std::mem::take(&mut self.contacts);
                for ctx in [&self.game, &self.tools] {
                    diags.extend(run_in(ctx, host, |lua| {
                        for ev in &contacts {
                            api::libs::fire_contact(lua, ev)?;
                        }
                        scheduler::drain_deferred(lua)?;
                        scheduler::resume_due(lua)?;
                        fire_dt(lua, Signal::PostStep, dt)
                    }));
                }
            }
            _ => {}
        }
        diags
    }

    /// Starts the script of every entity whose `ScriptRef` has not been started
    /// yet (in stable-id order). The script of an entity that is gone, or whose
    /// `ScriptRef` now names another script or context, is stopped: its callbacks
    /// are disconnected and its waiting threads dropped. `run_phase` calls this
    /// in `PreRender` and `PreSimulation`.
    pub fn start_attached(&mut self, host: &mut dyn ScriptHost) -> Vec<Diagnostic> {
        let wanted = attached_scripts(host.world());
        let (keep, stale): (Vec<Entry>, Vec<Entry>) = std::mem::take(&mut self.entries)
            .into_iter()
            .partition(|en| {
                !en.attached
                    || wanted
                        .iter()
                        .any(|(e, p, k)| Some(*e) == en.entity && *p == en.path && *k == en.kind)
            });
        self.entries = keep;
        for en in stale {
            let _ = scheduler::drop_owner(&self.context(en.kind).lua, en.owner);
        }
        let mut diags = Vec::new();
        for (e, path, kind) in wanted {
            if path.is_empty()
                || self
                    .entries
                    .iter()
                    .any(|en| en.attached && en.entity == Some(e))
            {
                continue;
            }
            let owner = self.new_owner();
            self.entries.push(Entry {
                kind,
                path: path.clone(),
                entity: Some(e),
                attached: true,
                owner,
            });
            diags.extend(self.start(host, &path, kind, Some(e), owner));
        }
        diags
    }

    fn new_owner(&mut self) -> Owner {
        self.next_owner += 1;
        self.next_owner
    }

    /// Runs the script file at `path` (project-relative, under `scripts/`) as a
    /// new thread in context `kind`, with `script.entity` set to `entity`. It runs
    /// until it first waits or ends; [`reload`](Self::reload) restarts it.
    pub fn run_script(
        &mut self,
        host: &mut dyn ScriptHost,
        path: &str,
        kind: ContextKind,
        entity: Option<Entity>,
    ) -> Vec<Diagnostic> {
        let owner = self.new_owner();
        self.entries.push(Entry {
            kind,
            path: path.to_string(),
            entity,
            attached: false,
            owner,
        });
        self.start(host, path, kind, entity, owner)
    }

    fn start(
        &self,
        host: &mut dyn ScriptHost,
        path: &str,
        kind: ContextKind,
        entity: Option<Entity>,
        owner: Owner,
    ) -> Vec<Diagnostic> {
        let source = match read_script(&self.config.project_root, path) {
            Ok(s) => s,
            Err(d) => return vec![d],
        };
        let ctx = self.context(kind);
        run_in(ctx, host, |lua| {
            let env = vm::new_env(lua, path, kind, entity)?;
            let f = lua
                .load(source)
                .set_name(format!("@{path}"))
                .set_compiler(crate::compile::compiler())
                .set_environment(env)
                .into_function()?;
            scheduler::start_owned(lua, f, MultiValue::new(), path.to_string(), Some(owner))?;
            Ok(())
        })
    }

    /// Runs a Luau snippet to completion in context `kind` and returns its first
    /// result as JSON (entities as their stable id). It runs synchronously under
    /// the CPU budget, so it cannot `task.wait`; threads it spawns keep running in
    /// later phases. `name` labels it in errors. Any error raised during the call
    /// fails it.
    pub fn exec(
        &mut self,
        host: &mut dyn ScriptHost,
        kind: ContextKind,
        name: &str,
        source: &str,
    ) -> Result<serde_json::Value, Vec<Diagnostic>> {
        let ctx = self.context(kind);
        let mut value = serde_json::Value::Null;
        let diags = run_in(ctx, host, |lua| {
            let env = vm::new_env(lua, name, kind, None)?;
            let f = lua
                .load(source)
                .set_name(format!("={name}"))
                .set_compiler(crate::compile::compiler())
                .set_environment(env)
                .into_function()?;
            let out: MultiValue = scheduler::call_budgeted(lua, || f.call(()))?;
            value = json::to_json(lua, out.front().unwrap_or(&Value::Nil))?;
            Ok(())
        });
        if diags.is_empty() {
            Ok(value)
        } else {
            Err(diags)
        }
    }

    /// Runs the ProcGraph builder script at `path` in the `tools` context and
    /// returns the graph it built (ADR-0015). It runs synchronously (no
    /// `task.wait`); any error raised during the call fails it.
    pub fn run_builder(
        &mut self,
        host: &mut dyn ScriptHost,
        path: &str,
    ) -> Result<GraphDoc, Vec<Diagnostic>> {
        let source = read_script(&self.config.project_root, path).map_err(|d| vec![d])?;
        let mut graph = None;
        let mut diags = run_in(&self.tools, host, |lua| {
            let env = vm::new_env(lua, path, ContextKind::Tools, None)?;
            let f = lua
                .load(source)
                .set_name(format!("@{path}"))
                .set_compiler(crate::compile::compiler())
                .set_environment(env)
                .into_function()?;
            let out: Value = scheduler::call_budgeted(lua, || f.call(()))?;
            graph = Some(
                api::proc::graph_from_value(&out)
                    .map_err(|m| api::error(codes::GRAPH_BUILDER, m))?,
            );
            Ok(())
        });
        // Errors raised inside the chunk name the builder; the "returned no graph"
        // error has no Luau frame, so point it at the file.
        for d in &mut diags {
            if d.location.is_none() {
                d.location = Some(Location::File {
                    file: path.to_string(),
                    line: None,
                    column: None,
                });
            }
        }
        match graph {
            Some(g) if diags.is_empty() => Ok(g),
            _ => Err(diags),
        }
    }

    /// Reloads the script file at `path` after it changed on disk (called by
    /// `re_app` for changes under `scripts/`). In every context:
    ///
    /// * a module loaded with `require` is reloaded: a module written as
    ///   `local M = ... or {}` is patched in place, any other is replaced for later
    ///   `require` calls;
    /// * an entry script (a `ScriptRef`'s, or one started with
    ///   [`run_script`](Self::run_script)) is stopped (every callback it connected
    ///   and every thread it started, directly or through modules, threads and
    ///   callbacks) and run again.
    ///
    /// Callbacks a reloaded module connects when it runs again belong to no entry
    /// script; callbacks it connected when first required belong to the script
    /// that required it.
    ///
    /// A file that does not compile changes nothing and is reported.
    pub fn reload(&mut self, host: &mut dyn ScriptHost, path: &str) -> ReloadReport {
        let mut report = ReloadReport::default();
        let source = match read_script(&self.config.project_root, path) {
            Ok(s) => s,
            Err(d) => {
                report.diagnostics.push(d);
                return report;
            }
        };
        let compile = crate::compile_check(&String::from_utf8_lossy(&source), path);
        if !compile.is_empty() {
            report.diagnostics = compile;
            return report;
        }
        for kind in ContextKind::ALL {
            let ctx = self.context(kind);
            if !ctx.loaded.borrow().contains(path) {
                continue;
            }
            let mut outcome = None;
            report.diagnostics.extend(run_in(ctx, host, |lua| {
                let env = vm::new_env(lua, path, kind, None)?;
                outcome = Some(modules::reload_module(lua, path, &source, env)?);
                Ok(())
            }));
            if let Some(o) = outcome {
                report.modules.push((kind, o));
            }
        }
        let restart: Vec<usize> = (0..self.entries.len())
            .filter(|&i| self.entries[i].path == path)
            .collect();
        for i in restart {
            let old = self.entries[i].owner;
            let kind = self.entries[i].kind;
            let _ = scheduler::drop_owner(&self.context(kind).lua, old);
            let owner = self.new_owner();
            self.entries[i].owner = owner;
            let entity = self.entries[i].entity;
            report.restarted += 1;
            report
                .diagnostics
                .extend(self.start(host, path, kind, entity, owner));
        }
        report
    }
}

/// Binds the host, runs `f`, runs deferred threads, and collects every error.
fn run_in(
    ctx: &Context,
    host: &mut dyn ScriptHost,
    f: impl FnOnce(&Lua) -> mlua::Result<()>,
) -> Vec<Diagnostic> {
    let lua = &ctx.lua;
    let mut diags = Vec::new();
    host::bind(lua, host, || {
        let r = f(lua).and_then(|()| scheduler::drain_deferred(lua));
        if let Err(e) = r {
            if matches!(e, mlua::Error::MemoryError(_)) {
                let _ = lua.gc_collect();
            }
            diags.push(infra(ctx.kind, &e));
        }
    });
    diags.extend(ctx.errors());
    diags
}

fn fire_dt(lua: &Lua, signal: Signal, dt: f64) -> mlua::Result<()> {
    scheduler::fire(
        lua,
        |s| *s == signal,
        MultiValue::from_iter([Value::Number(dt)]),
    )?;
    Ok(())
}

/// Every entity with a `ScriptRef`, in stable-id order.
fn attached_scripts(world: &mut World) -> Vec<(Entity, String, ContextKind)> {
    let mut found: Vec<(Option<String>, Entity, String, ContextKind)> = Vec::new();
    let mut q = world.ecs.query::<(Entity, &ScriptRef)>();
    for (e, s) in q.iter(&world.ecs) {
        found.push((None, e, s.path.clone(), s.context.into()));
    }
    for f in &mut found {
        f.0 = world.id_of(f.1).map(str::to_string);
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    found.into_iter().map(|(_, e, p, k)| (e, p, k)).collect()
}

/// Reads a script under `scripts/`.
fn read_script(root: &Path, path: &str) -> Result<Vec<u8>, Diagnostic> {
    let fail = |message: String| {
        report(&ScriptError {
            kind: ErrorKind::Runtime,
            code: codes::SCRIPT_READ,
            context: path.to_string(),
            chunk: Some(path.to_string()),
            line: None,
            message,
            traceback: String::new(),
        })
    };
    if modules::chunk_to_rel(&format!("@{path}")).is_none() {
        return Err(fail(format!(
            "{path:?} is not a script path: scripts are project-relative paths under scripts/"
        )));
    }
    std::fs::read(root.join(path)).map_err(|e| fail(format!("cannot read {path}: {e}")))
}
