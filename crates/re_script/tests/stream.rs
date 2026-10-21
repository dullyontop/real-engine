//! The `stream` library (ADR-0042) over a host whose streaming is scripted by
//! the test: unloads fire before loads, each tick's events fire once, and
//! `load`/`release` exist in the tools context only.

mod common;

use std::sync::Mutex;

use common::{STEP, Services, project};
use re_core::Phase;
use re_scene::{StreamCell, World};
use re_script::{ContextKind, RuntimeConfig, ScriptHost, ScriptRuntime, StreamAccess};
use re_types::glam::Vec3;
use re_types::host::HostServices;

#[derive(Default)]
struct State {
    events: Option<(u64, Vec<StreamCell>, Vec<StreamCell>)>,
    loaded: Vec<StreamCell>,
    pins: Vec<(String, bool)>,
}

static STATE: Mutex<State> = Mutex::new(State {
    events: None,
    loaded: Vec::new(),
    pins: Vec::new(),
});

struct Fake;

impl StreamAccess for Fake {
    fn events(&self, _: &World) -> Option<(u64, Vec<StreamCell>, Vec<StreamCell>)> {
        STATE.lock().unwrap().events.clone()
    }
    fn cells(&self, _: &World) -> Vec<StreamCell> {
        STATE.lock().unwrap().loaded.clone()
    }
    fn is_loaded(&self, _: &World, key: &str) -> bool {
        STATE.lock().unwrap().loaded.iter().any(|c| c.key == key)
    }
    fn pin(&self, _: &mut World, key: &str, on: bool) {
        STATE.lock().unwrap().pins.push((key.to_string(), on));
    }
}

struct Host<'a> {
    world: &'a mut World,
    services: &'a mut Services,
}

impl ScriptHost for Host<'_> {
    fn world(&mut self) -> &mut World {
        self.world
    }
    fn services(&mut self) -> &mut dyn HostServices {
        self.services
    }
    fn stream(&mut self) -> Option<&'static dyn StreamAccess> {
        Some(&Fake)
    }
}

fn cell(x: i32, z: i32) -> StreamCell {
    StreamCell {
        key: re_scene::cell_key(x, z),
        coord: [x, z],
        min: Vec3::new(x as f32 * 64.0, -1.0, z as f32 * 64.0),
        max: Vec3::new((x + 1) as f32 * 64.0, 5.0, (z + 1) as f32 * 64.0),
        sources: vec!["land".into()],
        version: "abc".into(),
    }
}

#[test]
fn stream_events_cells_and_pins() {
    let mut rt = ScriptRuntime::new(RuntimeConfig::new(project())).unwrap();
    let mut world = World::new();
    let mut services = Services::default();
    let mut phase = |rt: &mut ScriptRuntime, p: Phase| {
        let mut host = Host {
            world: &mut world,
            services: &mut services,
        };
        let d = rt.run_phase(&mut host, p, STEP);
        assert!(d.is_empty(), "{d:#?}");
    };
    let exec = |rt: &mut ScriptRuntime, world: &mut World, services: &mut Services, kind, src| {
        let mut host = Host { world, services };
        rt.exec(&mut host, kind, "probe", src)
    };
    exec(
        &mut rt,
        &mut World::new(),
        &mut Services::default(),
        ContextKind::Game,
        r#"
stream.on_unloaded(function(c) print("unloaded " .. c.key) end)
stream.on_loaded(function(c)
	print(`loaded {c.key} [{c.coord[1]},{c.coord[2]}] {c.min.y} {c.max.x} {c.sources[1]} {c.version} {stream.is_loaded(c.key)}`)
end)
time.on_step(function() print(`step {#stream.cells()}`) end)
"#,
    )
    .unwrap();
    {
        let mut s = STATE.lock().unwrap();
        s.loaded = vec![cell(0, 0), cell(1, 0)];
        s.events = Some((4, vec![cell(-1, 0)], vec![cell(0, 0), cell(1, 0)]));
    }
    phase(&mut rt, Phase::PreSimulation);
    // The same tick again (a second step before streaming runs) fires nothing.
    phase(&mut rt, Phase::PreSimulation);
    let out: Vec<String> = rt.take_output().into_iter().map(|l| l.text).collect();
    assert_eq!(
        out,
        [
            "unloaded c/-1_0",
            "loaded c/0_0 [0,0] -1 64 land abc true",
            "loaded c/1_0 [1,0] -1 128 land abc true",
            "step 2",
            "step 2",
        ]
    );

    // load and release: tools only, cell keys only.
    let game = exec(
        &mut rt,
        &mut World::new(),
        &mut Services::default(),
        ContextKind::Game,
        r#"stream.load("c/0_0")"#,
    )
    .unwrap_err();
    assert!(game[0].message.contains("stream.load"), "{game:?}");
    exec(
        &mut rt,
        &mut World::new(),
        &mut Services::default(),
        ContextKind::Tools,
        r#"stream.load("c/3_-2"); stream.release("c/3_-2")"#,
    )
    .unwrap();
    assert_eq!(
        STATE.lock().unwrap().pins,
        [("c/3_-2".to_string(), true), ("c/3_-2".to_string(), false)]
    );
    let bad = exec(
        &mut rt,
        &mut World::new(),
        &mut Services::default(),
        ContextKind::Tools,
        r#"stream.load("terrain")"#,
    )
    .unwrap_err();
    assert!(bad[0].message.contains("not a cell key"), "{bad:?}");
}

#[test]
fn hosts_without_streaming_have_no_cells() {
    let mut rig = common::Rig::new();
    assert_eq!(
        rig.exec(r#"return { #stream.cells(), stream.is_loaded("c/0_0") }"#)
            .unwrap(),
        serde_json::json!([0, false])
    );
    let d = rig
        .exec_in(ContextKind::Tools, r#"stream.load("c/0_0")"#)
        .unwrap_err();
    assert_eq!(d[0].code.to_string(), "RE3015", "{d:?}");
}
