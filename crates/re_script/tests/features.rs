//! The spike's feature tests, ported to `ScriptRuntime`, and the WP3 required
//! tests. Run with `cargo test -p re_script --test features -- --nocapture` to see
//! the messages.

mod common;

use std::time::Duration;

use common::{Rig, STEP, project};
use re_core::time::Instant;
use re_core::{Location, Phase};
use re_scene::{ScriptContext, ScriptRef, StableId};
use re_script::{ContextKind, Reload, RuntimeConfig, codes};
use re_types::glam::Vec3;
use re_types::physics::ContactEvent;
use serde_json::json;

fn line_of(d: &re_core::Diagnostic) -> Option<u32> {
    match &d.location {
        Some(Location::File { line, .. }) => *line,
        _ => None,
    }
}

fn file_of(d: &re_core::Diagnostic) -> Option<&str> {
    match &d.location {
        Some(Location::File { file, .. }) => Some(file),
        _ => None,
    }
}

#[test]
fn sandbox_blocks_dangerous_globals_and_freezes_libraries() {
    let mut rig = Rig::new();
    let probe = rig
        .exec("return { loadstring == nil, getfenv == nil, setfenv == nil, os.execute == nil, io == nil }")
        .unwrap();
    assert_eq!(probe, json!([true, true, true, true, true]));

    let d = rig.exec_err("math.sqrt = function() return 0 end");
    assert!(d.message.contains("readonly"), "{}", d.message);
    rig.exec_err("string.rep = nil");
    rig.exec_err("world.get = nil");
    rig.exec_err("getmetatable('').__index = {}");

    // A global written by one script stays in that script's environment.
    rig.exec("shared_value = 1").unwrap();
    assert_eq!(rig.exec("return shared_value").unwrap(), json!(null));
}

#[test]
fn entity_handles_fields_methods_metamethods() {
    let mut rig = Rig::new();
    let out = rig
        .exec(
            r#"
            local a = world.spawn({ id = "hero", name = "Hero", components = { Transform = { translation = vector.create(1, 2, 3) } } })
            local b = world.spawn({ id = "goblin", name = "Goblin", components = { Transform = {}, HudText = { text = "grr" } } })
            a:set("Transform", "translation", a:get("Transform", "translation") + vector.create(0, 0, 1))
            local same = world.query("HudText")[1]
            local again = world.get("goblin")
            return {
                tostring(a), a.id, a.name, same == b, rawequal(same, b), rawequal(again, b),
                a:get("Transform", "translation").z, b:has("HudText"), a:has("HudText"),
            }
            "#,
        )
        .unwrap();
    println!("{out}");
    let out = out.as_array().unwrap();
    assert!(out[0].as_str().unwrap().contains("\"hero\""));
    assert_eq!(out[1], "hero");
    assert_eq!(out[2], "Hero");
    assert_eq!(
        &out[3..6],
        [true, true, true],
        "interned handles compare equal and rawequal"
    );
    assert_eq!(out[6], 4);
    assert_eq!(&out[7..9], [true, false]);

    // Wrong field type, stale handles, unknown methods and read-only fields.
    // The registry's type error (re_reflect's code) names the field.
    let d = rig.exec_err("world.get('hero'):set('Transform', 'translation', 5)");
    assert!(d.message.contains("Transform.translation"), "{}", d.message);
    let d = rig.exec_err("local e = world.spawn(); e:destroy(); return e.name");
    assert_eq!(d.code, codes::STALE_HANDLE);
    assert!(d.message.contains("stale handle"));
    let d = rig.exec_err("world.get('hero'):jump()");
    assert_eq!(d.code, codes::UNSUPPORTED_MEMBER);
    rig.exec_err("world.get('hero').id = 'x'");
}

#[test]
fn vector_maps_to_glam() {
    let mut rig = Rig::new();
    let v = rig
        .exec("local v = vector.create(3, 4, 0); return v * 2 + vector.create(0, 0, vector.magnitude(v))")
        .unwrap();
    assert_eq!(v, json!([6, 8, 5]));
    assert_eq!(
        rig.exec("return typeof(vector.create(1, 2, 3))").unwrap(),
        "vector"
    );
    // Luau -> Rust -> Luau through an engine field.
    let v = rig
        .exec(
            r#"
            local e = world.spawn({ components = { Transform = { translation = vector.create(0, 3, 4) } } })
            local t = e:get("Transform", "translation")
            return { t.x + t.y + t.z, vector.normalize(t) }
            "#,
        )
        .unwrap();
    assert_eq!(v[0], 7);
    let n = v[1].as_array().unwrap();
    assert!(
        (n[1].as_f64().unwrap() - 0.6).abs() < 1e-6 && (n[2].as_f64().unwrap() - 0.8).abs() < 1e-6
    );
    let d =
        rig.exec_err("world.spawn({ components = { Transform = { translation = { 1, 2, 3 } } } })");
    assert_eq!(d.code, codes::BAD_ARGUMENT);
    assert!(d.message.contains("Transform.translation"), "{}", d.message);
}

