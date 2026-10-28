//! The `save` library (ADR-0059 §7) over a host whose game keeps slots in a
//! map: writes keep exactly what reads give back, values a save cannot hold
//! raise `RE3103` naming where they are, bad slot names `RE3102`, and reads
//! of older data run the registered migration steps in turn (`RE3104`,
//! `RE3105`, `RE3106` when they cannot).

mod common;

use std::collections::BTreeMap;

use common::{Services, project};
use re_core::Diagnostic;
use re_scene::World;
use re_script::{
    ContextKind, GameAccess, RuntimeConfig, SaveInfo, SaveSlot, SceneLoadState, SceneRefusal,
    SceneRequestKind, ScriptHost, ScriptRuntime, codes,
};
use re_types::host::HostServices;
use serde_json::json;

/// A game that keeps its slots in a map.
struct FakeGame {
    version: u32,
    slots: BTreeMap<String, SaveSlot>,
}

impl GameAccess for FakeGame {
    fn scene_request(&mut self, _: SceneRequestKind, _: &str) -> Result<u64, SceneRefusal> {
        Err(SceneRefusal::Unknown)
    }
    fn scene_unload(&mut self, _: &str) -> Result<(), SceneRefusal> {
        Err(SceneRefusal::Unknown)
    }
    fn scene_load(&self, _: u64) -> Option<SceneLoadState> {
        None
    }
    fn scene_current(&self) -> String {
        "main".into()
    }
    fn scenes_loaded(&self) -> Vec<String> {
        vec!["main".into()]
    }
    fn scenes_listed(&self) -> Vec<String> {
        vec!["main".into()]
    }
    fn keep(&mut self, _: &str, _: bool) {}
    fn is_kept(&self, _: &str) -> bool {
        false
    }
    fn save_version(&self) -> u32 {
        self.version
    }
    fn save_write(&mut self, slot: &str, saved_at: i64, data: serde_json::Value) {
        let data_version = self.version;
        self.slots.insert(
            slot.into(),
            SaveSlot {
                data_version,
                saved_at,
                data,
            },
        );
    }
    fn save_read(&mut self, slot: &str) -> Option<SaveSlot> {
        self.slots.get(slot).cloned()
    }
    fn save_exists(&mut self, slot: &str) -> bool {
        self.slots.contains_key(slot)
    }
    fn save_list(&mut self) -> Vec<SaveInfo> {
        self.slots
            .iter()
            .map(|(slot, s)| SaveInfo {
                slot: slot.clone(),
                data_version: s.data_version,
                saved_at: s.saved_at,
            })
            .collect()
    }
    fn save_delete(&mut self, slot: &str) {
        self.slots.remove(slot);
    }
}

struct Host<'a> {
    world: &'a mut World,
    services: &'a mut Services,
    game: Option<&'a mut FakeGame>,
}

impl ScriptHost for Host<'_> {
    fn world(&mut self) -> &mut World {
        self.world
    }
    fn services(&mut self) -> &mut dyn HostServices {
        self.services
    }
    fn game(&mut self) -> Option<&mut dyn GameAccess> {
        self.game.as_deref_mut().map(|g| g as &mut dyn GameAccess)
    }
}

struct Rig {
    rt: ScriptRuntime,
    world: World,
    services: Services,
    game: FakeGame,
    with_game: bool,
}

impl Rig {
    fn new(version: u32) -> Rig {
        let mut world = World::new();
        let doc = re_scene::scene::from_json(
            r#"{ "format": "real-engine/scene", "version": 1, "entities": [ { "id": "player", "name": "Player" } ] }"#,
        )
        .unwrap();
        world.load(&doc).unwrap();
        Rig {
            rt: ScriptRuntime::new(RuntimeConfig::new(project())).unwrap(),
            world,
            services: Services::default(),
            game: FakeGame {
                version,
                slots: BTreeMap::new(),
            },
            with_game: true,
        }
    }

    fn exec(&mut self, source: &str) -> Result<serde_json::Value, Vec<Diagnostic>> {
        let game = self.with_game.then_some(&mut self.game);
        let mut host = Host {
            world: &mut self.world,
            services: &mut self.services,
            game,
        };
        self.rt.exec(&mut host, ContextKind::Game, "probe", source)
    }

    fn exec_err(&mut self, source: &str) -> Diagnostic {
        let mut d = self.exec(source).expect_err("the snippet must fail");
        assert_eq!(d.len(), 1, "{d:#?}");
        let d = d.remove(0);
        println!("{}: {}", d.code, d.message);
        d
    }

    fn slot(&self, slot: &str) -> &SaveSlot {
        &self.game.slots[slot]
    }

    /// Puts version `data_version` data in `slot`, as an older build wrote it.
    fn put(&mut self, slot: &str, data_version: u32, data: serde_json::Value) {
        self.game.slots.insert(
            slot.into(),
            SaveSlot {
                data_version,
                saved_at: 1_767_225_600,
                data,
            },
        );
    }
}

