//! Online play (ADR-0050): the plaza served by a dedicated server App and
//! played by two client Apps over UDP on loopback, each App on its own
//! thread as each would be in its own process.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;

use glam::Vec3;
use re_app::{App, AppConfig, NetOptions, NetRole};
use re_compat::instance::tree::find_child;
use re_scene::World;

fn plaza() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plaza")
}

fn open() -> App {
    let dir = plaza();
    let config = AppConfig {
        headless: true,
        persist_edits: false,
        ..AppConfig::load(&dir).unwrap()
    };
    App::open(&dir, config).unwrap()
}

/// Serves the plaza on loopback until `stop`; sends its address first.
fn serve(addr: mpsc::Sender<SocketAddr>, stop: Arc<AtomicBool>) -> Vec<String> {
    let mut app = open();
    let mut options = NetOptions::new(NetRole::Server);
    options.bind = Some("127.0.0.1".into());
    options.port = Some(0);
    addr.send(app.set_net(options).unwrap().addr).unwrap();
    let dt = app.step();
    while !stop.load(Ordering::Relaxed) && !app.stopped() {
        app.frame(dt);
        thread::sleep(std::time::Duration::from_millis(1));
    }
    app.close_net();
    app.take_diagnostics()
        .into_iter()
        .filter(|d| d.is_error())
        .map(|d| d.message)
        .collect()
}

fn join(server: SocketAddr, name: &str) -> App {
    let mut app = open();
    let mut options = NetOptions::new(NetRole::Client(server.to_string()));
    options.name = Some(name.into());
    app.set_net(options).unwrap();
    app
}

/// Where `name`'s character is in `world`.
fn character_at(world: &World, name: &str) -> Option<Vec3> {
    let game = world.entity(re_compat::GAME_ID).ok()?;
    let ws = find_child(world, game, "Workspace", false)?;
    let root = find_child(
        world,
        find_child(world, ws, name, false)?,
        "HumanoidRootPart",
        false,
    )?;
    let t = world.ecs.get::<re_scene::Transform>(root)?;
    Some(t.translation)
}

fn players(world: &World) -> Vec<String> {
    let Some(game) = world.entity(re_compat::GAME_ID).ok() else {
        return Vec::new();
    };
    let Some(p) = find_child(world, game, "Players", false) else {
        return Vec::new();
    };
    let mut names: Vec<String> = re_scene::hierarchy::children(&world.ecs, p)
        .iter()
        .map(|e| world.name_of(*e).to_string())
        .collect();
    names.sort();
    names
}

#[test]
fn two_players_on_a_server_see_each_other_walk() {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();
    let server = {
        let stop = stop.clone();
        thread::spawn(move || serve(tx, stop))
    };
    let addr = rx.recv().unwrap();

    // Ann walks (the plaza's walk replay); Bob stands and watches.
    let ann = thread::spawn(move || {
        let mut app = join(addr, "Ann");
        let doc = re_input::load_replay(&plaza().join("tests/replays/walk.replay.json")).unwrap();
        let report = app.run_headless(Some(600), Some(doc)).unwrap();
        app.close_net();
        report
    });
    let mut bob = join(addr, "Bob");
    let dt = bob.step();
    let mut start = None;
    let mut moved = 0.0f32;
    let mut seen = Vec::new();
    for _ in 0..3000 {
        bob.frame(dt);
        assert!(!bob.stopped(), "{:?}", bob.take_diagnostics());
        let world = bob.compat_client().unwrap();
        if seen.len() < 2 {
            seen = players(world);
        }
        if let Some(p) = character_at(world, "Ann") {
            let s = *start.get_or_insert(p);
            moved = moved.max((p - s).length());
        }
        if moved > 4.0 && ann.is_finished() {
            break;
        }
    }
    let ann = ann.join().unwrap();
    bob.close_net();
    stop.store(true, Ordering::Relaxed);
    let server_errors = server.join().unwrap();

    assert!(ann.ok(), "{:?}", ann.diagnostics);
    assert!(server_errors.is_empty(), "{server_errors:?}");
    assert_eq!(seen, ["Ann", "Bob"]);
    assert!(moved > 4.0, "Bob saw Ann move {moved} studs");
}

#[test]
fn a_host_plays_with_a_player_who_connects() {
    let mut host = open();
    let mut options = NetOptions::new(NetRole::Host);
    options.bind = Some("127.0.0.1".into());
    options.port = Some(0);
    let addr = host.set_net(options).unwrap().addr;
    let dt = host.step();
    // The host's game starts (and its local player joins) on its first
    // frame, before anyone connects.
    host.frame(dt);
    let host_saw_ann = Arc::new(AtomicBool::new(false));
    let ann = {
        let host_saw_ann = host_saw_ann.clone();
        thread::spawn(move || {
            let mut app = join(addr, "Ann");
            let mut report = app.run_headless(Some(120), None).unwrap();
            // Her frames run unpaced, and on a fast machine 120 of them can
            // end before either game shows both players: she plays on until
            // she and the host see them.
            let dt = app.step();
            let mut seen = players(app.compat_client().unwrap());
            for _ in 0..3000 {
                if seen.len() == 2 && host_saw_ann.load(Ordering::Relaxed) {
                    break;
                }
                app.frame(dt);
                seen = players(app.compat_client().unwrap());
                thread::sleep(std::time::Duration::from_millis(1));
            }
            report.diagnostics.extend(app.take_diagnostics());
            app.close_net();
            (report, seen)
        })
    };
    let mut seen = Vec::new();
    for _ in 0..3000 {
        host.frame(dt);
        if seen.len() < 2 {
            seen = players(host.compat_client().unwrap());
            host_saw_ann.store(seen.len() == 2, Ordering::Relaxed);
        }
        if ann.is_finished() {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(1));
    }
    let (report, ann_seen) = ann.join().unwrap();
    let server = players(host.world());
    host.close_net();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(seen, ["Ann", "Player1"]);
    assert_eq!(ann_seen, ["Ann", "Player1"]);
    assert!(server.contains(&"Player1".to_string()), "{server:?}");
}
