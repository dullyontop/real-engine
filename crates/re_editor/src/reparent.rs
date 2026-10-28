//! Moving entities by dragging their Hierarchy rows (ADR-0044's amendment
//! for WP42, §7): where a drop puts them, and the commands that move them.
//!
//! Authored entities move with one `scene.apply` of `reparent` ops, so one
//! drop is one undo step, and each keeps its world transform: its local
//! `Transform` is worked out for its new parent in the same `scene.apply`.
//! A generated entity moves with `proc.move`, which keeps the move across
//! regenerations and works only within its source's output; there it keeps
//! its generated local transform, as its source placed it. A move the
//! editor refuses is a note (`RE8809`) that says why.

use glam::Mat4;
use re_core::Diagnostic;
use re_scene::hierarchy::{children, parent};
use re_scene::{Entity, Generated, GlobalTransform, World};
use serde_json::{Value as Json, json};

use crate::clipboard::local_under;
use crate::codes;

/// Where dragged rows were dropped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Drop {
    /// On a row: under that entity, after its children.
    Into(String),
    /// Between two rows: beside an entity, before it.
    Before(String),
    /// Between two rows: beside an entity, after it.
    After(String),
    /// Under the rows: at the scene's root.
    Root,
}

/// The commands a drop sends.
#[derive(Debug, Default)]
pub(crate) struct Moves {
    /// `scene.apply`'s arguments, for the authored entities.
    pub apply: Option<Json>,
    /// `proc.move`'s arguments, one per generated entity.
    pub proc_moves: Vec<Json>,
    /// Why entities were not moved.
    pub refused: Vec<Diagnostic>,
}

/// The moves that put `ids` (the dragged entities; one under another moves
/// with it) where `drop` says.
pub(crate) fn plan(world: &World, ids: &[String], drop: &Drop) -> Moves {
    let mut moves = Moves::default();
    let anchor = match drop {
        Drop::Into(id) | Drop::Before(id) | Drop::After(id) => match world.entity(id) {
            Ok(e) => Some(e),
            Err(_) => return moves,
        },
        Drop::Root => None,
    };
    let target = match drop {
        Drop::Into(_) => anchor,
        Drop::Before(_) | Drop::After(_) => anchor.and_then(|a| parent(&world.ecs, a)),
        Drop::Root => None,
    };
    let dragged: Vec<Entity> = ids
        .iter()
        .filter_map(|id| world.entity(id).ok())
        .filter(|e| Some(*e) != anchor || matches!(drop, Drop::Into(_)))
        .collect();
    let topmost: Vec<Entity> = dragged
        .iter()
        .copied()
        .filter(|e| !ancestors(world, *e).any(|a| dragged.contains(&a)))
        .collect();
    let id_of = |e: Entity| world.id_of(e).unwrap_or("?").to_string();
    let mut authored = Vec::new();
    let mut generated = Vec::new();
    for e in topmost {
        if let Some(t) = target
            && (t == e || ancestors(world, t).any(|a| a == e))
        {
            moves.refused.push(refuse(format!(
                "{} cannot move under itself or its own descendant",
                id_of(e)
            )));
            continue;
        }
        match world.ecs.get::<Generated>(e) {
            Some(g) => {
                let within = target.is_some_and(|t| {
                    t == g.source
                        || world
                            .ecs
                            .get::<Generated>(t)
                            .is_some_and(|tg| tg.source == g.source)
                });
                if within {
                    generated.push(e);
                } else {
                    let source = id_of(g.source);
                    moves.refused.push(
                        refuse(format!(
                            "{} is made by {source}, so it moves only within {source}'s output",
                            id_of(e)
                        ))
                        .with_hint(format!(
                            "drop it on {source} or on another of its entities, or bake \
                             {source} (`proc.bake`) to move what it made anywhere"
                        )),
                    );
                }
            }
            None => match target.filter(|t| world.ecs.get::<Generated>(*t).is_some()) {
                Some(t) => moves.refused.push(
                    refuse(format!(
                        "{} cannot go under {}: a generated entity has no authored children",
                        id_of(e),
                        id_of(t)
                    ))
                    .with_hint("add an entity to a procedural source's output with `proc.insert`"),
                ),
                None => authored.push(e),
            },
        }
    }
    moves.apply = authored_moves(world, &authored, target, anchor, drop);
    moves.proc_moves = generated_moves(world, &generated, target, anchor, drop);
    moves
}

