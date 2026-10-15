//! `Tags`, the `TagIndex` and `Attributes`: the index stays exact across
//! spawn, despawn, field writes, undo, redo, scene load and fork; the change
//! events describe every change; and both components save and load.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use indexmap::IndexMap;
use re_command::{History, Op, Origin, Transaction};
use re_reflect::{Doc, Value};
use re_scene::ids::splitmix64;
use re_scene::scene::{self, SaveOptions};
use re_scene::{Attributes, Entity, StableId, TagChanged, Tags, World};

const TAGS: [&str; 5] = ["Coin", "Door", "Enemy", "Hazard", "Spinner"];

/// Every (tag, entity) pair found by scanning the world.
fn scan(w: &mut World) -> BTreeSet<(String, Entity)> {
    let mut q = w.ecs.query::<(Entity, &Tags)>();
    q.iter(&w.ecs)
        .flat_map(|(e, t)| t.tags.iter().map(move |tag| (tag.clone(), e)))
        .collect()
}

/// Every (tag, entity) pair in the index.
fn indexed(w: &World) -> BTreeSet<(String, Entity)> {
    let index = w.tag_index();
    index
        .tags()
        .into_iter()
        .flat_map(|tag| index.tagged(tag).map(move |e| (tag.to_string(), e)))
        .collect()
}

/// The same pairs named by stable id, comparable across worlds.
fn by_id(w: &World, pairs: &BTreeSet<(String, Entity)>) -> BTreeSet<(String, String)> {
    pairs
        .iter()
        .map(|(t, e)| (t.clone(), w.id_of(*e).unwrap_or("?").to_string()))
        .collect()
}

fn tag_doc(tags: &[String]) -> Doc {
    Doc::List(tags.iter().cloned().map(Doc::Str).collect())
}

struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        (splitmix64(&mut self.0) % n as u64) as usize
    }
}

#[test]
fn tag_index_equals_a_full_scan_after_10000_random_steps() {
    let mut w = World::new();
    w.record_tag_changes();
    let mut history = History::default();
    let mut rng = Rng(0x5eed);
    // The index as rebuilt from the change events alone.
    let mut from_events: BTreeSet<(String, Entity)> = BTreeSet::new();
    let mut next = 0u32;
    let mut counts: HashMap<&str, usize> = HashMap::new();

    for step in 0..10_000 {
        let ids: Vec<String> = {
            let mut v: Vec<String> = w.ids().iter().map(|(id, _)| id.to_string()).collect();
            v.sort();
            v
        };
        let pick = |rng: &mut Rng| ids[rng.below(ids.len())].clone();
        let tags_of = |w: &World, id: &str| -> Option<Vec<String>> {
            let e = w.entity(id).ok()?;
            w.ecs.get::<Tags>(e).map(|t| t.tags.clone())
        };
        let roll = if ids.is_empty() { 0 } else { rng.below(100) };
        let (kind, ops): (&str, Vec<Op>) = match roll {
            0..=17 => {
                next += 1;
                let mut components = IndexMap::new();
                if rng.below(2) == 0 {
                    let tag = TAGS[rng.below(TAGS.len())].to_string();
                    components.insert(
                        "Tags".to_string(),
                        IndexMap::from([("tags".to_string(), tag_doc(&[tag]))]),
                    );
                }
                let parent = (!ids.is_empty() && rng.below(3) == 0).then(|| pick(&mut rng));
                (
                    "spawn",
                    vec![Op::Spawn {
                        id: Some(format!("n{next}")),
                        name: String::new(),
                        parent,
                        index: None,
                        components,
                    }],
                )
            }
            18..=44 => {
                let id = pick(&mut rng);
                let tag = TAGS[rng.below(TAGS.len())].to_string();
                match tags_of(&w, &id) {
                    Some(mut tags) => {
                        tags.push(tag);
                        (
                            "tag",
                            vec![Op::SetField {
                                id,
                                component: "Tags".into(),
                                field: "tags".into(),
                                value: tag_doc(&tags),
                            }],
                        )
                    }
                    None => (
                        "add",
                        vec![Op::AddComponent {
                            id,
                            component: "Tags".into(),
                            fields: IndexMap::from([("tags".to_string(), tag_doc(&[tag]))]),
                        }],
                    ),
                }
            }
            45..=59 => {
                let id = pick(&mut rng);
                match tags_of(&w, &id) {
                    Some(mut tags) if !tags.is_empty() => {
                        tags.remove(rng.below(tags.len()));
                        (
                            "untag",
                            vec![Op::SetField {
                                id,
                                component: "Tags".into(),
                                field: "tags".into(),
                                value: tag_doc(&tags),
                            }],
                        )
                    }
                    Some(_) => (
                        "remove",
                        vec![Op::RemoveComponent {
                            id,
                            component: "Tags".into(),
                        }],
                    ),
                    None => ("noop", vec![]),
                }
            }
            60..=71 => ("despawn", vec![Op::Despawn { id: pick(&mut rng) }]),
            72..=87 => ("undo", vec![]),
            _ => ("redo", vec![]),
        };
        match kind {
            "undo" => {
                history.undo(&mut w).unwrap();
            }
            "redo" => {
                history.redo(&mut w).unwrap();
            }
            "noop" => {}
            _ => {
                history
                    .execute(&mut w, Transaction::new(kind, Origin::User, ops))
                    .unwrap_or_else(|e| panic!("step {step} ({kind}): {e}"));
            }
        }
        *counts.entry(kind).or_default() += 1;

        let full = scan(&mut w);
        assert_eq!(indexed(&w), full, "step {step} ({kind})");
        for TagChanged { entity, tag, added } in w.drain_tag_changes() {
            if added {
                assert!(
                    from_events.insert((tag, entity)),
                    "step {step}: added twice"
                );
            } else {
                assert!(
                    from_events.remove(&(tag, entity)),
                    "step {step}: removed twice"
                );
            }
        }
        assert_eq!(from_events, full, "step {step} ({kind}): events");
    }
    for kind in [
        "spawn", "tag", "add", "untag", "remove", "despawn", "undo", "redo",
    ] {
        assert!(
            counts.get(kind).copied().unwrap_or(0) > 50,
            "{kind}: {counts:?}"
        );
    }

    // A fork and a save-load rebuild the same index.
    let full = scan(&mut w);
    let want = by_id(&w, &full);
    assert!(
        want.len() > 20,
        "the run ends with tagged entities: {}",
        want.len()
    );
    let mut fork = w.fork();
    assert_eq!(by_id(&fork, &indexed(&fork)), want);
    let forked = scan(&mut fork);
    assert_eq!(by_id(&fork, &forked), want);
    let text = scene::to_json(&w.save(SaveOptions::default()));
    let mut loaded = World::new();
    loaded.load(&scene::from_json(&text).unwrap()).unwrap();
    assert_eq!(by_id(&loaded, &indexed(&loaded)), want);
    assert_eq!(loaded.state_hash(), w.state_hash());
}

