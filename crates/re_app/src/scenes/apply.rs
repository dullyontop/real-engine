//! What a scene change does to the World (ADR-0059 §4): a switch despawns
//! everything that is not kept and loads the new scene in its place, a load
//! adds a scene beside the loaded ones, and an unload despawns what a load
//! added. Each change is checked first ([`check`]), so one that fails
//! changes nothing.

use std::collections::{BTreeSet, HashSet};

use re_core::Diagnostic;
use re_proc::stream::{StreamConfig, StreamEvents, Streamer, TerrainView};
use re_scene::{
    Entity, MaterialRef, MeshRef, Procedural, SceneDoc, SceneError, Stash, Suppressions, World,
    hierarchy,
};

use super::check::{Existing, check, without_stand_ins};
use crate::media::DERIVED;

/// What a change did to the World.
#[derive(Debug, Default)]
pub struct Change {
    /// The ids of the entities that kept ones stood in for, in file order.
    pub stood_in: Vec<String>,
    /// The ids of the scene's roots: its entities whose parent is not in its
    /// file, in file order.
    pub roots: Vec<String>,
    /// The kept roots that lived through a switch: the kept ids that still
    /// name a live root.
    pub kept: BTreeSet<String>,
    /// The derived assets the despawned entities drew with: release the ones
    /// nothing uses once the new sources have generated ([`unreferenced`]).
    pub released: BTreeSet<String>,
    /// Problems that did not stop the change.
    pub diagnostics: Vec<Diagnostic>,
}

/// The kept entities a change keeps: the ids in `kept` that name a live
/// root, with their entities.
pub fn kept_roots(world: &World, kept: &BTreeSet<String>) -> Vec<(String, Entity)> {
    kept.iter()
        .filter_map(|id| world.ids().get(id).map(|e| (id.clone(), e)))
        .filter(|(_, e)| hierarchy::parent(&world.ecs, *e).is_none())
        .collect()
}

/// Every id in the subtrees of `roots`.
fn subtree_ids(world: &World, roots: &[(String, Entity)]) -> BTreeSet<String> {
    roots
        .iter()
        .flat_map(|(_, root)| hierarchy::subtree(&world.ecs, *root))
        .filter_map(|e| world.id_of(e).map(str::to_string))
        .collect()
}

/// The derived assets the subtrees of `roots` draw with.
fn derived_assets(world: &World, roots: &[(String, Entity)]) -> BTreeSet<String> {
    let mut assets = BTreeSet::new();
    for (_, root) in roots {
        for e in hierarchy::subtree(&world.ecs, *root) {
            let mesh = world.ecs.get::<MeshRef>(e).map(|m| &m.asset);
            let material = world.ecs.get::<MaterialRef>(e).map(|m| &m.asset);
            for asset in mesh.into_iter().chain(material) {
                if asset.starts_with(DERIVED) {
                    assets.insert(asset.clone());
                }
            }
        }
    }
    assets
}

/// The ids of the procedural sources in the subtrees of `roots`.
fn sources(world: &World, roots: &[(String, Entity)]) -> BTreeSet<String> {
    roots
        .iter()
        .flat_map(|(_, root)| hierarchy::subtree(&world.ecs, *root))
        .filter(|e| world.ecs.get::<Procedural>(*e).is_some())
        .filter_map(|e| world.id_of(e).map(str::to_string))
        .collect()
}

/// The ids of a suppressed generated id's possible sources: every prefix
/// before a `/` (source ids and keys may both hold `/`).
fn source_prefixes(id: &str) -> impl Iterator<Item = &str> {
    id.match_indices('/').map(move |(i, _)| &id[..i])
}

/// Keeps the session's suppressions that `keep` keeps, forgetting the rest.
fn retain_suppressions(world: &mut World, keep: impl Fn(&World, &str) -> bool) {
    let all: Vec<String> = world.suppressions().iter().map(str::to_string).collect();
    let kept: Vec<String> = all.iter().filter(|id| keep(world, id)).cloned().collect();
    if kept.len() == all.len() {
        return;
    }
    world.ecs.insert_resource(Suppressions::default());
    for id in kept {
        // Each was a valid generated id when it was suppressed.
        let _ = world.suppress(&id);
    }
}

