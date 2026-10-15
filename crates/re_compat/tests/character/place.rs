//! A compat place with a server or client VM and a scriptable host.

#![allow(dead_code)]

use std::sync::Arc;

use re_compat::scene::create_place;
use re_compat::{CompatSetup, CompatVm, SignalBehavior};
use re_scene::World;
use re_script::host::{NoServices, WorldHost};
use re_script::{ContextKind, RuntimeConfig};
use re_types::glam::Vec3;
use re_types::input::{MouseLock, RawInputFrame};
use re_types::{
    ActionFrame, CharacterState, EntityId, GroupMask, HostResult, HostServices, RayHit,
};

/// Host services with injectable raw input; everything else is
/// [`NoServices`].
#[derive(Default)]
pub struct Services {
    pub base: NoServices,
    pub raw: Option<RawInputFrame>,
    pub mouse: Option<(MouseLock, bool)>,
}

impl HostServices for Services {
    fn raycast(&self, o: Vec3, d: Vec3, m: f32, g: GroupMask) -> Option<RayHit> {
        self.base.raycast(o, d, m, g)
    }
    fn apply_impulse(&mut self, e: EntityId, i: Vec3) -> HostResult<()> {
        self.base.apply_impulse(e, i)
    }
    fn character_move(&mut self, e: EntityId, v: Vec3, j: bool) -> HostResult<()> {
        self.base.character_move(e, v, j)
    }
    fn character_state(&self, e: EntityId) -> HostResult<CharacterState> {
        self.base.character_state(e)
    }
    fn body_velocity(&self, e: EntityId) -> HostResult<(Vec3, Vec3)> {
        self.base.body_velocity(e)
    }
    fn play_sound(&mut self, c: &str, a: Option<Vec3>, v: f32) -> HostResult<()> {
        self.base.play_sound(c, a, v)
    }
    fn actions(&self) -> &ActionFrame {
        self.base.actions()
    }
    fn set_hud_text(&mut self, e: EntityId, t: &str) -> HostResult<()> {
        self.base.set_hud_text(e, t)
    }
    fn request_screenshot(&mut self, p: &str) -> HostResult<()> {
        self.base.request_screenshot(p)
    }
    fn regenerate(&mut self, s: &str, seed: i64) -> HostResult<()> {
        self.base.regenerate(s, seed)
    }
    fn request_restart(&mut self) {}
    fn raw_input(&self) -> Option<&RawInputFrame> {
        self.raw.as_ref()
    }
    fn set_mouse_mode(&mut self, lock: MouseLock, icon: bool) -> HostResult<()> {
        self.mouse = Some((lock, icon));
        Ok(())
    }
}

pub struct Place {
    pub world: World,
    pub services: Services,
    pub vm: CompatVm,
    _dir: tempfile::TempDir,
}

pub fn place(kind: ContextKind) -> Place {
    let setup: Arc<CompatSetup> = CompatSetup::standard().unwrap();
    let mut world = World::new();
    create_place(
        &mut world,
        &setup.classes,
        &["Workspace", "ReplicatedStorage", "StarterPlayer"],
    )
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let vm = CompatVm::new(
        &setup,
        kind,
        &mut world,
        &RuntimeConfig::new(dir.path()),
        SignalBehavior::Deferred,
    )
    .unwrap();
    Place {
        world,
        services: Services::default(),
        vm,
        _dir: dir,
    }
}

impl Place {
    /// Runs a script; fails the test on any error it raises.
    pub fn run(&mut self, source: &str) {
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = self.vm.exec(&mut host, "test", source);
        assert!(d.is_empty(), "{d:#?}");
    }

    /// Runs `f` with the host bound; fails the test on any error.
    pub fn with(&mut self, f: impl FnOnce(&mlua::Lua) -> mlua::Result<()>) {
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = self.vm.run(&mut host, f);
        assert!(d.is_empty(), "{d:#?}");
    }

    pub fn step(&mut self, dt: f64) {
        let mut host = WorldHost {
            world: &mut self.world,
            services: &mut self.services,
        };
        let d = self.vm.step(&mut host, dt);
        assert!(d.is_empty(), "{d:#?}");
    }

    /// The printed lines since the last call; warnings start with `W `.
    pub fn out(&mut self) -> Vec<String> {
        self.vm
            .take_output()
            .into_iter()
            .map(|l| {
                let t = l.text.replace('\t', " ");
                if l.warn { format!("W {t}") } else { t }
            })
            .collect()
    }
}
