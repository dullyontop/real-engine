//! A headless harness for tests: loads a compat project's entry scene into a
//! `server` and a `client` context, starts their scripts and steps frames on
//! the virtual clock.
//!
//! ```no_run
//! use re_compat::testing::{Harness, HarnessOptions, Side};
//!
//! let mut h = Harness::open("my-project".as_ref(), HarnessOptions::default()).unwrap();
//! h.start();
//! h.step(3);
//! assert!(h.uncaught().is_empty(), "{:#?}", h.uncaught());
//! let printed = h.take_output(Side::Server);
//! # let _ = printed;
//! ```
//!
//! By default both contexts load the same scene into worlds of their own:
//! the client world drops what the server keeps to itself (the contents of
//! `ServerScriptService` and `ServerStorage`), and the client scripts of
//! `StarterPlayerScripts` and `StarterGui` start in place, standing in for a
//! player's copies ([`StartOptions::templates_as_player`]). A frame is the
//! server's [`CompatVm::frame`] then the client's.
//!
//! With [`HarnessOptions::two_worlds`] the run is a play test instead
//! ([`crate::net`]): the client world is the server's replica, `start` joins
//! `[compat] players` players (the first is the local player), and a frame is
//! `pump(server)`, the server's frame, `replicate`, `pump(client)`, the
//! client's frame. Scripts copied to a player or a character start at the
//! next pump.
//!
//! Frames have one fixed step of [`HarnessOptions::dt`] seconds, physics
//! included: a client world simulates the parts it made itself, and the
//! parts the server replicates to it stand still between replications.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use re_core::Diagnostic;
use re_scene::{Entity, World, hierarchy};
use re_script::host::{NoServices, WorldHost};
use re_script::{ContextKind, OutputLine, RuntimeConfig};

use super::scripts::StartOptions;
use super::{CompatSetup, CompatVm};
use crate::class::ClassMarker;
use crate::error::CompatError;
use crate::net::{self, CompatWorlds, ReplicationSet};
use crate::players::{self, PlayerSpec};
use crate::scene::{CompatConfig, ServiceTag};

/// The two contexts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// The `server` context.
    Server,
    /// The `client` context.
    Client,
}

impl Side {
    /// Both sides, server first.
    pub const BOTH: [Side; 2] = [Side::Server, Side::Client];

    /// The script context of the side.
    pub fn kind(self) -> ContextKind {
        match self {
            Side::Server => ContextKind::Server,
            Side::Client => ContextKind::Client,
        }
    }
}

/// How [`Harness::open`] sets the contexts up.
#[derive(Clone, Debug)]
pub struct HarnessOptions {
    /// The fixed step and frame time, in seconds (1/60 by default).
    pub dt: f64,
    /// Start the client scripts of the player templates in place (true by
    /// default, until players exist).
    pub templates_as_player: bool,
    /// The CPU time one resume of a script thread may use, in milliseconds;
    /// `None` takes `real.toml` `[scripts] cpu_budget_ms` or the default.
    pub cpu_budget_ms: Option<u64>,
    /// Override of the `[compat]` section of `real.toml`.
    pub compat: Option<CompatConfig>,
    /// Run the server and the client as two worlds with players and
    /// replication ([`crate::net`]); `templates_as_player` is then ignored.
    pub two_worlds: bool,
}

impl Default for HarnessOptions {
    fn default() -> Self {
        HarnessOptions {
            dt: 1.0 / 60.0,
            templates_as_player: true,
            cpu_budget_ms: None,
            compat: None,
            two_worlds: false,
        }
    }
}

struct Ctx {
    world: World,
    services: NoServices,
    vm: CompatVm,
    output: Vec<OutputLine>,
}