#[test]
fn tags_hold_each_tag_once_and_feed_only_when_recording() {
    let mut w = World::new();
    let e = w.ecs.spawn(StableId("a".into())).id();
    let key = w.registry.key("Tags").unwrap();
    let list =
        |tags: &[&str]| Value::List(tags.iter().map(|t| Value::Str(t.to_string())).collect());
    w.registry
        .insert(&mut w.ecs, e, key, vec![list(&["x", "y", "x"])])
        .unwrap();
    assert_eq!(w.ecs.get::<Tags>(e).unwrap().tags, ["x", "y"]);
    // A write equal after dropping repeats changes nothing.
    w.ecs.clear_trackers();
    w.registry
        .set_field(&mut w.ecs, e, key, 0, list(&["x", "y", "y"]))
        .unwrap();
    let mut changed = w
        .ecs
        .query_filtered::<Entity, re_scene::ecs::query::Changed<Tags>>();
    assert_eq!(changed.iter(&w.ecs).count(), 0);
    assert!(w.drain_tag_changes().is_empty(), "not recording yet");
    w.record_tag_changes();
    w.registry
        .set_field(&mut w.ecs, e, key, 0, list(&["y", "z"]))
        .unwrap();
    assert_eq!(
        w.drain_tag_changes(),
        [
            TagChanged {
                entity: e,
                tag: "x".into(),
                added: false
            },
            TagChanged {
                entity: e,
                tag: "z".into(),
                added: true
            }
        ]
    );
    assert_eq!(w.tag_index().tagged("z").collect::<Vec<_>>(), [e]);
    assert_eq!(w.tag_index().count("x"), 0);
    assert!(w.tag_index().has("y", e));
    w.ecs.despawn(e);
    assert_eq!(w.drain_tag_changes().len(), 2, "despawn removes every tag");
    assert!(w.tag_index().tags().is_empty());
}

