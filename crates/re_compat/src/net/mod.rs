//! Two worlds in one process: the server and the client, side by side, the
//! way a play test runs them ([`CompatWorlds`]).
//!
//! * The **server world** is authoritative: physics runs there, its scripts
//!   run in the `server` VM, and it is what is saved.
//! * The **client world** is the local player's replica: the replicated
//!   containers ([`ReplicationSet::containers`]) are copied from the server
//!   each frame by [`replicate`], and client-only instances (`PlayerGui`,
//!   `PlayerScripts`, whatever client scripts create) live only there. Its
//!   scripts run in the `client` VM, and rendering and input use it.
//! * An instance is the same instance in both worlds when it has the same
//!   stable id. Entities are never compared across worlds.
//! * The two VMs talk through remotes ([`remotes`]): messages queue in the
//!   [`NetLink`] both worlds share and are delivered by [`pump`].
//!
//! A frame of a two-world run is:
//!
//! ```text
//! pump(server VM)    remotes from the client; queued character loads;
//!                    scripts that became startable
//! server phases      PreRender .. PostSimulation (scripts, physics)
//! replicate          owned components client → server, then server → client
//! pump(client VM)    player and character events (removals included),
//!                    PlayerScripts and PlayerGui, removals, remotes from
//!                    the server, prompts; scripts that became startable
//! client phases      PreRender .. PostSimulation, then render from the client
//! ```
//!
//! M2 has no real network: there is one client world, the local player's.
//! Other players are simulated on the server only (`players::add_player`).

pub mod remotes;
mod replicate;
pub mod value;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use mlua::{Function, Lua, MultiValue, Result};
use re_core::Diagnostic;
use re_scene::ecs as bevy_ecs;
use re_scene::ecs::prelude::Resource;
use re_scene::{Entity, IdGen, World, hierarchy};
use re_script::{ScriptHost, sched};

pub use replicate::{
    NetworkOwner, Replica, ReplicationReport, UPSTREAM_ALL, network_owner, replicate,
    set_network_owner,
};

use crate::error::CompatError;
use crate::instance::tree;
use crate::runtime::{Bindings, CompatSetup, CompatVm, state, with_world};
use crate::scene::ServiceTag;
use crate::{GAME_ID, ROOT_CLASS};
use value::NetValue;

/// The seed of the client world's id generator, so a seeded run makes the same
/// client ids every time (and never the server's: a clash is resolved anyway).
const CLIENT_ID_SEED: u64 = 0x636c_6965_6e74_0001;

/// What replicates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicationSet {
    /// The service classes whose subtrees replicate from server to client.
    /// Services the place lacks are skipped. `ServerStorage` and
    /// `ServerScriptService` never belong here.
    pub containers: Vec<String>,
    /// The components a client owns for the entities its player is the
    /// network owner of ([`set_network_owner`]; `LoadCharacter` makes the
    /// player own its whole character): the client's changes to them flow to
    /// the server, and the server's own changes still reach the client.
    /// [`UPSTREAM_ALL`] (`"*"`, the default) means every component: own and
    /// aliased properties, attributes and tags. Children a client creates
    /// never flow up.
    pub upstream: Vec<String>,
}

impl Default for ReplicationSet {
    fn default() -> ReplicationSet {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect();
        ReplicationSet {
            containers: s(&[
                "Workspace",
                "ReplicatedStorage",
                "ReplicatedFirst",
                "Players",
                "Lighting",
                "StarterGui",
                "StarterPlayer",
                "StarterPack",
                "SoundService",
                "TextChatService",
                "Teams",
            ]),
            upstream: s(&[UPSTREAM_ALL]),
        }
    }
}

/// Which side of the two worlds a world is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// The authoritative world.
    Server,
    /// The local player's replica.
    Client,
}