#[test]
fn engine_functions_take_tables_and_callbacks_report_luau_locations() {
    let mut rig = Rig::new();
    let n = rig
        .exec(
            r#"
            for i = 1, 5 do
                world.spawn({ id = `crate_{i}`, components = { Transform = { translation = vector.create(i, 0, i * 10) } } })
            end
            local found = world.query("Transform")
            return { #found, found[5].id, found[5]:get("Transform", "translation").z }
            "#,
        )
        .unwrap();
    assert_eq!(n, json!([5, "crate_5", 50]));

    // An error inside a callback the engine calls carries the callback's location.
    let errs = rig.run("scripts/broken.luau");
    assert_eq!(errs.len(), 1);
    rig.exec("time.on_step(function()\n\terror('placement failed')\nend)")
        .unwrap();
    let errs = rig.step(STEP);
    assert_eq!(errs.len(), 1, "{errs:#?}");
    assert!(errs[0].message.contains("placement failed"));
    assert_eq!(file_of(&errs[0]), Some("probe"));
    assert_eq!(line_of(&errs[0]), Some(2));
    assert!(errs[0].hint.as_deref().unwrap().contains("time.on_step"));
}

#[test]
fn input_handlers_can_wait() {
    let mut rig = Rig::new();
    let errs = rig.exec(
        r#"
        input.on_pressed("jump", function()
            print("jump pressed", input.down("jump"))
            task.wait(0.5)
            print("landed")
        end)
        input.on_pressed("jump", function() error("handler two broke") end)
        "#,
    );
    assert!(errs.is_ok());
    rig.services.press("jump");
    let errs = rig.step(STEP);
    rig.services.clear_input();
    assert_eq!(errs.len(), 1, "{errs:#?}");
    assert!(errs[0].message.contains("handler two broke"));
    assert_eq!(rig.output(), ["jump pressed\ttrue"]);
    rig.run_for(0.3, 0.1);
    assert!(rig.output().is_empty(), "still waiting");
    rig.run_for(0.3, 0.1);
    assert_eq!(rig.output(), ["landed"]);
}

#[test]
fn scheduler_wait_defer_delay() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
    std::fs::write(
        dir.path().join("scripts/loop.luau"),
        r#"
        task.defer(function(tag) print("deferred", tag) end, "A")
        task.delay(1.0, function() print("delayed") end)
        for i = 1, 3 do
            local dt = task.wait(0.25)
            print(string.format("tick %d dt=%.2f", i, dt))
        end
        local ok, err = pcall(function() task.wait(0.25) error("inside pcall") end)
        print("pcall across yield", ok, err)
        "#,
    )
    .unwrap();
    let mut rig = Rig::at(dir.path());
    assert!(rig.run("scripts/loop.luau").is_empty());
    rig.run_for(2.0, 0.05);
    let out = rig.output();
    println!("{out:#?}");
    assert_eq!(out[0], "deferred\tA");
    assert!(out.iter().any(|l| l == "delayed"));
    assert!(out.iter().any(|l| l == "tick 1 dt=0.25"), "{out:?}");
    assert!(out.iter().any(|l| l.starts_with("tick 3")));
    assert!(out.last().unwrap().starts_with("pcall across yield\tfalse"));
    assert_eq!(rig.rt.context(ContextKind::Game).sleeping_threads(), 0);

    // task.wait from a synchronous engine call fails clearly.
    let d = rig.exec_err("task.wait(1)");
    assert_eq!(d.code, codes::YIELD);
}

