//! Playing a compat project (`real.toml` `[compat]`, docs/M2_PLAN.md WP16):
//! the Roblox-style layer of `re_compat` in a server and a client context.
//!
//! When the game first runs a frame, [`CompatPlay::start`] splits the App's
//! World into two ([`re_compat::CompatWorlds`]): the App keeps the
//! authoritative server world (physics runs there, and it is what is saved
//! and hashed) and [`CompatPlay`] holds the local player's client world, which
//! is what is rendered. Each context gets a [`CompatVm`]; the server's
//! scripts start, the `[compat] players` join, then the client's scripts
//! start. A frame follows WP12's order:
//!
//! ```text
//! pump(server)  server phases (with physics)  replicate  pump(client)  client phases
//! ```
//!
//! The client's phases run once per fixed step the server ran, without
//! physics; then tweens and sounds advance and the sound commands go to the
//! mixer. Native scripts (`re_script`'s `game` context) do not run in a
//! compat project: its scripts are the instances' `ScriptRef`s.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use glam::{Quat, Vec3};
use re_compat::players::{self, PlayerSpec};
use re_compat::runtime::scripts::StartOptions;
use re_compat::{CompatConfig, CompatSetup, CompatVm, CompatWorlds, ReplicationSet};
use re_core::{Diagnostic, Location};
use re_scene::World;
use re_script::{ContextKind, OutputLine, RuntimeConfig, ScriptHost};

use crate::codes;
use crate::config::AppConfig;

/// The part of a park label that counts as stuck when a run ends
/// (`<path>:WaitForChild("<name>")`): a thread still waiting in
/// `WaitForChild` never got the instance it needs.
pub const STUCK_LABEL: &str = ":WaitForChild(";

/// The seed of the server world's id generator (the client world's has its
/// own, in `re_compat::net`).
const SERVER_ID_SEED: u64 = 0x7365_7276_6572_0001;

/// The `[compat]` section of `config`, if the project has one.
///
/// # Errors
///
/// `RE0501` when the section does not read as a [`CompatConfig`].
pub fn compat_config(config: &AppConfig) -> Result<Option<CompatConfig>, Diagnostic> {
    let Some(table) = &config.compat else {
        return Ok(None);
    };
    toml::Value::Table(table.clone())
        .try_into::<CompatConfig>()
        .map(Some)
        .map_err(|e| {
            Diagnostic::error(
                codes::CONFIG_INVALID,
                format!("the [compat] section of real.toml is invalid: {e}"),
            )
            .at(Location::file(crate::config::CONFIG_FILE))
        })
}

/// What a compat run reports beyond a native one ([`crate::RunReport::compat`]).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize)]
pub struct CompatRunInfo {
    /// Errors scripts raised and did not catch, with their context
    /// (`server` or `client`), in the order they were raised.
    pub uncaught: Vec<(String, Diagnostic)>,
    /// Threads still parked in `WaitForChild` at the end:
    /// `(context, what it waits for, the waiting thread)`.
    pub stuck_yields: Vec<(String, String, String)>,
    /// Members scripts reached that the layer does not have (`RE9001`), with
    /// how often, over both contexts.
    pub unsupported: BTreeMap<String, u32>,
}

/// The compat side of a running game.
pub struct CompatPlay {
    /// The local player's world, the one that is rendered.
    pub client: World,
    set: ReplicationSet,
    pub(crate) server_vm: CompatVm,
    pub(crate) client_vm: CompatVm,
    uncaught: Vec<(ContextKind, Diagnostic)>,
}

impl std::fmt::Debug for CompatPlay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompatPlay").finish_non_exhaustive()
    }
}

/// The setup every compat run shares (the class layer and its bindings),
/// built once per process.
pub fn setup() -> Result<Arc<CompatSetup>, Diagnostic> {
    static SETUP: std::sync::OnceLock<Result<Arc<CompatSetup>, Diagnostic>> =
        std::sync::OnceLock::new();
    SETUP
        .get_or_init(|| CompatSetup::standard().map_err(Diagnostic::from))
        .clone()
}