impl Ctx {
    fn with<R>(&mut self, f: impl FnOnce(&CompatVm, &mut WorldHost<'_>) -> R) -> R {
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let r = f(&self.vm, &mut host);
        self.output.extend(self.vm.take_output());
        r
    }
}

/// A compat project running headless in a `server` and a `client` context.
pub struct Harness {
    project: PathBuf,
    opts: HarnessOptions,
    server: Ctx,
    client: Ctx,
    frames: u64,
    uncaught: Vec<(Side, Diagnostic)>,
    /// The two-world run's replication set and player count.
    net: Option<(ReplicationSet, u32)>,
}

impl std::fmt::Debug for Harness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Harness")
            .field("project", &self.project)
            .field("frames", &self.frames)
            .finish()
    }
}

/// The parts of `real.toml` the harness reads.
#[derive(serde::Deserialize, Default)]
struct ProjectToml {
    #[serde(default)]
    project: ProjectSection,
    #[serde(default)]
    scripts: ScriptsSection,
    #[serde(default)]
    compat: Option<toml::Value>,
}

#[derive(serde::Deserialize)]
struct ProjectSection {
    #[serde(default = "default_entry_scene")]
    entry_scene: String,
}

impl Default for ProjectSection {
    fn default() -> Self {
        ProjectSection {
            entry_scene: default_entry_scene(),
        }
    }
}

fn default_entry_scene() -> String {
    "scenes/main.scene.json".into()
}

#[derive(serde::Deserialize, Default)]
struct ScriptsSection {
    cpu_budget_ms: Option<u64>,
    memory_limit_mb: Option<usize>,
}

fn scene_error(m: impl std::fmt::Display) -> Diagnostic {
    CompatError::Scene(m.to_string()).into()
}

impl Harness {
    /// Loads the project at `project` (its `real.toml` and entry scene) into
    /// both contexts. Scripts do not start until [`Harness::start`].
    ///
    /// # Errors
    ///
    /// `real.toml` or the scene does not read, or the scene breaks the compat
    /// conventions (`RE9004`).
    pub fn open(project: &Path, opts: HarnessOptions) -> Result<Harness, Diagnostic> {
        let setup = CompatSetup::standard().map_err(Diagnostic::from)?;
        Harness::open_with(project, &setup, opts)
    }

    /// [`Harness::open`] with a setup of the caller's (extra classes or
    /// bindings).
    pub fn open_with(
        project: &Path,
        setup: &Arc<CompatSetup>,
        opts: HarnessOptions,
    ) -> Result<Harness, Diagnostic> {
        let toml_path = project.join("real.toml");
        let text = std::fs::read_to_string(&toml_path)
            .map_err(|e| scene_error(format!("{}: {e}", toml_path.display())))?;
        let doc: ProjectToml =
            toml::from_str(&text).map_err(|e| scene_error(format!("real.toml: {e}")))?;
        let compat = match (&opts.compat, doc.compat) {
            (Some(c), _) => c.clone(),
            (None, Some(v)) => v
                .try_into()
                .map_err(|e: toml::de::Error| scene_error(format!("real.toml [compat]: {e}")))?,
            (None, None) => CompatConfig::default(),
        };
        let mut config = RuntimeConfig::new(project);
        if let Some(ms) = opts.cpu_budget_ms.or(doc.scripts.cpu_budget_ms) {
            config.cpu_budget = std::time::Duration::from_millis(ms);
        }
        if let Some(mb) = doc.scripts.memory_limit_mb {
            config.memory_limit = mb * 1024 * 1024;
        }
        let scene_path = project.join(&doc.project.entry_scene);
        let scene_text = std::fs::read_to_string(&scene_path)
            .map_err(|e| scene_error(format!("{}: {e}", scene_path.display())))?;
        let scene = re_scene::scene::from_json(&scene_text)
            .map_err(|e| scene_error(format!("{}: {e}", doc.project.entry_scene)))?;
        let load = || -> Result<World, Diagnostic> {
            let mut world = World::new();
            setup
                .classes
                .register_components(&mut world)
                .map_err(Diagnostic::from)?;
            world
                .load(&scene)
                .map_err(|e| scene_error(format!("{}: {e}", doc.project.entry_scene)))?;
            Ok(world)
        };
        let make = |side: Side, mut world: World| -> Result<Ctx, Diagnostic> {
            let vm = CompatVm::with_config(setup, side.kind(), &mut world, &config, &compat)?;
            Ok(Ctx {
                world,
                services: NoServices::default(),
                vm,
                output: Vec::new(),
            })
        };
        let (server, client, net) = if opts.two_worlds {
            let worlds = CompatWorlds::new(setup, load()?).map_err(Diagnostic::from)?;
            let CompatWorlds {
                server,
                client,
                set,
            } = worlds;
            (
                make(Side::Server, server)?,
                make(Side::Client, client)?,
                Some((set, compat.players)),
            )
        } else {
            let mut client = load()?;
            hide_server_only(&mut client);
            (
                make(Side::Server, load()?)?,
                make(Side::Client, client)?,
                None,
            )
        };
        Ok(Harness {
            project: project.to_path_buf(),
            opts,
            server,
            client,
            frames: 0,
            uncaught: Vec::new(),
            net,
        })
    }

