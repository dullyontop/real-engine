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
    assert!(boot.diagnostics.iter().any(|d| d.code.to_string() == "RE0526"));
    let stuck = &boot.data["stuck_yields"][0];
    assert_eq!(stuck["context"], "client");
    assert!(stuck["wait"].as_str().unwrap().contains("WaitForChild(\"Never\")"));
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

