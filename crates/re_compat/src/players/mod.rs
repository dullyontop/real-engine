//! `Players`, `Player` and characters.
//!
//! * Players join through the host ([`add_player`]): the harness adds
//!   `[compat] players` of them at boot, and tests add and remove them. The
//!   server makes the `Player` under `Players` with a `Backpack`, fires
//!   `PlayerAdded`, and, when `Players.CharacterAutoLoads` is on, loads the
//!   character at the start of the next server frame (so `PlayerAdded`
//!   handlers can connect `CharacterAdded` first).
//! * The first player that joins as local is the client's `LocalPlayer`. The
//!   client makes its `PlayerScripts` (from `StarterPlayerScripts`) and
//!   `PlayerGui` (from `StarterGui`); both exist only in the client world.
//!   Other players exist only on the server and in replication.
//! * [`load_character`] (and `Player:LoadCharacter`) replaces the character:
//!   a copy of `StarterPlayer.StarterCharacter`, else the default character
//!   (WP14's `Players:CreateHumanoidModelFromDescription` when registered,
//!   else WP14's default R15 rig, or an empty `Model` until the part
//!   classes exist); with copies of `StarterCharacterScripts`, at a
//!   `SpawnLocation` (enabled, matching the player's team or neutral) with a
//!   `ForceField` for the spawn's `Duration`; parented to `Workspace`, owned
//!   by the player ([`crate::net::set_network_owner`]). The `Backpack` is
//!   refilled from `StarterPack`, and the client resets the `PlayerGui`
//!   children whose `ResetOnSpawn` is on.
//! * The client fires `PlayerAdded`, `PlayerRemoving`, `CharacterAdded` and
//!   `CharacterRemoving` from what replication brings ([`client_frame`]).
//! * Chat comes from the host ([`say`]): `Player.Chatted` fires on both
//!   sides and a matching `TextChatCommand` triggers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mlua::{Function, Lua, MultiValue, Result, Value as LuaValue};
use re_core::Diagnostic;
use re_reflect::Value;
use re_scene::{Entity, World};
use re_script::ScriptHost;

use crate::class::{ClassRegistry, own_component_name};
use crate::error::CompatError;
use crate::instance::methods_api::{args, ret};
use crate::instance::{self, clone::clone_subtree, push, push_list, tree};
use crate::net::{self, NetLink, Side};
use crate::runtime::{Bindings, CompatVm, compat_error, lua_error, state, with_world};
use crate::scene::spawn_instance;
use crate::services::core::{ensure_service, find_service};
use crate::signal::{self, events};

/// Who joins ([`add_player`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerSpec {
    /// The player's `Name` and `DisplayName`.
    pub name: String,
    /// `UserId`.
    pub user_id: i64,
    /// Joins as the local player (the client world's `LocalPlayer`), when no
    /// local player is in the game yet.
    pub local: bool,
}

impl PlayerSpec {
    /// The `n`th player a harness adds (from 1): `Player1`, user id `n`, the
    /// first one local.
    pub fn numbered(n: u32) -> PlayerSpec {
        PlayerSpec {
            name: format!("Player{n}"),
            user_id: i64::from(n),
            local: n == 1,
        }
    }
}

/// The server's players whose character loads at the next server frame.
#[derive(Default)]
struct PendingSpawns(Vec<Entity>);

/// What the client has seen: players and their characters, and whether the
/// local player's `PlayerGui` was filled.
#[derive(Default)]
struct ClientView {
    players: Vec<Entity>,
    characters: HashMap<Entity, Option<Entity>>,
    local_ready: HashSet<Entity>,
}

fn classes(lua: &Lua) -> Result<Arc<ClassRegistry>> {
    Ok(state(lua)?.setup.classes.clone())
}

fn game(lua: &Lua) -> Result<Entity> {
    Ok(state(lua)?.game)
}

/// Writes own property `field` of `class` directly (read-only ones too).
fn set_own(
    w: &mut World,
    class: &str,
    e: Entity,
    field: &str,
    v: Value,
) -> std::result::Result<(), CompatError> {
    let key = w.registry.key(&own_component_name(class))?;
    let index = w.registry.schema(key).field_index(field)?;
    w.registry.set_field(&mut w.ecs, e, key, index, v)?;
    Ok(())
}

fn get_own(w: &World, class: &str, e: Entity, field: &str) -> Option<Value> {
    let key = w.registry.key(&own_component_name(class)).ok()?;
    let index = w.registry.schema(key).field_index(field).ok()?;
    w.registry.get_field(&w.ecs, e, key, index)
}

/// The `Character` of a player.
pub fn character_of(w: &World, player: Entity) -> Option<Entity> {
    match get_own(w, "Player", player, "Character") {
        Some(Value::Entity(Some(c))) if tree::alive(w, c) => Some(c),
        _ => None,
    }
}