#[test]
fn task_wait_in_a_user_coroutine_is_refused() {
    let mut rig = Rig::new();
    let d = rig
        .exec(
            r#"
            task.spawn(function()
                local co = coroutine.wrap(function() task.wait(1) end)
                co()
            end)
            "#,
        )
        .expect_err("task.wait inside coroutine.wrap must fail");
    assert_eq!(d.len(), 1, "{d:#?}");
    assert_eq!(d[0].code, codes::YIELD);
    assert!(d[0].message.contains("coroutine"), "{}", d[0].message);
    assert_eq!(line_of(&d[0]), Some(3), "the line of the task.wait call");
}

#[test]
fn require_resolves_inside_scripts_only() {
    let mut rig = Rig::new();
    let errs = rig.run("scripts/main.luau");
    assert!(errs.is_empty(), "{errs:#?}");
    assert_eq!(rig.output(), ["double\t42\tsword"]);

    for escape in ["scripts/escape.luau", "scripts/escape_far.luau"] {
        let errs = rig.run(escape);
        assert_eq!(errs.len(), 1, "{errs:#?}");
        println!("{escape}: {}", errs[0].message);
        assert_eq!(errs[0].code, codes::REQUIRE, "{}", errs[0].message);
    }
    // An exec snippet requires as if it were a file in scripts/.
    assert_eq!(
        rig.exec("return require('./lib/util').double(4)").unwrap(),
        8
    );
    let d = rig.exec_err("return require('./nope')");
    println!("missing: {}", d.message);

    // A script path outside scripts/ is not run at all.
    let errs = rig.run("outside/secret.luau");
    assert_eq!(errs[0].code, codes::SCRIPT_READ);
}

#[test]
fn errors_have_chunk_line_and_traceback() {
    let mut rig = Rig::new();
    let errs = rig.run("scripts/broken.luau");
    let d = &errs[0];
    println!("{d:#?}");
    assert_eq!(d.code, codes::RUNTIME);
    assert_eq!(file_of(d), Some("scripts/broken.luau"));
    assert_eq!(line_of(d), Some(2));
    let hint = d.hint.as_deref().unwrap();
    assert!(hint.contains("inner") && hint.contains("outer"), "{hint}");

    let d = rig.exec_err("local x = nil\nlocal y = x + 1");
    assert_eq!(line_of(&d), Some(2));

    let d = rig.exec_err("local x = = 1");
    assert_eq!(d.code, codes::SYNTAX);
    assert_eq!(line_of(&d), Some(1));
}

#[test]
fn runtime_error_appears_at_most_once() {
    let mut rig = Rig::new();
    let d = rig.exec_err("task.spawn(function() error('boom') end)");
    let text = format!("{} {}", d.message, d.hint.unwrap_or_default());
    assert!(text.matches("runtime error").count() <= 1, "{text}");
    let errs = rig.run("scripts/broken.luau");
    let text = format!(
        "{} {}",
        errs[0].message,
        errs[0].hint.clone().unwrap_or_default()
    );
    assert!(text.matches("runtime error").count() <= 1, "{text}");
}

#[test]
fn hot_reload_keeps_state_and_swaps_functions() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
    std::fs::copy(
        project().join("scripts/counter.luau"),
        dir.path().join("scripts/counter.luau"),
    )
    .unwrap();
    std::fs::write(
        dir.path().join("scripts/user.luau"),
        "local counter = require(\"./counter\")\nwhile true do\n\tprint(counter.bump())\n\ttask.wait(1)\nend\n",
    )
    .unwrap();
    let mut rig = Rig::at(dir.path());
    assert!(rig.run("scripts/user.luau").is_empty());
    rig.run_for(1.0, 0.5);
    assert_eq!(rig.output(), ["v1\t1", "v1\t2"]);

    let reload = |rig: &mut Rig| {
        let Rig {
            rt,
            world,
            services,
        } = rig;
        let mut host = re_script::WorldHost { world, services };
        rt.reload(&mut host, "scripts/counter.luau")
    };
    std::fs::write(
        dir.path().join("scripts/counter.luau"),
        "local M = ... or {}\nM.count = M.count or 1000\nif ... then print('reloaded with count', M.count) end\nfunction M.bump()\n\tM.count += 10\n\treturn \"v2\", M.count\nend\nreturn M\n",
    )
    .unwrap();
    let r = reload(&mut rig);
    println!("{r:?}");
    assert_eq!(r.modules, [(ContextKind::Game, Reload::InPlace)]);
    assert!(r.diagnostics.is_empty());
    rig.run_for(1.0, 0.5);
    assert_eq!(rig.output(), ["reloaded with count\t2", "v2\t12"]);

    // A reload with a syntax error keeps the running version.
    std::fs::write(dir.path().join("scripts/counter.luau"), "return {").unwrap();
    let r = reload(&mut rig);
    assert_eq!(r.diagnostics[0].code, codes::SYNTAX);
    assert!(r.modules.is_empty());
    rig.run_for(1.0, 0.5);
    assert_eq!(rig.output(), ["v2\t22"]);

    // A module that ignores `...` is replaced; holders keep the old table.
    std::fs::write(
        dir.path().join("scripts/counter.luau"),
        "return { count = 0, bump = function() return 'v3', 0 end }",
    )
    .unwrap();
    let r = reload(&mut rig);
    assert_eq!(r.modules, [(ContextKind::Game, Reload::Replaced)]);
    rig.run_for(1.0, 0.5);
    assert_eq!(rig.output(), ["v2\t32"], "the old holder still runs v2");
    assert_eq!(
        rig.exec("return select(1, require('./counter').bump())")
            .unwrap(),
        "v3"
    );
}

