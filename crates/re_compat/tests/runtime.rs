//! The script runtime: script start, `require`, `RunService` and the frame
//! order, `task` and the legacy globals, the `game` fields and
//! `BindToClose`, all through the headless harness (`re_compat::testing`).

use std::path::Path;
use std::sync::Arc;

use re_compat::scene::{create_place, spawn_instance};
use re_compat::services::core::ensure_service;
use re_compat::testing::{Harness, HarnessOptions, Side};
use re_compat::{CompatConfig, CompatSetup, script_context};
use re_reflect::Value;
use re_scene::{Entity, SaveOptions, ScriptRef, World};

/// Builds a compat project in a temporary directory.
struct Project {
    setup: Arc<CompatSetup>,
    world: World,
    dir: tempfile::TempDir,
    scripts: usize,
    toml: String,
}

impl Project {
    fn new() -> Project {
        Project::with_setup(CompatSetup::standard().unwrap())
    }

    fn with_setup(setup: Arc<CompatSetup>) -> Project {
        let mut world = World::new();
        let game = create_place(
            &mut world,
            &setup.classes,
            &[
                "Workspace",
                "ServerScriptService",
                "ServerStorage",
                "ReplicatedStorage",
                "StarterGui",
            ],
        )
        .unwrap();
        ensure_service(&mut world, &setup.classes, game, "StarterPlayer").unwrap();
        Project {
            setup,
            world,
            dir: tempfile::tempdir().unwrap(),
            scripts: 0,
            toml: String::new(),
        }
    }

    fn service(&self, name: &str) -> Entity {
        self.world.entity(name).unwrap()
    }

    fn folder(&mut self, name: &str, parent: Entity) -> Entity {
        spawn_instance(
            &mut self.world,
            &self.setup.classes,
            "Folder",
            None,
            name,
            Some(parent),
        )
        .unwrap()
    }

    /// Adds a script instance with `source`; `run_context` sets `RunContext`.
    fn script(
        &mut self,
        class: &str,
        name: &str,
        parent: Entity,
        run_context: Option<&str>,
        source: &str,
    ) -> Entity {
        let r = &self.setup.classes;
        let e = spawn_instance(&mut self.world, r, class, None, name, Some(parent)).unwrap();
        if let Some(rc) = run_context {
            r.set_property(&mut self.world, e, "RunContext", Value::Enum(rc.into()))
                .unwrap();
        }
        self.scripts += 1;
        let path = format!("scripts/{}_{name}.luau", self.scripts);
        let file = self.dir.path().join(&path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, source).unwrap();
        self.world.ecs.entity_mut(e).insert(ScriptRef {
            path,
            context: script_context(class, run_context).unwrap(),
        });
        e
    }

    /// Extra `real.toml` text after the `[compat]` header.
    fn compat(mut self, toml: &str) -> Project {
        self.toml = toml.to_string();
        self
    }

    /// The source path the last [`Project::script`] call wrote.
    fn last_path(&self, name: &str) -> String {
        format!("scripts/{}_{name}.luau", self.scripts)
    }

    /// Saves the project and opens it in a harness.
    fn open(self) -> (Harness, tempfile::TempDir) {
        self.open_with(HarnessOptions::default())
    }

    /// [`Project::open`] with harness options.
    fn open_with(mut self, opts: HarnessOptions) -> (Harness, tempfile::TempDir) {
        let dir = self.dir.path();
        std::fs::create_dir_all(dir.join("scenes")).unwrap();
        std::fs::write(
            dir.join("scenes/main.scene.json"),
            re_scene::scene::to_json(&self.world.save(SaveOptions::default())),
        )
        .unwrap();
        std::fs::write(
            dir.join("real.toml"),
            format!(
                "[project]\nname = \"t\"\nentry_scene = \"scenes/main.scene.json\"\n\n[scripts]\ncpu_budget_ms = 1000\n\n[compat]\napi = \"roblox\"\n{}",
                self.toml
            ),
        )
        .unwrap();
        let h = Harness::open_with(dir, &self.setup, opts).unwrap();
        (h, self.dir)
    }
}

/// The printed lines of one side; warnings start with `W `.
fn out(h: &mut Harness, side: Side) -> Vec<String> {
    h.take_output(side)
        .into_iter()
        .map(|l| {
            if l.warn {
                format!("W {}", l.text)
            } else {
                l.text
            }
        })
        .collect()
}

fn no_errors(h: &Harness) {
    assert!(h.uncaught().is_empty(), "{:#?}", h.uncaught());
}

const FRAME_ORDER_CLIENT: &str = include_str!("fixtures/model/frame_order.client.luau");
const FRAME_ORDER_SERVER: &str = include_str!("fixtures/model/frame_order.server.luau");

// Grouped so `cargo test -p re_compat model` selects them.
mod model {
    use super::*;

