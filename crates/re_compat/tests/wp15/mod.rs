#![allow(dead_code)]
//! The test place shared by the WP15 tests: a client VM over a place with the
//! `TweenTarget` test class. The GUI frames a place steps are compared with
//! committed values when it is dropped ([`gui_log`]).

mod gui_log;

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
    pub game: re_scene::Entity,
    pub services: NoServices,
    pub vm: CompatVm,
    gui_log: gui_log::GuiLog,
    _dir: tempfile::TempDir,
}

impl Drop for Place {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.gui_log.check();
        }
    }
}

pub fn place() -> Place {
    place_with(SignalBehavior::Deferred)
}

/// The test place with handlers run as `behavior` says.
pub fn place_with(behavior: SignalBehavior) -> Place {
    place_in(behavior, &["Workspace", "ReplicatedStorage"])
}

/// The test place with `services` created.
pub fn place_in(behavior: SignalBehavior, services: &[&str]) -> Place {
    let mut b = Bindings::standard().unwrap();
    b.classes(|c| {
        c.classes(
            "tests/fixtures/gui/tween_target.json",
            ClassFile::parse("tween_target.json", TWEEN_TARGET).unwrap(),
        )
    });
    let setup: Arc<CompatSetup> = CompatSetup::new(b).unwrap();
    let mut world = World::new();
    let game = create_place(&mut world, &setup.classes, services).unwrap();
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
        game,
        services: NoServices::default(),
        vm,
        gui_log: gui_log::GuiLog::default(),
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

    /// One UI frame inside `ctx`'s current pass, then the deferred handlers.
    pub fn gui(
        &mut self,
        ctx: &egui::Context,
        frame: &re_compat::gui::GuiFrame,
    ) -> re_compat::gui::GuiOutput {
        let vm = &self.vm;
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let mut out = None;
        let d = vm.run(&mut host, |lua| {
            out = Some(re_compat::gui::step_recorded(lua, ctx, frame)?);
            Ok(())
        });
        assert!(d.is_empty(), "{d:#?}");
        let (out, record) = out.expect("the step ran");
        self.gui_log.frame(&self.world, &out, &record);
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = vm.step(&mut host, 0.0);
        assert!(d.is_empty(), "{d:#?}");
        out
    }

    /// One physics step of `dt` (static cells, moved parts), then the
    /// deferred handlers.
    pub fn physics(&mut self, dt: f64) {
        let vm = &self.vm;
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = vm.physics(&mut host, dt);
        assert!(d.is_empty(), "{d:#?}");
        let d = vm.step(&mut host, 0.0);
        assert!(d.is_empty(), "{d:#?}");
    }

    /// One frame of the 3D view.
    pub fn render(
        &mut self,
        input: &re_compat::render::RenderInput,
    ) -> re_compat::render::RenderFrame {
        let input = *input;
        self.lua(|lua| re_compat::render::build(lua, &input))
            .unwrap_or_else(|e| panic!("{e}"))
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
