//! Calls the implemented members no other test reaches, from the fixtures in
//! `fixtures/members/` (docs/M2_PLAN.md, definition of done item 9). With
//! `REAL_COMPAT_MEMBER_TRACE` set, every test records the members its scripts
//! reach, and `cargo xtask members` checks that the union covers every
//! `implemented` member in `coverage.json`.

use re_compat::CompatSetup;
use re_compat::instance::tree::find_child;
use re_compat::scene::{create_place, spawn_instance};
use re_compat::testing::{Harness, HarnessOptions, Side};
use re_scene::{SaveOptions, World};

const SERVER: &str = include_str!("fixtures/members/server.luau");
const CLIENT: &str = include_str!("fixtures/members/client.luau");

#[test]
fn every_uncovered_member_is_called_from_a_fixture() {
    let setup = CompatSetup::standard().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut w = World::new();
    create_place(
        &mut w,
        &setup.classes,
        &[
            "Workspace",
            "ServerScriptService",
            "ReplicatedStorage",
            "Lighting",
        ],
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("scenes")).unwrap();
    std::fs::write(
        dir.path().join("scenes/main.scene.json"),
        re_scene::scene::to_json(&w.save(SaveOptions::default())),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("real.toml"),
        "[project]\nname = \"members\"\nentry_scene = \"scenes/main.scene.json\"\n\n[compat]\napi = \"roblox\"\nplayers = 1\n",
    )
    .unwrap();
    let mut h = Harness::open_with(
        dir.path(),
        &setup,
        HarnessOptions {
            two_worlds: true,
            ..Default::default()
        },
    )
    .unwrap();
    let d = h.start();
    assert!(d.is_empty(), "{d:#?}");
    h.step(5);
    let d = h.exec(Side::Server, "members_server", SERVER);
    assert!(d.is_empty(), "{d:#?}");
    // A player's TextSource is made when they first chat, which only the
    // host can do; add one directly and reach it from a script.
    let game = h.vm(Side::Server).game();
    let w = h.world_mut(Side::Server);
    let general = find_child(w, game, "TextChatService", false)
        .and_then(|tcs| find_child(w, tcs, "TextChannels", false))
        .and_then(|c| find_child(w, c, "RBXGeneral", false))
        .unwrap();
    spawn_instance(
        w,
        &setup.classes,
        "TextSource",
        None,
        "Player1",
        Some(general),
    )
    .unwrap();
    let d = h.exec(
        Side::Server,
        "members_text_source",
        r#"local s = game.TextChatService.TextChannels.RBXGeneral:FindFirstChildOfClass("TextSource")
        s.CanSend = s.CanSend"#,
    );
    assert!(d.is_empty(), "{d:#?}");
    h.step(2);
    let d = h.exec(Side::Client, "members_client", CLIENT);
    assert!(d.is_empty(), "{d:#?}");
    h.step(5);
    // Last, because it replaces the character the client half uses.
    let d = h.exec(
        Side::Server,
        "members_respawn",
        r#"game:GetService("Players"):GetPlayers()[1]:LoadCharacterAsync()"#,
    );
    assert!(d.is_empty(), "{d:#?}");
    h.step(2);
    assert!(h.uncaught().is_empty(), "{:#?}", h.uncaught());
    let text = |h: &mut Harness, side| -> Vec<String> {
        h.take_output(side).into_iter().map(|l| l.text).collect()
    };
    let server = text(&mut h, Side::Server);
    let client = text(&mut h, Side::Client);
    assert!(
        server.iter().any(|l| l == "server members ok"),
        "{server:?}"
    );
    assert!(
        client.iter().any(|l| l == "client members ok"),
        "{client:?}"
    );
    assert!(
        server.iter().any(|l| l == "unreliable from Player1: back"),
        "{server:?}"
    );
}