    /// ARCHITECTURE §3: render-step bindings (by priority), `RenderStepped`,
    /// `PreAnimation`, `Stepped`, physics, `Heartbeat`; then due waits.
    #[test]
    fn frame_order_follows_the_engine_phases() {
        let mut p = Project::new();
        let sps = p.world.entity("StarterPlayerScripts").unwrap();
        p.script("LocalScript", "Recorder", sps, None, FRAME_ORDER_CLIENT);
        let sss = p.service("ServerScriptService");
        p.script("Script", "Recorder", sss, None, FRAME_ORDER_SERVER);
        let (mut h, _dir) = p.open();
        h.start();
        h.step(3);
        no_errors(&h);

        let client = out(&mut h, Side::Client);
        let mut want = Vec::new();
        for f in 1..=3 {
            for what in [
                "bind 199",
                "bind 200",
                "bind 201",
                "PreRender",
                "RenderStepped",
                "PreAnimation",
                "PreSimulation",
                "Stepped",
                "PostSimulation",
                "Heartbeat",
                "task.wait",
            ] {
                want.push(format!("{f} {what}"));
            }
        }
        assert_eq!(client, want);

        let server = out(&mut h, Side::Server);
        let mut want = Vec::new();
        for f in 1..=3 {
            want.push(format!("{f} PreAnimation"));
            want.push(format!("{f} PreSimulation"));
            want.push(format!(
                "{f} Stepped {:.4} {:.4}",
                f as f64 / 60.0,
                1.0 / 60.0
            ));
            want.push(format!("{f} PostSimulation"));
            want.push(format!("{f} Heartbeat"));
        }
        assert_eq!(server, want, "the server has no render step");
    }

    #[test]
    fn scripts_start_by_class_place_and_run_context() {
        let mut p = Project::new();
        let say = |what: &str| {
            format!(
                "print(\"{what}\", game:GetService(\"RunService\"):IsServer() and \"server\" or \"client\", script.Name)"
            )
        };
        let sss = p.service("ServerScriptService");
        let ws = p.service("Workspace");
        let ss = p.service("ServerStorage");
        let rs = p.service("ReplicatedStorage");
        let gui = p.service("StarterGui");
        let sps = p.world.entity("StarterPlayerScripts").unwrap();
        let scs = p.world.entity("StarterCharacterScripts").unwrap();
        let nested = p.folder("Nested", sss);
        p.script("Script", "A", sss, None, &say("start"));
        p.script("Script", "B", nested, None, &say("start"));
        p.script("Script", "C", ws, None, &say("start"));
        p.script("Script", "D", ss, None, &say("start"));
        p.script("Script", "E", rs, None, &say("start"));
        p.script("Script", "F", ws, Some("Client"), &say("start"));
        p.script("Script", "G", rs, Some("Client"), &say("start"));
        p.script("Script", "H", rs, Some("Server"), &say("start"));
        p.script("LocalScript", "I", sps, None, &say("start"));
        p.script("LocalScript", "J", gui, None, &say("start"));
        p.script("LocalScript", "K", scs, None, &say("start"));
        p.script("LocalScript", "L", ws, None, &say("start"));
        p.script("LocalScript", "M", rs, None, &say("start"));
        p.script("Script", "N", sps, Some("Server"), &say("start"));
        let (mut h, _dir) = p.open();
        h.start();
        no_errors(&h);
        assert_eq!(
            out(&mut h, Side::Server),
            ["start\tserver\tC", "start\tserver\tB", "start\tserver\tA"],
            "server scripts start in tree order"
        );
        assert_eq!(
            out(&mut h, Side::Client),
            ["start\tclient\tF", "start\tclient\tJ", "start\tclient\tI"],
            "client scripts start in tree order"
        );
    }

