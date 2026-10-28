//! `scene`: switching scenes, loading scenes beside the main one, and keeping
//! entities across switches (ADR-0059 §3). Requests answer at once with a
//! `SceneLoad`; the game applies them at the end of a frame (§4). The library
//! reaches the game through [`ScriptHost::game`](crate::ScriptHost::game) and
//! raises `RE3015` where no game runs.

use std::collections::BTreeMap;

use mlua::{AnyUserData, Lua, MultiValue, Result, UserData, UserDataFields, UserDataMethods};

use super::Lib;
use super::entity::alive;
use super::game::{GameAccess, SceneLoadState, SceneRefusal, SceneRequestKind};
use super::libs::entity_arg;
use crate::codes;
use crate::diag::ScriptError;
use crate::host::{ScriptHost, with_host};
use crate::runtime::ScriptRuntime;
use crate::scheduler::{self, WakeMode, YieldToken};
use crate::vm::ContextKind;

/// The declaration of `SceneLoad` for `engine.d.luau`.
pub(crate) const SCENE_LOAD_DECL: &str = "-- A scene load started by scene.switch or scene.load.
declare extern type SceneLoad with
    -- The scene's name.
    read name: string
    -- How far the load got, from 0 to 1.
    read progress: number
    -- True once the load is over, applied or failed.
    read done: boolean
    -- Why the load failed (nothing changed then), or nil.
    read error: string?
    -- Suspends this script thread until the load is over; returns at once when it is.
    function wait(self): ()
end";

/// Registry key of the yielding function behind `SceneLoad:wait`.
const WAIT_FN: &str = "re_script.scene_load_wait";

/// The threads parked in `SceneLoad:wait`, by load id (the VM's app data).
#[derive(Default)]
struct SceneWaits(BTreeMap<u64, Vec<YieldToken>>);

/// A load handle (`SceneLoad` in Luau).
#[derive(Clone, Copy, Debug)]
struct SceneLoadRef(u64);

impl UserData for SceneLoadRef {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("name", |lua, this| Ok(state(lua, this.0)?.name));
        fields.add_field_method_get("progress", |lua, this| Ok(state(lua, this.0)?.progress));
        fields.add_field_method_get("done", |lua, this| Ok(state(lua, this.0)?.done));
        fields.add_field_method_get("error", |lua, this| Ok(state(lua, this.0)?.error));
        fields.add_field_function_get("wait", |lua, _| {
            lua.named_registry_value::<mlua::Function>(WAIT_FN)
        });
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        super::strict_userdata(methods, "SceneLoad");
    }
}

/// The game, or `RE3015` naming `what` when this host runs none.
fn game<'h>(h: &'h mut dyn ScriptHost, what: &str) -> Result<&'h mut dyn GameAccess> {
    h.game().ok_or_else(|| {
        super::error(
            codes::SERVICE_UNAVAILABLE,
            format!("{what} is not available here: no game is running"),
        )
    })
}

/// The state of load `id`.
fn state(lua: &Lua, id: u64) -> Result<SceneLoadState> {
    with_host(lua, |h| {
        game(h, "SceneLoad")?.scene_load(id).ok_or_else(|| {
            super::error(
                codes::SERVICE_UNAVAILABLE,
                format!("scene load {id} belongs to another run"),
            )
        })
    })
}

/// A refusal as the error the calling script gets.
fn refused(game: &dyn GameAccess, what: &str, name: &str, why: SceneRefusal) -> mlua::Error {
    match why {
        SceneRefusal::Unknown => {
            let listed = game.scenes_listed();
            let hint = match closest(name, &listed) {
                Some(c) => format!(" (did you mean {c:?}?)"),
                None if listed.is_empty() => String::new(),
                None => format!("; the scenes are {}", listed.join(", ")),
            };
            super::error(
                codes::SCENE_UNKNOWN,
                format!("{what}: no scene is named {name:?}{hint}"),
            )
        }
        SceneRefusal::Refused(why) => super::error(codes::SCENE_REFUSED, format!("{what}: {why}")),
    }
}

/// The candidate closest to `name` by edit distance, when it is close enough
/// to be a typo.
fn closest<'a>(name: &str, candidates: &'a [String]) -> Option<&'a str> {
    candidates
        .iter()
        .map(|c| (levenshtein(name, c), c.as_str()))
        .min_by_key(|(d, _)| *d)
        .filter(|(d, c)| *d <= (c.len() / 3).max(2))
        .map(|(_, c)| c)
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != cb)).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

fn request(lua: &Lua, kind: SceneRequestKind, name: &str) -> Result<SceneLoadRef> {
    let what = match kind {
        SceneRequestKind::Switch => "scene.switch",
        SceneRequestKind::Load => "scene.load",
    };
    with_host(lua, |h| {
        let game = game(h, what)?;
        match game.scene_request(kind, name) {
            Ok(id) => Ok(SceneLoadRef(id)),
            Err(why) => Err(refused(game, what, name, why)),
        }
    })
}

/// The stable id of root entity `e` for `scene.keep` and `scene.is_kept`.
fn root_id(h: &mut dyn ScriptHost, e: re_scene::Entity, what: &str) -> Result<String> {
    let world = h.world();
    alive(world, e)?;
    let id = world.id_of(e).map(str::to_string).ok_or_else(|| {
        super::error(
            codes::SCENE_REFUSED,
            format!("{what}: the entity has no stable id"),
        )
    })?;
    if re_scene::hierarchy::parent(&world.ecs, e).is_some() {
        return Err(super::error(
            codes::SCENE_REFUSED,
            format!(
                "{what}: entity {id:?} is not a root (it has a parent); keep its root, which takes its subtree with it"
            ),
        ));
    }
    if world.ecs.get::<re_scene::Streamed>(e).is_some() {
        return Err(super::error(
            codes::SCENE_REFUSED,
            format!(
                "{what}: entity {id:?} is streamed, and streamed entities belong to the main scene"
            ),
        ));
    }
    Ok(id)
}

