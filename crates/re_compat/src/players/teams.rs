//! `Team`, `Teams` and the team members of `Player`.
//!
//! * A player's team is not stored: it is the first `Team` child of `Teams`
//!   whose `TeamColor` is the player's, when the player's `Neutral` is false
//!   ([`team_of`]). `Player.Team` reads it; setting it writes `TeamColor` and
//!   `Neutral`, and a script's write of `Player.TeamColor` clears `Neutral`
//!   ([`team_color_written`]), so both put the player on that team.
//! * A player who joins goes on the `AutoAssignable` team with the fewest
//!   players, the first in child order on a tie, or stays neutral when no
//!   team is auto-assignable ([`auto_assign`]).
//! * Each frame, on the server and on the client, [`frame`] compares each
//!   player's team with the one it saw last and fires `Team.PlayerRemoved`
//!   on the old team and `Team.PlayerAdded` on the new one; a player who
//!   leaves the game leaves their team ([`left`]).
//! * On the server, a character that starts touching an enabled
//!   `SpawnLocation` whose `AllowTeamChangeOnTouch` is on gives its player
//!   the spawn's `TeamColor` and `Neutral` ([`touched`]).

use std::collections::BTreeMap;
use std::sync::Arc;

use mlua::{Lua, MultiValue, Result};
use re_reflect::Value;
use re_scene::{Entity, World};
use re_script::codes::BAD_ARGUMENT;

use super::{game, get_own, players_in, set_own};
use crate::class::Accessor;
use crate::datatypes::{BrickColor, brick_value};
use crate::error::CompatError;
use crate::instance::methods_api::ret;
use crate::instance::{push, push_list, tree};
use crate::runtime::{Bindings, compat_error, with_world};
use crate::services::core::find_service;
use crate::signal::{self, events};

/// The `Team` children of the game's `Teams`, in child order.
pub fn teams_in(w: &World, game: Entity) -> Vec<Entity> {
    let Some(teams) = find_service(w, game, "Teams") else {
        return Vec::new();
    };
    tree::children(w, teams)
        .into_iter()
        .filter(|t| tree::class(w, *t) == Some("Team") && !tree::is_destroyed(w, *t))
        .collect()
}

/// The stored `TeamColor` of a `Player`, a `Team` or a `SpawnLocation`.
fn color(w: &World, class: &str, e: Entity) -> Option<Value> {
    get_own(w, class, e, "TeamColor")
}

/// The team `player` is on: when its `Neutral` is false, the first team
/// whose `TeamColor` is the player's.
pub fn team_of(w: &World, player: Entity) -> Option<Entity> {
    if get_own(w, "Player", player, "Neutral") != Some(Value::Bool(false)) {
        return None;
    }
    let mine = color(w, "Player", player)?;
    let game = w.entity(crate::GAME_ID).ok()?;
    teams_in(w, game)
        .into_iter()
        .find(|t| color(w, "Team", *t).as_ref() == Some(&mine))
}

/// Puts `player` on `team` (`None`: makes it neutral).
fn join(
    w: &mut World,
    player: Entity,
    team: Option<Entity>,
) -> std::result::Result<(), CompatError> {
    let Some(team) = team else {
        return set_own(w, "Player", player, "Neutral", Value::Bool(true));
    };
    let c = match color(w, "Team", team) {
        Some(c) => c,
        None => brick_value(BrickColor::WHITE).map_err(|e| bad(e.to_string()))?,
    };
    set_own(w, "Player", player, "TeamColor", c)?;
    set_own(w, "Player", player, "Neutral", Value::Bool(false))
}

fn bad(message: impl Into<String>) -> CompatError {
    CompatError::Runtime {
        code: BAD_ARGUMENT,
        message: message.into(),
    }
}

/// The builtin `Player.Team`.
pub(crate) fn accessors() -> Vec<(&'static str, &'static str, Accessor)> {
    vec![(
        "Player",
        "Team",
        Accessor {
            get: Arc::new(|w, e| Ok(Value::Entity(team_of(w, e)))),
            set: Some(Arc::new(|w, e, v| match v {
                Value::Entity(None) => join(w, e, None),
                Value::Entity(Some(t)) if tree::class(w, t) == Some("Team") => join(w, e, Some(t)),
                _ => Err(bad("Player.Team must be a Team or nil")),
            })),
        },
    )]
}

/// A script wrote `TeamColor` of `e`: a player who writes it is no longer
/// neutral, so it joins the team of that colour.
pub(crate) fn team_color_written(lua: &Lua, e: Entity) -> Result<()> {
    with_world(lua, |w| {
        if tree::class(w, e) == Some("Player") {
            set_own(w, "Player", e, "Neutral", Value::Bool(false))
        } else {
            Ok(())
        }
    })?
    .map_err(compat_error)
}

/// Puts a player who just joined on the auto-assignable team with the
/// fewest players (the first in child order on a tie); with none, the
/// player is neutral.
pub(crate) fn auto_assign(
    w: &mut World,
    game: Entity,
    player: Entity,
) -> std::result::Result<(), CompatError> {
    let mut best: Option<(usize, Entity)> = None;
    for t in teams_in(w, game) {
        if get_own(w, "Team", t, "AutoAssignable") != Some(Value::Bool(true)) {
            continue;
        }
        let n = players_in(w, game)
            .into_iter()
            .filter(|p| *p != player && team_of(w, *p) == Some(t))
            .count();
        if best.is_none_or(|(m, _)| n < m) {
            best = Some((n, t));
        }
    }
    join(w, player, best.map(|(_, t)| t))
}

