//! Reading and changing the instance tree in a [`World`].
//!
//! An instance is a live entity with a [`ClassMarker`]. The tree is the
//! native hierarchy (`ChildOf` / `Children`); entities without a class marker
//! (native entities parented under instances) are not instances, so the
//! queries here skip them and their subtrees.

use re_scene::ecs as bevy_ecs;
use re_scene::ecs::component::Component;
use re_scene::ecs::hierarchy::ChildOf;
use re_scene::{Entity, Name, World, hierarchy};

use crate::class::{ClassMarker, ClassRegistry};
use crate::codes;
use crate::error::CompatError;
use crate::scene::ServiceTag;
use crate::{GAME_ID, ROOT_CLASS};

/// Marks an instance (and each of its descendants) that `Destroy` took out of
/// the tree. It stays alive, readable and locked until the current resumption
/// cycle settles, so deferred handlers still see its state; then it is
/// despawned.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Destroyed;

/// True if `e` was destroyed (despawned, or waiting to be).
pub fn is_destroyed(w: &World, e: Entity) -> bool {
    !alive(w, e) || w.ecs.get::<Destroyed>(e).is_some()
}

/// True for the `game` root, the `UserSettings()` root and the services,
/// whose `Parent` never changes.
pub fn is_locked(w: &World, e: Entity) -> bool {
    w.ecs.get::<ServiceTag>(e).is_some()
        || (w.id_of(e) == Some(GAME_ID) && class(w, e) == Some(ROOT_CLASS))
        || (class(w, e) == Some("UserSettings") && parent(w, e).is_none())
}

/// True if `e` is `game` or one of its descendants.
pub fn in_game(w: &World, game: Entity, e: Entity) -> bool {
    e == game || hierarchy::is_self_or_ancestor(&w.ecs, game, e)
}

/// True if `e` is alive.
pub fn alive(w: &World, e: Entity) -> bool {
    w.ecs.get_entity(e).is_ok()
}

/// The class of a live instance.
pub fn class(w: &World, e: Entity) -> Option<&str> {
    w.ecs.get::<ClassMarker>(e).map(|m| m.class.as_str())
}

/// True if `e` is a live instance.
pub fn is_instance(w: &World, e: Entity) -> bool {
    w.ecs.get::<ClassMarker>(e).is_some()
}

/// The `Name` of `e` (empty when it has none).
pub fn name(w: &World, e: Entity) -> String {
    w.name_of(e).to_string()
}

/// Sets the `Name` of `e`; an equal name changes nothing.
pub fn set_name(w: &mut World, e: Entity, name: &str) {
    if w.ecs.get::<Name>(e).is_some_and(|n| n.0 == name) {
        return;
    }
    w.ecs.entity_mut(e).insert(Name(name.to_string()));
}

/// The parent of `e`.
pub fn parent(w: &World, e: Entity) -> Option<Entity> {
    hierarchy::parent(&w.ecs, e)
}

/// The instance children of `e`, in child order.
pub fn children(w: &World, e: Entity) -> Vec<Entity> {
    hierarchy::children(&w.ecs, e)
        .iter()
        .copied()
        .filter(|c| is_instance(w, *c))
        .collect()
}

/// The instance descendants of `e`, depth-first pre-order, siblings in order
/// (without `e`).
pub fn descendants(w: &World, e: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut stack: Vec<Entity> = children(w, e).into_iter().rev().collect();
    while let Some(x) = stack.pop() {
        out.push(x);
        stack.extend(children(w, x).into_iter().rev());
    }
    out
}

/// The ancestors of `e`, its parent first.
pub fn ancestors(w: &World, e: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut cur = parent(w, e);
    while let Some(p) = cur {
        out.push(p);
        cur = parent(w, p);
    }
    out
}

/// The first child of `e` named `name` (or, with `recursive`, the first
/// descendant in pre-order).
pub fn find_child(w: &World, e: Entity, name: &str, recursive: bool) -> Option<Entity> {
    if recursive {
        descendants(w, e)
            .into_iter()
            .find(|c| w.name_of(*c) == name)
    } else {
        hierarchy::children(&w.ecs, e)
            .iter()
            .copied()
            .find(|c| is_instance(w, *c) && w.name_of(*c) == name)
    }
}

/// The dotted path of names from below `game` (`Workspace.Model.Part`); for an
/// instance outside the place, from its root.
pub fn full_name(w: &World, e: Entity) -> String {
    let mut parts = vec![name(w, e)];
    let mut cur = parent(w, e);
    while let Some(p) = cur {
        if w.id_of(p) == Some(GAME_ID) && class(w, p) == Some(ROOT_CLASS) {
            break;
        }
        parts.push(name(w, p));
        cur = parent(w, p);
    }
    parts.reverse();
    parts.join(".")
}

/// True if `e`'s class is `class` or inherits from it.
pub fn is_a(w: &World, classes: &ClassRegistry, e: Entity, class: &str) -> bool {
    self::class(w, e).is_some_and(|c| classes.is_a(c, class))
}

/// Moves `e` under `new` (`None` makes it a root). Refuses a cycle, a
/// destroyed instance or parent, and moving `game` or a service (`RE9008`).
pub fn set_parent(w: &mut World, e: Entity, new: Option<Entity>) -> Result<(), CompatError> {
    if is_destroyed(w, e) {
        return Err(CompatError::Runtime {
            code: codes::INVALID_PARENT,
            message: "The Parent property is locked: the instance was destroyed".into(),
        });
    }
    if is_locked(w, e) && new != parent(w, e) {
        return Err(CompatError::Runtime {
            code: codes::INVALID_PARENT,
            message: format!("The Parent property of {} is locked", name(w, e)),
        });
    }
    match new {
        None => {
            if w.ecs.get::<ChildOf>(e).is_some() {
                hierarchy::remove_parent(&mut w.ecs, e);
            }
        }
        Some(p) => {
            if is_destroyed(w, p) {
                return Err(CompatError::Runtime {
                    code: codes::INVALID_PARENT,
                    message: format!(
                        "cannot set the Parent of {} to a destroyed instance",
                        name(w, e)
                    ),
                });
            }
            if parent(w, e) == Some(p) {
                return Ok(());
            }
            if hierarchy::is_self_or_ancestor(&w.ecs, e, p) {
                return Err(CompatError::Runtime {
                    code: codes::INVALID_PARENT,
                    message: format!(
                        "Attempt to set parent of {} to {} would result in circular reference",
                        full_name(w, e),
                        full_name(w, p)
                    ),
                });
            }
            hierarchy::add_child(&mut w.ecs, p, e);
        }
    }
    Ok(())
}