    #[test]
    fn the_client_world_has_no_server_only_contents() {
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        p.script("Script", "Secret", sss, None, "");
        let ss = p.service("ServerStorage");
        p.folder("Hidden", ss);
        let (mut h, _dir) = p.open();
        h.exec(
            Side::Client,
            "probe",
            r#"
            print(#game:GetService("ServerScriptService"):GetChildren(), #game:GetService("ServerStorage"):GetChildren())
            "#,
        );
        h.exec(
            Side::Server,
            "probe",
            r#"print(#game:GetService("ServerScriptService"):GetChildren(), #game:GetService("ServerStorage"):GetChildren())"#,
        );
        no_errors(&h);
        assert_eq!(out(&mut h, Side::Client), ["0\t0"]);
        assert_eq!(out(&mut h, Side::Server), ["1\t1"]);
    }

    #[test]
    fn require_runs_a_module_once_per_context_and_caches_it() {
        let mut p = Project::new();
        let rs = p.service("ReplicatedStorage");
        let sss = p.service("ServerScriptService");
        let sps = p.world.entity("StarterPlayerScripts").unwrap();
        let shared = p.folder("Shared", rs);
        p.script(
            "ModuleScript",
            "Counter",
            shared,
            None,
            r#"
            print("loading Counter in " .. (game:GetService("RunService"):IsServer() and "server" or "client"))
            local util = require(script.Parent.Util)
            return { n = 0, double = util.double, name = script:GetFullName() }
            "#,
        );
        p.script(
            "ModuleScript",
            "Util",
            shared,
            None,
            "task.wait(0.1) return { double = function(x) return x * 2 end }",
        );
        let user = r#"
            local c = require(game.ReplicatedStorage.Shared.Counter)
            c.n += 1
            print(script.Name, c.n, c.double(21), c.name)
        "#;
        p.script("Script", "One", sss, None, user);
        p.script("Script", "Two", sss, None, user);
        p.script("LocalScript", "Three", sps, None, user);
        let (mut h, _dir) = p.open();
        h.start();
        h.step(10);
        no_errors(&h);
        assert_eq!(
            out(&mut h, Side::Server),
            [
                "loading Counter in server",
                "One\t1\t42\tReplicatedStorage.Shared.Counter",
                "Two\t2\t42\tReplicatedStorage.Shared.Counter",
            ],
            "the second script waits for the module the first is loading"
        );
        assert_eq!(
            out(&mut h, Side::Client),
            [
                "loading Counter in client",
                "Three\t1\t42\tReplicatedStorage.Shared.Counter",
            ],
            "each context has its own cache"
        );
    }

    #[test]
    fn require_errors_name_the_cycle_and_failed_modules() {
        let mut p = Project::new();
        let rs = p.service("ReplicatedStorage");
        let sss = p.service("ServerScriptService");
        p.script(
            "ModuleScript",
            "A",
            rs,
            None,
            "return require(script.Parent.B)",
        );
        p.script(
            "ModuleScript",
            "B",
            rs,
            None,
            "return require(script.Parent.A)",
        );
        p.script("ModuleScript", "Bad", rs, None, "error('boom')");
        p.script("ModuleScript", "Two", rs, None, "return 1, 2");
        p.script("ModuleScript", "None", rs, None, "local x = 1");
        p.script(
            "Script",
            "Main",
            sss,
            None,
            r#"
            local rs = game.ReplicatedStorage
            for _, name in { "A", "Bad", "Bad", "Two", "None" } do
                local ok, err = pcall(require, rs[name])
                print(name, ok, err)
            end
            print(pcall(require, rs))
            print(pcall(require, 123))
            print(pcall(require, "path/to/module"))
            "#,
        );
        let (mut h, _dir) = p.open();
        h.start();
        no_errors(&h);
        let lines = out(&mut h, Side::Server);
        let want = [
            (
                "A\tfalse",
                "[RE3004]",
                "required recursively: ReplicatedStorage.A -> ReplicatedStorage.B -> ReplicatedStorage.A",
            ),
            ("Bad\tfalse", "boom", ""),
            (
                "Bad\tfalse",
                "[RE3004]",
                "Requested module experienced an error while loading",
            ),
            (
                "Two\tfalse",
                "[RE3004]",
                "Module code did not return exactly one value",
            ),
            (
                "None\tfalse",
                "[RE3004]",
                "Module code did not return exactly one value",
            ),
            ("false", "[RE3004]", "ModuleScript"),
            ("false", "[RE9002]", ""),
            ("false", "[RE3004]", ""),
        ];
        assert_eq!(lines.len(), want.len(), "{lines:#?}");
        for (line, (start, a, b)) in lines.iter().zip(want) {
            assert!(
                line.starts_with(start) && line.contains(a) && line.contains(b),
                "{line:?} should start with {start:?} and contain {a:?} and {b:?}"
            );
        }
    }

    /// The standard setup plus `TestFunction`, a class with an `OnInvoke`
    /// callback property that `Invoke` calls.
    fn test_function_setup() -> Arc<CompatSetup> {
        let mut b = re_compat::Bindings::standard().unwrap();
        b.classes(|r| {
            r.classes(
                "test_callbacks.json",
                re_compat::data::ClassFile::parse(
                    "test_callbacks.json",
                    r#"{
                      "format": "real-engine/compat-classes", "version": 1,
                      "classes": [{
                        "name": "TestFunction", "superclass": "Instance", "creatable": true, "status": "implemented",
                        "properties": [{ "name": "OnInvoke", "type": "function", "storage": { "kind": "builtin" }, "status": "implemented" }],
                        "methods": [{ "name": "Invoke", "params": [{ "name": "x", "type": "any" }], "returns": ["any"], "status": "implemented" }]
                      }]
                    }"#,
                )
                .unwrap(),
            )
        });
        b.accessor(
            "TestFunction",
            "OnInvoke",
            re_compat::Accessor {
                get: Arc::new(|_, _| Ok(Value::Entity(None))),
                set: None,
            },
        );
        b.method(
            "TestFunction",
            "Invoke",
            Arc::new(|lua: &mlua::Lua, e, args| {
                match re_compat::instance::callback(lua, e, "OnInvoke")? {
                    Some(f) => f.call(args),
                    None => Err(mlua::Error::runtime("no OnInvoke")),
                }
            }),
        );
        CompatSetup::new(b).unwrap()
    }