/// Despawns the subtrees of `roots`, in stable-id order (not as a script's
/// despawn: unloading is not the play-state deletion that suppressions
/// record).
fn despawn(world: &mut World, mut roots: Vec<(String, Entity)>) {
    roots.sort();
    for (_, root) in roots {
        hierarchy::despawn_recursive(&mut world.ecs, root);
    }
}

/// The roots of a scene file: the ids of its entities whose parent is not in
/// the file.
fn file_roots(scene: &SceneDoc) -> Vec<String> {
    let ids: HashSet<&str> = scene.entities.iter().map(|d| d.id.as_str()).collect();
    scene
        .entities
        .iter()
        .filter(|d| d.parent.as_deref().is_none_or(|p| !ids.contains(p)))
        .map(|d| d.id.clone())
        .collect()
}

/// Switches `world` to `scene`, keeping the roots `kept` names (with their
/// subtrees): checks the scene against the kept entities, then despawns
/// every other root, forgets the suppressions of the sources that went,
/// empties the stream stash, puts a fresh streamer (with `stream`) in place
/// of the old one, loads the scene and propagates transforms. A kept entity
/// stands in for the scene's entity of its id.
///
/// # Errors
///
/// The check's error; then nothing has changed. A load that fails after the
/// check passed (it repeats the load's checks, so it should not) leaves the
/// kept entities alone in the World.
pub fn switch(
    world: &mut World,
    scene: &SceneDoc,
    kept: &BTreeSet<String>,
    stream: &StreamConfig,
) -> Result<Change, SceneError> {
    let keep = kept_roots(world, kept);
    let keep_ids: BTreeSet<String> = keep.iter().map(|(id, _)| id.clone()).collect();
    let existing = subtree_ids(world, &keep);
    let (scene, stood_in) = without_stand_ins(scene, &keep_ids);
    check(world, &scene, Existing::Only(&existing))?;

    let gone: Vec<(String, Entity)> = world
        .ids()
        .iter()
        .filter(|(id, e)| !keep_ids.contains(*id) && hierarchy::parent(&world.ecs, *e).is_none())
        .map(|(id, e)| (id.to_string(), e))
        .collect();
    let released = derived_assets(world, &gone);
    despawn(world, gone);
    // A source loaded again starts as authored.
    retain_suppressions(world, |w, id| {
        source_prefixes(id).any(|s| {
            w.ids()
                .get(s)
                .is_some_and(|e| w.ecs.get::<Procedural>(e).is_some())
        })
    });
    // The stash and the streamer only held the old main scene's cells.
    world.ecs.insert_resource(Stash::default());
    world.ecs.remove_resource::<TerrainView>();
    world.ecs.remove_resource::<StreamEvents>();
    let mut diagnostics = Vec::new();
    match Streamer::new(stream.clone()) {
        Ok(s) => {
            world.ecs.insert_resource(s);
        }
        Err(d) => {
            world.ecs.remove_resource::<Streamer>();
            diagnostics.push(d);
        }
    }
    world.load(&scene)?;
    world.propagate_all();
    Ok(Change {
        stood_in,
        roots: file_roots(&scene),
        kept: keep_ids,
        released,
        diagnostics,
    })
}

/// Loads `scene` beside the loaded scenes, keeping the roots `kept` names: a
/// kept entity stands in for the scene's entity of its id, and every other
/// id must be free.
///
/// # Errors
///
/// The check's error, or `SceneError::Invalid` for a scene that holds a
/// streamed entity (streaming belongs to the main scene); then nothing has
/// changed.
pub fn load(
    world: &mut World,
    scene: &SceneDoc,
    kept: &BTreeSet<String>,
) -> Result<Change, SceneError> {
    let keep_ids: BTreeSet<String> = kept_roots(world, kept)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    let (scene, stood_in) = without_stand_ins(scene, &keep_ids);
    if let Some(d) = scene
        .entities
        .iter()
        .find(|d| d.components.contains_key("Streamed"))
    {
        return Err(SceneError::Invalid(format!(
            "entity {:?} is streamed, and only the main scene streams: load it with scene.switch, or remove its Streamed component",
            d.id
        )));
    }
    check(world, &scene, Existing::World)?;
    world.load(&scene)?;
    world.propagate_all();
    Ok(Change {
        stood_in,
        roots: file_roots(&scene),
        ..Change::default()
    })
}

