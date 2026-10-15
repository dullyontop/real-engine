//! The `server` and `client` contexts the compatibility layer drives: their
//! sandbox, and the park / wake protocol of yielding host calls.

use std::cell::Cell;
use std::rc::Rc;

use mlua::{Lua, MultiValue, Value};
use re_scene::World;
use re_script::sched::{self, WakeMode, YieldToken};
use re_script::{Context, ContextKind, NoServices, RuntimeConfig, WorldHost};

struct Rig {
    world: World,
    services: NoServices,
    ctx: Context,
    _dir: tempfile::TempDir,
}

/// The token of the last `hold()` call.
#[derive(Default)]
struct Held(Cell<Option<u64>>);

/// A compat context with a yielding global `hold(timeout?)` that parks its
/// caller until the test wakes it.
fn rig(kind: ContextKind) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let install = |lua: &Lua| -> mlua::Result<()> {
        lua.set_app_data(Rc::new(Held::default()));
        let hold = lua.create_function(|lua, timeout: Option<f64>| {
            let (values, token) = sched::park(
                lua,
                "hold()",
                timeout.map(|t| (t, MultiValue::from_iter([Value::Boolean(false)]))),
            )?;
            let held = lua.app_data_ref::<Rc<Held>>().unwrap().clone();
            held.0.set(Some(token.0));
            Ok(values)
        })?;
        let hold = sched::yielding(lua, "hold", hold)?;
        lua.globals().raw_set("hold", hold)
    };
    let ctx = Context::compat(&RuntimeConfig::new(dir.path()), kind, &install).unwrap();
    Rig {
        world: World::new(),
        services: NoServices::default(),
        ctx,
        _dir: dir,
    }
}

impl Rig {
    fn run(&mut self, f: impl FnOnce(&Lua) -> mlua::Result<()>) -> Vec<re_core::Diagnostic> {
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        self.ctx.run(&mut host, f)
    }

    fn script(&mut self, source: &str) {
        let src = source.to_string();
        let d = self.run(move |lua| {
            let f = lua.load(src).into_function()?;
            sched::spawn(lua, f, MultiValue::new(), "test")?;
            Ok(())
        });
        assert!(d.is_empty(), "{d:#?}");
    }

    fn held(&self) -> YieldToken {
        let held = self.ctx.lua().app_data_ref::<Rc<Held>>().unwrap().clone();
        YieldToken(held.0.get().expect("a hold() call"))
    }

    fn out(&self) -> Vec<String> {
        self.ctx.take_output().into_iter().map(|l| l.text).collect()
    }
}

#[test]
fn server_and_client_contexts_are_sandboxed_compat_contexts() {
    for kind in [ContextKind::Server, ContextKind::Client] {
        assert!(kind.is_compat());
        let mut r = rig(kind);
        assert_eq!(r.ctx.kind(), kind);
        r.script(
            r#"
            assert(loadstring == nil and getfenv == nil and setfenv == nil)
            assert(type(task.wait) == "function" and type(task.spawn) == "function")
            print(setmetatable({}, { __tostring = function() return "ok" end }))
            "#,
        );
        let out = r.ctx.take_output();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].context, kind);
        assert_eq!(out[0].text, "ok");
    }
    assert_eq!(ContextKind::Server.name(), "server");
    assert_eq!(ContextKind::Client.name(), "client");
    assert!(!ContextKind::Game.is_compat());
}