/// The `LocalPlayer` of a client world (`None` on the server).
pub fn local_player_in(w: &World) -> Option<Entity> {
    let g = w.entity(crate::GAME_ID).ok()?;
    let s = find_service(w, g, "Players")?;
    match get_own(w, "Players", s, "LocalPlayer") {
        Some(Value::Entity(p)) => p,
        _ => None,
    }
}

/// The player whose character `model` is, if any.
pub fn player_of_character(w: &World, model: Entity) -> Option<Entity> {
    let g = w.entity(crate::GAME_ID).ok()?;
    players_in(w, g)
        .into_iter()
        .find(|p| character_of(w, *p) == Some(model))
}

fn user_id(w: &World, player: Entity) -> i64 {
    match get_own(w, "Player", player, "UserId") {
        Some(Value::Int(i)) => i,
        _ => 0,
    }
}

/// A property read through the class layer, when the class has it (classes
/// of other WPs may not exist yet).
fn prop(w: &World, classes: &ClassRegistry, e: Entity, name: &str) -> Option<Value> {
    classes.get_property(w, e, name).ok()
}

fn prop_bool(w: &World, classes: &ClassRegistry, e: Entity, name: &str, default: bool) -> bool {
    match prop(w, classes, e, name) {
        Some(Value::Bool(b)) => b,
        _ => default,
    }
}

/// The players in the game, in join order.
pub fn players_in(w: &World, game: Entity) -> Vec<Entity> {
    let Some(players) = find_service(w, game, "Players") else {
        return Vec::new();
    };
    tree::children(w, players)
        .into_iter()
        .filter(|p| tree::class(w, *p) == Some("Player") && !tree::is_destroyed(w, *p))
        .collect()
}

pub(crate) fn players_of(lua: &Lua) -> Result<Vec<Entity>> {
    let g = game(lua)?;
    with_world(lua, |w| players_in(w, g))
}

fn players_service(lua: &Lua) -> Result<Entity> {
    let (c, g) = (classes(lua)?, game(lua)?);
    with_world(lua, |w| ensure_service(w, &c, g, "Players"))?.map_err(compat_error)
}

/// Adds a player to the server world of `vm` and fires `PlayerAdded`.
/// Returns the `Player` (or why it could not join, `RE9302`), and the
/// diagnostics of errors that script handlers raised meanwhile.
pub fn add_player(
    vm: &CompatVm,
    host: &mut dyn ScriptHost,
    spec: &PlayerSpec,
) -> (std::result::Result<Entity, Diagnostic>, Vec<Diagnostic>) {
    let mut out = Err(step_error("the player did not join"));
    let diags = vm.run(host, |lua| {
        out = add_player_lua(lua, spec).map_err(|e| step_error(e.to_string()));
        Ok(())
    });
    (out, diags)
}

fn step_error(msg: impl Into<String>) -> Diagnostic {
    Diagnostic::error(crate::codes::PLAYER_STEP, msg)
}

/// Checks that `player` is a `Player` in the game of the VM.
fn check_player(lua: &Lua, player: Entity, what: &str) -> std::result::Result<(), Diagnostic> {
    let ok = with_world(lua, |w| {
        tree::alive(w, player)
            && tree::class(w, player) == Some("Player")
            && tree::parent(w, player).is_some_and(|p| tree::class(w, p) == Some("Players"))
    })
    .unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(step_error(format!(
            "{what}: the entity is not a Player in the game"
        )))
    }
}

fn add_player_lua(lua: &Lua, spec: &PlayerSpec) -> Result<Entity> {
    let c = classes(lua)?;
    let players = players_service(lua)?;
    let player = with_world(lua, |w| -> std::result::Result<Entity, CompatError> {
        let p = spawn_instance(w, &c, "Player", None, &spec.name, Some(players))?;
        set_own(w, "Player", p, "UserId", Value::Int(spec.user_id))?;
        set_own(w, "Player", p, "DisplayName", Value::Str(spec.name.clone()))?;
        spawn_instance(w, &c, "Backpack", None, "Backpack", Some(p))?;
        if spec.local
            && let Some(link) = w.ecs.get_resource::<NetLink>().cloned()
        {
            let mut s = link.lock();
            if s.local_player.is_none() {
                s.local_player = w.id_of(p).map(str::to_string);
            }
        }
        Ok(p)
    })?
    .map_err(compat_error)?;
    events::after_write(lua)?;
    signal::fire(
        lua,
        players,
        "PlayerAdded",
        MultiValue::from_iter([push(lua, player)?]),
    )?;
    pending(lua).0.push(player);
    Ok(player)
}

fn pending(lua: &Lua) -> mlua::AppDataRefMut<'_, PendingSpawns> {
    if lua.app_data_ref::<PendingSpawns>().is_none() {
        lua.set_app_data(PendingSpawns::default());
    }
    lua.app_data_mut::<PendingSpawns>().expect("just set")
}

