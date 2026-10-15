//! A compat place with a server or client VM and a scriptable host.

#![allow(dead_code)]

use std::sync::Arc;

use re_compat::data::ClassFile;
use re_compat::humanoid::body::CharacterPhysics;
use re_compat::scene::create_place;
use re_compat::{Bindings, CompatSetup, CompatVm, SignalBehavior};
use re_core::TaskPool;
use re_physics::{Physics, PhysicsConfig};
use re_scene::{Entity, World};
use re_script::host::{NoServices, WorldHost};
use re_script::{ContextKind, RuntimeConfig};
use re_types::glam::Vec3;
use re_types::input::{MouseLock, RawInputFrame};
use re_types::physics::{BodyDesc, BodyKind, PhysicsCmd, ShapeDesc};
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
    fn set_mouse_behavior(&mut self, lock: MouseLock, icon: bool) -> HostResult<()> {
        self.mouse = Some((lock, icon));
        Ok(())
    }
}

pub struct Place {
    pub setup: Arc<CompatSetup>,
    pub game: Entity,
    pub world: World,
    pub services: Services,
    pub vm: CompatVm,
    _dir: tempfile::TempDir,
}

pub fn place(kind: ContextKind) -> Place {
    build(kind, false)
}

/// Stand-in part classes until the part classes land (WP13).
const PARTS: &str = include_str!("../fixtures/character/parts.json");

/// A place with part classes and a physics world for characters: a floor
/// whose top is at y = 0, and gravity of 196.2 studs/s².
pub fn character_place(kind: ContextKind) -> Place {
    build(kind, true)
}

fn build(kind: ContextKind, characters: bool) -> Place {
    let setup: Arc<CompatSetup> = if characters {
        let mut b = Bindings::standard().unwrap();
        b.classes(|c| {
            c.classes(
                "tests/fixtures/character/parts.json",
                ClassFile::parse("parts.json", PARTS).unwrap(),
            )
        });
        CompatSetup::new(b).unwrap()
    } else {
        CompatSetup::standard().unwrap()
    };
    let mut world = World::new();
    // Fixed instance ids, so two runs hash the same (replays).
    world.ecs.insert_resource(re_scene::IdGen::seeded(7));
    if characters {
        let config = PhysicsConfig {
            gravity: Vec3::new(0.0, -196.2, 0.0),
            ..PhysicsConfig::default()
        };
        let mut physics = Physics::new(config, TaskPool::global()).unwrap();
        let mut cmds = vec![PhysicsCmd::AddBody {
            id: EntityId(u64::MAX - 1),
            desc: BodyDesc {
                kind: BodyKind::Static,
                shape: ShapeDesc::Box {
                    half_extents: Vec3::new(500.0, 1.0, 500.0),
                },
                pos: Vec3::new(0.0, -1.0, 0.0),
                ..BodyDesc::default()
            },
        }];
        physics.apply(&mut cmds);
        world.ecs.insert_resource(CharacterPhysics(physics));
    }
    let game = create_place(
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
        setup,
        game,
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

    /// One frame of a character place: the Animator, the Humanoids before
    /// physics, the physics step, the Humanoids after it, then the script
    /// scheduler.
    pub fn frame(&mut self, dt: f64) {
        self.begin_frame();
        self.finish_frame(dt);
    }

    /// The start of a frame, before render-step bindings run.
    pub fn begin_frame(&mut self) {
        self.with(re_compat::anim::begin_frame);
    }

    /// The rest of a frame after [`Place::begin_frame`].
    pub fn finish_frame(&mut self, dt: f64) {
        self.with(|lua| {
            re_compat::anim::step(lua, dt)?;
            re_compat::humanoid::pre_simulation(lua, dt)
        });
        self.world
            .ecs
            .resource_mut::<CharacterPhysics>()
            .0
            .step(dt as f32)
            .unwrap();
        self.with(|lua| re_compat::humanoid::post_simulation(lua, dt));
        self.step(dt);
    }

    /// Runs `frames` frames of 1/60 s.
    pub fn frames(&mut self, frames: u32) {
        for _ in 0..frames {
            self.frame(1.0 / 60.0);
        }
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

impl Place {
    /// A child of `game` by name (a service).
    pub fn service(&self, name: &str) -> Entity {
        re_compat::instance::tree::find_child(&self.world, self.game, name, false).unwrap()
    }

    /// The instance at a path of names below `game`.
    pub fn find(&self, path: &[&str]) -> Entity {
        let mut e = self.game;
        for n in path {
            e = re_compat::instance::tree::find_child(&self.world, e, n, false)
                .unwrap_or_else(|| panic!("no {n} in {path:?}"));
        }
        e
    }

    /// A CFrame property.
    pub fn cframe(&self, e: Entity, prop: &str) -> re_compat::datatypes::CFrame {
        re_compat::anim::pose::cframe_prop(&self.world, &self.setup.classes, e, prop).unwrap()
    }

    /// Builds a character of `rig_type` named `name` in Workspace, standing on
    /// the floor at `x, z`.
    pub fn character(&mut self, rig_type: &str, name: &str, x: f32, z: f32) -> Entity {
        let classes = self.setup.classes.clone();
        let at = re_compat::datatypes::CFrame::from_position([x, 0.0, z]);
        let model =
            re_compat::humanoid::rig::build(&mut self.world, &classes, rig_type, name, None, at)
                .unwrap();
        let ws = self.service("Workspace");
        re_compat::instance::tree::set_parent(&mut self.world, model, Some(ws)).unwrap();
        model
    }
}