#[test]
fn parked_threads_resume_with_the_wake_values() {
    for mode in [WakeMode::Deferred, WakeMode::Immediate] {
        let mut r = rig(ContextKind::Server);
        r.script(r#"print("got", hold())"#);
        assert_eq!(r.ctx.parked_threads(), 1);
        assert!(r.out().is_empty());
        let token = r.held();
        let d = r.run(|lua| {
            assert!(sched::is_parked(lua, token)?);
            let woke = sched::wake(lua, token, MultiValue::from_iter([Value::Integer(7)]), mode)?;
            assert!(woke);
            // A second wake finds nothing.
            assert!(!sched::wake(lua, token, MultiValue::new(), mode)?);
            Ok(())
        });
        assert!(d.is_empty(), "{d:#?}");
        assert_eq!(r.out(), ["got\t7"], "{mode:?}");
        assert_eq!(r.ctx.parked_threads(), 0);
    }
}

#[test]
fn a_park_times_out_with_its_timeout_values() {
    let mut r = rig(ContextKind::Server);
    r.script(r#"print("timed out", hold(1))"#);
    let d = r.run(|lua| {
        sched::advance(lua, 0.5)?;
        sched::resume_due(lua).map(|_| ())
    });
    assert!(d.is_empty(), "{d:#?}");
    assert!(r.out().is_empty());
    let d = r.run(|lua| {
        sched::advance(lua, 0.6)?;
        sched::resume_due(lua).map(|_| ())
    });
    assert!(d.is_empty(), "{d:#?}");
    assert_eq!(r.out(), ["timed out\tfalse"]);
}

#[test]
fn a_cancelled_park_never_resumes() {
    let mut r = rig(ContextKind::Server);
    r.script(r#"hold() print("resumed")"#);
    let token = r.held();
    let d = r.run(|lua| {
        sched::cancel_park(lua, token)?;
        assert!(!sched::wake(
            lua,
            token,
            MultiValue::new(),
            WakeMode::Immediate
        )?);
        Ok(())
    });
    assert!(d.is_empty(), "{d:#?}");
    assert!(r.out().is_empty());
    assert_eq!(r.ctx.parked_threads(), 0);
}

#[test]
fn yielding_outside_a_script_thread_is_refused() {
    let mut r = rig(ContextKind::Server);
    let d = r.run(|lua| {
        let hold: mlua::Function = lua.globals().get("hold")?;
        hold.call::<()>(())
    });
    assert_eq!(d.len(), 1, "{d:#?}");
    assert!(
        d[0].message.contains("RE3005") || d[0].code.to_string() == "RE3005",
        "{d:#?}"
    );
}

#[test]
fn native_entry_points_refuse_compat_contexts() {
    let dir = tempfile::tempdir().unwrap();
    let r = Context::compat(&RuntimeConfig::new(dir.path()), ContextKind::Game, &|_| {
        Ok(())
    });
    assert!(r.is_err());
}

#[test]
fn coroutines_may_wait_and_their_resumer_continues_at_once() {
    let mut r = rig(ContextKind::Server);
    r.script(
        r#"
        local co = coroutine.wrap(function()
            local dt = task.wait(1)
            print("waited", dt >= 1)
            print("held", hold())
        end)
        co()
        print("resumer goes on")
        "#,
    );
    assert_eq!(r.out(), ["resumer goes on"]);
    let d = r.run(|lua| {
        sched::advance(lua, 1.0)?;
        sched::resume_due(lua).map(|_| ())
    });
    assert!(d.is_empty(), "{d:#?}");
    assert_eq!(r.out(), ["waited\ttrue"]);
    let token = r.held();
    let d = r.run(|lua| {
        sched::wake(
            lua,
            token,
            MultiValue::from_iter([Value::Integer(3)]),
            WakeMode::Deferred,
        )?;
        Ok(())
    });
    assert!(d.is_empty(), "{d:#?}");
    assert_eq!(r.out(), ["held\t3"]);
}

#[test]
fn pcall_gives_errors_raised_by_rust_as_strings() {
    let mut r = rig(ContextKind::Server);
    r.script(
        r#"
        local ok, err = pcall(task.spawn, 42)
        print(ok, type(err), string.find(err, "task.spawn expects a function") ~= nil)
        print(xpcall(task.spawn, function(e) return type(e) end, 42))
        local ok2, err2 = pcall(error, { code = 1 })
        print(type(err2), err2.code)
        print(pcall(function() return 1, 2 end))
        print("across a yield", pcall(hold))
        "#,
    );
    assert_eq!(
        r.out(),
        [
            "false\tstring\ttrue",
            "false\tstring",
            "table\t1",
            "true\t1\t2"
        ]
    );
    let token = r.held();
    let d = r.run(|lua| {
        sched::wake(
            lua,
            token,
            MultiValue::from_iter([Value::Integer(5)]),
            WakeMode::Immediate,
        )?;
        Ok(())
    });
    assert!(d.is_empty(), "{d:#?}");
    assert_eq!(r.out(), ["across a yield\ttrue\t5"]);
}
