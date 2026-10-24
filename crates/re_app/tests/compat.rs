//! A `[compat]` project in the running game: a server and a client context
//! over two worlds, the `[compat] players`, and the compat boot stage of
//! `real verify` (uncaught errors, stuck `WaitForChild` yields, the output).

use std::path::Path;

use re_app::verify::{self, VerifyOptions};
use re_app::{App, AppConfig};
use re_compat::ClassRegistry;

/// A compat project built from a loader that makes a server `Script` and a
/// `LocalScript` under `StarterPlayerScripts`.
fn project(server: &str, client: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let classes = ClassRegistry::builtin().unwrap();
    re_import::new_project(dir.path(), Some("compat"), &classes).unwrap();
    let loader = format!(
        r#"local function make(className, name, parent)
	local inst = Instance.new(className)
	inst.Name = name
	inst.Parent = parent
	return inst
end
do
	local s = make("Script", "Main", game:GetService("ServerScriptService"))
	s.Source = [==[{server}]==]
	local sps = make("StarterPlayerScripts", "StarterPlayerScripts", game:GetService("StarterPlayer"))
	local c = make("LocalScript", "Hud", sps)
	c.Source = [==[{client}]==]
end
"#
    );
    let input = dir.path().join("loader.luau");
    std::fs::write(&input, loader).unwrap();
    let report = re_import::import(&input, dir.path(), &classes, &Default::default()).unwrap();
    assert!(report.ok(), "{report:?}");
    std::fs::remove_file(input).unwrap();
    dir
}

fn config(dir: &Path) -> AppConfig {
    AppConfig {
        headless: true,
        persist_edits: false,
        ..AppConfig::load(dir).unwrap()
    }
}

const SERVER: &str = r#"
local Players = game:GetService("Players")
print("server up")
Players.PlayerAdded:Connect(function(p) print("joined", p.Name) end)
local f = Instance.new("Folder")
f.Name = "Shared"
f.Parent = game:GetService("ReplicatedStorage")
task.wait(0.1)
print("server waited")
"#;

const CLIENT: &str = r#"
local Players = game:GetService("Players")
print("client up", Players.LocalPlayer.Name)
local shared = game:GetService("ReplicatedStorage"):WaitForChild("Shared")
print("client sees", shared.Name)
"#;

#[test]
fn a_compat_project_runs_a_server_and_a_client_with_one_player() {
    let dir = project(SERVER, CLIENT);
    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    let r = app.run_headless(Some(30), None).unwrap();
    assert!(r.ok(), "{:?}", r.diagnostics);
    assert_eq!(
        r.output,
        [
            "[server] server up",
            "[server] joined\tPlayer1",
            "[client] client up\tPlayer1",
            "[client] client sees\tShared",
            "[server] server waited",
        ]
    );
    let info = r.compat.unwrap();
    assert!(info.uncaught.is_empty() && info.stuck_yields.is_empty());
}

#[test]
fn compat_runs_are_deterministic() {
    let dir = project(SERVER, CLIENT);
    let hash = || {
        let mut app = App::open(dir.path(), config(dir.path())).unwrap();
        app.run_headless(Some(40), None).unwrap().state_hash
    };
    assert_eq!(hash(), hash());
}

fn quick() -> VerifyOptions {
    VerifyOptions {
        compat_boot_frames: 60,
        ..VerifyOptions::default()
    }
}

#[test]
fn verify_boots_a_compat_project_and_reports_its_output() {
    let dir = project(SERVER, CLIENT);
    let report = verify::run_project(dir.path(), &config(dir.path()), &quick(), &[]);
    let schema = report.stage("schema").unwrap();
    assert!(schema.ok, "{:?}", schema.diagnostics);
    let boot = report.stage("boot").unwrap();
    assert!(boot.ok, "{:?}", boot.diagnostics);
    assert_eq!(boot.data["frames"], 60);
    assert_eq!(boot.data["uncaught"], serde_json::json!([]));
    assert_eq!(boot.data["stuck_yields"], serde_json::json!([]));
    assert_eq!(boot.data["output"].as_array().unwrap().len(), 5);
}