/// Removes a player from the server world of `vm`: `PlayerRemoving` fires,
/// the character is destroyed, `InvokeClient` calls waiting on the player
/// fail, and the `Player` is destroyed. Refused (`RE9302`) when `player` is
/// not a `Player` in the game.
pub fn remove_player(
    vm: &CompatVm,
    host: &mut dyn ScriptHost,
    player: Entity,
) -> (std::result::Result<(), Diagnostic>, Vec<Diagnostic>) {
    let mut out = Ok(());
    let diags = vm.run(host, |lua| {
        out = check_player(lua, player, "remove_player");
        if out.is_ok() {
            remove_player_lua(lua, player)?;
        }
        Ok(())
    });
    (out, diags)
}

fn remove_player_lua(lua: &Lua, player: Entity) -> Result<()> {
    let ok = with_world(lua, |w| {
        tree::alive(w, player) && tree::class(w, player) == Some("Player")
    })?;
    if !ok {
        return Ok(());
    }
    let players = players_service(lua)?;
    signal::fire(
        lua,
        players,
        "PlayerRemoving",
        MultiValue::from_iter([push(lua, player)?]),
    )?;
    if let Some(ch) = with_world(lua, |w| character_of(w, player))? {
        signal::fire(
            lua,
            player,
            "CharacterRemoving",
            MultiValue::from_iter([push(lua, ch)?]),
        )?;
        destroy_after_handlers(lua, ch)?;
    }
    let id = with_world(lua, |w| w.id_of(player).map(str::to_string))?.unwrap_or_default();
    net::remotes::fail_calls_for(lua, &id)?;
    if let Some(link) = net_link(lua)? {
        let mut s = link.lock();
        if s.local_player.as_deref() == Some(id.as_str()) {
            s.local_player = None;
        }
    }
    pending(lua).0.retain(|p| *p != player);
    destroy_after_handlers(lua, player)
}

/// Destroys `e` once the handlers already queued (`PlayerRemoving`,
/// `CharacterRemoving`) have run, so they still see it in the game.
pub(crate) fn destroy_after_handlers(lua: &Lua, e: Entity) -> Result<()> {
    let f = lua.create_function(move |lua, ()| instance::destroy(lua, e))?;
    re_script::sched::defer(lua, f, MultiValue::new(), "Destroy", None)?;
    Ok(())
}

fn net_link(lua: &Lua) -> Result<Option<NetLink>> {
    with_world(lua, |w| w.ecs.get_resource::<NetLink>().cloned())
}

/// The server's per-frame work: loads the characters of players that
/// joined, and respawns characters that died (a `Humanoid` child whose
/// `Health` is 0) after `Players.RespawnTime`, while `CharacterAutoLoads` is
/// on.
pub(crate) fn server_frame(lua: &Lua) -> Result<()> {
    let due = std::mem::take(&mut pending(lua).0);
    let (c, players) = (classes(lua)?, players_service(lua)?);
    let (auto, respawn) = with_world(lua, |w| {
        (
            prop_bool(w, &c, players, "CharacterAutoLoads", true),
            match prop(w, &c, players, "RespawnTime") {
                Some(Value::Float(t)) => t,
                Some(Value::Int(t)) => t as f64,
                _ => 5.0,
            },
        )
    })?;
    if !auto {
        return Ok(());
    }
    for p in due {
        let alive = with_world(lua, |w| tree::alive(w, p))?;
        if alive {
            load_character(lua, p)?;
        }
    }
    let g = game(lua)?;
    let dead: Vec<(Entity, Entity)> = with_world(lua, |w| {
        players_in(w, g)
            .into_iter()
            .filter_map(|p| {
                let ch = character_of(w, p)?;
                let died = tree::children(w, ch).into_iter().any(|h| {
                    tree::class(w, h) == Some("Humanoid")
                        && matches!(prop(w, &c, h, "Health"), Some(Value::Float(x)) if x <= 0.0)
                });
                died.then_some((p, ch))
            })
            .collect()
    })?;
    for (p, ch) in dead {
        if !respawning(lua).0.insert(p) {
            continue;
        }
        let f = lua.create_function(move |lua, ()| {
            respawning(lua).0.remove(&p);
            let same = with_world(lua, |w| tree::alive(w, p) && character_of(w, p) == Some(ch))?;
            if same {
                load_character(lua, p)?;
            }
            Ok(())
        })?;
        net::after(lua, respawn.max(0.0), "Respawn", f)?;
    }
    Ok(())
}

/// The players whose respawn is scheduled.
#[derive(Default)]
struct Respawning(HashSet<Entity>);

fn respawning(lua: &Lua) -> mlua::AppDataRefMut<'_, Respawning> {
    if lua.app_data_ref::<Respawning>().is_none() {
        lua.set_app_data(Respawning::default());
    }
    lua.app_data_mut::<Respawning>().expect("just set")
}

/// Registry key of the Luau helper that places a character at a spawn.
const PLACE_KEY: &str = "re_compat.players.place";