    #[test]
    fn require_never_waits_forever() {
        let mut p = Project::new();
        let rs = p.service("ReplicatedStorage");
        let sss = p.service("ServerScriptService");
        // Two threads that each load a module the other one waits for.
        p.script(
            "ModuleScript",
            "M1",
            rs,
            None,
            "task.wait(0.05) return require(script.Parent.M2)",
        );
        p.script(
            "ModuleScript",
            "M2",
            rs,
            None,
            "return require(script.Parent.M1)",
        );
        // A module whose loader is destroyed while it loads.
        p.script(
            "ModuleScript",
            "Slow",
            rs,
            None,
            "task.wait(0.1) return 'slow'",
        );
        // A module whose loader runs out of time.
        p.script("ModuleScript", "Spin", rs, None, "while true do end");
        p.script("ModuleScript", "Syntax", rs, None, "return (");
        p.script(
            "Script",
            "A",
            sss,
            None,
            "print('A', pcall(require, game.ReplicatedStorage.M1))",
        );
        p.script(
            "Script",
            "B",
            sss,
            None,
            "print('B', pcall(require, game.ReplicatedStorage.M2))",
        );
        p.script(
            "Script",
            "Loader",
            sss,
            None,
            "require(game.ReplicatedStorage.Slow)",
        );
        p.script(
            "Script",
            "Spinner",
            sss,
            None,
            "require(game.ReplicatedStorage.Spin)",
        );
        p.script(
            "Script",
            "Main",
            sss,
            None,
            r#"
            local rs = game.ReplicatedStorage
            task.wait(1/60)
            game.ServerScriptService.Loader:Destroy()
            print("Slow", require(rs.Slow))
            print("Spin", pcall(require, rs.Spin))
            for _ = 1, 2 do
                print("Syntax", pcall(require, rs.Syntax))
            end
            "#,
        );
        let (mut h, _dir) = p.open();
        h.start();
        h.step(30);
        // Only the spinning loader's own budget error is uncaught.
        assert_eq!(h.uncaught().len(), 1, "{:#?}", h.uncaught());
        let hint = h.uncaught()[0].1.hint.clone().unwrap_or_default();
        assert!(hint.contains("Spinner"), "{:#?}", h.uncaught());
        let lines = out(&mut h, Side::Server);
        let find = |p: &str| {
            lines
                .iter()
                .filter(|l| l.starts_with(p))
                .cloned()
                .collect::<Vec<_>>()
        };
        let a = find("A\t");
        assert!(
            a.len() == 1
                && a[0].starts_with("A\tfalse")
                && a[0].contains("[RE3004]")
                && a[0].contains(
                    "ReplicatedStorage.M2 -> ReplicatedStorage.M1 -> ReplicatedStorage.M2"
                ),
            "{lines:#?}"
        );
        let b = find("B\t");
        assert!(
            b.len() == 1 && b[0].starts_with("B\tfalse") && b[0].contains("[RE3004]"),
            "{lines:#?}"
        );
        assert_eq!(find("Slow"), ["Slow\tslow"], "{lines:#?}");
        let spin = find("Spin");
        assert!(
            spin.len() == 1
                && spin[0].starts_with("Spin\tfalse")
                && spin[0].contains("experienced an error while loading"),
            "{lines:#?}"
        );
        let syntax = find("Syntax");
        assert_eq!(syntax.len(), 2, "{lines:#?}");
        assert!(!syntax[0].contains("experienced an error"), "{lines:#?}");
        assert!(syntax[1].contains("experienced an error"), "{lines:#?}");
        assert!(h.stuck(Side::Server).is_empty());
    }