#[test]
fn verify_fails_on_a_stuck_wait_for_child() {
    let client = r#"local x = game:GetService("ReplicatedStorage"):WaitForChild("Never")"#;
    let dir = project(SERVER, client);
    let report = verify::run_project(dir.path(), &config(dir.path()), &quick(), &[]);
    let boot = report.stage("boot").unwrap();
    assert!(!boot.ok);
    assert!(
        boot.diagnostics
            .iter()
            .any(|d| d.code.to_string() == "RE0526")
    );
    let stuck = &boot.data["stuck_yields"][0];
    assert_eq!(stuck["context"], "client");
    assert!(
        stuck["wait"]
            .as_str()
            .unwrap()
            .contains("WaitForChild(\"Never\")")
    );
}

#[test]
fn verify_fails_on_an_uncaught_error_and_lists_it_once() {
    let server = r#"game:GetService("RunService").Heartbeat:Connect(function() error("boom") end)"#;
    let dir = project(server, "");
    let report = verify::run_project(dir.path(), &config(dir.path()), &quick(), &[]);
    let boot = report.stage("boot").unwrap();
    assert!(!boot.ok);
    let uncaught = boot.data["uncaught"].as_array().unwrap();
    assert_eq!(uncaught.len(), 1, "{uncaught:?}");
    assert_eq!(uncaught[0]["context"], "server");
    assert!(uncaught[0]["message"].as_str().unwrap().contains("boom"));
}

const HOOKS: &str = r#"
local Players = game:GetService("Players")
local hook = Instance.new("BindableFunction")
hook.Name = "Score"
hook.OnInvoke = function(x)
	return x * 2, #Players:GetPlayers()
end
hook.Parent = game:GetService("ServerStorage")
Players.PlayerAdded:Connect(function(p)
	p:SetAttribute("Coins", 0)
	p.Chatted:Connect(function(text)
		print(p.Name, "said", text)
		p:SetAttribute("Coins", p:GetAttribute("Coins") + 1)
	end)
end)
"#;

fn steps(json: serde_json::Value) -> Vec<re_app::play::PlayStep> {
    serde_json::from_value(json).unwrap()
}

#[test]
fn play_steps_join_players_chat_invoke_hooks_and_dump_the_tree() {
    let dir = project(HOOKS, "");
    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    let plan = steps(serde_json::json!([
        "add_player",
        { "frames": 5 },
        { "say": { "player": 2, "text": "hello" } },
        { "invoke": { "path": "ServerStorage.Score", "args": [21] } },
        { "dump_tree": { "path": "Players", "depth": 1 } },
        { "invoke": { "path": "ServerStorage.Missing" } },
    ]));
    let (report, results, _) = app.play_copy(10, None, &plan, false).unwrap();
    assert_eq!(report.frames, 15);
    assert!(
        report
            .output
            .contains(&"[server] Player2\tsaid\thello".to_string()),
        "{:?}",
        report.output
    );
    assert_eq!(results[0]["player"], "Player2");
    assert_eq!(results[3]["results"], serde_json::json!([42, 2]));
    let players = &results[4]["roots"][0]["children"];
    assert_eq!(players.as_array().unwrap().len(), 2, "{players}");
    assert_eq!(players[1]["attributes"]["Coins"], 1.0);
    assert_eq!(results[5]["ok"], false);
    assert_eq!(results[5]["error"]["code"], "RE0527");
    // The live app did not start its game.
    assert!(app.world().find("game/Players/Player1").is_none());
}

#[test]
fn compat_steps_need_a_compat_project() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("scenes")).unwrap();
    std::fs::write(dir.path().join("real.toml"), "[project]\nname = \"n\"\n").unwrap();
    std::fs::write(
        dir.path().join("scenes/main.scene.json"),
        r#"{ "format": "real-engine/scene", "version": 1, "entities": [] }"#,
    )
    .unwrap();
    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    let (_, results, _) = app
        .play_copy(1, None, &steps(serde_json::json!(["add_player"])), false)
        .unwrap();
    assert_eq!(results[0]["ok"], false);
    assert_eq!(results[0]["error"]["code"], "RE0527");
}