fn place_fn(lua: &Lua) -> Result<Function> {
    if let Some(f) = lua.named_registry_value::<Option<Function>>(PLACE_KEY)? {
        return Ok(f);
    }
    // The spawn's and the model's members belong to other WPs; when they are
    // missing the character stays where it was made.
    let f: Function = lua
        .load(
            r#"return function(model, spawn)
                return pcall(function()
                    local cf = spawn.CFrame + Vector3.new(0, spawn.Size.Y / 2 + 3, 0)
                    model:PivotTo(cf)
                end)
            end"#,
        )
        .set_name("=LoadCharacter")
        .call(())?;
    lua.set_named_registry_value(PLACE_KEY, &f)?;
    Ok(f)
}

/// The spawn a player's character loads at: the player's `RespawnLocation`
/// when set, else the first enabled `SpawnLocation` in `Workspace` (in tree
/// order) that is neutral or matches the player's team.
fn spawn_for(w: &World, c: &ClassRegistry, workspace: Entity, player: Entity) -> Option<Entity> {
    if let Some(Value::Entity(Some(r))) = get_own(w, "Player", player, "RespawnLocation")
        && tree::alive(w, r)
    {
        return Some(r);
    }
    let neutral = matches!(
        get_own(w, "Player", player, "Neutral"),
        Some(Value::Bool(true)) | None
    );
    let team = prop(w, c, player, "TeamColor");
    tree::descendants(w, workspace).into_iter().find(|s| {
        tree::class(w, *s) == Some("SpawnLocation")
            && prop_bool(w, c, *s, "Enabled", true)
            && (prop_bool(w, c, *s, "Neutral", true)
                || (!neutral && team.is_some() && prop(w, c, *s, "TeamColor") == team))
    })
}

fn clone_children_into(w: &mut World, from: Entity, into: Entity) {
    for child in tree::children(w, from) {
        if let Some(copy) = clone_subtree(w, child) {
            let _ = tree::set_parent(w, copy, Some(into));
        }
    }
}

/// Loads (or reloads) `player`'s character on the server; see the module
/// docs. Returns the new character.
pub fn load_character(lua: &Lua, player: Entity) -> Result<Entity> {
    if !net::is_server(lua)? {
        return Err(lua_error(
            crate::codes::REMOTE_CALL,
            "LoadCharacter can only be called from the server",
        ));
    }
    let (c, g) = (classes(lua)?, game(lua)?);
    if let Some(old) = with_world(lua, |w| character_of(w, player))? {
        signal::fire(
            lua,
            player,
            "CharacterRemoving",
            MultiValue::from_iter([push(lua, old)?]),
        )?;
        destroy_after_handlers(lua, old)?;
    }
    let name = with_world(lua, |w| tree::name(w, player))?;
    let starter = with_world(lua, |w| {
        find_service(w, g, "StarterPlayer")
            .and_then(|sp| tree::find_child(w, sp, "StarterCharacter", false))
            .filter(|s| tree::class(w, *s) == Some("Model"))
    })?;
    let model = match starter {
        Some(s) => with_world(lua, |w| clone_subtree(w, s))?,
        None => default_character(lua)?,
    };
    // The rig is made of parts; until the part classes exist (WP13) the
    // default character is an empty `Model`.
    let rig = c.class("Part").is_ok();
    let model = match model {
        Some(m) => m,
        None if !rig => with_world(lua, |w| spawn_instance(w, &c, "Model", None, &name, None))?
            .map_err(compat_error)?,
        None => with_world(lua, |w| {
            crate::humanoid::rig::build(
                w,
                &c,
                DEFAULT_RIG,
                &name,
                None,
                crate::datatypes::CFrame::IDENTITY,
            )
        })?
        .map_err(compat_error)?,
    };
    let workspace =
        with_world(lua, |w| ensure_service(w, &c, g, "Workspace"))?.map_err(compat_error)?;
    let spawn = with_world(lua, |w| {
        tree::set_name(w, model, &name);
        if let Some(sp) = find_service(w, g, "StarterPlayer")
            && let Some(scripts) = tree::find_child(w, sp, "StarterCharacterScripts", false)
        {
            clone_children_into(w, scripts, model);
        }
        spawn_for(w, &c, workspace, player)
    })?;
    if let Some(spawn) = spawn {
        let placed: MultiValue = place_fn(lua)?.call((push(lua, model)?, push(lua, spawn)?))?;
        let _ = placed;
        let duration = with_world(lua, |w| match prop(w, &c, spawn, "Duration") {
            Some(Value::Float(d)) => d,
            Some(Value::Int(d)) => d as f64,
            _ => 0.0,
        })?;
        if duration > 0.0 {
            let ff = with_world(lua, |w| {
                spawn_instance(w, &c, "ForceField", None, "ForceField", None)
            })?
            .map_err(compat_error)?;
            with_world(lua, |w| tree::set_parent(w, ff, Some(model)))?.map_err(compat_error)?;
            let remove = lua.create_function(move |lua, ()| instance::destroy(lua, ff))?;
            net::after(lua, duration, "ForceField", remove)?;
        }
    }
    let old_tools = with_world(lua, |w| -> std::result::Result<Vec<Entity>, CompatError> {
        tree::set_parent(w, model, Some(workspace))?;
        set_own(w, "Player", player, "Character", Value::Entity(Some(model)))?;
        net::set_network_owner(w, model, Some(player));
        let mut old = Vec::new();
        if let Some(bp) = tree::find_child(w, player, "Backpack", false) {
            old = tree::children(w, bp);
            if let Some(pack) = find_service(w, g, "StarterPack") {
                clone_children_into(w, pack, bp);
            }
        }
        Ok(old)
    })?
    .map_err(compat_error)?;
    for t in old_tools {
        instance::destroy(lua, t)?;
    }
    events::after_write(lua)?;
    let m = push(lua, model)?;
    signal::fire(
        lua,
        player,
        "CharacterAdded",
        MultiValue::from_iter([m.clone()]),
    )?;
    signal::fire(
        lua,
        player,
        "CharacterAppearanceLoaded",
        MultiValue::from_iter([m]),
    )?;
    Ok(model)
}