    fn ctx(&mut self, side: Side) -> &mut Ctx {
        match side {
            Side::Server => &mut self.server,
            Side::Client => &mut self.client,
        }
    }

    fn keep(&mut self, side: Side, d: Vec<Diagnostic>) -> Vec<Diagnostic> {
        self.uncaught.extend(d.iter().cloned().map(|d| (side, d)));
        d
    }

    /// Starts the scripts of both contexts (server first), in tree order;
    /// in a two-world run, then joins the `[compat] players` players.
    /// Returns the errors raised meanwhile (also kept in
    /// [`Harness::uncaught`]).
    pub fn start(&mut self) -> Vec<Diagnostic> {
        let opts = StartOptions {
            templates_as_player: self.opts.templates_as_player && self.net.is_none(),
        };
        let mut out = Vec::new();
        for side in Side::BOTH {
            let d = self.ctx(side).with(|vm, h| vm.start_scripts(h, opts));
            out.extend(self.keep(side, d));
        }
        let players = self.net.as_ref().map_or(0, |(_, n)| *n);
        for n in 1..=players {
            let (r, d) = self.add_player(&PlayerSpec::numbered(n));
            out.extend(d);
            if let Err(e) = r {
                out.extend(self.keep(Side::Server, vec![e]));
            }
        }
        out
    }

    /// Joins a player in a two-world run ([`players::add_player`]); returns
    /// the server's `Player` (or why it could not join, `RE9302`) and the
    /// errors handlers raised (also kept in [`Harness::uncaught`]).
    pub fn add_player(
        &mut self,
        spec: &PlayerSpec,
    ) -> (Result<Entity, Diagnostic>, Vec<Diagnostic>) {
        if self.net.is_none() {
            return (Err(one_world("add_player")), Vec::new());
        }
        let (r, d) = self.server.with(|vm, h| players::add_player(vm, h, spec));
        (r, self.keep(Side::Server, d))
    }

    /// Removes a player in a two-world run ([`players::remove_player`]).
    pub fn remove_player(&mut self, player: Entity) -> (Result<(), Diagnostic>, Vec<Diagnostic>) {
        if self.net.is_none() {
            return (Err(one_world("remove_player")), Vec::new());
        }
        let (r, d) = self
            .server
            .with(|vm, h| players::remove_player(vm, h, player));
        (r, self.keep(Side::Server, d))
    }

    /// Runs `n` frames; returns the errors raised meanwhile (also kept in
    /// [`Harness::uncaught`]).
    pub fn step(&mut self, n: u32) -> Vec<Diagnostic> {
        let dt = self.opts.dt;
        let mut out = Vec::new();
        for _ in 0..n {
            for side in Side::BOTH {
                let two = self.net.is_some();
                let mut d = Vec::new();
                if two && side == Side::Client {
                    self.replicate();
                }
                if two {
                    d.extend(self.ctx(side).with(|vm, h| net::pump(vm, h)));
                }
                d.extend(self.ctx(side).with(|vm, h| vm.frame(h, dt)));
                out.extend(self.keep(side, d));
            }
            self.frames += 1;
        }
        out
    }

    fn replicate(&mut self) {
        if let Some((set, _)) = &self.net {
            net::replicate(&mut self.server.world, &mut self.client.world, set);
        }
    }