#[test]
fn a_replays_raw_keys_reach_client_scripts() {
    use re_types::replay::{RawReplayInput, ReplayDoc};
    let client = r#"
local UIS = game:GetService("UserInputService")
UIS.InputBegan:Connect(function(input) print("began", input.KeyCode.Name) end)
UIS.InputEnded:Connect(function(input) print("ended", input.KeyCode.Name) end)
"#;
    let dir = project("", client);
    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    let key = |frame, down| RawReplayInput {
        frame,
        key: Some("KeyW".into()),
        down: Some(down),
        ..RawReplayInput::default()
    };
    let mut doc = ReplayDoc::new(1, 30);
    doc.raw = vec![key(5, true), key(20, false)];
    let r = app.run_headless(None, Some(doc)).unwrap();
    assert!(r.ok(), "{:?}", r.diagnostics);
    assert_eq!(r.output, ["[client] began\tW", "[client] ended\tW"]);
}

#[test]
fn a_restart_starts_the_compat_game_again_the_same_way() {
    let dir = project(SERVER, CLIENT);
    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    let first = app.run_headless(Some(40), None).unwrap();
    app.restart();
    let second = app.run_headless(Some(40), None).unwrap();
    assert!(second.ok(), "{:?}", second.diagnostics);
    assert_eq!(first.state_hash, second.state_hash);
    assert_eq!(first.output, second.output);
}

#[test]
fn verify_replays_a_compat_project_twice_with_the_same_hash() {
    let dir = project(SERVER, CLIENT);
    let replays = dir.path().join("tests/replays");
    std::fs::create_dir_all(&replays).unwrap();
    let doc = re_types::replay::ReplayDoc::new(1, 90);
    std::fs::write(
        replays.join("idle.replay.json"),
        serde_json::to_string(&doc).unwrap(),
    )
    .unwrap();
    let report = verify::run_project(dir.path(), &config(dir.path()), &quick(), &[]);
    let stage = report.stage("replays").unwrap();
    assert!(stage.ok, "{:?} {}", stage.diagnostics, stage.data);
}

#[test]
fn play_after_a_live_start_copies_the_scene() {
    let dir = project(HOOKS, "");
    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    app.run_headless(Some(10), None).unwrap();
    let plan = steps(serde_json::json!([
        { "invoke": { "path": "game.ServerStorage.Score", "args": [1] } },
    ]));
    let (report, results, _) = app.play_copy(10, None, &plan, false).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(results[0]["results"], serde_json::json!([2, 1]));
}

#[test]
fn a_scene_that_cannot_start_compat_keeps_its_world() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("scenes")).unwrap();
    std::fs::write(
        dir.path().join("real.toml"),
        "[project]\nname = \"n\"\n\n[compat]\napi = \"roblox\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("scenes/main.scene.json"),
        r#"{ "format": "real-engine/scene", "version": 1, "entities": [
            { "id": "box", "name": "Box", "components": {} } ] }"#,
    )
    .unwrap();
    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    let r = app.run_headless(Some(2), None).unwrap();
    assert!(!r.ok());
    assert!(app.world().entity("box").is_ok());
}

#[test]
fn the_servers_parts_fall_and_touch_and_the_client_sees_them_land() {
    let server = r#"
local base = Instance.new("Part")
base.Name = "Base"
base.Anchored = true
base.Size = Vector3.new(50, 1, 50)
base.Parent = workspace
local box = Instance.new("Part")
box.Name = "Box"
box.Size = Vector3.new(2, 2, 2)
box.Position = Vector3.new(20, 10, 0)
box.Parent = workspace
local said = false
box.Touched:Connect(function(o)
	if not said then said = true print("box touched", o.Name) end
end)
task.wait(1.5)
print("box at", string.format("%.1f", box.Position.Y))
local hit = workspace:Raycast(Vector3.new(20, 50, 0), Vector3.new(0, -100, 0))
print("ray hits", hit and hit.Instance.Name)
"#;
    let client = r#"
local box = workspace:WaitForChild("Box")
task.wait(1.5)
print("client box at", string.format("%.1f", box.Position.Y))
"#;
    let dir = project(server, client);
    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    let r = app.run_headless(Some(150), None).unwrap();
    assert!(r.ok(), "{:?}", r.diagnostics);
    for line in [
        "[server] box touched\tBase",
        "[server] box at\t1.5",
        "[server] ray hits\tBox",
        "[client] client box at\t1.5",
    ] {
        assert!(
            r.output.contains(&line.to_string()),
            "{line}: {:?}",
            r.output
        );
    }
}