/// The rig of the default character when no description makes one.
const DEFAULT_RIG: &str = "R15";

/// The default character from WP14's `CreateHumanoidModelFromDescription`,
/// when that is registered.
fn default_character(lua: &Lua) -> Result<Option<Entity>> {
    let st = state(lua)?;
    let Some(imp) = st
        .setup
        .method("Players", "CreateHumanoidModelFromDescription")
    else {
        return Ok(None);
    };
    let players = players_service(lua)?;
    let r = (imp.f)(
        lua,
        players,
        MultiValue::from_iter([LuaValue::Nil, LuaValue::Nil]),
    )?;
    match r.into_iter().next() {
        Some(LuaValue::UserData(ud)) => Ok(instance::entity_of(&ud)),
        _ => Ok(None),
    }
}

/// The client's per-frame work: player and character events from what
/// replication brought, and the local player's `PlayerScripts` and
/// `PlayerGui`.
pub(crate) fn client_frame(lua: &Lua, link: &NetLink) -> Result<()> {
    if link.side != Side::Client {
        return Ok(());
    }
    if lua.app_data_ref::<ClientView>().is_none() {
        lua.set_app_data(ClientView::default());
    }
    let g = game(lua)?;
    let c = classes(lua)?;
    let local = link.local_player();
    // What the server removed is still in place until this pump destroys
    // it, so the removal events see it whole.
    let gone = net::removing(lua)?;
    let now: Vec<Entity> = with_world(lua, |w| players_in(w, g))?
        .into_iter()
        .filter(|p| !gone.contains(p))
        .collect();
    let (joined, left, changes) = {
        let mut view = lua.app_data_mut::<ClientView>().expect("set above");
        let joined: Vec<Entity> = now
            .iter()
            .copied()
            .filter(|p| !view.players.contains(p))
            .collect();
        let left: Vec<Entity> = view
            .players
            .iter()
            .copied()
            .filter(|p| !now.contains(p))
            .collect();
        view.players = now.clone();
        let mut changes = Vec::new();
        for p in &now {
            let ch = with_world(lua, |w| character_of(w, *p))?.filter(|c| !gone.contains(c));
            let old = view.characters.insert(*p, ch).flatten();
            if old != ch {
                changes.push((*p, old, ch));
            }
        }
        for p in &left {
            if let Some(Some(old)) = view.characters.remove(p) {
                changes.push((*p, Some(old), None));
            }
        }
        (joined, left, changes)
    };
    let players = with_world(lua, |w| find_service(w, g, "Players"))?;
    if let Some(s) = players {
        // `LocalPlayer` is a client-local write: the server never sets it.
        let me = with_world(lua, |w| net::value::instance_by_id(w, local.as_deref()))?;
        let changed = with_world(lua, |w| -> std::result::Result<bool, CompatError> {
            let old = get_own(w, "Players", s, "LocalPlayer");
            let new = Value::Entity(me);
            if old.as_ref() == Some(&new) {
                return Ok(false);
            }
            set_own(w, "Players", s, "LocalPlayer", new)?;
            Ok(true)
        })?
        .map_err(compat_error)?;
        if changed {
            events::after_write(lua)?;
        }
    }
    for p in &joined {
        if let Some(s) = players {
            signal::fire(
                lua,
                s,
                "PlayerAdded",
                MultiValue::from_iter([push(lua, *p)?]),
            )?;
        }
        let is_local = with_world(lua, |w| w.id_of(*p).map(str::to_string))? == local;
        if is_local {
            make_local_containers(lua, &c, g, *p)?;
        }
    }
    for p in &left {
        let was_local = lua
            .app_data_mut::<ClientView>()
            .is_some_and(|mut v| v.local_ready.remove(p));
        if was_local {
            net::remotes::fail_all_calls(lua, "the local player left before the server answered")?;
        }
        if let Some(s) = players {
            signal::fire(
                lua,
                s,
                "PlayerRemoving",
                MultiValue::from_iter([push(lua, *p)?]),
            )?;
        }
    }
    for (p, old, new) in changes {
        if let Some(old) = old {
            signal::fire(
                lua,
                p,
                "CharacterRemoving",
                MultiValue::from_iter([push(lua, old)?]),
            )?;
        }
        if let Some(new) = new {
            let ready = lua
                .app_data_ref::<ClientView>()
                .is_some_and(|v| v.local_ready.contains(&p));
            if ready {
                reset_gui(lua, &c, g, p)?;
            }
            signal::fire(
                lua,
                p,
                "CharacterAdded",
                MultiValue::from_iter([push(lua, new)?]),
            )?;
            signal::fire(
                lua,
                p,
                "CharacterAppearanceLoaded",
                MultiValue::from_iter([push(lua, new)?]),
            )?;
        }
    }
    Ok(())
}