#[test]
fn a_write_keeps_what_a_read_gives_back() {
    let mut rig = Rig::new(1);
    let v = rig
        .exec(
            r#"
            local before = os.time()
            save.write("slot1", {
                coins = 12, ratio = 0.25, name = "Ada", alive = true,
                visited = { "town", "harbour" }, empty = {},
                player = { pos = { 1.5, 0, -2 }, ["two words"] = -0.0 },
            })
            local after = os.time()
            local back = save.read("slot1")
            back.coins = 99 -- a read is a copy
            local again = save.read("slot1")
            return {
                times = { before, after },
                checks = { again.coins, again.visited[2], again.player.pos[3],
                           save.exists("slot1"), save.exists("slot2"), save.read("slot2") == nil },
            }
            "#,
        )
        .unwrap();
    let slot = rig.slot("slot1");
    assert_eq!(slot.data_version, 1);
    assert_eq!(
        slot.data,
        json!({
            "alive": true, "coins": 12, "empty": {}, "name": "Ada",
            "player": { "pos": [1.5, 0, -2], "two words": -0.0 },
            "ratio": 0.25, "visited": ["town", "harbour"],
        })
    );
    // Keys are written sorted, so the file does not depend on Luau's order.
    let text = serde_json::to_string(&slot.data).unwrap();
    assert!(
        text.starts_with(r#"{"alive":true,"coins":12,"empty":{},"name":"Ada","player""#),
        "{text}"
    );
    // `saved_at` is the game's own os.time() at the write.
    let (before, after) = (
        v["times"][0].as_i64().unwrap(),
        v["times"][1].as_i64().unwrap(),
    );
    assert!((before..=after).contains(&slot.saved_at), "{v}");
    assert_eq!(v["checks"], json!([12, "harbour", -2, true, false, true]));
}

#[test]
fn list_delete_and_version() {
    let mut rig = Rig::new(3);
    rig.put("b", 2, json!({}));
    let v = rig
        .exec(
            r#"
            save.write("a", { n = 1 })
            local listed = save.list()
            save.delete("b")
            save.delete("never-written")
            return { save.version(), listed, save.exists("b"), #save.list() }
            "#,
        )
        .unwrap();
    let a_at = rig.slot("a").saved_at;
    assert_eq!(
        v,
        json!([
            3,
            [
                { "slot": "a", "version": 3, "saved_at": a_at },
                { "slot": "b", "version": 2, "saved_at": 1_767_225_600 },
            ],
            false,
            1
        ])
    );
}

#[test]
fn values_a_save_cannot_hold_raise_re3103_naming_where_they_are() {
    let mut rig = Rig::new(1);
    let cases = [
        (
            r#"save.write("s", { player = { pos = vector.create(1, 2, 3) } })"#,
            "data.player.pos is a vector, which a save cannot hold; store {x, y, z} instead",
        ),
        (
            r#"save.write("s", { target = world.get("player") })"#,
            "data.target is an entity, which a save cannot hold; store the entity's id instead",
        ),
        (
            r#"save.write("s", { on_load = print })"#,
            "data.on_load is a function, which a save cannot hold",
        ),
        (
            r#"save.write("s", { ["hp max"] = 0/0 })"#,
            r#"data["hp max"] is NaN, which JSON cannot hold"#,
        ),
        (
            r#"save.write("s", { list = { 1, 2, 3, [5] = 5 } })"#,
            "data.list has the number key 5, which is not part of a sequence 1 to n",
        ),
        (
            r#"save.write("s", { 1, 2, x = 3 })"#,
            "data mixes number keys with other keys",
        ),
        (
            r#"save.write("s", { [true] = 1 })"#,
            "data has a boolean key",
        ),
        (
            r#"save.write("s", { name = "\255" })"#,
            "data.name is a string that is not UTF-8 text",
        ),
        (
            r#"save.write("s", 12)"#,
            "the data must be a table, not a number",
        ),
        (
            r#"local t = {}; t.self = t; save.write("s", t)"#,
            "data nests tables deeper than 64 levels (or a table holds itself)",
        ),
        (
            r#"save.write("s", { blob = string.rep("x", 4 * 1024 * 1024) })"#,
            r#"slot "s" would hold 4.0 MiB as JSON, more than the 4 MiB a slot holds"#,
        ),
    ];
    for (source, message) in cases {
        let d = rig.exec_err(source);
        assert_eq!(d.code, codes::SAVE_VALUE_INVALID, "{source}");
        assert!(d.message.starts_with("save.write: "), "{}", d.message);
        assert!(d.message.contains(message), "{source}: {}", d.message);
    }
    assert!(rig.game.slots.is_empty());

    // 64 levels are fine; the 65th is not.
    rig.exec(
        r#"local t = {}; local at = t
           for _ = 2, 64 do at.next = {}; at = at.next end
           save.write("deep", t)"#,
    )
    .unwrap();
    let d = rig.exec_err(
        r#"local t = {}; local at = t
           for _ = 2, 65 do at.next = {}; at = at.next end
           save.write("deep", t)"#,
    );
    assert_eq!(d.code, codes::SAVE_VALUE_INVALID);
}

#[test]
fn slot_names_are_checked_by_every_call() {
    let mut rig = Rig::new(1);
    for call in [
        r#"save.write("Slot 1", {})"#,
        r#"save.read("")"#,
        r#"save.exists("../x")"#,
        r#"save.delete("A")"#,
    ] {
        let d = rig.exec_err(call);
        assert_eq!(d.code, codes::SAVE_SLOT_INVALID, "{call}");
        assert!(
            d.message
                .contains("use 1 to 64 characters from a-z, 0-9, _ and -"),
            "{}",
            d.message
        );
    }
}

#[test]
fn reads_migrate_older_data_step_by_step() {
    let mut rig = Rig::new(3);
    rig.put("old", 1, json!({ "gold": 5 }));
    rig.put("mid", 2, json!({ "coins": 7 }));
    let v = rig
        .exec(
            r#"
            local ran = {}
            save.migrate(1, function(d)
                table.insert(ran, 1)
                return { coins = d.gold }
            end)
            save.migrate(2, function(d)
                table.insert(ran, 2)
                d.coins_x2 = d.coins * 2
                return d
            end)
            local old = save.read("old")
            local mid = save.read("mid")
            return { old, mid, ran }
            "#,
        )
        .unwrap();
    assert_eq!(
        v,
        json!([
            { "coins": 5, "coins_x2": 10 },
            { "coins": 7, "coins_x2": 14 },
            [1, 2, 2]
        ])
    );
    // A read leaves the slot as it was until the game writes it.
    assert_eq!(rig.slot("old").data_version, 1);
}

#[test]
fn migrations_that_cannot_run_raise() {
    let mut rig = Rig::new(3);
    rig.put("old", 1, json!({ "gold": 5 }));
    rig.put("new", 4, json!({}));

    // Every step is found before any runs.
    let v = rig
        .exec(
            r#"
            local ran = 0
            save.migrate(1, function(d) ran += 1; return d end)
            local ok, err = pcall(save.read, "old")
            return { ok, ran }
            "#,
        )
        .unwrap();
    assert_eq!(v, json!([false, 0]));
    let d = rig.exec_err(r#"save.read("old")"#);
    assert_eq!(d.code, codes::SAVE_MIGRATION_MISSING);
    assert!(
        d.message.contains(
            r#"slot "old" holds version 1 data, and no step migrates version 2 to 3; add save.migrate(2, step)"#
        ),
        "{}",
        d.message
    );

    let d = rig.exec_err(
        r#"save.migrate(2, function(d) error("no coins") end)
           save.read("old")"#,
    );
    assert_eq!(d.code, codes::SAVE_MIGRATION_FAILED);
    // The step's own position goes last, so the diagnostic stays at the read.
    assert!(
        d.message
            .ends_with(r#"the step from version 2 failed on slot "old": no coins (probe:1)"#),
        "{}",
        d.message
    );
    assert!(
        matches!(
            &d.location,
            Some(re_core::Location::File { line: Some(2), .. })
        ),
        "{d:?}"
    );
    let d = rig.exec_err(
        r#"save.migrate(2, function(d) return 3 end)
           save.read("old")"#,
    );
    assert_eq!(d.code, codes::SAVE_MIGRATION_FAILED);
    assert!(
        d.message
            .contains("the step from version 2 returned a number, not a table"),
        "{}",
        d.message
    );

    let d = rig.exec_err(r#"save.read("new")"#);
    assert_eq!(d.code, codes::SAVE_TOO_NEW);
    assert!(
        d.message.contains(
            r#"slot "new" holds version 4 data, newer than this build's [saves] version 3"#
        ),
        "{}",
        d.message
    );

    for from in ["0", "1.5", "3"] {
        let d = rig.exec_err(&format!("save.migrate({from}, function(d) return d end)"));
        assert_eq!(d.code, codes::BAD_ARGUMENT, "{from}");
        assert!(
            d.message.contains("it must be a whole number from 1 to 2"),
            "{}",
            d.message
        );
    }
}

#[test]
fn without_a_game_the_library_raises_re3015() {
    let mut rig = Rig::new(1);
    rig.with_game = false;
    let d = rig.exec_err(r#"save.write("slot1", {})"#);
    assert_eq!(d.code, codes::SERVICE_UNAVAILABLE);
    assert!(
        d.message
            .contains("save.write is not available here: no game is running"),
        "{}",
        d.message
    );
}