/// The source file of the server `Script` the loader made.
fn main_script(dir: &Path) -> String {
    let rel = |p: &Path| {
        p.strip_prefix(dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    };
    let mut todo = vec![dir.join("scripts")];
    while let Some(d) = todo.pop() {
        for entry in std::fs::read_dir(d).unwrap() {
            let p = entry.unwrap().path();
            if p.is_dir() {
                todo.push(p);
            } else if p.file_name().unwrap().to_string_lossy().starts_with("Main") {
                return rel(&p);
            }
        }
    }
    panic!("no Main script under scripts/");
}

/// ADR-0048 §3: while files are watched (`real run`, the editor), a changed
/// compat script restarts, and a source that does not compile keeps the
/// old run (`RE9420`); without a watcher (verify, replays, `play.run`)
/// nothing restarts.
#[test]
fn a_changed_compat_script_restarts_while_files_are_watched() {
    let dir = project("print(\"v1\")", "print(\"hud\")");
    let main = main_script(dir.path());
    let write = |source: &str| std::fs::write(dir.path().join(&main), source).unwrap();

    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    app.run_headless(Some(5), None).unwrap();
    write("print(\"v2\")");
    app.apply_file_changes(&[re_assets::AssetChange::Modified(main.clone())]);
    let r = app.run_headless(Some(5), None).unwrap();
    assert!(r.output.is_empty(), "{:?}", r.output);

    // Runs frames until `done` holds for the output and diagnostics so far.
    let until = |app: &mut App, done: &dyn Fn(&[String], &[re_core::Diagnostic]) -> bool| {
        let (mut output, mut diags) = (Vec::new(), Vec::new());
        let deadline = re_core::time::Instant::now() + std::time::Duration::from_secs(10);
        while !done(&output, &diags) {
            assert!(
                re_core::time::Instant::now() < deadline,
                "the change never arrived: {output:?} {diags:?}"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
            let r = app.run_headless(Some(1), None).unwrap();
            output.extend(r.output);
            diags.extend(r.diagnostics);
        }
        // A few more frames, to see that nothing else restarts.
        let r = app.run_headless(Some(10), None).unwrap();
        output.extend(r.output);
        diags.extend(r.diagnostics);
        (output, diags)
    };
    write("print(\"v1\")");
    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    app.enable_hot_reload().unwrap();
    let r = app.run_headless(Some(5), None).unwrap();
    assert_eq!(r.output, ["[server] v1", "[client] hud"]);

    write("print(\"v2\")");
    let (output, diags) = until(&mut app, &|o, _| !o.is_empty());
    assert_eq!(output, ["[server] v2"]);
    assert!(diags.iter().all(|d| !d.is_error()), "{diags:?}");

    write("print(");
    let reload_compile = re_compat::codes::RELOAD_COMPILE;
    let (output, diags) = until(&mut app, &|_, d| d.iter().any(|d| d.code == reload_compile));
    assert!(output.is_empty(), "{output:?}");
    let bad: Vec<_> = diags.iter().filter(|d| d.code == reload_compile).collect();
    assert_eq!(bad.len(), 1, "{diags:?}");
    assert!(
        bad[0]
            .message
            .starts_with(&format!("{main} changed but does not compile")),
        "{diags:?}"
    );

    write("print(\"v3\")");
    let (output, _) = until(&mut app, &|o, _| !o.is_empty());
    assert_eq!(output, ["[server] v3"]);
}

#[test]
fn the_compat_contexts_attach_to_the_debugger_before_and_during_play() {
    let names = |d: &re_script::debug::Debugger| {
        let mut n: Vec<String> = d
            .contexts()
            .into_iter()
            .map(|c| c.name)
            .filter(|n| n != "game")
            .collect();
        n.sort();
        n
    };
    let dir = project(SERVER, CLIENT);
    let mut app = App::open(dir.path(), config(dir.path())).unwrap();
    let debugger = re_script::debug::Debugger::new();
    app.set_debugger(Some(debugger.clone()));
    app.frame(std::time::Duration::from_millis(16));
    assert_eq!(names(&debugger), ["client", "server"]);

    // A restart makes new VMs: the old contexts end, the new ones attach.
    app.restart();
    app.frame(std::time::Duration::from_millis(16));
    assert_eq!(names(&debugger), ["client", "server"]);

    // A debugger set while the game runs attaches the running VMs.
    app.set_debugger(None);
    assert!(names(&debugger).is_empty());
    let late = re_script::debug::Debugger::new();
    app.set_debugger(Some(late.clone()));
    assert_eq!(names(&late), ["client", "server"]);
}
