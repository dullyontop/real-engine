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

    /// Saves the project and opens it in a harness.
    fn open(mut self) -> (Harness, tempfile::TempDir) {
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
        let h = Harness::open_with(dir, &self.setup, HarnessOptions::default()).unwrap();
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