/// `scene.apply`'s arguments moving `moved` under `target` (the root when
/// `None`), beside `anchor` as `drop` says: those already under `target`
/// are taken out first, then each is put in its place, then each whose
/// parent changed gets the local `Transform` that keeps it where it is.
/// `None` when nothing would change.
fn authored_moves(
    world: &World,
    moved: &[Entity],
    target: Option<Entity>,
    anchor: Option<Entity>,
    drop: &Drop,
) -> Option<Json> {
    let id_of = |e: Entity| world.id_of(e).map(str::to_string);
    let mut ops = Vec::new();
    match target {
        Some(t) => {
            let now = children(&world.ecs, t);
            let rest: Vec<Entity> = now.iter().copied().filter(|c| !moved.contains(c)).collect();
            let at = match drop {
                Drop::Before(_) => anchor.and_then(|a| rest.iter().position(|c| *c == a)),
                Drop::After(_) => anchor
                    .and_then(|a| rest.iter().position(|c| *c == a))
                    .map(|i| i + 1),
                Drop::Into(_) | Drop::Root => None,
            }
            .unwrap_or(rest.len());
            let mut after = rest.clone();
            for (k, m) in moved.iter().enumerate() {
                after.insert(at + k, *m);
            }
            if after == now {
                return None;
            }
            for m in moved.iter().filter(|m| now.contains(m)) {
                ops.push(json!({ "op": "reparent", "id": id_of(*m), "parent": null }));
            }
            for (k, m) in moved.iter().enumerate() {
                ops.push(json!({
                    "op": "reparent", "id": id_of(*m), "parent": id_of(t), "index": at + k,
                }));
            }
        }
        None => {
            for m in moved.iter().filter(|m| parent(&world.ecs, **m).is_some()) {
                ops.push(json!({ "op": "reparent", "id": id_of(*m), "parent": null }));
            }
            if ops.is_empty() {
                return None;
            }
        }
    }
    let transform = world.registry.try_key("Transform");
    for m in moved {
        let has_transform = transform.is_some_and(|k| world.registry.has(&world.ecs, *m, k));
        if parent(&world.ecs, *m) == target || !has_transform {
            continue;
        }
        let Some(global) = world.ecs.get::<GlobalTransform>(*m) else {
            continue;
        };
        let Some((t, q, s)) = local_under(world, Mat4::from(global.0), target) else {
            continue;
        };
        for (field, value) in [
            ("translation", json!(t.to_array())),
            ("rotation", json!(q.to_array())),
            ("scale", json!(s.to_array())),
        ] {
            ops.push(json!({
                "op": "set_field", "id": id_of(*m), "component": "Transform",
                "field": field, "value": value,
            }));
        }
    }
    let label = match moved {
        [one] => format!("move {}", id_of(*one).unwrap_or_default()),
        _ => format!("move {} entities", moved.len()),
    };
    Some(json!({ "label": label, "ops": ops }))
}

/// `proc.move`'s arguments for each generated entity in `moved`, under
/// `target` (its source, or another entity of the source's output).
fn generated_moves(
    world: &World,
    moved: &[Entity],
    target: Option<Entity>,
    anchor: Option<Entity>,
    drop: &Drop,
) -> Vec<Json> {
    let Some(t) = target else {
        return Vec::new();
    };
    let id_of = |e: Entity| world.id_of(e).unwrap_or("?").to_string();
    // The index counts the new parent's generated children.
    let rest: Vec<Entity> = children(&world.ecs, t)
        .iter()
        .copied()
        .filter(|c| !moved.contains(c) && world.ecs.get::<Generated>(*c).is_some())
        .collect();
    let at = match drop {
        Drop::Before(_) => anchor.and_then(|a| rest.iter().position(|c| *c == a)),
        Drop::After(_) => anchor
            .and_then(|a| rest.iter().position(|c| *c == a))
            .map(|i| i + 1),
        Drop::Into(_) | Drop::Root => None,
    };
    moved
        .iter()
        .enumerate()
        .map(|(k, m)| {
            let mut args = json!({ "id": id_of(*m) });
            if world.ecs.get::<Generated>(t).is_some() {
                args["parent"] = json!(id_of(t));
            }
            if let Some(at) = at {
                args["index"] = json!(at + k);
            }
            args
        })
        .collect()
}

/// `e`'s parent, its parent, and so on to the root.
fn ancestors(world: &World, e: Entity) -> impl Iterator<Item = Entity> + '_ {
    std::iter::successors(parent(&world.ecs, e), |p| parent(&world.ecs, *p))
}

fn refuse(message: String) -> Diagnostic {
    Diagnostic::note(codes::MOVE_REFUSED, format!("not moved: {message}"))
}

#[cfg(test)]
mod tests {
    use re_scene::EntityDoc;

    use super::*;

    fn doc(id: &str, parent: Option<&str>, transform: Json) -> EntityDoc {
        serde_json::from_value(json!({
            "id": id, "parent": parent, "components": { "Transform": transform },
        }))
        .unwrap()
    }

