//! The check a scene passes before anything changes (ADR-0059 §4): what
//! `World::load` checks before it spawns, made without spawning, against the
//! ids that will exist when the scene applies. So a load that fails, a switch
//! included, changes nothing.

use std::collections::{BTreeSet, HashSet};

use re_reflect::ReflectError;
use re_scene::{Entity, EntityDoc, SceneDoc, SceneError, World};

/// The ids besides the document's that exist when a scene applies.
#[derive(Clone, Copy, Debug)]
pub enum Existing<'a> {
    /// Every id the World has, stashed ones included: a scene loaded beside
    /// the loaded ones.
    World,
    /// Only these ids, each the id of a World entity: a switch, which keeps
    /// the kept subtrees and despawns everything else (the World still holds
    /// the old scene while the check runs).
    Only(&'a BTreeSet<String>),
}

impl Existing<'_> {
    /// True if a new entity cannot take `id`.
    fn taken(self, world: &World, id: &str) -> bool {
        match self {
            Existing::World => world.id_taken(id),
            Existing::Only(ids) => ids.contains(id),
        }
    }

    /// The entity a reference to `id` resolves to.
    fn entity(self, world: &World, id: &str) -> Option<Entity> {
        match self {
            Existing::World => world.ids().get(id),
            Existing::Only(ids) if ids.contains(id) => world.ids().get(id),
            Existing::Only(_) => None,
        }
    }
}

/// Checks that `scene` loads into `world` once only the ids of `existing`
/// are there besides its own, spawning nothing: its format and version,
/// every id valid, unique and free, every parent earlier in the file or
/// existing, every component registered and every field value convertible,
/// with entity references resolved among the file's ids and the existing
/// ones. Declares the file's script components first, as a load does (they
/// stay declared when the check fails).
///
/// # Errors
///
/// The first problem, as the `SceneError` that `World::load` would give.
pub fn check(world: &mut World, scene: &SceneDoc, existing: Existing<'_>) -> Result<(), SceneError> {
    if scene.format != re_scene::scene::FORMAT {
        return Err(SceneError::UnsupportedFormat(format!(
            "expected format {:?}, found {:?}",
            re_scene::scene::FORMAT,
            scene.format
        )));
    }
    if scene.version != re_scene::scene::VERSION {
        return Err(SceneError::UnsupportedFormat(format!(
            "unsupported scene version {} (this build reads {})",
            scene.version,
            re_scene::scene::VERSION
        )));
    }
    world.declare_script_components(&scene.script_components)?;
    let at = |i: usize, d: &EntityDoc| format!("entities[{i}] (id {:?})", d.id);

    // Ids: valid, unique in the file, and free.
    let mut ids: HashSet<&str> = HashSet::new();
    for (i, d) in scene.entities.iter().enumerate() {
        if d.id.is_empty() {
            return Err(SceneError::Parse(format!(
                "{}: id must not be empty",
                at(i, d)
            )));
        }
        re_scene::validate_id(&d.id).map_err(|e| SceneError::ParseAt {
            at: at(i, d),
            error: Box::new(e),
        })?;
        if !ids.insert(d.id.as_str()) || existing.taken(world, &d.id) {
            return Err(SceneError::DuplicateId(d.id.clone()));
        }
    }

    // Parents: earlier in the file, or existing.
    let mut earlier: HashSet<&str> = HashSet::new();
    for (i, d) in scene.entities.iter().enumerate() {
        if let Some(p) = &d.parent
            && !earlier.contains(p.as_str())
            && existing.entity(world, p).is_none()
        {
            return Err(SceneError::ForwardParent {
                at: at(i, d),
                parent: p.clone(),
            });
        }
        earlier.insert(&d.id);
    }

    // Components and values. A reference to one of the file's own entities
    // resolves to a stand-in handle: only whether it resolves matters here.
    let resolve = |id: &str| {
        if ids.contains(id) {
            Some(Entity::PLACEHOLDER)
        } else {
            existing.entity(world, id)
        }
    };
    for (i, d) in scene.entities.iter().enumerate() {
        let wrap = |e: SceneError| SceneError::ParseAt {
            at: at(i, d),
            error: Box::new(e),
        };
        for (component, fields) in &d.components {
            let key = world
                .registry
                .key(component)
                .map_err(|e| wrap(e.into()))?;
            let schema = world.registry.schema(key);
            for (name, doc) in fields {
                let index = schema.field_index(name).map_err(|e| wrap(e.into()))?;
                let field = &schema.fields[index];
                let bad = |error| {
                    wrap(SceneError::Reflect(ReflectError::Value {
                        path: format!("{}.{}", schema.name, field.name),
                        error,
                    }))
                };
                let value = doc.to_value(&field.ty, &resolve).map_err(bad)?;
                field.ty.coerce(value).map_err(bad)?;
            }
        }
    }
    Ok(())
}