    #[test]
    fn destroying_a_script_drops_its_bindings_and_callbacks() {
        let mut p = Project::with_setup(test_function_setup());
        let sps = p.world.entity("StarterPlayerScripts").unwrap();
        p.script(
            "LocalScript",
            "Binder",
            sps,
            None,
            r#"
            game:GetService("RunService"):BindToRenderStep("x", 100, function() print("step") end)
            task.wait(1/60)
            script:Destroy()
            "#,
        );
        let sss = p.service("ServerScriptService");
        let rs = p.service("ReplicatedStorage");
        let f = p.folder("F", rs);
        spawn_instance(
            &mut p.world,
            &p.setup.classes,
            "TestFunction",
            None,
            "Fn",
            Some(f),
        )
        .unwrap();
        p.script(
            "Script",
            "Closer",
            sss,
            None,
            r#"
            game:BindToClose(function() print("closer") end)
            game.ReplicatedStorage.F.Fn.OnInvoke = function() return 1 end
            task.wait(1/60)
            script:Destroy()
            "#,
        );
        p.script(
            "Script",
            "Caller",
            sss,
            None,
            r#"
            task.wait(3/60)
            print("invoke", pcall(function() return game.ReplicatedStorage.F.Fn:Invoke() end))
            "#,
        );
        let (mut h, _dir) = p.open();
        h.start();
        h.step(5);
        no_errors(&h);
        let steps = out(&mut h, Side::Client)
            .into_iter()
            .filter(|l| l == "step")
            .count();
        assert!(steps <= 2, "the binding outlived its script: {steps} steps");
        let lines = out(&mut h, Side::Server);
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].starts_with("invoke\tfalse"), "{lines:?}");
        let (d, finished) = h.close();
        assert!(d.is_empty() && finished, "{d:#?}");
        assert!(out(&mut h, Side::Server).is_empty());
    }

    #[test]
    fn a_destroyed_instance_keeps_its_property_values() {
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Main",
            sss,
            None,
            r#"
            local part = Instance.new("Part")
            part.Name = "Root"
            part.Position = Vector3.new(1, 2, 3)
            part.Transparency = 0.5
            part.Parent = workspace
            part:Destroy()
            task.wait(2/60)
            print(part.Parent, part.Name, part.ClassName, part.Position.Y, part.Transparency)
            local ok, err = pcall(function() part.Transparency = 1 end)
            print(ok, string.find(err, "RE3007", 1, true) ~= nil)
            "#,
        );
        let (mut h, _dir) = p.open();
        h.start();
        h.step(4);
        no_errors(&h);
        assert_eq!(
            out(&mut h, Side::Server),
            ["nil\tRoot\tPart\t2\t0.5", "false\ttrue"]
        );
    }

    #[test]
    fn a_reference_to_a_despawned_instance_no_script_held_reads_nil() {
        let mut p = Project::new();
        let ws = p.service("Workspace");
        let folder = p.folder("Box", ws);
        let r = p.setup.classes.clone();
        let part = spawn_instance(&mut p.world, &r, "Part", None, "Inner", Some(folder)).unwrap();
        let weld = spawn_instance(&mut p.world, &r, "Weld", None, "W", Some(ws)).unwrap();
        r.set_property(&mut p.world, weld, "Part0", Value::Entity(Some(part)))
            .unwrap();
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Main",
            sss,
            None,
            r#"
            local held = Instance.new("Part")
            held.Name = "Held"
            held.Parent = workspace
            local w2 = Instance.new("Weld")
            w2.Part0 = held
            w2.Parent = workspace
            workspace.Box:Destroy()
            held:Destroy()
            task.wait(2/60)
            print(workspace.W.Part0, w2.Part0 == held, w2.Part0.Name)
            "#,
        );
        let (mut h, _dir) = p.open();
        h.start();
        h.step(4);
        no_errors(&h);
        assert_eq!(out(&mut h, Side::Server), ["nil\ttrue\tHeld"]);
    }

    #[test]
    fn the_stock_animate_script_reads_class_name_and_model_scale() {
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Main",
            sss,
            None,
            r#"
            local m = Instance.new("Model")
            local gone = Instance.new("Part")
            gone:Destroy()
            task.wait(2/60)
            print(script.className, m.className, m:GetScale(), gone.className)
            "#,
        );
        let (mut h, _dir) = p.open();
        h.start();
        h.step(4);
        no_errors(&h);
        assert_eq!(out(&mut h, Side::Server), ["Script\tModel\t1\tPart"]);
    }

    #[test]
    fn destroying_a_script_stops_its_threads() {
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Ticker",
            sss,
            None,
            r#"
            task.spawn(function()
                while true do
                    print("tick")
                    task.wait(1/60)
                end
            end)
            game:GetService("RunService").Heartbeat:Connect(function() print("beat") end)
            "#,
        );
        p.script(
            "Script",
            "Killer",
            sss,
            None,
            r#"
            task.wait(2/60)
            game.ServerScriptService.Ticker:Destroy()
            print("destroyed")
            "#,
        );
        let (mut h, _dir) = p.open();
        h.start();
        h.step(5);
        no_errors(&h);
        let lines = out(&mut h, Side::Server);
        let after: Vec<&String> = lines
            .iter()
            .skip_while(|l| *l != "destroyed")
            .skip(1)
            .collect();
        assert!(lines.contains(&"destroyed".to_string()), "{lines:?}");
        assert!(after.is_empty(), "{lines:?}");
    }

    #[test]
    fn task_extras_and_legacy_globals() {
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Main",
            sss,
            None,
            r#"
            task.synchronize()
            task.desynchronize()
            local waited, now = wait()
            print("wait", string.format("%.4f %.4f", waited, now))
            wait(0.1)
            delay(0.05, function() print("delayed") end)
            spawn(function() print("spawned") end)
            print("after spawn")
            debug.profilebegin("outer")
            debug.profilebegin("inner")
            debug.profileend()
            debug.profileend()
            debug.profileend()
            print(type(debug.traceback()))
            "#,
        );
        let (mut h, _dir) = p.open();
        h.start();
        h.step(30);
        no_errors(&h);
        let lines = out(&mut h, Side::Server);
        assert_eq!(
            lines,
            [
                "W [RE9101] wait is deprecated; use task.wait",
                "wait\t0.0333 0.0333",
                "W [RE9101] delay is deprecated; use task.delay",
                "W [RE9101] spawn is deprecated; use task.defer",
                "after spawn",
                "string",
                "spawned",
                "delayed",
            ]
        );
    }

    #[test]
    fn run_service_queries_and_game_fields_come_from_real_toml() {
        let probe = r#"
            local rs = game:GetService("RunService")
            print(rs:IsServer(), rs:IsClient(), rs:IsStudio(), rs:IsRunning())
            print(game.PlaceId, game.GameId, game.CreatorId, game.JobId, game.PrivateServerId, game.PrivateServerOwnerId)
            print(pcall(function() game.PlaceId = 5 end))
        "#;
        let p = Project::new();
        let (mut h, _dir) = p.open();
        h.exec(Side::Server, "probe", probe);
        h.exec(Side::Client, "probe", probe);
        assert_eq!(h.uncaught().len(), 0, "{:#?}", h.uncaught());
        let s = out(&mut h, Side::Server);
        assert_eq!(s[0], "true\tfalse\ttrue\ttrue");
        assert_eq!(s[1], "0\t0\t0\t\t\t0");
        assert!(
            s[2].starts_with("false\t") && s[2].contains("[RE9005]"),
            "{s:?}"
        );
        let c = out(&mut h, Side::Client);
        assert_eq!(c[0], "false\ttrue\ttrue\ttrue");

        let p = Project::new().compat(
            "\n[compat.game]\nplace_id = 1234\ngame_id = 99\njob_id = \"job-1\"\nprivate_server_id = \"ps\"\nprivate_server_owner_id = 7\nstudio = false\n",
        );
        let (mut h, _dir) = p.open();
        h.exec(Side::Server, "probe", probe);
        let s = out(&mut h, Side::Server);
        assert_eq!(s[0], "true\tfalse\tfalse\ttrue");
        assert_eq!(s[1], "1234\t99\t0\tjob-1\tps\t7");
    }

    #[test]
    fn bind_to_close_callbacks_run_and_may_yield() {
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Saver",
            sss,
            None,
            r#"
            game:BindToClose(function()
                print("closing")
                task.wait(1)
                print("saved")
            end)
            game:BindToClose(function()
                print("second")
            end)
            game:BindToClose(function()
                game:GetService("RunService").Heartbeat:Wait()
                print("after a frame")
            end)
            "#,
        );
        let (mut h, _dir) = p.open();
        h.start();
        h.step(1);
        let (d, finished) = h.close();
        assert!(d.is_empty(), "{d:#?}");
        assert!(finished);
        assert_eq!(
            out(&mut h, Side::Server),
            ["closing", "second", "after a frame", "saved"]
        );

        // A callback that never ends is abandoned after 30 s of game time.
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Stuck",
            sss,
            None,
            "game:BindToClose(function() while true do task.wait(1) end end)",
        );
        let (mut h, _dir) = p.open();
        h.start();
        let (_, finished) = h.close();
        assert!(!finished);
    }

    #[test]
    fn unsupported_members_are_counted() {
        let p = Project::new();
        let (mut h, _dir) = p.open();
        h.exec(
            Side::Server,
            "probe",
            r#"
            pcall(function() return game.NoSuchThing end)
            pcall(function() return game.NoSuchThing end)
            pcall(function() return game:GetService("NoSuchService") end)
            pcall(function() return Instance.new("NoSuchClass") end)
            "#,
        );
        no_errors(&h);
        let u = h.unsupported();
        assert_eq!(u.get("DataModel.NoSuchThing"), Some(&2), "{u:?}");
        assert_eq!(u.get("GetService(NoSuchService)"), Some(&1), "{u:?}");
        assert_eq!(u.get("Instance.new(NoSuchClass)"), Some(&1), "{u:?}");
    }

    #[test]
    fn stuck_yields_are_listed() {
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Waiter",
            sss,
            None,
            "workspace:WaitForChild(\"Never\")",
        );
        let (mut h, _dir) = p.open();
        h.start();
        h.step(2);
        let stuck = h.stuck(Side::Server);
        assert_eq!(stuck.len(), 1, "{stuck:?}");
        assert!(stuck[0].0.contains("WaitForChild"), "{stuck:?}");
    }

    /// Luau resolves `global.a.b` when a chunk loads; an instance read must
    /// still happen when the code runs.
    #[test]
    fn instance_reads_are_not_fixed_when_a_chunk_loads() {
        let mut p = Project::new();
        let ws = p.service("Workspace");
        p.folder("Box", ws);
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Main",
            sss,
            None,
            r#"
            local first = workspace.Box
            first:Destroy()
            local second = Instance.new("Folder")
            second.Name = "Box"
            second.Parent = workspace
            print(workspace.Box == second, workspace.Box ~= first)
            "#,
        );
        let (mut h, _dir) = p.open();
        h.start();
        no_errors(&h);
        assert_eq!(out(&mut h, Side::Server), ["true\ttrue"]);
        assert!(h.unsupported().is_empty(), "{:?}", h.unsupported());
    }

    #[test]
    fn datatypes_and_enum_items_round_trip_through_properties_and_attributes() {
        let p = Project::new();
        let (mut h, _dir) = p.open();
        h.exec(
            Side::Server,
            "probe",
            r#"
            local s = Instance.new("Script")
            print(s.RunContext == Enum.RunContext.Legacy, typeof(s.RunContext))
            s.RunContext = Enum.RunContext.Client
            print(s.RunContext.Name)
            s.RunContext = "Server"
            print(s.RunContext.Name)
            s.RunContext = 2
            print(s.RunContext.Name)
            print(pcall(function() s.RunContext = Enum.Material.Plastic end))
            local f = Instance.new("Folder")
            f:SetAttribute("c", Color3.new(1, 0, 0))
            f:SetAttribute("r", NumberRange.new(1, 2))
            f:SetAttribute("m", Enum.Material.Plastic)
            print(typeof(f:GetAttribute("c")), f:GetAttribute("c").R, typeof(f:GetAttribute("r")), f:GetAttribute("r").Max)
            print(typeof(f:GetAttribute("m")), f:GetAttribute("m"))
            print(Vector3.new(1, 2, 3).Y, CFrame.new(1, 2, 3).Position.Z)
            "#,
        );
        no_errors(&h);
        let s = out(&mut h, Side::Server);
        assert_eq!(s[0], "true\tEnumItem");
        assert_eq!(&s[1..4], ["Client", "Server", "Client"]);
        assert!(
            s[4].starts_with("false\t") && s[4].contains("RunContext"),
            "{s:?}"
        );
        assert_eq!(s[5], "Color3\t1\tNumberRange\t2");
        // An enum item attribute keeps its type and reads back as the item.
        assert_eq!(s[6], "EnumItem\tEnum.Material.Plastic");
        assert_eq!(s[7], "2\t3");
    }

    /// A `function`-typed builtin property holds a Luau function in the VM
    /// (`OnInvoke`, `OnServerInvoke`); engine code calls it.
    #[test]
    fn callback_properties_hold_functions() {
        let p = Project::with_setup(test_function_setup());
        let (mut h, _dir) = p.open();
        h.exec(
            Side::Server,
            "probe",
            r#"
            local f = Instance.new("TestFunction")
            print(f.OnInvoke)
            f.OnInvoke = function(x) return x * 2 end
            print(type(f.OnInvoke), f:Invoke(21))
            print(pcall(function() f.OnInvoke = 5 end))
            f.OnInvoke = nil
            print(f.OnInvoke, pcall(f.Invoke, f, 1))
            "#,
        );
        no_errors(&h);
        let s = out(&mut h, Side::Server);
        assert_eq!(s[0], "nil");
        assert_eq!(s[1], "function\t42");
        assert!(
            s[2].starts_with("false\t") && s[2].contains("must be a function or nil"),
            "{s:?}"
        );
        assert!(
            s[3].starts_with("nil\tfalse\t") && s[3].contains("no OnInvoke"),
            "{s:?}"
        );
    }

    #[test]
    fn a_project_without_compat_settings_opens_with_the_defaults() {
        let p = Project::new();
        let (h, dir) = p.open();
        assert_eq!(h.frames(), 0);
        assert_eq!(h.project(), dir.path());
        let _ = CompatConfig::default();
        assert!(Path::new(dir.path()).join("real.toml").is_file());
    }
}

