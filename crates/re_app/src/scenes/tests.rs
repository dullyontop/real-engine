//! The scenes state over a World and files held in memory: requests, the
//! order they apply in, failures, kept entities, the log and its check.

use std::collections::BTreeMap;

use re_core::Severity;
use re_core::files::{Form, LayerKind, MemLayer, ProjectFiles, ProjectPath};
use re_proc::stream::{Lateness, StreamConfig};
use re_scene::World;
use re_script::{GameAccess, SceneRefusal, SceneRequestKind};
use re_types::{SceneChange, SceneChangeKind};

use super::{EndFrame, SceneList, Scenes};
use crate::codes;
use crate::config::ProjectConfig;

fn scene(entities: &str) -> String {
    format!(r#"{{ "format": "real-engine/scene", "version": 1, "entities": [{entities}] }}"#)
}

fn files() -> ProjectFiles {
    let mut layer = MemLayer::new(LayerKind::Memory);
    let mut add = |path: &str, text: String| {
        layer.insert(
            ProjectPath::new(path).unwrap(),
            text.into_bytes(),
            Form::Source,
        );
    };
    add(
        "scenes/title.scene.json",
        scene(r#"{ "id": "menu" }, { "id": "player" }"#),
    );
    add(
        "scenes/town.scene.json",
        scene(
            r#"{ "id": "player" }, { "id": "square" }, { "id": "fountain", "parent": "square" }"#,
        ),
    );
    add("scenes/harbour.scene.json", scene(r#"{ "id": "pier" }"#));
    add(
        "scenes/hud.scene.json",
        scene(r#"{ "id": "hud" }, { "id": "hud_text", "parent": "hud" }"#),
    );
    add("scenes/broken.scene.json", "{ not json".to_string());
    add("scenes/clash.scene.json", scene(r#"{ "id": "menu" }"#));
    ProjectFiles::for_tests(layer)
}

fn list() -> SceneList {
    let project = ProjectConfig {
        entry_scene: "scenes/title.scene.json".into(),
        ..ProjectConfig::default()
    };
    let scenes: BTreeMap<String, String> = ["title", "town", "harbour", "hud", "broken", "clash"]
        .iter()
        .map(|n| (n.to_string(), format!("scenes/{n}.scene.json")))
        .collect();
    SceneList::new(&project, &scenes).unwrap()
}

/// A run begun in the title scene.
struct Run {
    world: World,
    files: ProjectFiles,
    scenes: Scenes,
    frame: u64,
    lateness: Lateness,
}

impl Run {
    fn new() -> Run {
        let files = files();
        let mut world = World::new();
        let title = super::read::read(&files, "title", "scenes/title.scene.json").unwrap();
        world.load(&title).unwrap();
        Run {
            world,
            files,
            scenes: Scenes::new(list(), "scenes/title.scene.json"),
            frame: 0,
            lateness: Lateness::Wait,
        }
    }

    /// Ends the frame; returns its diagnostics.
    fn end_frame(&mut self) -> super::Applied {
        let stream = StreamConfig::default();
        let mut w = EndFrame {
            world: &mut self.world,
            files: &self.files,
            stream: &stream,
            frame: self.frame,
            lateness: self.lateness,
            warm: None,
        };
        let applied = self.scenes.end_frame(&mut w);
        self.frame += 1;
        applied
    }

    fn switch(&mut self, name: &str) -> u64 {
        self.scenes
            .scene_request(SceneRequestKind::Switch, name)
            .unwrap()
    }

    fn load(&mut self, name: &str) -> Result<u64, SceneRefusal> {
        self.scenes.scene_request(SceneRequestKind::Load, name)
    }

    fn ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .world
            .ids()
            .iter()
            .map(|(id, _)| id.to_string())
            .collect();
        ids.sort();
        ids
    }

    fn error(&self, id: u64) -> Option<String> {
        let load = self.scenes.scene_load(id).unwrap();
        assert!(load.done, "load {id} is not over");
        load.error
    }
}

fn change(frame: u64, kind: SceneChangeKind, name: &str) -> SceneChange {
    SceneChange::new(frame, kind, name)
}

#[test]
fn a_switch_applies_at_the_end_of_the_frame_that_asked() {
    let mut run = Run::new();
    assert_eq!(run.scenes.scene_current(), "title");
    let id = run.switch("town");
    let load = run.scenes.scene_load(id).unwrap();
    assert_eq!(
        (load.name.as_str(), load.progress, load.done),
        ("town", 0.0, false)
    );
    assert_eq!(
        run.scenes.scene_current(),
        "title",
        "nothing changes before EndFrame"
    );

    let applied = run.end_frame();
    assert!(applied.changed && applied.switched);
    assert!(applied.diagnostics.is_empty(), "{:?}", applied.diagnostics);
    assert_eq!(run.ids(), ["fountain", "player", "square"]);
    assert_eq!(run.scenes.scene_current(), "town");
    let load = run.scenes.scene_load(id).unwrap();
    assert_eq!((load.progress, load.done, load.error), (1.0, true, None));
    assert_eq!(run.scenes.take_ended(), [id]);
    assert_eq!(
        run.scenes.log(),
        [change(0, SceneChangeKind::Switch, "town")]
    );
}

#[test]
fn loads_go_beside_the_main_scene_and_unloads_take_them_away() {
    let mut run = Run::new();
    let hud = run.load("hud").unwrap();
    assert_eq!(
        run.load("hud"),
        Err(SceneRefusal::Refused(
            r#"scene "hud" is already asked for"#.into()
        ))
    );
    run.end_frame();
    assert_eq!(run.error(hud), None);
    assert_eq!(run.scenes.scenes_loaded(), ["title", "hud"]);
    assert_eq!(run.ids(), ["hud", "hud_text", "menu", "player"]);
    assert_eq!(
        run.load("hud"),
        Err(SceneRefusal::Refused(
            r#"scene "hud" is already loaded"#.into()
        ))
    );
    assert_eq!(
        run.load("title"),
        Err(SceneRefusal::Refused(
            r#"scene "title" is already loaded"#.into()
        ))
    );
    assert_eq!(run.load("nowhere"), Err(SceneRefusal::Unknown));

    // Unloading the main scene, or one not loaded, is refused.
    assert!(
        matches!(run.scenes.scene_unload("title"), Err(SceneRefusal::Refused(m)) if m.contains("main scene"))
    );
    assert!(
        matches!(run.scenes.scene_unload("harbour"), Err(SceneRefusal::Refused(m)) if m.contains("not loaded"))
    );
    run.scenes.scene_unload("hud").unwrap();
    run.end_frame();
    assert_eq!(run.scenes.scenes_loaded(), ["title"]);
    assert_eq!(run.ids(), ["menu", "player"]);
    assert_eq!(
        run.scenes.log(),
        [
            change(0, SceneChangeKind::Load, "hud"),
            change(1, SceneChangeKind::Unload, "hud"),
        ]
    );
}

#[test]
fn unloading_a_scene_still_loading_cancels_its_load() {
    let mut run = Run::new();
    let hud = run.load("hud").unwrap();
    run.scenes.scene_unload("hud").unwrap();
    assert_eq!(run.error(hud).as_deref(), Some("cancelled by scene.unload"));
    let applied = run.end_frame();
    assert!(!applied.changed);
    assert!(run.scenes.log().is_empty());
}

#[test]
fn a_later_switch_replaces_one_still_waiting() {
    let mut run = Run::new();
    let town = run.switch("town");
    let hud = run.load("hud").unwrap();
    let harbour = run.switch("harbour");
    assert_eq!(
        run.error(town).as_deref(),
        Some(r#"replaced by the switch to "harbour""#)
    );
    run.end_frame();
    // The load asked for before the switch applies first; the switch then
    // takes it away with the rest.
    assert_eq!(run.error(hud), None);
    assert_eq!(run.error(harbour), None);
    assert_eq!(run.scenes.scenes_loaded(), ["harbour"]);
    assert_eq!(run.ids(), ["pier"]);
    assert_eq!(
        run.scenes.log(),
        [
            change(0, SceneChangeKind::Load, "hud"),
            change(0, SceneChangeKind::Switch, "harbour"),
        ]
    );
}

#[test]
fn a_load_that_fails_changes_nothing_and_says_why() {
    let mut run = Run::new();
    let broken = run.switch("broken");
    let applied = run.end_frame();
    let error = run.error(broken).unwrap();
    assert!(
        error.starts_with(r#"scene "broken" (scenes/broken.scene.json) does not parse: "#),
        "{error}"
    );
    assert_eq!(applied.diagnostics.len(), 1);
    assert_eq!(applied.diagnostics[0].code, codes::SCENE_LOAD_FAILED);
    assert_eq!(run.ids(), ["menu", "player"]);
    assert_eq!(run.scenes.scene_current(), "title");

    // Beside the title scene, `menu` is taken.
    let clash = run.load("clash").unwrap();
    let applied = run.end_frame();
    assert!(!applied.changed);
    assert!(
        run.error(clash)
            .unwrap()
            .contains(r#"an entity with id "menu" already exists"#)
    );
    assert_eq!(run.scenes.scenes_loaded(), ["title"]);
    assert!(run.scenes.log().is_empty());
}

#[test]
fn kept_entities_live_through_a_switch_and_stand_in() {
    let mut run = Run::new();
    run.scenes.keep("player", true);
    run.switch("town");
    let applied = run.end_frame();
    // The title's player stands in for the town's.
    let notes: Vec<_> = applied
        .diagnostics
        .iter()
        .filter(|d| d.code == codes::KEPT_STANDS_IN)
        .collect();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].severity, Severity::Note);
    assert_eq!(run.ids(), ["fountain", "player", "square"]);
    // Still kept: it lived through the switch.
    assert!(run.scenes.is_kept("player"));

    // A kept id whose entity is gone is forgotten at the next switch.
    let player = run.world.ids().get("player").unwrap();
    re_scene::hierarchy::despawn_recursive(&mut run.world.ecs, player);
    run.switch("title");
    run.end_frame();
    assert!(!run.scenes.is_kept("player"));
    assert_eq!(run.ids(), ["menu", "player"]);
}

#[test]
fn a_switch_to_the_current_scene_reloads_it() {
    let mut run = Run::new();
    run.world.ecs.spawn(re_scene::StableId("spawned".into()));
    run.switch("title");
    run.end_frame();
    assert_eq!(run.ids(), ["menu", "player"]);
    assert_eq!(
        run.scenes.log(),
        [change(0, SceneChangeKind::Switch, "title")]
    );
}

#[test]
fn the_log_is_checked_against_the_replays_list() {
    // The same changes: nothing to report.
    let mut run = Run::new();
    run.scenes.expect(Some(vec![
        change(0, SceneChangeKind::Switch, "town"),
        change(2, SceneChangeKind::Load, "hud"),
    ]));
    run.switch("town");
    assert!(run.end_frame().diagnostics.is_empty());
    assert!(run.end_frame().diagnostics.is_empty());
    run.load("hud").unwrap();
    assert!(run.end_frame().diagnostics.is_empty());
    assert_eq!(run.scenes.replay_ended(), None);

    // Another scene: reported once, at the change.
    let mut run = Run::new();
    run.scenes
        .expect(Some(vec![change(0, SceneChangeKind::Switch, "town")]));
    run.switch("harbour");
    let d = run.end_frame().diagnostics;
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].code, codes::SCENE_CHANGES_DIFFER);
    assert!(d[0].message.contains(r#"the run made a switch to "harbour", where the replay lists a switch to "town" at frame 0"#), "{}", d[0].message);
    run.switch("town");
    assert!(run.end_frame().diagnostics.is_empty());

    // A listed change that does not come: reported when its frame ends.
    let mut run = Run::new();
    run.scenes
        .expect(Some(vec![change(1, SceneChangeKind::Switch, "town")]));
    assert!(run.end_frame().diagnostics.is_empty());
    let d = run.end_frame().diagnostics;
    assert_eq!(d.len(), 1);
    assert!(
        d[0].message.contains("which the run did not make"),
        "{}",
        d[0].message
    );

    // Or when the replay ends before its frame.
    let mut run = Run::new();
    run.scenes
        .expect(Some(vec![change(9, SceneChangeKind::Switch, "town")]));
    run.end_frame();
    assert_eq!(
        run.scenes.replay_ended().unwrap().code,
        codes::SCENE_CHANGES_DIFFER
    );

    // An empty list expects no change at all; no list checks nothing.
    let mut run = Run::new();
    run.scenes.expect(Some(Vec::new()));
    run.switch("town");
    assert_eq!(run.end_frame().diagnostics.len(), 1);
    let mut run = Run::new();
    run.switch("town");
    assert!(run.end_frame().diagnostics.is_empty());
}

#[test]
fn a_restart_drops_requests_and_returns_to_the_scene_the_run_began_in() {
    let mut run = Run::new();
    run.load("hud").unwrap();
    run.end_frame();
    run.scenes.keep("player", true);
    run.switch("town");
    let d = run.scenes.restarted(1);
    assert!(d.is_empty());
    assert!(!run.scenes.pending());
    assert!(!run.scenes.is_kept("player"));
    assert_eq!(run.scenes.scenes_loaded(), ["title"]);
    assert_eq!(
        run.scenes.log(),
        [
            change(0, SceneChangeKind::Load, "hud"),
            change(1, SceneChangeKind::Restart, "title"),
        ]
    );

    // A run begun in a scene the list does not name restarts there.
    let mut scenes = Scenes::new(list(), "levels/test.scene.json");
    assert_eq!(scenes.scene_current(), "levels/test.scene.json");
    assert!(
        scenes
            .scenes_listed()
            .contains(&"levels/test.scene.json".to_string())
    );
    scenes.restarted(4);
    assert_eq!(
        scenes.log(),
        [change(
            4,
            SceneChangeKind::Restart,
            "levels/test.scene.json"
        )]
    );
}

#[test]
fn a_late_run_reads_on_a_worker_and_applies_when_ready() {
    let mut run = Run::new();
    run.lateness = Lateness::Late;
    let town = run.switch("town");
    let mut frames = 0;
    while run.scenes.pending() {
        run.end_frame();
        frames += 1;
        assert!(frames < 10_000, "the read never finished");
        std::thread::yield_now();
    }
    assert_eq!(run.error(town), None);
    assert_eq!(run.scenes.scene_current(), "town");
    assert_eq!(run.ids(), ["fountain", "player", "square"]);
}