/// A message between the two VMs.
#[derive(Clone, Debug)]
pub(crate) enum Msg {
    /// A remote event: `FireServer` (with the sender), `FireClient` or
    /// `FireAllClients`.
    Event {
        remote: String,
        player: Option<String>,
        args: Vec<NetValue>,
    },
    /// A remote function call, answered by a [`Msg::Reply`] with `call`.
    Invoke {
        remote: String,
        player: Option<String>,
        call: u64,
        args: Vec<NetValue>,
    },
    /// The results of an invocation, or its error message.
    Reply {
        call: u64,
        result: std::result::Result<Vec<NetValue>, String>,
    },
    /// A proximity prompt action of the local player, client to server.
    Prompt {
        prompt: String,
        player: String,
        action: PromptAction,
    },
    /// A chat message a player sent, server to client.
    Chat { player: String, text: String },
    /// The local player kicked itself (`Player:Kick` on the client).
    Kick { player: String },
}

/// What a player did with a proximity prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptAction {
    HoldBegan,
    HoldEnded,
    Triggered,
    TriggerEnded,
}

/// The state both worlds share.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    pub(crate) to_server: VecDeque<Msg>,
    pub(crate) to_client: VecDeque<Msg>,
    /// The stable id of the local player's `Player`.
    pub(crate) local_player: Option<String>,
    pub(crate) next_call: u64,
    /// Messages for players that have no client in this process.
    pub(crate) undelivered: u64,
}

/// The link between the two worlds, a resource of each: which side the world
/// is, and the queues and local player they share.
#[derive(Resource, Clone, Debug)]
pub struct NetLink {
    /// This world's side.
    pub side: Side,
    shared: Arc<Mutex<Shared>>,
}

impl NetLink {
    pub(crate) fn lock(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The stable id of the local player's `Player`, if one has joined.
    pub fn local_player(&self) -> Option<String> {
        self.lock().local_player.clone()
    }

    /// How many messages were addressed to players without a client here.
    pub fn undelivered(&self) -> u64 {
        self.lock().undelivered
    }
}

/// The link of the VM's world, if it is one of two worlds.
pub(crate) fn link(lua: &Lua) -> Result<Option<NetLink>> {
    with_world(lua, |w| w.ecs.get_resource::<NetLink>().cloned())
}

/// The server and the client world, and what replicates between them.
pub struct CompatWorlds {
    /// The authoritative world.
    pub server: World,
    /// The local player's replica.
    pub client: World,
    /// What replicates.
    pub set: ReplicationSet,
}

impl std::fmt::Debug for CompatWorlds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompatWorlds")
            .field("set", &self.set)
            .finish_non_exhaustive()
    }
}

impl CompatWorlds {
    /// Makes the client world from `server`, which holds a compat scene,
    /// with the default [`ReplicationSet`].
    ///
    /// # Errors
    ///
    /// The server world has no `game` root, or the class layer's components
    /// do not register (`RE9004`, `RE9003`).
    pub fn new(
        setup: &CompatSetup,
        server: World,
    ) -> std::result::Result<CompatWorlds, CompatError> {
        CompatWorlds::with_set(setup, server, ReplicationSet::default())
    }

    /// Makes the client world from `server` with replication set `set`. The
    /// client starts as a copy of the server (same stable ids) in which the
    /// services outside `set` are empty; the services themselves stay, as
    /// they do on a Roblox client.
    pub fn with_set(
        setup: &CompatSetup,
        mut server: World,
        set: ReplicationSet,
    ) -> std::result::Result<CompatWorlds, CompatError> {
        setup.classes.register_components(&mut server)?;
        let game = server
            .entity(GAME_ID)
            .ok()
            .filter(|g| tree::class(&server, *g) == Some(ROOT_CLASS))
            .ok_or_else(|| {
                CompatError::Scene(format!(
                    "the server world has no {ROOT_CLASS} root with id {GAME_ID:?}"
                ))
            })?;
        let mut client = server.fork();
        let game_c = client.entity(GAME_ID).expect("the fork keeps ids");
        let _ = game;
        for child in hierarchy::children(&client.ecs, game_c).to_vec() {
            let keep = client.ecs.get::<ServiceTag>(child).is_some();
            let replicated = keep
                && tree::class(&client, child)
                    .is_some_and(|c| set.containers.iter().any(|x| x == c));
            if !keep {
                let _ = client.despawn_runtime(child);
            } else if !replicated {
                for c in hierarchy::children(&client.ecs, child).to_vec() {
                    let _ = client.despawn_runtime(c);
                }
            }
        }
        client.ecs.insert_resource(IdGen::seeded(CLIENT_ID_SEED));
        let shared = Arc::new(Mutex::new(Shared::default()));
        server.ecs.insert_resource(NetLink {
            side: Side::Server,
            shared: shared.clone(),
        });
        client.ecs.insert_resource(NetLink {
            side: Side::Client,
            shared,
        });
        replicate::init(&mut server, &mut client, &set);
        Ok(CompatWorlds {
            server,
            client,
            set,
        })
    }