// Grouped so `cargo test -p re_compat reload` selects them (ADR-0048 §3).
mod reload {
    use super::*;

    const WORKER: &str = r#"
print("worker start", script.Enabled, script.Disabled)
local beats = 0
game:GetService("RunService").Heartbeat:Connect(function()
    beats += 1
    script:SetAttribute("beats", beats)
end)
task.spawn(function()
    while true do
        task.wait()
        script:SetAttribute("loop", (script:GetAttribute("loop") or 0) + 1)
    end
end)
local p = Instance.new("Part")
p.Name = "FromWorker"
p.Parent = workspace
"#;

    const CONTROL: &str = r#"
local worker = script.Parent.Worker
local function made()
    local n = 0
    for _, x in workspace:GetChildren() do
        if x.Name == "FromWorker" then
            n += 1
        end
    end
    return n
end
task.wait(0.05)
print("before", worker:GetAttribute("beats"), made())
worker.Enabled = true
task.wait(0.1)
print("running", worker:GetAttribute("beats") > 0)
worker.Disabled = true
print("disabled", worker.Enabled, worker.Disabled)
local beats, loop = worker:GetAttribute("beats"), worker:GetAttribute("loop")
task.wait(0.1)
print("stopped", worker:GetAttribute("beats") == beats, worker:GetAttribute("loop") == loop, made())
worker.Enabled = true
worker.Enabled = true
task.wait(0.05)
print("again", made())
worker.Enabled = false
worker.Enabled = true
worker.Enabled = false
task.wait(0.05)
print("cancelled", made())
"#;