/// Unloads a scene that [`load`] added: despawns its roots (`roots`), except
/// kept ones, and forgets the suppressions of the sources that went.
pub fn unload(world: &mut World, roots: &[String], kept: &BTreeSet<String>) -> Change {
    let gone: Vec<(String, Entity)> = roots
        .iter()
        .filter(|id| !kept.contains(*id))
        .filter_map(|id| world.ids().get(id).map(|e| (id.clone(), e)))
        .collect();
    let released = derived_assets(world, &gone);
    let sources = sources(world, &gone);
    despawn(world, gone);
    if !sources.is_empty() {
        retain_suppressions(world, |_, id| {
            !source_prefixes(id).any(|s| sources.contains(s))
        });
    }
    Change {
        released,
        ..Change::default()
    }
}

/// The assets of `released` that nothing in `world` draws with any more:
/// call it after the new scene's sources have generated, and release these.
pub fn unreferenced(world: &mut World, released: &BTreeSet<String>) -> Vec<String> {
    if released.is_empty() {
        return Vec::new();
    }
    let mut live: HashSet<String> = HashSet::new();
    let mut q = world
        .ecs
        .query::<(Option<&MeshRef>, Option<&MaterialRef>)>();
    for (mesh, material) in q.iter(&world.ecs) {
        live.extend(mesh.map(|m| m.asset.clone()));
        live.extend(material.map(|m| m.asset.clone()));
    }
    released
        .iter()
        .filter(|a| !live.contains(*a))
        .cloned()
        .collect()
}