/// Makes the local player's `PlayerScripts` and `PlayerGui` (client only).
fn make_local_containers(lua: &Lua, c: &ClassRegistry, g: Entity, p: Entity) -> Result<()> {
    with_world(lua, |w| -> std::result::Result<(), CompatError> {
        let scripts = match tree::find_child(w, p, "PlayerScripts", false) {
            Some(s) => s,
            None => spawn_instance(w, c, "PlayerScripts", None, "PlayerScripts", Some(p))?,
        };
        if let Some(sp) = find_service(w, g, "StarterPlayer")
            && let Some(from) = tree::find_child(w, sp, "StarterPlayerScripts", false)
        {
            clone_children_into(w, from, scripts);
        }
        let gui = match tree::find_child(w, p, "PlayerGui", false) {
            Some(s) => s,
            None => spawn_instance(w, c, "PlayerGui", None, "PlayerGui", Some(p))?,
        };
        if let Some(sg) = find_service(w, g, "StarterGui") {
            clone_children_into(w, sg, gui);
        }
        Ok(())
    })?
    .map_err(compat_error)?;
    if let Some(mut v) = lua.app_data_mut::<ClientView>() {
        v.local_ready.insert(p);
    }
    events::after_write(lua)
}

/// On respawn: the `PlayerGui` children whose `ResetOnSpawn` is on are
/// replaced by fresh copies of `StarterGui`'s.
fn reset_gui(lua: &Lua, c: &ClassRegistry, g: Entity, p: Entity) -> Result<()> {
    let (gui, old) = with_world(lua, |w| {
        let gui = tree::find_child(w, p, "PlayerGui", false);
        let old: Vec<Entity> = gui
            .map(|gui| {
                tree::children(w, gui)
                    .into_iter()
                    .filter(|x| prop_bool(w, c, *x, "ResetOnSpawn", true))
                    .collect()
            })
            .unwrap_or_default();
        (gui, old)
    })?;
    let Some(gui) = gui else { return Ok(()) };
    for x in old {
        instance::destroy(lua, x)?;
    }
    with_world(lua, |w| {
        if let Some(sg) = find_service(w, g, "StarterGui") {
            for child in tree::children(w, sg) {
                if prop_bool(w, c, child, "ResetOnSpawn", true)
                    && let Some(copy) = clone_subtree(w, child)
                {
                    let _ = tree::set_parent(w, copy, Some(gui));
                }
            }
        }
    })?;
    events::after_write(lua)
}

/// A player says `text` in chat (the harness's `say` step), on the server
/// world of `vm`: `Player.Chatted` fires on both sides, and a
/// `TextChatCommand` whose alias starts the message triggers on both sides.
/// Refused (`RE9302`) when `player` is not a `Player` in the game.
pub fn say(
    vm: &CompatVm,
    host: &mut dyn ScriptHost,
    player: Entity,
    text: &str,
) -> (std::result::Result<(), Diagnostic>, Vec<Diagnostic>) {
    let mut out = Ok(());
    let diags = vm.run(host, |lua| {
        out = check_player(lua, player, "say");
        if out.is_ok() {
            say_lua(lua, player, text)?;
        }
        Ok(())
    });
    (out, diags)
}

fn say_lua(lua: &Lua, player: Entity, text: &str) -> Result<()> {
    let source = text_source(lua, player)?;
    chat_here(lua, player, source, text)?;
    if let Some(link) = net_link(lua)? {
        let id = with_world(lua, |w| w.id_of(player).map(str::to_string))?.unwrap_or_default();
        let mut s = link.lock();
        if s.local_player.is_some() {
            s.to_client.push_back(net::Msg::Chat {
                player: id,
                text: text.to_string(),
            });
        }
    }
    Ok(())
}