    /// A disabled script does not start; enabling it starts it from the
    /// top, disabling stops its threads and connections and keeps what it
    /// made, and a script starts at most once per enable.
    #[test]
    fn enabled_and_disabled_start_and_stop_a_script() {
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        let worker = p.script("Script", "Worker", sss, None, WORKER);
        p.setup
            .classes
            .set_property(&mut p.world, worker, "Enabled", Value::Bool(false))
            .unwrap();
        p.script("Script", "Control", sss, None, CONTROL);
        let (mut h, _dir) = p.open();
        h.start();
        h.step(40);
        no_errors(&h);
        assert_eq!(
            out(&mut h, Side::Server),
            [
                "before\tnil\t0",
                "worker start\ttrue\tfalse",
                "running\ttrue",
                "disabled\tfalse\ttrue",
                "stopped\ttrue\ttrue\t1",
                "worker start\ttrue\tfalse",
                "again\t2",
                "cancelled\t2",
            ]
        );
    }

    /// In a play test, a write the VM did not make (the editor's, or a
    /// replicated one) takes effect at the next pump.
    #[test]
    fn a_write_from_outside_takes_effect_at_the_next_pump() {
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Ticker",
            sss,
            None,
            "print(\"tick start\")\nwhile true do task.wait(0.05) print(\"tick\") end",
        );
        let (mut h, _dir) = p.open_with(HarnessOptions {
            two_worlds: true,
            ..HarnessOptions::default()
        });
        h.start();
        h.step(4);
        assert_eq!(out(&mut h, Side::Server), ["tick start", "tick"]);
        let classes = h.vm(Side::Server).setup().classes.clone();
        let ticker = h.world(Side::Server).entity("Ticker").ok();
        let ticker = ticker.unwrap_or_else(|| {
            let w = h.world(Side::Server);
            re_compat::instance::tree::descendants(w, w.entity("ServerScriptService").unwrap())
                .into_iter()
                .find(|e| re_compat::instance::tree::name(w, *e) == "Ticker")
                .unwrap()
        });
        classes
            .set_property(
                h.world_mut(Side::Server),
                ticker,
                "Disabled",
                Value::Bool(true),
            )
            .unwrap();
        h.step(10);
        assert!(out(&mut h, Side::Server).is_empty());
        classes
            .set_property(
                h.world_mut(Side::Server),
                ticker,
                "Enabled",
                Value::Bool(true),
            )
            .unwrap();
        h.step(4);
        no_errors(&h);
        assert_eq!(out(&mut h, Side::Server), ["tick start", "tick"]);
    }

    /// A changed script restarts; a changed module restarts every running
    /// script that required it, directly or through another module, and
    /// nothing else; a source that does not compile restarts nothing until
    /// the next good save.
    #[test]
    fn changed_scripts_and_modules_restart_what_uses_them() {
        let mut p = Project::new();
        let rs = p.service("ReplicatedStorage");
        let sss = p.service("ServerScriptService");
        p.script("ModuleScript", "M", rs, None, "return { v = 1 }");
        let m = p.last_path("M");
        p.script(
            "ModuleScript",
            "N",
            rs,
            None,
            "local M = require(script.Parent.M)\nreturn { v = M.v * 10 }",
        );
        p.script(
            "Script",
            "UsesN",
            sss,
            None,
            "print(\"UsesN\", require(game.ReplicatedStorage.N).v)",
        );
        p.script(
            "Script",
            "UsesM",
            sss,
            None,
            "task.wait(0.05)\nprint(\"UsesM\", require(game.ReplicatedStorage.M).v)",
        );
        p.script("Script", "Plain", sss, None, "print(\"Plain\", 1)");
        let plain = p.last_path("Plain");
        let dir = p.dir.path().to_path_buf();
        let (mut h, _dir) = p.open();
        h.start();
        h.step(10);
        no_errors(&h);
        assert_eq!(
            out(&mut h, Side::Server),
            ["UsesN\t10", "Plain\t1", "UsesM\t1"]
        );

        std::fs::write(dir.join(&m), "return { v = 2 }").unwrap();
        assert!(h.reload(std::slice::from_ref(&m)).is_empty());
        h.step(10);
        no_errors(&h);
        assert_eq!(out(&mut h, Side::Server), ["UsesN\t20", "UsesM\t2"]);

        std::fs::write(dir.join(&plain), "print(\"Plain\", 2)").unwrap();
        assert!(h.reload(std::slice::from_ref(&plain)).is_empty());
        h.step(2);
        assert_eq!(out(&mut h, Side::Server), ["Plain\t2"]);

        std::fs::write(dir.join(&m), "return { v = ").unwrap();
        let d = h.reload(std::slice::from_ref(&m));
        assert_eq!(d.len(), 1, "{d:#?}");
        assert_eq!(d[0].code, re_compat::codes::RELOAD_COMPILE);
        assert!(
            d[0].message
                .starts_with(&format!("{m} changed but does not compile")),
            "{d:?}"
        );
        h.step(10);
        no_errors(&h);
        assert!(out(&mut h, Side::Server).is_empty());
        assert_eq!(
            h.required(Side::Server).len(),
            2,
            "the cache keeps the old modules"
        );

        std::fs::write(dir.join(&m), "return { v = 3 }").unwrap();
        assert!(h.reload(std::slice::from_ref(&m)).is_empty());
        h.step(10);
        no_errors(&h);
        assert_eq!(out(&mut h, Side::Server), ["UsesN\t30", "UsesM\t3"]);
    }

    /// A disabled script is not restarted by a change; it runs the new
    /// source when it is enabled.
    #[test]
    fn a_disabled_script_runs_its_new_source_when_enabled() {
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        let e = p.script("Script", "Later", sss, None, "print(\"old\")");
        let path = p.last_path("Later");
        p.setup
            .classes
            .set_property(&mut p.world, e, "Disabled", Value::Bool(true))
            .unwrap();
        let dir = p.dir.path().to_path_buf();
        let (mut h, _dir) = p.open();
        h.start();
        h.step(2);
        std::fs::write(dir.join(&path), "print(\"new\")").unwrap();
        assert!(h.reload(std::slice::from_ref(&path)).is_empty());
        h.step(2);
        assert!(out(&mut h, Side::Server).is_empty());
        let d = h.exec(
            Side::Server,
            "enable",
            "game.ServerScriptService.Later.Disabled = false",
        );
        assert!(d.is_empty(), "{d:#?}");
        h.step(2);
        no_errors(&h);
        assert_eq!(out(&mut h, Side::Server), ["new"]);
    }

    /// Scripts and modules register their chunks with the debugger under
    /// their source path (ADR-0045), and a restart swaps the old chunk for
    /// the new one, so breakpoints keep working after a reload.
    #[test]
    fn breakpoints_reach_scripts_and_survive_a_reload() {
        use re_script::debug::{self, Debugger};
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        let source = |step: u32| {
            format!(
                "local n = 0\ngame:GetService(\"RunService\").Heartbeat:Connect(function()\n    n += {step}\nend)\n"
            )
        };
        p.script("Script", "Counter", sss, None, &source(1));
        let path = p.last_path("Counter");
        let dir = p.dir.path().to_path_buf();
        let (mut h, _dir) = p.open();
        let debugger = Debugger::new();
        let lua = h.vm(Side::Server).context().lua().clone();
        debugger.attach(&lua, "server", None).unwrap();
        h.start();
        debugger.set_breakpoints(&path, &[3]);
        debug::service(&lua);
        h.step(2);
        let stop = debugger.last_unpaused().expect("a recorded stop");
        let top = stop.top().unwrap();
        assert_eq!(
            (top.source.as_deref(), top.line),
            (Some(path.as_str()), Some(3))
        );

        std::fs::write(dir.join(&path), source(2)).unwrap();
        assert!(h.reload(std::slice::from_ref(&path)).is_empty());
        debug::service(&lua);
        h.step(2);
        let stop = debugger.last_unpaused().expect("a stop after the reload");
        assert_eq!(stop.top().unwrap().line, Some(3));
    }

    #[test]
    fn a_debugger_attached_to_a_running_game_reaches_its_scripts() {
        use re_script::debug::{self, Debugger};
        let mut p = Project::new();
        let sss = p.service("ServerScriptService");
        p.script(
            "Script",
            "Counter",
            sss,
            None,
            "local n = 0\ngame:GetService(\"RunService\").Heartbeat:Connect(function()\n    n += 1\nend)\n",
        );
        let path = p.last_path("Counter");
        let (mut h, _dir) = p.open();
        h.start();
        h.step(2);
        let debugger = Debugger::new();
        h.vm(Side::Server).attach_debugger(&debugger).unwrap();
        debugger.set_breakpoints(&path, &[3]);
        let lua = h.vm(Side::Server).context().lua().clone();
        debug::service(&lua);
        h.step(2);
        // With the context's parking, the stop pauses the handler's thread.
        let stop = debugger
            .stopped()
            .or_else(|| debugger.last_unpaused())
            .expect("a stop");
        let top = stop.top().unwrap();
        assert_eq!(
            (top.source.as_deref(), top.line),
            (Some(path.as_str()), Some(3))
        );
    }
}
