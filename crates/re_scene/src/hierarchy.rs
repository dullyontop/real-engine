//! Hierarchy helpers and transform propagation.
//!
//! The hierarchy itself is bevy_ecs's built-in `ChildOf` / `Children`
//! relationship: `ChildOf` is the source of truth, `Children` is maintained by
//! its hooks, keeps sibling order, and despawning a parent despawns its
//! subtree. There is no `Parent` or `Children` component of our own.
//!
//! Two propagation systems compute [`GlobalTransform`]: [`propagate_all`]
//! (everything) and [`propagate_changed`] (only subtrees whose `Transform` or
//! `ChildOf` changed, including removed `ChildOf`, which `Changed<ChildOf>`
//! does not see).

use std::collections::HashSet;

use bevy_ecs::change_detection::DetectChangesMut;
use bevy_ecs::entity::Entity;
use bevy_ecs::hierarchy::{ChildOf, Children};
use bevy_ecs::lifecycle::RemovedComponents;
use bevy_ecs::query::{Changed, Or, With};
use bevy_ecs::system::Query;
use bevy_ecs::world::World as EcsWorld;
use glam::Affine3A;

use crate::components::{GlobalTransform, Transform};
use crate::error::SceneError;
use crate::ids::StableId;

/// The parent of `entity`, if it has one.
pub fn parent(world: &EcsWorld, entity: Entity) -> Option<Entity> {
    world.get::<ChildOf>(entity).map(|c| c.parent())
}

/// The children of `entity`, in order (empty if none).
pub fn children(world: &EcsWorld, entity: Entity) -> &[Entity] {
    world.get::<Children>(entity).map(|c| &c[..]).unwrap_or(&[])
}

/// The position of `entity` among its parent's children, if it has a parent.
pub fn sibling_index(world: &EcsWorld, entity: Entity) -> Option<usize> {
    let p = parent(world, entity)?;
    children(world, p).iter().position(|c| *c == entity)
}

/// True if `ancestor` is `entity` or one of its ancestors.
pub fn is_self_or_ancestor(world: &EcsWorld, ancestor: Entity, entity: Entity) -> bool {
    let mut cur = Some(entity);
    while let Some(e) = cur {
        if e == ancestor {
            return true;
        }
        cur = parent(world, e);
    }
    false
}

/// `entity` and all its descendants, depth-first pre-order, siblings in order.
pub fn subtree(world: &EcsWorld, entity: Entity) -> Vec<Entity> {
    let mut out = Vec::new();
    let mut stack = vec![entity];
    while let Some(e) = stack.pop() {
        out.push(e);
        stack.extend(children(world, e).iter().rev());
    }
    out
}

/// Appends `child` to `parent`'s children (moving it from its old parent).
pub fn add_child(world: &mut EcsWorld, parent: Entity, child: Entity) {
    world.entity_mut(parent).add_child(child);
}

/// Places `child` at `index` among `parent`'s children; this also reorders a
/// child that is already there. `index` is clamped to the valid range.
pub fn insert_child(world: &mut EcsWorld, parent: Entity, index: usize, child: Entity) {
    let len = children(world, parent).len();
    let already = children(world, parent).contains(&child);
    let max = if already { len.saturating_sub(1) } else { len };
    world.entity_mut(parent).insert_child(index.min(max), child);
}

/// Makes `entity` a root.
pub fn remove_parent(world: &mut EcsWorld, entity: Entity) {
    world.entity_mut(entity).remove::<ChildOf>();
}

/// Despawns `entity` and its whole subtree (bevy's `Children` is
/// `linked_spawn`). A missing entity is ignored.
pub fn despawn_recursive(world: &mut EcsWorld, entity: Entity) {
    if let Ok(e) = world.get_entity_mut(entity) {
        e.despawn();
    }
}

