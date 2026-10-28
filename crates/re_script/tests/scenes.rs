//! The `scene` library (ADR-0059 §3) over a host whose game is scripted by the
//! test: requests answer with `SceneLoad`s, refusals raise `RE3100` and
//! `RE3101`, `scene.keep` takes roots only, `SceneLoad:wait` parks until the
//! game ends the load, and a host without a game raises `RE3015`.

mod common;

use std::collections::BTreeSet;

use common::{STEP, Services, project};
use re_core::{Diagnostic, Phase};
use re_scene::World;
use re_script::{
    ContextKind, GameAccess, RuntimeConfig, SceneLoadState, SceneRefusal, SceneRequestKind,
    ScriptHost, ScriptRuntime, codes,
};
use re_types::host::HostServices;

/// A game with three scenes, `title` loaded, that records what it is asked.
struct FakeGame {
    current: String,
    added: Vec<String>,
    loads: Vec<SceneLoadState>,
    kept: BTreeSet<String>,
    unloads: Vec<String>,
}

const SCENES: [&str; 3] = ["harbour", "title", "town"];

impl FakeGame {
    fn new() -> FakeGame {
        FakeGame {
            current: "title".into(),
            added: Vec::new(),
            loads: Vec::new(),
            kept: BTreeSet::new(),
            unloads: Vec::new(),
        }
    }

    /// Ends load `id`, as the game does at EndFrame.
    fn finish(&mut self, id: u64, error: Option<&str>) {
        let load = &mut self.loads[id as usize - 1];
        load.done = true;
        load.progress = if error.is_some() { load.progress } else { 1.0 };
        load.error = error.map(str::to_string);
    }
}

impl GameAccess for FakeGame {
    fn scene_request(&mut self, kind: SceneRequestKind, name: &str) -> Result<u64, SceneRefusal> {
        if !SCENES.contains(&name) {
            return Err(SceneRefusal::Unknown);
        }
        if kind == SceneRequestKind::Load
            && (self.current == name || self.added.iter().any(|a| a == name))
        {
            return Err(SceneRefusal::Refused(format!(
                "scene {name:?} is already loaded"
            )));
        }
        self.loads.push(SceneLoadState {
            name: name.into(),
            progress: 0.0,
            done: false,
            error: None,
        });
        Ok(self.loads.len() as u64)
    }
    fn scene_unload(&mut self, name: &str) -> Result<(), SceneRefusal> {
        if !SCENES.contains(&name) {
            return Err(SceneRefusal::Unknown);
        }
        if self.current == name {
            return Err(SceneRefusal::Refused(format!(
                "{name:?} is the main scene; switch to another scene instead"
            )));
        }
        self.unloads.push(name.into());
        Ok(())
    }
    fn scene_load(&self, id: u64) -> Option<SceneLoadState> {
        self.loads.get(id.checked_sub(1)? as usize).cloned()
    }
    fn scene_current(&self) -> String {
        self.current.clone()
    }
    fn scenes_loaded(&self) -> Vec<String> {
        std::iter::once(self.current.clone())
            .chain(self.added.iter().cloned())
            .collect()
    }
    fn scenes_listed(&self) -> Vec<String> {
        SCENES.iter().map(|s| s.to_string()).collect()
    }
    fn keep(&mut self, id: &str, keep: bool) {
        if keep {
            self.kept.insert(id.into());
        } else {
            self.kept.remove(id);
        }
    }
    fn is_kept(&self, id: &str) -> bool {
        self.kept.contains(id)
    }
    // The save library has its own tests (tests/saves.rs).
    fn save_version(&self) -> u32 {
        1
    }
    fn save_write(&mut self, _: &str, _: i64, _: serde_json::Value) {}
    fn save_read(&mut self, _: &str) -> Option<re_script::SaveSlot> {
        None
    }
    fn save_exists(&mut self, _: &str) -> bool {
        false
    }
    fn save_list(&mut self) -> Vec<re_script::SaveInfo> {
        Vec::new()
    }
    fn save_delete(&mut self, _: &str) {}
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
    /// False runs scripts on a host without a game.
    with_game: bool,
}

impl Rig {
    fn new() -> Rig {
        let mut world = World::new();
        let doc = re_scene::scene::from_json(
            r#"{ "format": "real-engine/scene", "version": 1, "entities": [
                { "id": "player", "name": "Player" },
                { "id": "cam", "name": "Cam", "parent": "player" }
            ] }"#,
        )
        .unwrap();
        world.load(&doc).unwrap();
        Rig {
            rt: ScriptRuntime::new(RuntimeConfig::new(project())).unwrap(),
            world,
            services: Services::default(),
            game: FakeGame::new(),
            with_game: true,
        }
    }

    fn host(&mut self) -> (&mut ScriptRuntime, Host<'_>) {
        let game = self.with_game.then_some(&mut self.game);
        (
            &mut self.rt,
            Host {
                world: &mut self.world,
                services: &mut self.services,
                game,
            },
        )
    }

    fn exec(&mut self, source: &str) -> Result<serde_json::Value, Vec<Diagnostic>> {
        let (rt, mut host) = self.host();
        rt.exec(&mut host, ContextKind::Game, "probe", source)
    }

    fn exec_err(&mut self, source: &str) -> Diagnostic {
        let mut d = self.exec(source).expect_err("the snippet must fail");
        assert_eq!(d.len(), 1, "{d:#?}");
        let d = d.remove(0);
        println!("{}: {}", d.code, d.message);
        d
    }

    fn phase(&mut self, phase: Phase) -> Vec<Diagnostic> {
        let (rt, mut host) = self.host();
        rt.run_phase(&mut host, phase, STEP)
    }

    fn output(&mut self) -> Vec<String> {
        self.rt.take_output().into_iter().map(|l| l.text).collect()
    }
}

