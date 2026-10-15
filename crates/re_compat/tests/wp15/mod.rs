#![allow(dead_code)]
//! The test place shared by the WP15 tests: a client VM over a place with the
//! `TweenTarget` test class.

use std::sync::Arc;

use re_compat::data::ClassFile;
use re_compat::scene::create_place;
use re_compat::{Bindings, CompatSetup, CompatVm, SignalBehavior};
use re_scene::World;
use re_script::host::{NoServices, WorldHost};
use re_script::{ContextKind, RuntimeConfig};

const TWEEN_TARGET: &str = include_str!("../fixtures/gui/tween_target.json");

pub struct Place {
    pub world: World,
    pub services: NoServices,
    pub vm: CompatVm,
    _dir: tempfile::TempDir,
}

pub fn place() -> Place {
    place_with(SignalBehavior::Deferred)
}

/// The test place with handlers run as `behavior` says.
pub fn place_with(behavior: SignalBehavior) -> Place {
    let mut b = Bindings::standard().unwrap();
    b.classes(|c| {
        c.classes(
            "tests/fixtures/gui/tween_target.json",
            ClassFile::parse("tween_target.json", TWEEN_TARGET).unwrap(),
        )
    });
    let setup: Arc<CompatSetup> = CompatSetup::new(b).unwrap();
    let mut world = World::new();
    create_place(
        &mut world,
        &setup.classes,
        &["Workspace", "ReplicatedStorage"],
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let vm = CompatVm::new(
        &setup,
        ContextKind::Client,
        &mut world,
        &RuntimeConfig::new(dir.path()),
        behavior,
    )
    .unwrap();
    Place {
        world,
        services: NoServices::default(),
        vm,
        _dir: dir,
    }
}

impl Place {
    /// Runs a chunk that gets the test's state table `S` as `...`.
    pub fn script(&mut self, source: &str) -> Vec<re_core::Diagnostic> {
        let vm = &self.vm;
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        vm.run(&mut host, |lua| {
            let state = match lua.named_registry_value::<mlua::Value>("wp15.state")? {
                mlua::Value::Table(t) => t,
                _ => {
                    let t = lua.create_table()?;
                    lua.set_named_registry_value("wp15.state", &t)?;
                    t
                }
            };
            lua.load(format!("local S = ...\n{source}"))
                .set_name("test")
                .call::<()>(state)
        })
    }

    /// Runs a script; fails the test on any error it raises.
    pub fn run(&mut self, source: &str) {
        let d = self.script(source);
        assert!(d.is_empty(), "{d:#?}");
    }

    /// Runs a script and returns the message of the error it raises.
    pub fn fails(&mut self, source: &str) -> String {
        let d = self.script(source);
        assert_eq!(d.len(), 1, "{d:#?}");
        format!("[{}] {}", d[0].code, d[0].message)
    }

    /// One frame: tweens, then sounds.
    pub fn frame(&mut self, dt: f64) {
        let vm = &self.vm;
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = vm.run(&mut host, |lua| {
            re_compat::tween::step(lua, dt)?;
            re_compat::sound::step(lua, dt)
        });
        assert!(d.is_empty(), "{d:#?}");
        let d = vm.step(&mut host, dt);
        assert!(d.is_empty(), "{d:#?}");
    }

    pub fn out(&mut self) -> Vec<String> {
        self.vm
            .take_output()
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

    pub fn lua<R>(&mut self, f: impl FnOnce(&mlua::Lua) -> R) -> R {
        let vm = &self.vm;
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let mut out = None;
        let d = vm.run(&mut host, |lua| {
            out = Some(f(lua));
            Ok(())
        });
        assert!(d.is_empty(), "{d:#?}");
        out.unwrap()
    }
}