    /// One replication pass ([`replicate`]).
    pub fn replicate(&mut self) -> ReplicationReport {
        replicate(&mut self.server, &mut self.client, &self.set)
    }

    /// The link the two worlds share.
    pub fn link(&self) -> NetLink {
        self.server
            .ecs
            .get_resource::<NetLink>()
            .cloned()
            .expect("a CompatWorlds server has a link")
    }

    /// The local player's `Player` in the server world.
    pub fn local_player(&self) -> Option<Entity> {
        let id = self.link().local_player()?;
        self.server.entity(&id).ok()
    }

    /// The client entity of a server entity (same stable id), if the client
    /// has it.
    pub fn client_of(&self, server_entity: Entity) -> Option<Entity> {
        let id = self.server.id_of(server_entity)?;
        self.client.entity(id).ok()
    }

    /// The server entity of a client entity (same stable id), if the server
    /// has it.
    pub fn server_of(&self, client_entity: Entity) -> Option<Entity> {
        let id = self.client.id_of(client_entity)?;
        self.server.entity(id).ok()
    }
}

/// Delivers what arrived for `vm` and runs the per-frame work of its side
/// (see the module docs for where it sits in a frame). A VM whose world is
/// not one of two worlds has nothing to pump. Returns the diagnostics of the
/// errors raised meanwhile (handlers included).
pub fn pump(vm: &CompatVm, host: &mut dyn ScriptHost) -> Vec<Diagnostic> {
    let mut out = vm.run(host, pump_lua);
    // Scripts that became startable since the last pump (copied to a player
    // or a character, cloned into the tree) start now, each on its own.
    let mut new = Vec::new();
    out.extend(vm.run(host, |lua| {
        if link(lua)?.is_some() {
            new = crate::runtime::scripts::unstarted(lua)?;
        }
        Ok(())
    }));
    for e in new {
        out.extend(vm.start_script(host, e));
    }
    out
}

fn pump_lua(lua: &Lua) -> Result<()> {
    let Some(link) = link(lua)? else {
        return Ok(());
    };
    match link.side {
        Side::Server => {
            crate::players::server_frame(lua)?;
            remotes::deliver(lua, &link)?;
        }
        Side::Client => {
            crate::players::client_frame(lua, &link)?;
            destroy_removed(lua)?;
            remotes::deliver(lua, &link)?;
            crate::services::extra::prompts::client_frame(lua, &link)?;
        }
    }
    Ok(())
}

/// Destroys, in the client VM, the subtrees the server removed, as
/// `Destroy` would (`Destroying` fires, the handles keep their name and
/// class), after the handlers already queued this pump.
fn destroy_removed(lua: &Lua) -> Result<()> {
    let roots = with_world(lua, replicate::take_removed)?;
    for root in roots {
        crate::players::destroy_after_handlers(lua, root)?;
    }
    Ok(())
}

/// The client instances whose removal by the server is pending this pump.
pub(crate) fn removing(lua: &Lua) -> Result<std::collections::HashSet<Entity>> {
    with_world(lua, |w| replicate::removing(w))
}