impl CompatPlay {
    /// Splits `world` (the App's, which becomes the server world) and makes
    /// the two contexts. Nothing runs yet: [`CompatPlay::start_scripts`]
    /// starts the game.
    ///
    /// # Errors
    ///
    /// The scene breaks the compat conventions, or a VM cannot be made.
    pub fn new(
        project: &Path,
        config: &AppConfig,
        compat: &CompatConfig,
        world: &mut World,
        clip_seconds: HashMap<String, f64>,
    ) -> Result<CompatPlay, Diagnostic> {
        let setup = setup()?;
        // Instances scripts make get ids from a fixed sequence, so a run
        // and its replay end with the same state hash.
        world
            .ecs
            .insert_resource(re_scene::IdGen::seeded(SERVER_ID_SEED));
        let server = std::mem::replace(world, World::new());
        let worlds = match CompatWorlds::new(&setup, server) {
            Ok(w) => w,
            Err(e) => return Err(e.into()),
        };
        let CompatWorlds {
            server,
            mut client,
            set,
        } = worlds;
        *world = server;
        let runtime = RuntimeConfig::new(project).with_limits(
            config.scripts.cpu_budget_ms.max(0.0).ceil() as u64,
            config.scripts.memory_limit_mb as usize,
        );
        let server_vm =
            CompatVm::with_config(&setup, ContextKind::Server, world, &runtime, compat)?;
        let client_vm =
            CompatVm::with_config(&setup, ContextKind::Client, &mut client, &runtime, compat)?;
        let clips = Arc::new(clip_seconds);
        for vm in [&server_vm, &client_vm] {
            let clips = Arc::clone(&clips);
            let lookup: Arc<re_compat::sound::ClipSecondsFn> =
                Arc::new(move |path: &str| clips.get(path).copied());
            re_compat::sound::configure(
                vm.context().lua(),
                re_compat::sound::SoundSettings::from_config(compat, Some(lookup)),
            );
        }
        Ok(CompatPlay {
            client,
            set,
            server_vm,
            client_vm,
            uncaught: Vec::new(),
        })
    }

    /// The VM of a context.
    pub fn vm(&self, kind: ContextKind) -> &CompatVm {
        match kind {
            ContextKind::Client => &self.client_vm,
            _ => &self.server_vm,
        }
    }

    /// Keeps a context's script errors for [`CompatPlay::info`] and returns
    /// them.
    pub fn keep(&mut self, kind: ContextKind, diags: Vec<Diagnostic>) -> Vec<Diagnostic> {
        for d in diags.iter().filter(|d| d.is_error()) {
            // A callback that fails every frame is one entry.
            if !self.uncaught.iter().any(|(k, u)| *k == kind && u == d) {
                self.uncaught.push((kind, d.clone()));
            }
        }
        diags
    }

    /// One replication pass, server to client.
    pub fn replicate(&mut self, server: &mut World) {
        re_compat::net::replicate(server, &mut self.client, &self.set);
    }

    /// What the run reports: uncaught errors, stuck yields and unsupported
    /// members.
    pub fn info(&self) -> CompatRunInfo {
        let name = |k: ContextKind| k.name().to_string();
        let mut stuck = Vec::new();
        for (kind, vm) in [
            (ContextKind::Server, &self.server_vm),
            (ContextKind::Client, &self.client_vm),
        ] {
            let waits = re_script::sched::parked_waits(vm.context().lua()).unwrap_or_default();
            for (label, thread) in waits {
                if label.contains(STUCK_LABEL) {
                    stuck.push((name(kind), label, thread));
                }
            }
        }
        let mut unsupported = self.server_vm.unsupported();
        for (k, n) in self.client_vm.unsupported() {
            *unsupported.entry(k).or_default() += n;
        }
        CompatRunInfo {
            uncaught: self
                .uncaught
                .iter()
                .map(|(k, d)| (name(*k), d.clone()))
                .collect(),
            stuck_yields: stuck,
            unsupported,
        }
    }

    /// Drains what both contexts printed.
    pub fn take_output(&self) -> Vec<OutputLine> {
        let mut out = self.server_vm.take_output();
        out.extend(self.client_vm.take_output());
        out
    }
}

/// Starts a context's scripts.
pub fn start_scripts(vm: &CompatVm, host: &mut dyn ScriptHost) -> Vec<Diagnostic> {
    vm.start_scripts(
        host,
        StartOptions {
            templates_as_player: false,
        },
    )
}

/// Joins player `n` (1-based; player 1 is the local player).
pub fn add_player(vm: &CompatVm, host: &mut dyn ScriptHost, n: u32) -> Vec<Diagnostic> {
    let (joined, mut diags) = players::add_player(vm, host, &PlayerSpec::numbered(n));
    if let Err(d) = joined {
        diags.push(d);
    }
    diags
}

/// The client frame's tween, sound and listener work; returns the sound
/// commands for the mixer.
pub fn client_media(
    vm: &CompatVm,
    host: &mut dyn ScriptHost,
    dt: f64,
    listener: Option<(Vec3, Quat)>,
) -> (Vec<re_types::AudioCmd>, Vec<Diagnostic>) {
    let lua = vm.context().lua();
    if let Some((p, r)) = listener {
        re_compat::sound::set_listener(lua, p, r);
    }
    let diags = vm.run(host, |lua| {
        re_compat::sound::step(lua, dt)?;
        Ok(())
    });
    (re_compat::sound::take_commands(lua), diags)
}

/// Advances the tweens of a context by `dt` (before its `Heartbeat`).
pub fn step_tweens(vm: &CompatVm, host: &mut dyn ScriptHost, dt: f64) -> Vec<Diagnostic> {
    vm.run(host, |lua| re_compat::tween::step(lua, dt))
}