#[test]
fn attribute_events_name_each_changed_key() {
    let mut w = World::new();
    w.record_attribute_changes();
    let mut history = History::default();
    let map = |pairs: &[(&str, Doc)]| {
        Doc::Map(
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect(),
        )
    };
    let run = |w: &mut World, h: &mut History, ops: Vec<Op>| {
        h.execute(w, Transaction::new("t", Origin::User, ops))
            .unwrap();
    };
    run(
        &mut w,
        &mut history,
        vec![Op::Spawn {
            id: Some("door".into()),
            name: "Door".into(),
            parent: None,
            index: None,
            components: IndexMap::from([(
                "Attributes".to_string(),
                IndexMap::from([(
                    "values".to_string(),
                    map(&[("Open", Doc::Bool(false)), ("Speed", Doc::Int(4))]),
                )]),
            )]),
        }],
    );
    let e = w.entity("door").unwrap();
    let keys = |w: &mut World| -> Vec<String> {
        w.drain_attribute_changes()
            .into_iter()
            .map(|c| {
                assert_eq!(c.entity, e);
                c.key
            })
            .collect()
    };
    assert_eq!(keys(&mut w), ["Open", "Speed"]);
    assert_eq!(
        w.ecs.get::<Attributes>(e).unwrap().get("Speed"),
        Some(&Value::Float(4.0))
    );

    // Change one key, add one, drop one.
    run(
        &mut w,
        &mut history,
        vec![Op::SetField {
            id: "door".into(),
            component: "Attributes".into(),
            field: "values".into(),
            value: map(&[("Open", Doc::Bool(true)), ("Owner", Doc::Str("Ana".into()))]),
        }],
    );
    assert_eq!(keys(&mut w), ["Open", "Owner", "Speed"]);
    // An equal write records nothing.
    run(
        &mut w,
        &mut history,
        vec![Op::SetField {
            id: "door".into(),
            component: "Attributes".into(),
            field: "values".into(),
            value: map(&[("Open", Doc::Bool(true)), ("Owner", Doc::Str("Ana".into()))]),
        }],
    );
    assert_eq!(keys(&mut w), Vec::<String>::new());
    history.undo(&mut w).unwrap();
    history.undo(&mut w).unwrap();
    assert_eq!(keys(&mut w), ["Open", "Owner", "Speed"]);
    run(
        &mut w,
        &mut history,
        vec![Op::RemoveComponent {
            id: "door".into(),
            component: "Attributes".into(),
        }],
    );
    assert_eq!(keys(&mut w), ["Open", "Speed"]);
    history.undo(&mut w).unwrap();
    assert_eq!(keys(&mut w), ["Open", "Speed"]);
}

#[test]
fn attributes_and_tags_save_and_load() {
    let mut w = World::new();
    let text = r#"{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    {
      "id": "coin",
      "name": "Coin",
      "components": {
        "Attributes": {
          "values": {
            "Value": 5.0,
            "Glow": {"$type": "color", "value": [1.0, 0.8, 0.0, 1.0]},
            "Spin": {"$type": "vec3", "value": [0.0, 1.0, 0.0]},
            "Kind": "gold"
          }
        },
        "Tags": { "tags": ["Coin", "Pickup"] }
      }
    }
  ]
}"#;
    w.load(&scene::from_json(text).unwrap()).unwrap();
    let e = w.entity("coin").unwrap();
    assert!(w.tag_index().has("Pickup", e));
    let attrs = &w.ecs.get::<Attributes>(e).unwrap().values;
    assert_eq!(attrs["Value"], Value::Float(5.0));
    assert_eq!(attrs["Kind"], Value::Str("gold".into()));
    let saved = scene::to_json(&w.save(SaveOptions::default()));
    let mut again = World::new();
    again.load(&scene::from_json(&saved).unwrap()).unwrap();
    assert_eq!(again.state_hash(), w.state_hash());
    assert_eq!(scene::to_json(&again.save(SaveOptions::default())), saved);

    // The scene schema describes both components.
    let schema = w.scene_json_schema();
    assert_eq!(
        schema["$defs"]["Tags"]["properties"]["tags"]["items"]["type"],
        "string"
    );
    assert_eq!(
        schema["$defs"]["Attributes"]["properties"]["values"]["type"],
        "object"
    );
    let described: BTreeMap<String, serde_json::Value> = w.describe()["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["name"].as_str().unwrap().to_string(), c.clone()))
        .collect();
    assert_eq!(described["Attributes"]["fields"][0]["type"]["kind"], "map");
}