/// Registry key of the function that sleeps through a park with a timeout.
const SLEEP_KEY: &str = "re_compat.net.sleep";
/// Registry key of the Luau runner of [`after`].
const AFTER_KEY: &str = "re_compat.net.after";

/// Runs `f` once `secs` seconds of simulation time have passed, in a script
/// thread labelled `label` (it shows in `parked()` while it waits).
pub(crate) fn after(lua: &Lua, secs: f64, label: &str, f: Function) -> Result<()> {
    let runner: Function = match lua.named_registry_value::<Option<Function>>(AFTER_KEY)? {
        Some(r) => r,
        None => {
            let raw = lua.create_function(|lua, (label, secs): (String, f64)| {
                let (values, _) = sched::park(lua, &label, Some((secs, MultiValue::new())))?;
                Ok(values)
            })?;
            let sleep = sched::yielding(lua, "sleep", raw)?;
            lua.set_named_registry_value(SLEEP_KEY, &sleep)?;
            let r: Function = lua
                .load(
                    "local sleep = ...\nreturn function(label, secs, f) sleep(label, secs) f() end",
                )
                .set_name("=after")
                .call(sleep)?;
            lua.set_named_registry_value(AFTER_KEY, &r)?;
            r
        }
    };
    sched::spawn(
        lua,
        runner,
        MultiValue::from_iter([
            mlua::Value::String(lua.create_string(label)?),
            mlua::Value::Number(secs),
            mlua::Value::Function(f),
        ]),
        label,
    )?;
    Ok(())
}

/// True when the VM runs on the server side (or alone, with no client).
pub(crate) fn is_server(lua: &Lua) -> Result<bool> {
    Ok(state(lua)?.kind == re_script::ContextKind::Server)
}

/// Registers the remotes' methods and callbacks.
pub fn register(b: &mut Bindings) {
    remotes::register(b);
}

/// Calls `BindableFunction` `entity`'s `OnInvoke` in `vm` from the host (a
/// harness's `invoke` step), with the arguments of JSON array `args_json`,
/// and returns its results as a JSON array. Fails with `RE9301` when the
/// entity is not a `BindableFunction`, the arguments are not a JSON array,
/// the handler raises or is not set, or it yields past the end of the call.
/// The diagnostics are those of other errors raised meanwhile.
pub fn invoke_bindable(
    vm: &CompatVm,
    host: &mut dyn ScriptHost,
    entity: Entity,
    args_json: &str,
) -> (std::result::Result<String, Diagnostic>, Vec<Diagnostic>) {
    let fail = |m: String| Diagnostic::error(crate::codes::REMOTE_CALL, format!("invoke: {m}"));
    let mut out = Err(fail("the call did not run".into()));
    let diags = vm.run(host, |lua| {
        let is_bindable = with_world(lua, |w| {
            tree::alive(w, entity) && tree::class(w, entity) == Some("BindableFunction")
        })?;
        if !is_bindable {
            out = Err(fail("the entity is not a BindableFunction".into()));
            return Ok(());
        }
        let args: Vec<serde_json::Value> = match serde_json::from_str(args_json) {
            Ok(serde_json::Value::Array(a)) => a,
            Ok(_) => {
                out = Err(fail("the arguments must be a JSON array".into()));
                return Ok(());
            }
            Err(e) => {
                out = Err(fail(format!("the arguments are not JSON: {e}")));
                return Ok(());
            }
        };
        let mut values = MultiValue::new();
        for a in args {
            values.push_back(crate::services::extra::from_json(lua, a)?);
        }
        out = match remotes::invoke_bindable_now(lua, entity, values)? {
            Ok(results) => {
                let mut list = Vec::new();
                for r in results {
                    match crate::services::extra::to_json(r, 0, &mut Vec::new()) {
                        Ok(j) => list.push(j),
                        Err(e) => {
                            out = Err(fail(format!("a result has no JSON form: {e}")));
                            return Ok(());
                        }
                    }
                }
                Ok(serde_json::Value::Array(list).to_string())
            }
            Err(e) => Err(fail(e)),
        };
        Ok(())
    });
    (out, diags)
}
