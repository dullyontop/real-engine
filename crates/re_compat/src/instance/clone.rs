//! `Clone()`: a deep copy of an instance's subtree, honouring `Archivable`.

use std::collections::HashMap;

use re_reflect::Value;
use re_scene::{Entity, Name, StableId, World, hierarchy};

use crate::class::own_component_name;

/// True unless the entity's `Archivable` is false (entities without the
/// property are archivable).
pub fn archivable(w: &World, e: Entity) -> bool {
    let Some(key) = w.registry.try_key(&own_component_name("Instance")) else {
        return true;
    };
    let Ok(index) = w.registry.schema(key).field_index("Archivable") else {
        return true;
    };
    !matches!(
        w.registry.get_field(&w.ecs, e, key, index),
        Some(Value::Bool(false))
    )
}

/// Copies `e` and its archivable descendants (a non-archivable entity is left
/// out with its subtree). The copy is a root with fresh stable ids; every
/// registered component is copied by value, and entity references to
/// entities inside the copied subtree point at their copies. `None` when `e`
/// itself is not archivable.
pub fn clone_subtree(w: &mut World, e: Entity) -> Option<Entity> {
    if !archivable(w, e) {
        return None;
    }
    // Pre-order, parents before children, pruning non-archivable subtrees.
    let mut order = Vec::new();
    let mut stack = vec![e];
    while let Some(x) = stack.pop() {
        order.push(x);
        let kids: Vec<Entity> = hierarchy::children(&w.ecs, x)
            .iter()
            .copied()
            .filter(|c| archivable(w, *c))
            .collect();
        stack.extend(kids.into_iter().rev());
    }
    let mut map: HashMap<Entity, Entity> = HashMap::with_capacity(order.len());
    for &old in &order {
        let id = w.new_id();
        let name = w.ecs.get::<Name>(old).cloned().unwrap_or_default();
        let new = w.ecs.spawn((name, StableId(id))).id();
        if old != e
            && let Some(p) = hierarchy::parent(&w.ecs, old).and_then(|p| map.get(&p))
        {
            hierarchy::add_child(&mut w.ecs, *p, new);
        }
        map.insert(old, new);
    }
    for &old in &order {
        let new = map[&old];
        let comps: Vec<(re_reflect::ComponentKey, Vec<Value>)> = w
            .registry
            .components_of(&w.ecs, old)
            .into_iter()
            .filter_map(|k| w.registry.get_all(&w.ecs, old, k).map(|v| (k, v)))
            .collect();
        for (key, values) in comps {
            let values = values.into_iter().map(|v| remap(v, &map)).collect();
            // Same registry, values read from it: the insert cannot fail.
            let _ = w.registry.insert(&mut w.ecs, new, key, values);
        }
    }
    map.get(&e).copied()
}

fn remap(v: Value, map: &HashMap<Entity, Entity>) -> Value {
    match v {
        Value::Entity(Some(x)) => Value::Entity(Some(map.get(&x).copied().unwrap_or(x))),
        Value::List(items) => Value::List(items.into_iter().map(|i| remap(i, map)).collect()),
        other => other,
    }
}