/// True if the World holds an entity that streams or is streamed.
#[cfg(test)]
fn streams(world: &mut World) -> bool {
    world
        .ecs
        .query_filtered::<(), re_scene::ecs::query::With<re_scene::Streamed>>()
        .iter(&world.ecs)
        .next()
        .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(entities: &str) -> SceneDoc {
        re_scene::scene::from_json(&format!(
            r#"{{ "format": "real-engine/scene", "version": 1, "entities": [{entities}] }}"#
        ))
        .unwrap()
    }

    fn town() -> World {
        let mut world = World::new();
        world
            .load(&doc(
                r#"{ "id": "player", "components": { "Transform": { "translation": [1, 2, 3] } } },
                   { "id": "player_cam", "parent": "player" },
                   { "id": "ground" },
                   { "id": "lamp", "parent": "ground" }"#,
            ))
            .unwrap();
        world.propagate_all();
        world
    }

    fn ids(world: &World) -> Vec<String> {
        let mut ids: Vec<String> = world.ids().iter().map(|(id, _)| id.to_string()).collect();
        ids.sort();
        ids
    }

    fn stream() -> StreamConfig {
        StreamConfig::default()
    }

    #[test]
    fn a_switch_keeps_the_kept_and_replaces_the_rest() {
        let mut world = town();
        let kept: BTreeSet<String> = ["player".to_string()].into();
        let harbour = doc(
            r#"{ "id": "player", "components": { "Transform": { "translation": [9, 9, 9] } } },
               { "id": "player_hat", "parent": "player" },
               { "id": "ground" },
               { "id": "boat", "components": { "Transform": { "translation": [0, 0, 5] } } },
               { "id": "rope", "parent": "player" }"#,
        );
        let change = switch(&mut world, &harbour, &kept, &stream()).unwrap();
        assert_eq!(change.stood_in, ["player"]);
        assert_eq!(change.roots, ["ground", "boat"]);
        assert_eq!(ids(&world), ["boat", "ground", "player", "player_cam"]);
        // The kept player is the old one, where it was.
        let pos = world.get("player", "Transform", "translation").unwrap();
        assert_eq!(pos, re_reflect::Value::Vec3(glam::Vec3::new(1.0, 2.0, 3.0)));
        assert!(world.ecs.contains_resource::<Streamer>());
        assert!(!streams(&mut world));
    }

    #[test]
    fn a_switch_that_fails_its_check_changes_nothing() {
        let mut world = town();
        let kept: BTreeSet<String> = ["player".to_string()].into();
        let before = world.state_hash();
        // `player_cam` is kept with the player, so the scene cannot take it.
        let clash = doc(r#"{ "id": "player_cam" }, { "id": "boat" }"#);
        assert_eq!(
            switch(&mut world, &clash, &kept, &stream()).unwrap_err(),
            SceneError::DuplicateId("player_cam".into())
        );
        let dangling = doc(r#"{ "id": "boat", "parent": "lamp" }"#);
        assert!(switch(&mut world, &dangling, &kept, &stream()).is_err());
        assert_eq!(world.state_hash(), before);
        assert_eq!(ids(&world), ["ground", "lamp", "player", "player_cam"]);
    }

    #[test]
    fn a_kept_child_is_not_kept_on_its_own() {
        let mut world = town();
        let kept: BTreeSet<String> = ["player_cam".to_string()].into();
        switch(&mut world, &doc(r#"{ "id": "boat" }"#), &kept, &stream()).unwrap();
        assert_eq!(ids(&world), ["boat"]);
    }

    #[test]
    fn loads_add_beside_and_unloads_take_away() {
        let mut world = town();
        let kept = BTreeSet::new();
        let hud = doc(r#"{ "id": "hud" },
               { "id": "hud_text", "parent": "hud" },
               { "id": "badge", "parent": "player" }"#);
        let change = load(&mut world, &hud, &kept).unwrap();
        assert_eq!(change.roots, ["hud", "badge"]);
        assert_eq!(
            ids(&world),
            [
                "badge",
                "ground",
                "hud",
                "hud_text",
                "lamp",
                "player",
                "player_cam"
            ]
        );
        // An id the World has is refused, and nothing is added.
        let clash = doc(r#"{ "id": "extra" }, { "id": "lamp" }"#);
        assert_eq!(
            load(&mut world, &clash, &kept).unwrap_err(),
            SceneError::DuplicateId("lamp".into())
        );
        assert!(world.ids().get("extra").is_none());
        // Streaming belongs to the main scene.
        let streamed = doc(r#"{ "id": "far", "components": { "Streamed": {} } }"#);
        assert!(matches!(
            load(&mut world, &streamed, &kept),
            Err(SceneError::Invalid(_))
        ));

        unload(&mut world, &change.roots, &kept);
        assert_eq!(ids(&world), ["ground", "lamp", "player", "player_cam"]);
    }

    #[test]
    fn kept_entities_stand_in_for_a_loaded_scene_too() {
        let mut world = town();
        let kept: BTreeSet<String> = ["player".to_string()].into();
        let hud = doc(r#"{ "id": "player" }, { "id": "hud" }"#);
        let change = load(&mut world, &hud, &kept).unwrap();
        assert_eq!(change.stood_in, ["player"]);
        assert_eq!(change.roots, ["hud"]);
        // A kept root survives the unload of a scene that lists it.
        unload(
            &mut world,
            &["hud".to_string(), "player".to_string()],
            &kept,
        );
        assert!(world.ids().get("player").is_some());
        assert!(world.ids().get("hud").is_none());
    }

    #[test]
    fn suppressions_of_sources_that_go_are_forgotten() {
        let mut world = town();
        world.suppress("ground/rock_1").unwrap();
        world.suppress("player/spark").unwrap();
        // `ground` and `player` are not procedural sources: a switch keeps
        // only the suppressions of live sources.
        let kept: BTreeSet<String> = ["player".to_string()].into();
        switch(&mut world, &doc(r#"{ "id": "boat" }"#), &kept, &stream()).unwrap();
        assert!(world.suppressions().is_empty());
        assert_eq!(source_prefixes("a/b/c").collect::<Vec<_>>(), ["a", "a/b"]);
    }
}