/// The `scene` library.
pub(crate) fn lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lua.set_app_data(SceneWaits::default());
    let raw = lua.create_function(|lua, this: AnyUserData| -> Result<MultiValue> {
        let id = this.borrow::<SceneLoadRef>()?.0;
        let load = state(lua, id)?;
        if load.done {
            return Ok(MultiValue::new());
        }
        let (marker, token) =
            scheduler::park(lua, &format!("SceneLoad:wait ({})", load.name), None)?;
        lua.app_data_mut::<SceneWaits>()
            .ok_or_else(|| mlua::Error::runtime("this VM has no scene waits"))?
            .0
            .entry(id)
            .or_default()
            .push(token);
        Ok(marker)
    })?;
    lua.set_named_registry_value(WAIT_FN, scheduler::yielding(lua, "SceneLoad:wait", raw)?)?;

    lib.func(
        lua,
        "switch",
        "(name: string) -> SceneLoad",
        "Unloads every scene and loads `name` in their place, keeping the kept entities; the change applies at the end of a frame.",
        |lua, name: String| request(lua, SceneRequestKind::Switch, &name),
    )?;
    lib.func(
        lua,
        "load",
        "(name: string) -> SceneLoad",
        "Loads `name` beside the loaded scenes; it applies at the end of a frame.",
        |lua, name: String| request(lua, SceneRequestKind::Load, &name),
    )?;
    lib.func(
        lua,
        "unload",
        "(name: string) -> ()",
        "Unloads a scene that scene.load added, at the end of this frame.",
        |lua, name: String| {
            with_host(lua, |h| {
                let game = game(h, "scene.unload")?;
                match game.scene_unload(&name) {
                    Ok(()) => Ok(()),
                    Err(why) => Err(refused(game, "scene.unload", &name, why)),
                }
            })
        },
    )?;
    lib.func(
        lua,
        "current",
        "() -> string",
        "The main scene: the entry scene at start, then the last one switched to.",
        |lua, ()| with_host(lua, |h| Ok(game(h, "scene.current")?.scene_current())),
    )?;
    lib.func(
        lua,
        "loaded",
        "() -> {string}",
        "The loaded scenes: the main scene, then the added ones in load order.",
        |lua, ()| with_host(lua, |h| Ok(game(h, "scene.loaded")?.scenes_loaded())),
    )?;
    lib.func(
        lua,
        "list",
        "() -> {string}",
        "Every scene the game can load (real.toml [scenes] and the entry scene), sorted.",
        |lua, ()| with_host(lua, |h| Ok(game(h, "scene.list")?.scenes_listed())),
    )?;
    lib.func(
        lua,
        "keep",
        "(entity: Entity, keep: boolean?) -> ()",
        "Keeps a root entity, with its subtree, across scene switches; `false` stops keeping it.",
        |lua, (entity, keep): (mlua::Value, Option<bool>)| {
            let e = entity_arg(&entity, "scene.keep")?;
            with_host(lua, |h| {
                let id = root_id(h, e, "scene.keep")?;
                game(h, "scene.keep")?.keep(&id, keep.unwrap_or(true));
                Ok(())
            })
        },
    )?;
    lib.func(
        lua,
        "is_kept",
        "(entity: Entity) -> boolean",
        "True if the entity is kept across scene switches.",
        |lua, entity: mlua::Value| {
            let e = entity_arg(&entity, "scene.is_kept")?;
            with_host(lua, |h| {
                let world = h.world();
                alive(world, e)?;
                let Some(id) = world.id_of(e).map(str::to_string) else {
                    return Ok(false);
                };
                Ok(game(h, "scene.is_kept")?.is_kept(&id))
            })
        },
    )?;
    Ok(())
}

/// Wakes the threads waiting in `SceneLoad:wait` for the loads in `done`
/// (every load the game ended, applied or failed, since the last call): they
/// run on in the next phase that runs scripts. `re_app` calls it at EndFrame.
///
/// # Errors
///
/// A context's scheduler cannot be reached.
pub fn wake_scene_loads(
    runtime: &ScriptRuntime,
    done: &[u64],
) -> std::result::Result<(), ScriptError> {
    if done.is_empty() {
        return Ok(());
    }
    for kind in ContextKind::ALL {
        let lua = runtime.context(kind).lua();
        let tokens: Vec<YieldToken> = match lua.app_data_mut::<SceneWaits>() {
            Some(mut waits) => done
                .iter()
                .filter_map(|id| waits.0.remove(id))
                .flatten()
                .collect(),
            None => continue,
        };
        for token in tokens {
            scheduler::wake(lua, token, MultiValue::new(), WakeMode::Deferred)
                .map_err(|e| ScriptError::from_mlua(kind.name(), &e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::closest;

    #[test]
    fn a_close_name_is_suggested_and_a_far_one_is_not() {
        let names = vec!["harbour".to_string(), "title".into(), "town".into()];
        assert_eq!(closest("towm", &names), Some("town"));
        assert_eq!(closest("harbor", &names), Some("harbour"));
        assert_eq!(closest("dungeon", &names), None);
    }
}