/// Fires `Chatted` and the matching commands in this VM.
fn chat_here(lua: &Lua, player: Entity, source: Option<Entity>, text: &str) -> Result<()> {
    let s = lua.create_string(text)?;
    signal::fire(
        lua,
        player,
        "Chatted",
        MultiValue::from_iter([LuaValue::String(s.clone())]),
    )?;
    let g = game(lua)?;
    let c = classes(lua)?;
    let first = text.split_whitespace().next().unwrap_or("").to_lowercase();
    if first.is_empty() {
        return Ok(());
    }
    let commands = with_world(lua, |w| {
        let Some(tcs) = find_service(w, g, "TextChatService") else {
            return Vec::new();
        };
        tree::descendants(w, tcs)
            .into_iter()
            .filter(|x| tree::class(w, *x) == Some("TextChatCommand"))
            .filter(|x| prop_bool(w, &c, *x, "Enabled", true))
            .filter(|x| {
                ["PrimaryAlias", "SecondaryAlias"].iter().any(|a| {
                    matches!(prop(w, &c, *x, a), Some(Value::Str(s)) if !s.is_empty() && s.to_lowercase() == first)
                })
            })
            .collect::<Vec<_>>()
    })?;
    for cmd in commands {
        let src = match source {
            Some(e) => push(lua, e)?,
            None => LuaValue::Nil,
        };
        signal::fire(
            lua,
            cmd,
            "Triggered",
            MultiValue::from_iter([src, LuaValue::String(s.clone())]),
        )?;
    }
    Ok(())
}

/// The player's `TextSource` in `TextChatService.TextChannels.RBXGeneral`,
/// made on the server when missing (it replicates).
fn text_source(lua: &Lua, player: Entity) -> Result<Option<Entity>> {
    let (c, g) = (classes(lua)?, game(lua)?);
    let r = with_world(
        lua,
        |w| -> std::result::Result<Option<Entity>, CompatError> {
            let uid = user_id(w, player);
            let name = tree::name(w, player);
            let tcs = ensure_service(w, &c, g, "TextChatService")?;
            let channels = match tree::find_child(w, tcs, "TextChannels", false) {
                Some(x) => x,
                None => spawn_instance(w, &c, "Folder", None, "TextChannels", Some(tcs))?,
            };
            let general = match tree::find_child(w, channels, "RBXGeneral", false) {
                Some(x) => x,
                None => spawn_instance(w, &c, "TextChannel", None, "RBXGeneral", Some(channels))?,
            };
            let existing = tree::children(w, general).into_iter().find(|x| {
            tree::class(w, *x) == Some("TextSource")
                && matches!(get_own(w, "TextSource", *x, "UserId"), Some(Value::Int(u)) if u == uid)
        });
            if let Some(x) = existing {
                return Ok(Some(x));
            }
            let src = spawn_instance(w, &c, "TextSource", None, &name, Some(general))?;
            set_own(w, "TextSource", src, "UserId", Value::Int(uid))?;
            Ok(Some(src))
        },
    )?
    .map_err(compat_error)?;
    events::after_write(lua)?;
    Ok(r)
}

/// A chat message that arrived on the client.
pub(crate) fn chat_on_client(lua: &Lua, _link: &NetLink, player: &str, text: &str) -> Result<()> {
    let p = with_world(lua, |w| net::value::instance_by_id(w, Some(player)))?;
    let Some(p) = p else { return Ok(()) };
    let source = with_world(lua, |w| {
        let uid = user_id(w, p);
        let sources: Vec<Entity> = w
            .ecs
            .query::<(Entity, &crate::class::ClassMarker)>()
            .iter(&w.ecs)
            .filter(|(_, m)| m.class == "TextSource")
            .map(|(e, _)| e)
            .collect();
        sources.into_iter().find(|x| {
            !tree::is_destroyed(w, *x)
                && matches!(get_own(w, "TextSource", *x, "UserId"), Some(Value::Int(u)) if u == uid)
        })
    })?;
    chat_here(lua, p, source, text)
}

// Methods.

fn get_players(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    ret(lua, push_list(lua, &players_of(lua)?)?)
}

fn get_player_from_character(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let v: LuaValue = args(lua, a, "GetPlayerFromCharacter")?;
    let LuaValue::UserData(ud) = v else {
        return ret(lua, LuaValue::Nil);
    };
    let Some(model) = instance::entity_of(&ud) else {
        return ret(lua, LuaValue::Nil);
    };
    let g = game(lua)?;
    let found = with_world(lua, |w| {
        players_in(w, g)
            .into_iter()
            .find(|p| character_of(w, *p) == Some(model))
    })?;
    match found {
        Some(p) => ret(lua, push(lua, p)?),
        None => ret(lua, LuaValue::Nil),
    }
}

fn get_player_by_user_id(lua: &Lua, _: Entity, a: MultiValue) -> Result<MultiValue> {
    let id: f64 = args(lua, a, "GetPlayerByUserId")?;
    let g = game(lua)?;
    let found = with_world(lua, |w| {
        players_in(w, g)
            .into_iter()
            .find(|p| user_id(w, *p) as f64 == id)
    })?;
    match found {
        Some(p) => ret(lua, push(lua, p)?),
        None => ret(lua, LuaValue::Nil),
    }
}