#[test]
fn reloading_an_entry_script_reconnects_its_callbacks() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("scripts")).unwrap();
    let path = dir.path().join("scripts/ticker.luau");
    std::fs::write(&path, "time.on_step(function() print('v1') end)").unwrap();
    let mut rig = Rig::at(dir.path());
    assert!(rig.run("scripts/ticker.luau").is_empty());
    rig.step(STEP);
    assert_eq!(rig.output(), ["v1"]);
    std::fs::write(&path, "time.on_step(function() print('v2') end)").unwrap();
    let Rig {
        rt,
        world,
        services,
    } = &mut rig;
    let r = rt.reload(
        &mut re_script::WorldHost { world, services },
        "scripts/ticker.luau",
    );
    assert_eq!(r.restarted, 1);
    assert!(r.diagnostics.is_empty());
    rig.step(STEP);
    assert_eq!(rig.output(), ["v2"]);
    assert_eq!(rig.rt.context(ContextKind::Game).connections(), 1);
}

#[test]
fn cpu_budget_stops_runaway_loops_and_the_context_stays_usable() {
    let mut rig = Rig::with_config(Rig::budget(RuntimeConfig::new(project()), 20));
    let t = Instant::now();
    let d = rig.exec_err("while true do end");
    println!("after {:?}", t.elapsed());
    assert_eq!(d.code, codes::TIMEOUT);
    assert!(t.elapsed() < Duration::from_secs(2));
    assert_eq!(rig.exec("return 40 + 2").unwrap(), 42);

    // In a script thread: the thread ends, the context keeps going.
    rig.exec("task.spawn(function() local n = 0 while true do n += 1 end end)")
        .expect_err("the spawned thread times out");
    rig.exec("time.on_step(function() print('still alive') end)")
        .unwrap();
    assert!(rig.step(STEP).is_empty());
    assert_eq!(rig.output(), ["still alive"]);

    // pcall cannot swallow it: the budget stays tripped until re-armed.
    let t = Instant::now();
    let d = rig.exec_err("for i = 1, 1e9 do pcall(function() while true do end end) end");
    assert_eq!(d.code, codes::TIMEOUT);
    assert!(t.elapsed() < Duration::from_secs(2), "{:?}", t.elapsed());
    assert_eq!(rig.exec("return 'ok'").unwrap(), "ok");
}

#[test]
fn memory_limit_raises_and_the_vm_recovers() {
    let mut config = Rig::budget(RuntimeConfig::new(project()), 10_000);
    config.memory_limit = 16 * 1024 * 1024;
    let mut rig = Rig::with_config(config);
    let d = rig.exec_err("local t = {} for i = 1, 1e8 do t[i] = string.rep('x', 64) .. i end");
    assert_eq!(d.code, codes::MEMORY);
    let n = rig
        .exec("local t = {} for i = 1, 1000 do t[i] = i end return #t")
        .unwrap();
    assert_eq!(n, 1000);
}

#[test]
fn loaded_modules_are_tracked() {
    let mut rig = Rig::new();
    assert!(rig.run("scripts/main.luau").is_empty());
    let modules = rig.rt.context(ContextKind::Game).loaded_modules();
    assert_eq!(
        modules,
        [
            "scripts/lib/inventory/init.luau",
            "scripts/lib/inventory/item.luau",
            "scripts/lib/util.luau"
        ]
    );
    assert!(
        rig.rt
            .context(ContextKind::Tools)
            .loaded_modules()
            .is_empty()
    );
}