/// Each player's team when [`frame`] last looked (players on no team are
/// left out).
#[derive(Default)]
struct Seen(BTreeMap<Entity, Entity>);

fn seen(lua: &Lua) -> mlua::AppDataRefMut<'_, Seen> {
    if lua.app_data_ref::<Seen>().is_none() {
        lua.set_app_data(Seen::default());
    }
    lua.app_data_mut::<Seen>().expect("just set")
}

/// Fires `Team.PlayerRemoved` and `Team.PlayerAdded` for the players among
/// `players` whose team changed since the last call: every removal first,
/// then every addition, each in the order of `players`.
pub(crate) fn frame(lua: &Lua, players: &[Entity]) -> Result<()> {
    let now: Vec<(Entity, Option<Entity>)> = with_world(lua, |w| {
        players.iter().map(|&p| (p, team_of(w, p))).collect()
    })?;
    let mut changes = Vec::new();
    {
        let mut seen = seen(lua);
        for &(p, team) in &now {
            let before = match team {
                Some(t) => seen.0.insert(p, t),
                None => seen.0.remove(&p),
            };
            if before != team {
                changes.push((p, before, team));
            }
        }
    }
    for (p, before, _) in &changes {
        if let Some(t) = before {
            fire(lua, *t, "PlayerRemoved", *p)?;
        }
    }
    for (p, _, team) in &changes {
        if let Some(t) = team {
            fire(lua, *t, "PlayerAdded", *p)?;
        }
    }
    Ok(())
}

/// `player` is leaving the game: it leaves the team [`frame`] last saw it
/// on.
pub(crate) fn left(lua: &Lua, player: Entity) -> Result<()> {
    let team = seen(lua).0.remove(&player);
    match team {
        Some(t) => fire(lua, t, "PlayerRemoved", player),
        None => Ok(()),
    }
}

fn fire(lua: &Lua, team: Entity, event: &str, player: Entity) -> Result<()> {
    let alive = with_world(lua, |w| {
        tree::alive(w, team) && !tree::is_destroyed(w, team) && tree::alive(w, player)
    })?;
    if alive {
        signal::fire(
            lua,
            team,
            event,
            MultiValue::from_iter([push(lua, player)?]),
        )?;
    }
    Ok(())
}

/// A character part began touching `other`, on the server: when `other` is
/// an enabled `SpawnLocation` with `AllowTeamChangeOnTouch`, the
/// character's player takes the spawn's `TeamColor` and `Neutral`.
pub(crate) fn touched(lua: &Lua, part: Entity, other: Entity) -> Result<()> {
    if !crate::net::is_server(lua)? {
        return Ok(());
    }
    let changed = with_world(lua, |w| -> std::result::Result<bool, CompatError> {
        if tree::class(w, other) != Some("SpawnLocation")
            || get_own(w, "SpawnLocation", other, "Enabled") == Some(Value::Bool(false))
            || get_own(w, "SpawnLocation", other, "AllowTeamChangeOnTouch")
                != Some(Value::Bool(true))
        {
            return Ok(false);
        }
        let Some(player) = tree::ancestors(w, part)
            .into_iter()
            .find_map(|m| super::player_of_character(w, m))
        else {
            return Ok(false);
        };
        let (Some(c), Some(n)) = (
            color(w, "SpawnLocation", other),
            get_own(w, "SpawnLocation", other, "Neutral"),
        ) else {
            return Ok(false);
        };
        if color(w, "Player", player).as_ref() == Some(&c)
            && get_own(w, "Player", player, "Neutral").as_ref() == Some(&n)
        {
            return Ok(false);
        }
        set_own(w, "Player", player, "TeamColor", c)?;
        set_own(w, "Player", player, "Neutral", n)?;
        Ok(true)
    })?
    .map_err(compat_error)?;
    if changed {
        events::after_write(lua)?;
    }
    Ok(())
}

fn get_players(lua: &Lua, team: Entity, _: MultiValue) -> Result<MultiValue> {
    let g = game(lua)?;
    let on = with_world(lua, |w| {
        players_in(w, g)
            .into_iter()
            .filter(|p| team_of(w, *p) == Some(team))
            .collect::<Vec<_>>()
    })?;
    ret(lua, push_list(lua, &on)?)
}

fn get_teams(lua: &Lua, teams: Entity, _: MultiValue) -> Result<MultiValue> {
    let list = with_world(lua, |w| {
        tree::children(w, teams)
            .into_iter()
            .filter(|t| tree::class(w, *t) == Some("Team") && !tree::is_destroyed(w, *t))
            .collect::<Vec<_>>()
    })?;
    ret(lua, push_list(lua, &list)?)
}

/// Registers the `Team` and `Teams` methods.
pub(crate) fn register(b: &mut Bindings) {
    b.method("Team", "GetPlayers", Arc::new(get_players));
    b.method("Teams", "GetTeams", Arc::new(get_teams));
    for (class, prop, a) in accessors() {
        b.accessor(class, prop, a);
    }
}