/// `scene` without the entities that kept ones stand in for (ADR-0059 §4):
/// each entity whose id is in `kept`, with its subtree in the file. Returns
/// the document and the ids stood in for, in file order.
pub fn without_stand_ins(scene: &SceneDoc, kept: &BTreeSet<String>) -> (SceneDoc, Vec<String>) {
    let mut stood_in = Vec::new();
    let mut dropped: HashSet<&str> = HashSet::new();
    let mut entities = Vec::with_capacity(scene.entities.len());
    for d in &scene.entities {
        if kept.contains(&d.id) {
            stood_in.push(d.id.clone());
            dropped.insert(&d.id);
        } else if d.parent.as_deref().is_some_and(|p| dropped.contains(p)) {
            dropped.insert(&d.id);
        } else {
            entities.push(d.clone());
        }
    }
    let doc = SceneDoc {
        entities,
        ..scene.clone()
    };
    (doc, stood_in)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> World {
        let mut world = World::new();
        let doc = re_scene::scene::from_json(
            r#"{ "format": "real-engine/scene", "version": 1, "entities": [
                { "id": "player", "name": "Player" },
                { "id": "player_cam", "parent": "player" },
                { "id": "old_ground" }
            ] }"#,
        )
        .unwrap();
        world.load(&doc).unwrap();
        world
    }

    fn scene(entities: &str) -> SceneDoc {
        re_scene::scene::from_json(&format!(
            r#"{{ "format": "real-engine/scene", "version": 1, "entities": [{entities}] }}"#
        ))
        .unwrap()
    }

    fn kept() -> BTreeSet<String> {
        ["player".to_string(), "player_cam".to_string()].into()
    }

    #[test]
    fn a_switch_sees_only_the_kept_entities() {
        let mut world = world();
        let kept = kept();
        // The old scene's ids neither clash nor resolve; kept ones do both.
        let ok = scene(
            r#"{ "id": "old_ground" },
               { "id": "lamp", "parent": "player" }"#,
        );
        check(&mut world, &ok, Existing::Only(&kept)).unwrap();
        let clash = scene(r#"{ "id": "player_cam" }"#);
        assert_eq!(
            check(&mut world, &clash, Existing::Only(&kept)),
            Err(SceneError::DuplicateId("player_cam".into()))
        );
        let gone_parent = scene(r#"{ "id": "lamp", "parent": "old_ground" }"#);
        assert!(matches!(
            check(&mut world, &gone_parent, Existing::Only(&kept)),
            Err(SceneError::ForwardParent { .. })
        ));
        // Beside the loaded scenes, every id of the World counts.
        assert_eq!(
            check(&mut world, &ok, Existing::World),
            Err(SceneError::DuplicateId("old_ground".into()))
        );
        let beside = scene(r#"{ "id": "lamp", "parent": "old_ground" }"#);
        check(&mut world, &beside, Existing::World).unwrap();
        // Nothing was spawned.
        assert!(world.ids().get("lamp").is_none());
    }

    #[test]
    fn values_and_components_are_checked_as_a_load_checks_them() {
        let mut world = world();
        let kept = kept();
        let unknown = scene(r#"{ "id": "a", "components": { "Wobble": {} } }"#);
        let e = check(&mut world, &unknown, Existing::Only(&kept)).unwrap_err();
        assert!(e.to_string().contains("entities[0]"), "{e}");
        let bad = scene(r#"{ "id": "a", "components": { "Transform": { "translation": "up" } } }"#);
        assert!(check(&mut world, &bad, Existing::Only(&kept)).is_err());
        assert_eq!(
            check(&mut world, &bad, Existing::Only(&kept)).unwrap_err(),
            world.load(&bad).unwrap_err()
        );
        let bad_id = scene(r#"{ "id": "a/b" }"#);
        assert!(check(&mut world, &bad_id, Existing::Only(&kept)).is_err());
        let mut wrong = scene("");
        wrong.version = 99;
        assert!(matches!(
            check(&mut world, &wrong, Existing::Only(&kept)),
            Err(SceneError::UnsupportedFormat(_))
        ));
    }

    #[test]
    fn stood_in_entities_leave_with_their_subtrees() {
        let doc = scene(
            r#"{ "id": "player" },
               { "id": "hat", "parent": "player" },
               { "id": "feather", "parent": "hat" },
               { "id": "ground" }"#,
        );
        let (doc, stood_in) = without_stand_ins(&doc, &kept());
        assert_eq!(stood_in, ["player"]);
        let ids: Vec<&str> = doc.entities.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["ground"]);
    }
}