    /// Runs Luau `source` as a new thread in one context (its `script` is
    /// nil); returns the errors it raised.
    pub fn exec(&mut self, side: Side, name: &str, source: &str) -> Vec<Diagnostic> {
        let d = self.ctx(side).with(|vm, h| vm.exec(h, name, source));
        self.keep(side, d)
    }

    /// Shuts both contexts down (the `BindToClose` callbacks); returns the
    /// errors and whether every callback finished in time.
    pub fn close(&mut self) -> (Vec<Diagnostic>, bool) {
        let dt = self.opts.dt;
        let mut out = Vec::new();
        let mut all = true;
        for side in Side::BOTH {
            let (d, ok) = self.ctx(side).with(|vm, h| vm.close(h, dt));
            out.extend(self.keep(side, d));
            all &= ok;
        }
        (out, all)
    }

    /// The frames run so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Every error raised since the harness opened, with its context.
    pub fn uncaught(&self) -> &[(Side, Diagnostic)] {
        &self.uncaught
    }

    /// Drains what `print` and `warn` wrote in one context.
    pub fn take_output(&mut self, side: Side) -> Vec<OutputLine> {
        let c = self.ctx(side);
        c.output.extend(c.vm.take_output());
        std::mem::take(&mut c.output)
    }

    /// The threads parked in yielding calls (`WaitForChild`, `Signal:Wait`)
    /// in one context: `(what it waits for, the thread)`.
    pub fn stuck(&self, side: Side) -> Vec<(String, String)> {
        let vm = match side {
            Side::Server => &self.server.vm,
            Side::Client => &self.client.vm,
        };
        re_script::sched::parked_waits(vm.context().lua()).unwrap_or_default()
    }

    /// The full names of the modules `require` has run in one context,
    /// sorted.
    pub fn required(&self, side: Side) -> Vec<String> {
        let w = self.world(side);
        let mut names: Vec<String> = self
            .vm(side)
            .required()
            .into_iter()
            .map(|e| crate::instance::tree::full_name(w, e))
            .collect();
        names.sort();
        names
    }

    /// How often scripts reached members the layer does not have, by member,
    /// over both contexts.
    pub fn unsupported(&self) -> BTreeMap<String, u32> {
        let mut out = self.server.vm.unsupported();
        for (k, n) in self.client.vm.unsupported() {
            *out.entry(k).or_default() += n;
        }
        out
    }

    /// A context's world.
    pub fn world(&self, side: Side) -> &World {
        match side {
            Side::Server => &self.server.world,
            Side::Client => &self.client.world,
        }
    }

    /// A context's world, to change it between frames.
    pub fn world_mut(&mut self, side: Side) -> &mut World {
        &mut self.ctx(side).world
    }

    /// A context's VM.
    pub fn vm(&self, side: Side) -> &CompatVm {
        match side {
            Side::Server => &self.server.vm,
            Side::Client => &self.client.vm,
        }
    }

    /// The project root.
    pub fn project(&self) -> &Path {
        &self.project
    }
}

fn one_world(what: &str) -> Diagnostic {
    Diagnostic::error(
        crate::codes::PLAYER_STEP,
        format!("Harness::{what} needs a two-world run (HarnessOptions::two_worlds)"),
    )
}

/// Removes from a client world the contents of the services the server keeps
/// to itself.
fn hide_server_only(w: &mut World) {
    let Ok(game) = w.entity(crate::GAME_ID) else {
        return;
    };
    let hidden: Vec<Entity> = hierarchy::children(&w.ecs, game)
        .iter()
        .copied()
        .filter(|s| {
            w.ecs.get::<ServiceTag>(*s).is_some()
                && w.ecs
                    .get::<ClassMarker>(*s)
                    .is_some_and(|m| m.class == "ServerScriptService" || m.class == "ServerStorage")
        })
        .flat_map(|s| hierarchy::children(&w.ecs, s).to_vec())
        .collect();
    for e in hidden {
        hierarchy::despawn_recursive(&mut w.ecs, e);
    }
}