#[test]
fn requests_answer_with_scene_loads_and_refusals_raise() {
    let mut rig = Rig::new();
    let v = rig
        .exec(
            r#"
            local l = scene.switch("town")
            return { l.name, l.progress, l.done, l.error == nil, scene.current(), scene.loaded(), scene.list() }
            "#,
        )
        .unwrap();
    assert_eq!(
        v,
        serde_json::json!([
            "town",
            0,
            false,
            true,
            "title",
            ["title"],
            ["harbour", "title", "town"]
        ])
    );
    assert_eq!(rig.game.loads.len(), 1);

    let d = rig.exec_err(r#"scene.switch("towm")"#);
    assert_eq!(d.code, codes::SCENE_UNKNOWN);
    assert!(
        d.message.contains(r#"did you mean "town""#),
        "{}",
        d.message
    );
    let d = rig.exec_err(r#"scene.load("dungeon")"#);
    assert_eq!(d.code, codes::SCENE_UNKNOWN);
    assert!(d.message.contains("harbour, title, town"), "{}", d.message);

    let d = rig.exec_err(r#"scene.load("title")"#);
    assert_eq!(d.code, codes::SCENE_REFUSED);
    assert!(d.message.starts_with("scene.load: "), "{}", d.message);
    let d = rig.exec_err(r#"scene.unload("title")"#);
    assert_eq!(d.code, codes::SCENE_REFUSED);
    assert!(rig.exec(r#"scene.unload("harbour")"#).is_ok());
    assert_eq!(rig.game.unloads, ["harbour"]);

    // SceneLoad is strict, like every engine type.
    let d = rig.exec_err(r#"local l = scene.load("harbour"); return l.nmae"#);
    assert_eq!(d.code, codes::UNSUPPORTED_MEMBER);
}

#[test]
fn keep_takes_roots_only() {
    let mut rig = Rig::new();
    let v = rig
        .exec(
            r#"
            local player = world.get("player")
            scene.keep(player)
            local kept = scene.is_kept(player)
            scene.keep(player, false)
            return { kept, scene.is_kept(player), scene.is_kept(world.get("cam")) }
            "#,
        )
        .unwrap();
    assert_eq!(v, serde_json::json!([true, false, false]));
    rig.exec(r#"scene.keep(world.get("player"))"#).unwrap();
    assert!(rig.game.kept.contains("player"));

    let d = rig.exec_err(r#"scene.keep(world.get("cam"))"#);
    assert_eq!(d.code, codes::SCENE_REFUSED);
    assert!(d.message.contains("not a root"), "{}", d.message);
    let d = rig.exec_err(r#"scene.keep("player")"#);
    assert_eq!(d.code, codes::BAD_ARGUMENT);
}

#[test]
fn wait_parks_until_the_game_ends_the_load() {
    let mut rig = Rig::new();
    rig.exec(
        r#"
        task.spawn(function()
            local l = scene.switch("town")
            l:wait()
            print("switched", l.done, l.progress)
            local again = scene.load("harbour")
            again:wait()
            print("failed", again.error)
        end)
        "#,
    )
    .unwrap();
    for _ in 0..3 {
        assert!(rig.phase(Phase::PreRender).is_empty());
    }
    assert!(rig.output().is_empty(), "still waiting");
    assert_eq!(rig.rt.context(ContextKind::Game).parked_threads(), 1);

    // The game ends the load at EndFrame and wakes the waiting threads; they
    // run on in the next phase that runs scripts, with no fixed step (a time
    // scale of 0 runs none).
    rig.game.finish(1, None);
    re_script::wake_scene_loads(&rig.rt, &[1]).unwrap();
    assert!(rig.phase(Phase::PreRender).is_empty());
    assert_eq!(rig.output(), ["switched\ttrue\t1"]);

    rig.game
        .finish(2, Some("scenes/harbour.scene.json does not parse"));
    re_script::wake_scene_loads(&rig.rt, &[2]).unwrap();
    assert!(rig.phase(Phase::PreRender).is_empty());
    assert_eq!(
        rig.output(),
        ["failed\tscenes/harbour.scene.json does not parse"]
    );
    assert_eq!(rig.rt.context(ContextKind::Game).parked_threads(), 0);

    // A load that is over does not wait: the thread reaches `wait` a step
    // after the game ended its load, and goes on at once.
    rig.exec(
        r#"
        task.spawn(function()
            local l = scene.switch("town")
            task.wait()
            l:wait()
            print("no wait", l.done)
        end)
        "#,
    )
    .unwrap();
    rig.game.finish(3, None);
    for phase in [
        Phase::PreRender,
        Phase::PreSimulation,
        Phase::PostSimulation,
    ] {
        assert!(rig.phase(phase).is_empty());
    }
    assert_eq!(rig.output(), ["no wait\ttrue"]);
    assert_eq!(rig.rt.context(ContextKind::Game).parked_threads(), 0);

    // From a callback the engine calls directly, `wait` cannot park.
    let d = rig.exec_err(r#"scene.load("harbour"):wait()"#);
    assert_eq!(d.code, codes::YIELD);
}

#[test]
fn a_host_without_a_game_raises_service_unavailable() {
    let mut rig = Rig::new();
    rig.with_game = false;
    for call in [
        r#"scene.switch("town")"#,
        "scene.current()",
        "scene.list()",
        r#"scene.keep(world.get("player"))"#,
    ] {
        let d = rig.exec_err(call);
        assert_eq!(d.code, codes::SERVICE_UNAVAILABLE, "{call}");
        assert!(d.message.contains("no game is running"), "{}", d.message);
    }
}