    /// a (at x = 1) holds a1 and a2; b (at x = 10, turned a quarter about
    /// Y) holds b1; c is a root.
    fn world() -> World {
        let at = |t: [f32; 3]| json!({ "translation": t });
        let mut w = World::new();
        let docs = [
            doc("a", None, at([1.0, 0.0, 0.0])),
            doc("a1", Some("a"), at([0.0, 1.0, 0.0])),
            doc("a2", Some("a"), at([0.0, 2.0, 0.0])),
            doc(
                "b",
                None,
                json!({ "translation": [10.0, 0.0, 0.0],
                        "rotation": [0.0, 0.70710677, 0.0, 0.70710677] }),
            ),
            doc("b1", Some("b"), at([0.0, 0.0, 1.0])),
            doc("c", None, at([0.0, 0.0, 5.0])),
        ];
        w.spawn_docs(&docs, None).unwrap();
        w.propagate();
        w
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn ops(moves: &Moves) -> Vec<String> {
        moves.apply.as_ref().unwrap()["ops"]
            .as_array()
            .unwrap()
            .iter()
            .map(|op| {
                let mut s = format!(
                    "{} {}",
                    op["op"].as_str().unwrap(),
                    op["id"].as_str().unwrap()
                );
                if op["op"] == "reparent" {
                    s += &format!(" {} {}", op["parent"], op["index"]);
                } else {
                    s += &format!(" {}", op["field"].as_str().unwrap());
                }
                s
            })
            .collect()
    }

    #[test]
    fn a_drop_on_a_row_moves_under_it_keeping_the_world_transform() {
        let w = world();
        let moves = plan(&w, &ids(&["a1"]), &Drop::Into("b".into()));
        assert!(moves.refused.is_empty() && moves.proc_moves.is_empty());
        assert_eq!(
            ops(&moves),
            [
                "reparent a1 \"b\" 1",
                "set_field a1 translation",
                "set_field a1 rotation",
                "set_field a1 scale",
            ]
        );
        let apply = moves.apply.unwrap();
        assert_eq!(apply["label"], "move a1");
        // a1 stands at (1, 1, 0); b at (10, 0, 0), turned a quarter about Y,
        // so a1 is (-9, 1, 0) from b, which is (0, 1, -9) in b's frame.
        let t = &apply["ops"][1]["value"];
        let t: Vec<f32> = serde_json::from_value(t.clone()).unwrap();
        assert!(
            glam::Vec3::from_slice(&t).abs_diff_eq(glam::Vec3::new(0.0, 1.0, -9.0), 1e-4),
            "{t:?}"
        );
    }

    #[test]
    fn a_drop_between_rows_puts_them_beside_in_order() {
        let w = world();
        // Before a1, within the same parent: a2 is taken out first.
        let moves = plan(&w, &ids(&["a2"]), &Drop::Before("a1".into()));
        assert_eq!(
            ops(&moves),
            ["reparent a2 null null", "reparent a2 \"a\" 0"]
        );
        // After a1 is where a2 is already: nothing to do.
        let moves = plan(&w, &ids(&["a2"]), &Drop::After("a1".into()));
        assert!(moves.apply.is_none());
        // Two from elsewhere, after b1, in the order dragged.
        let moves = plan(&w, &ids(&["c", "a1"]), &Drop::After("b1".into()));
        assert_eq!(
            &ops(&moves)[..2],
            ["reparent c \"b\" 1", "reparent a1 \"b\" 2"]
        );
        assert_eq!(moves.apply.unwrap()["label"], "move 2 entities");
        // To the root: only those not already there.
        let moves = plan(&w, &ids(&["a1", "c"]), &Drop::Root);
        assert_eq!(&ops(&moves)[..1], ["reparent a1 null null"]);
    }

    #[test]
    fn a_child_moves_with_its_dragged_parent_and_nothing_moves_under_itself() {
        let w = world();
        let moves = plan(&w, &ids(&["a", "a1"]), &Drop::Into("c".into()));
        assert_eq!(&ops(&moves)[..1], ["reparent a \"c\" 0"]);
        let moves = plan(&w, &ids(&["a"]), &Drop::Into("a2".into()));
        assert!(moves.apply.is_none());
        assert_eq!(moves.refused[0].code, codes::MOVE_REFUSED);
        assert!(moves.refused[0].message.contains("its own descendant"));
    }

    #[test]
    fn generated_entities_move_only_within_their_source() {
        let mut w = world();
        let source = w.entity("b").unwrap();
        for id in ["b1", "a2"] {
            let e = w.entity(id).unwrap();
            w.ecs.entity_mut(e).insert(Generated {
                source,
                key: id.into(),
            });
        }
        // b1 under a2, both made by b: proc.move; c under a generated
        // entity, and b1 out of b's output: refused.
        let moves = plan(&w, &ids(&["b1", "c"]), &Drop::Into("a2".into()));
        assert_eq!(moves.proc_moves, [json!({ "id": "b1", "parent": "a2" })]);
        assert!(moves.apply.is_none());
        assert!(moves.refused[0].message.contains("no authored children"));
        let moves = plan(&w, &ids(&["b1"]), &Drop::Root);
        assert!(moves.proc_moves.is_empty());
        assert!(
            moves.refused[0]
                .message
                .contains("moves only within b's output")
        );
        // Back under the source itself: no parent named.
        let moves = plan(&w, &ids(&["a2"]), &Drop::Into("b".into()));
        assert_eq!(moves.proc_moves, [json!({ "id": "a2" })]);
    }
}