fn load_character_method(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    let in_game = with_world(lua, |w| {
        tree::parent(w, e).is_some_and(|p| tree::class(w, p) == Some("Players"))
    })?;
    if !in_game {
        return Err(lua_error(
            crate::codes::REMOTE_CALL,
            "LoadCharacter: the player is not in the game",
        ));
    }
    load_character(lua, e)?;
    ret(lua, ())
}

fn kick(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let msg: Option<String> = args(lua, a, "Kick")?;
    let name = with_world(lua, |w| tree::name(w, e))?;
    let text = match msg.as_deref() {
        Some(m) if !m.is_empty() => format!("{name} was kicked: {m}"),
        _ => format!("{name} was kicked"),
    };
    if net::is_server(lua)? {
        re_script::vm::output(lua, false, text);
        remove_player_lua(lua, e)?;
    } else {
        // A client can only kick itself: the server removes the player.
        let id = with_world(lua, |w| w.id_of(e).map(str::to_string))?;
        let link = net_link(lua)?;
        match link {
            Some(link) if id.is_some() && id == link.local_player() => {
                re_script::vm::output(lua, false, text);
                link.lock().to_server.push_back(net::Msg::Kick {
                    player: id.unwrap_or_default(),
                });
            }
            _ => {
                return Err(lua_error(
                    re_script::codes::BAD_ARGUMENT,
                    "Kick: a client can only kick its own player",
                ));
            }
        }
    }
    ret(lua, ())
}

/// Removes a player a client kicked (on the server).
pub(crate) fn kicked(lua: &Lua, player: &str) -> Result<()> {
    if let Some(p) = with_world(lua, |w| net::value::instance_by_id(w, Some(player)))? {
        remove_player_lua(lua, p)?;
    }
    Ok(())
}

/// Registry key of the Luau forwarder of `Player:Move`.
const MOVE_KEY: &str = "re_compat.players.move";

fn move_method(lua: &Lua, e: Entity, a: MultiValue) -> Result<MultiValue> {
    let f: Function = match lua.named_registry_value::<Option<Function>>(MOVE_KEY)? {
        Some(f) => f,
        None => {
            let f: Function = lua
                .load(
                    r#"return function(character, dir, relative)
                        local h = character and character:FindFirstChildOfClass("Humanoid")
                        if h then h:Move(dir, relative) end
                    end"#,
                )
                .set_name("=Player.Move")
                .call(())?;
            lua.set_named_registry_value(MOVE_KEY, &f)?;
            f
        }
    };
    let ch = with_world(lua, |w| character_of(w, e))?;
    let ch = match ch {
        Some(c) => push(lua, c)?,
        None => LuaValue::Nil,
    };
    let mut all = MultiValue::from_iter([ch]);
    all.extend(a);
    f.call::<()>(all)?;
    ret(lua, ())
}

fn get_network_ping(lua: &Lua, _: Entity, _: MultiValue) -> Result<MultiValue> {
    ret(lua, 0.0)
}

fn has_appearance_loaded(lua: &Lua, e: Entity, _: MultiValue) -> Result<MultiValue> {
    ret(lua, with_world(lua, |w| character_of(w, e).is_some())?)
}

/// The implementation of an offline stub: raises `RE9002` naming the member.
pub(crate) fn offline(class: &'static str, member: &'static str) -> Arc<crate::runtime::MethodFn> {
    Arc::new(move |lua, _, _| {
        // Counted with the unsupported members (caught calls too), so a boot
        // report shows what a game needs from online services.
        crate::runtime::count_unsupported(lua, format!("{class}.{member} (RE9002)"));
        Err(compat_error(CompatError::NotAvailableOffline {
            member: format!("{class}.{member}"),
        }))
    })
}

type Imp = fn(&Lua, Entity, MultiValue) -> Result<MultiValue>;

/// Registers `Players` and `Player`.
pub fn register(b: &mut Bindings) {
    let plain: &[(&str, &str, Imp)] = &[
        ("Players", "GetPlayers", get_players),
        (
            "Players",
            "GetPlayerFromCharacter",
            get_player_from_character,
        ),
        ("Players", "GetPlayerByUserId", get_player_by_user_id),
        ("Player", "LoadCharacter", load_character_method),
        ("Player", "LoadCharacterAsync", load_character_method),
        ("Player", "Kick", kick),
        ("Player", "Move", move_method),
        ("Player", "GetNetworkPing", get_network_ping),
        ("Player", "HasAppearanceLoaded", has_appearance_loaded),
    ];
    for (class, name, f) in plain {
        b.method(class, name, Arc::new(*f));
    }
    for (class, name) in [
        ("Players", "GetUserIdFromNameAsync"),
        ("Players", "GetNameFromUserIdAsync"),
        ("Players", "GetUserThumbnailAsync"),
        ("Players", "GetFriendsAsync"),
        ("Player", "IsInGroup"),
        ("Player", "GetRankInGroup"),
        ("Player", "GetRoleInGroup"),
        ("Player", "IsFriendsWith"),
    ] {
        b.method(class, name, offline(class, name));
    }
}