/// Checks that `entity` may be parented under `new_parent`: refuses when
/// `new_parent` is `entity` or one of its descendants.
pub fn check_reparent(
    world: &EcsWorld,
    entity: Entity,
    new_parent: Entity,
) -> Result<(), SceneError> {
    if is_self_or_ancestor(world, entity, new_parent) {
        let id = |e: Entity| {
            world
                .get::<StableId>(e)
                .map(|s| s.0.clone())
                .unwrap_or_default()
        };
        return Err(SceneError::HierarchyCycle {
            entity: id(entity),
            new_parent: id(new_parent),
        });
    }
    Ok(())
}

type TransformNodes<'w, 's> = Query<'w, 's, (&'static Transform, Option<&'static Children>)>;

/// Entities whose `Transform` or parent changed.
type ChangedNodes<'w, 's> =
    Query<'w, 's, Entity, (With<Transform>, Or<(Changed<Transform>, Changed<ChildOf>)>)>;

fn propagate_from(
    entity: Entity,
    parent: &Affine3A,
    nodes: &TransformNodes,
    globals: &mut Query<&mut GlobalTransform>,
    mut visited: Option<&mut HashSet<Entity>>,
) {
    // Iterative DFS keeps deep hierarchies off the call stack.
    let mut stack = vec![(entity, *parent)];
    while let Some((e, parent)) = stack.pop() {
        let Ok((transform, children)) = nodes.get(e) else {
            continue;
        };
        let global = parent * transform.affine();
        if let Ok(mut g) = globals.get_mut(e) {
            g.set_if_neq(GlobalTransform(global));
        }
        if let Some(v) = visited.as_deref_mut() {
            v.insert(e);
        }
        if let Some(children) = children {
            stack.extend(children.iter().map(|c| (*c, global)));
        }
    }
}

/// System: recomputes every `GlobalTransform`. Propagation starts at every
/// entity with a `Transform` whose parent has none (or that has no parent),
/// from the identity: the same rule [`propagate_changed`] uses, so an entity
/// under a `Transform`-less ancestor is computed by both.
pub fn propagate_all(
    tops: Query<(Entity, Option<&ChildOf>), With<Transform>>,
    nodes: TransformNodes,
    mut globals: Query<&mut GlobalTransform>,
) {
    for (top, child_of) in &tops {
        if child_of.is_some_and(|c| nodes.contains(c.parent())) {
            continue;
        }
        propagate_from(top, &Affine3A::IDENTITY, &nodes, &mut globals, None);
    }
}

/// System: recomputes `GlobalTransform` only for subtrees whose `Transform` or
/// parent changed since this system last ran, including entities whose
/// `ChildOf` was removed.
pub fn propagate_changed(
    changed: ChangedNodes,
    mut unparented: RemovedComponents<ChildOf>,
    parents: Query<&ChildOf>,
    nodes: TransformNodes,
    mut globals: Query<&mut GlobalTransform>,
) {
    let mut dirty: Vec<(usize, Entity)> = changed
        .iter()
        .chain(unparented.read().filter(|e| nodes.contains(*e)))
        .map(|e| (depth(e, &parents), e))
        .collect();
    if dirty.is_empty() {
        return;
    }
    // Shallowest first: a subtree root's propagation covers its dirty descendants.
    dirty.sort_unstable();
    let mut visited = HashSet::new();
    for (_, e) in dirty {
        if visited.contains(&e) {
            continue;
        }
        // A parent without `Transform` counts as the identity (its
        // `GlobalTransform`, if any, is not maintained).
        let parent = parents
            .get(e)
            .ok()
            .filter(|c| nodes.contains(c.parent()))
            .and_then(|c| globals.get(c.parent()).ok())
            .map(|g| g.0)
            .unwrap_or(Affine3A::IDENTITY);
        propagate_from(e, &parent, &nodes, &mut globals, Some(&mut visited));
    }
}

fn depth(mut e: Entity, parents: &Query<&ChildOf>) -> usize {
    let mut d = 0;
    while let Ok(c) = parents.get(e) {
        e = c.parent();
        d += 1;
    }
    d
}