#[test]
fn vector_metatable_is_frozen() {
    let mut rig = Rig::new();
    let r = rig
        .exec(
            r#"
            local mt = getmetatable(vector.zero)
            local ok = pcall(function() mt.__index = function() return 1 end end)
            return { typeof(mt), ok }
            "#,
        )
        .unwrap();
    println!("vector metatable: {r}");
    assert_eq!(r[1], false);
}

#[test]
fn handles_are_released_after_despawn() {
    let mut rig = Rig::new();
    // A callback holds 50 handles; on the first step it destroys their entities
    // and lets go of them.
    rig.exec(
        r#"
        local held = {}
        for i = 1, 50 do table.insert(held, world.spawn({ id = `e{i}` })) end
        local c
        c = time.on_step(function()
            for _, e in held do e:destroy() end
            held = nil
            c:disconnect()
        end)
        "#,
    )
    .unwrap();
    let game = ContextKind::Game;
    assert!(
        rig.rt.context(game).live_handles() >= 50,
        "held handles stay alive"
    );
    assert!(rig.step(STEP).is_empty());
    assert!(rig.world.ids().get("e1").is_none(), "despawned");
    assert_eq!(rig.rt.context(game).live_handles(), 0);
}

#[test]
fn script_components_round_trip_f32() {
    let mut rig = Rig::new();
    let r = rig
        .exec(
            r#"
            world.define_component("Collectible", { value = "i32", weight = "f32" })
            world.define_component("Collectible", { value = "i32", weight = "f32" }) -- same shape again
            local coin = world.spawn({ id = "coin", components = { Collectible = { value = 5 } } })
            coin:set("Collectible", "weight", 0.1)
            local found = world.query("Collectible")
            return { #found, found[1] == coin, coin:get("Collectible", "value"), coin:get("Collectible", "weight") }
            "#,
        )
        .unwrap();
    assert_eq!(r[0], 1);
    assert_eq!(r[1], true);
    assert_eq!(r[2], 5);
    assert_eq!(r[3].as_f64().unwrap() as f32, 0.1f32);
    assert_eq!(
        rig.world.get("coin", "Collectible", "weight").unwrap(),
        re_reflect::Value::Float(f64::from(0.1f32))
    );
    let d = rig.exec_err("world.define_component('Collectible', { value = 'f64' })");
    assert_eq!(d.code, codes::COMPONENT_DEFINITION);
    let d = rig.exec_err("world.define_component('Transform', {})");
    assert_eq!(d.code, codes::COMPONENT_DEFINITION);
}

#[test]
fn container_fields_map_to_luau_tables() {
    use re_reflect::{FieldType, RecordField, RecordSchema, Value};
    let dim = RecordSchema::new(
        "LuauDim",
        vec![
            RecordField::new("scale", FieldType::F32),
            RecordField::new("offset", FieldType::I32),
        ],
    )
    .register()
    .unwrap();
    let mut rig = Rig::new();
    let r = rig
        .exec(
            r#"
            world.define_component("Inventory", {
                items = { type = "list", item = "string" },
                stats = "map",
                size = { type = "record", record = "LuauDim" },
                sizes = { type = "list", item = { type = "record", record = "LuauDim" } },
            })
            local bag = world.spawn({ id = "bag", components = { Inventory = {
                items = { "rope", "lamp" },
                stats = { hp = 10, at = vector.create(1, 2, 3) },
            } } })
            local fresh = bag:get("Inventory", "size")
            bag:set("Inventory", "size", { scale = 0.5, offset = 3 })
            local size = bag:get("Inventory", "size")
            local stats = bag:get("Inventory", "stats")
            stats.copy = size -- a record inside a map keeps its type
            bag:set("Inventory", "stats", stats)
            bag:set("Inventory", "sizes", { size, { 2, 4 } })
            local writable = pcall(function() size.scale = 1 end)
            bag:set("Inventory", "items", {})
            return {
                table.isfrozen(fresh), fresh.scale, size.scale, size.offset, writable,
                #bag:get("Inventory", "items"), bag:get("Inventory", "stats").copy.offset,
                bag:get("Inventory", "sizes")[2].offset,
            }
            "#,
        )
        .unwrap();
    assert_eq!(r, json!([true, 0, 0.5, 3, false, 0, 3, 4]));
    let rec = |s: f64, o: i64| Value::Record(dim, vec![Value::Float(s), Value::Int(o)]);
    let Value::Map(stats) = rig.world.get("bag", "Inventory", "stats").unwrap() else {
        panic!()
    };
    assert_eq!(stats["hp"], Value::Float(10.0));
    assert_eq!(stats["at"], Value::Vec3(glam::Vec3::new(1.0, 2.0, 3.0)));
    assert_eq!(stats["copy"], rec(0.5, 3));
    assert_eq!(
        rig.world.get("bag", "Inventory", "sizes").unwrap(),
        Value::List(vec![rec(0.5, 3), rec(2.0, 4)])
    );
    for (bad, msg) in [
        ("{ 1, x = 2 }", "cannot mix"),
        ("{ [1] = 'a', [3] = 'b' }", "no holes"),
        ("{ { 'a' } }", "expected string, got array"),
    ] {
        let d = rig.exec_err(&format!(
            "world.query('Inventory')[1]:set('Inventory', 'items', {bad})"
        ));
        assert!(d.message.contains(msg), "{bad}: {}", d.message);
    }
    let d = rig.exec_err("world.spawn({ components = { Inventory = { items = { 1, x = 2 } } } })");
    assert!(
        d.message.contains("Inventory.items: ") && d.message.contains("cannot mix"),
        "{}",
        d.message
    );
    let d = rig.exec_err(
        "world.query('Inventory')[1]:set('Inventory', 'size', { scale = 1, offest = 2 })",
    );
    assert!(d.message.contains("no field \"offest\""), "{}", d.message);
    let d = rig.exec_err(
        "world.query('Inventory')[1]:set('Inventory', 'stats', { me = world.query('Inventory')[1] })",
    );
    assert!(d.message.contains("entity reference"), "{}", d.message);
}

#[test]
fn task_wait_resumes_in_post_simulation() {
    let mut rig = Rig::new();
    rig.exec(
        r#"
        time.on_frame(function() print("frame") end)
        time.on_step(function() print("step") end)
        time.on_post_step(function() print("post_step") end)
        task.spawn(function()
            task.wait()
            print("woke")
        end)
        "#,
    )
    .unwrap();
    assert!(rig.phase(Phase::PreRender, STEP).is_empty());
    assert_eq!(rig.output(), ["frame"]);
    assert!(rig.phase(Phase::PreSimulation, STEP).is_empty());
    assert_eq!(rig.output(), ["step"]);
    for p in [Phase::Input, Phase::PreAnimation, Phase::Physics] {
        assert!(rig.phase(p, STEP).is_empty());
    }
    assert!(
        rig.output().is_empty(),
        "nothing resumes outside PostSimulation"
    );
    assert!(rig.phase(Phase::PostSimulation, STEP).is_empty());
    assert_eq!(rig.output(), ["woke", "post_step"]);
}

#[test]
fn unknown_api_members_raise_re9001() {
    let mut rig = Rig::new();
    for (src, member) in [
        ("return world.GetChildren", "world.GetChildren"),
        ("return physics.Raycast", "physics.Raycast"),
        ("return task.Wait", "task.Wait"),
    ] {
        let d = rig.exec_err(src);
        assert_eq!(d.code, codes::UNSUPPORTED_MEMBER);
        assert!(d.message.contains(member), "{}", d.message);
    }
    // The command library exists only in the tools context.
    assert_eq!(rig.exec("return command").unwrap(), json!(null));
    assert_eq!(
        rig.exec_in(ContextKind::Tools, "return typeof(command.run)")
            .unwrap(),
        "function"
    );
}

#[test]
fn attached_scripts_start_and_get_contacts() {
    let mut rig = Rig::new();
    let coin = rig
        .world
        .ecs
        .spawn((
            StableId("coin".into()),
            ScriptRef {
                path: "scripts/collect.luau".into(),
                context: ScriptContext::Game,
            },
            re_scene::HudText::default(),
        ))
        .id();
    let player = rig.world.ecs.spawn(StableId("player".into())).id();
    assert!(rig.step(STEP).is_empty());
    let begin = ContactEvent::Begin {
        a: re_scene::entity_id(coin),
        b: re_scene::entity_id(player),
        point: Vec3::ZERO,
        normal: Vec3::Y,
        sensor: true,
    };
    rig.rt.push_contacts(&[begin]);
    assert!(rig.step(STEP).is_empty());
    rig.rt.push_contacts(&[begin]);
    assert!(rig.step(STEP).is_empty());
    assert_eq!(
        rig.world.get("coin", "HudText", "text").unwrap(),
        re_reflect::Value::Str("touched 2".into())
    );
}

#[test]
fn builder_scripts_return_a_graph() {
    let mut rig = Rig::new();
    let Rig {
        rt,
        world,
        services,
    } = &mut rig;
    let g = rt
        .run_builder(
            &mut re_script::WorldHost { world, services },
            "scripts/builder.luau",
        )
        .unwrap();
    assert_eq!(g.name, "island");
    assert_eq!(g.seed, 1234);
    assert_eq!(g.nodes.len(), 4);
    assert_eq!(g.nodes[0].id, "n1");
    assert_eq!(g.edges.len(), 3);
    let errs = rt
        .run_builder(
            &mut re_script::WorldHost { world, services },
            "scripts/main.luau",
        )
        .unwrap_err();
    assert_eq!(errs[0].code, codes::GRAPH_BUILDER);
}

#[test]
fn builders_record_version_one_unless_a_kind_asks_for_another() {
    let dir = temp_project(&[
        (
            "scripts/tools/g.luau",
            "local g = proc.graph('v', { seed = 1 })\ng:node('terrain.heightfield', {})\ng:node('terrain.heightfield@3', {}, nil, 'h3')\nreturn g",
        ),
        (
            "scripts/tools/bad.luau",
            "local g = proc.graph('v', { seed = 1 })\ng:node('terrain.heightfield@0', {})\nreturn g",
        ),
    ]);
    let mut rig = Rig::at(dir.path());
    let Rig {
        rt,
        world,
        services,
    } = &mut rig;
    let g = rt
        .run_builder(
            &mut re_script::WorldHost { world, services },
            "scripts/tools/g.luau",
        )
        .unwrap();
    let versions: Vec<(&str, u32)> = g
        .nodes
        .iter()
        .map(|n| (n.kind.as_str(), n.version))
        .collect();
    assert_eq!(
        versions,
        [("terrain.heightfield", 1), ("terrain.heightfield", 3)]
    );
    let errs = rt
        .run_builder(
            &mut re_script::WorldHost { world, services },
            "scripts/tools/bad.luau",
        )
        .unwrap_err();
    assert_eq!(errs[0].code, codes::GRAPH_BUILDER);
    assert!(
        errs[0].message.contains("<kind>@<n>"),
        "{}",
        errs[0].message
    );
}

#[test]
fn errors_in_engine_plumbing_point_at_the_users_line() {
    let mut rig = Rig::new();
    let d = rig.exec_err("local e = world.spawn()\nlocal _ = e.zzz");
    assert_eq!(d.code, codes::UNSUPPORTED_MEMBER);
    assert_eq!((file_of(&d), line_of(&d)), (Some("probe"), Some(2)));
    let errs = rig.run("scripts/escape.luau");
    assert_eq!(errs[0].code, codes::REQUIRE);
    assert_eq!(
        (file_of(&errs[0]), line_of(&errs[0])),
        (Some("scripts/escape.luau"), Some(1))
    );
}

fn temp_project(files: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (path, text) in files {
        let p = dir.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }
    dir
}

fn reload(rig: &mut Rig, path: &str) -> re_script::ReloadReport {
    let Rig {
        rt,
        world,
        services,
    } = rig;
    rt.reload(&mut re_script::WorldHost { world, services }, path)
}

#[test]
fn reloading_an_entry_script_stops_its_threads_and_module_callbacks() {
    let dir = temp_project(&[
        (
            "scripts/helper.luau",
            "return { every_step = function(f) return time.on_step(f) end }",
        ),
        (
            "scripts/main.luau",
            "local h = require('./helper')\nh.every_step(function() print('cb v1') end)\ntask.spawn(function() while true do print('loop v1') task.wait(0.1) end end)",
        ),
    ]);
    let mut rig = Rig::at(dir.path());
    assert!(rig.run("scripts/main.luau").is_empty());
    rig.output();
    std::fs::write(
        dir.path().join("scripts/main.luau"),
        "local h = require('./helper')\nh.every_step(function() print('cb v2') end)\ntask.spawn(function() while true do print('loop v2') task.wait(0.1) end end)",
    )
    .unwrap();
    let r = reload(&mut rig, "scripts/main.luau");
    assert_eq!(r.restarted, 1);
    rig.run_for(0.5, STEP);
    let out = rig.output();
    assert!(out.iter().all(|l| l.ends_with("v2")), "{out:?}");
    let game = rig.rt.context(ContextKind::Game);
    assert_eq!(game.connections(), 1);
    assert_eq!(game.sleeping_threads(), 1);
}

#[test]
fn a_despawned_or_repointed_script_entity_stops_its_script() {
    let dir = temp_project(&[
        (
            "scripts/a.luau",
            "time.on_step(function() print('a', script.entity.alive) end)",
        ),
        ("scripts/b.luau", "time.on_step(function() print('b') end)"),
    ]);
    let mut rig = Rig::at(dir.path());
    let script = |path: &str| ScriptRef {
        path: path.into(),
        context: ScriptContext::Game,
    };
    let e = rig
        .world
        .ecs
        .spawn((StableId("x".into()), script("scripts/a.luau")))
        .id();
    assert!(rig.step(STEP).is_empty());
    assert_eq!(rig.output(), ["a\ttrue"]);
    rig.world.ecs.entity_mut(e).insert(script("scripts/b.luau"));
    assert!(rig.step(STEP).is_empty());
    assert_eq!(rig.output(), ["b"]);
    rig.world.ecs.despawn(e);
    assert!(rig.step(STEP).is_empty());
    assert!(rig.output().is_empty());
    assert_eq!(rig.rt.context(ContextKind::Game).connections(), 0);
}

#[test]
fn waits_are_exact_at_60_hz_and_wait_in_on_step_resumes_the_same_step() {
    let mut rig = Rig::new();
    rig.exec(
        r#"
        task.spawn(function() print(string.format("%.4f", task.wait(0.1))) end)
        local c
        c = time.on_step(function()
            c:disconnect()
            task.wait()
            print("after step wait")
        end)
        "#,
    )
    .unwrap();
    assert!(rig.phase(Phase::PreSimulation, STEP).is_empty());
    assert!(rig.output().is_empty());
    assert!(rig.phase(Phase::PostSimulation, STEP).is_empty());
    assert_eq!(rig.output(), ["after step wait"]);
    rig.run_for(5.0 * STEP, STEP);
    assert_eq!(rig.output(), ["0.1000"], "six steps of 1/60 s");
}

#[test]
fn resuming_a_scheduler_thread_by_hand_cannot_leak_the_wait() {
    let mut rig = Rig::new();
    rig.exec(
        r#"
        local t = task.spawn(function()
            task.wait(1)
            print("woke", pcall(task.wait, 1))
        end)
        print(coroutine.resume(t))
        "#,
    )
    .unwrap();
    let out = rig.output();
    println!("{out:?}");
    assert_eq!(out.len(), 2, "{out:?}");
    assert!(out[0].starts_with("woke\tfalse"), "{out:?}");
    assert!(out[1].starts_with("true"), "{out:?}");
    rig.run_for(2.0, 0.5);
    assert!(rig.output().is_empty());
}

#[test]
fn cancel_removes_waiting_threads_at_once() {
    let mut rig = Rig::new();
    rig.exec("local t = task.delay(1e6, print) task.cancel(t) local u = task.spawn(task.wait, 1e6) task.cancel(u)")
        .unwrap();
    assert_eq!(rig.rt.context(ContextKind::Game).sleeping_threads(), 0);
    rig.exec("task.spawn(function() task.cancel(coroutine.running()) task.wait(0.1) print('not reached') end)")
        .unwrap();
    rig.run_for(0.5, 0.1);
    assert!(rig.output().is_empty());
    assert_eq!(rig.rt.context(ContextKind::Game).sleeping_threads(), 0);
}

#[test]
fn vehicle_functions_are_refused_without_a_host() {
    let mut rig = Rig::new();
    rig.exec("world.spawn({ id = 'car' })").unwrap();
    let d = rig.exec_err("physics.drive(world.get('car'), { throttle = 1 })");
    assert_eq!(d.code, codes::SERVICE_UNAVAILABLE);
    assert!(d.message.contains("physics.drive"), "{}", d.message);
    let d = rig.exec_err("return physics.vehicle(world.get('car'))");
    assert_eq!(d.code, codes::SERVICE_UNAVAILABLE);
    assert!(d.message.contains("physics.vehicle"), "{}", d.message);
}
