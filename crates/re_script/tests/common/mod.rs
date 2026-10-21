//! A small rig around `ScriptRuntime` for the integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use re_core::{Diagnostic, Phase};
use re_scene::World;
use re_script::{ContextKind, NoServices, RuntimeConfig, ScriptRuntime, WorldHost};
use re_types::EntityId;
use re_types::audio::{AudioCmd, BusLevel};
use re_types::glam::Vec3;
use re_types::host::{HostResult, HostServices};
use re_types::input::{ActionFrame, ButtonState};
use re_types::physics::{CharacterState, GroupMask, RayHit, VehicleInput, VehicleState};

pub fn project() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/project")
}

/// Services that record what scripts asked for.
#[derive(Default)]
pub struct Services {
    pub actions: ActionFrame,
    pub sounds: Vec<String>,
    /// Mixer commands (`audio_mix`).
    pub mix: Vec<AudioCmd>,
    pub impulses: Vec<(EntityId, Vec3)>,
    pub restarts: usize,
    none: NoServices,
}

impl Services {
    pub fn press(&mut self, action: &str) {
        self.actions.buttons.insert(
            action.to_string(),
            ButtonState {
                down: true,
                pressed: true,
                released: false,
            },
        );
    }
    pub fn clear_input(&mut self) {
        self.actions.buttons.clear();
    }
}

impl HostServices for Services {
    fn raycast(&self, _: Vec3, _: Vec3, _: f32, _: GroupMask) -> Option<RayHit> {
        None
    }
    fn apply_impulse(&mut self, e: EntityId, v: Vec3) -> HostResult<()> {
        self.impulses.push((e, v));
        Ok(())
    }
    fn character_move(&mut self, e: EntityId, v: Vec3, j: bool) -> HostResult<()> {
        self.none.character_move(e, v, j)
    }
    fn character_state(&self, e: EntityId) -> HostResult<CharacterState> {
        self.none.character_state(e)
    }
    fn body_velocity(&self, e: EntityId) -> HostResult<(Vec3, Vec3)> {
        self.none.body_velocity(e)
    }
    fn vehicle_drive(&mut self, e: EntityId, i: VehicleInput) -> HostResult<()> {
        self.none.vehicle_drive(e, i)
    }
    fn vehicle_state(&self, e: EntityId) -> HostResult<VehicleState> {
        self.none.vehicle_state(e)
    }
    fn play_sound(&mut self, clip: &str, _: Option<Vec3>, _: f32) -> HostResult<()> {
        self.sounds.push(clip.to_string());
        Ok(())
    }
    fn audio_mix(&mut self, cmd: AudioCmd) -> HostResult<()> {
        self.mix.push(cmd);
        Ok(())
    }
    /// `sfx` measures -12 dB RMS and -6 dB peak; other names are unknown.
    fn audio_level(&self, bus: &str) -> HostResult<BusLevel> {
        if bus == "sfx" {
            Ok(BusLevel {
                bus: bus.into(),
                peak_db: -6.0,
                rms_db: -12.0,
            })
        } else {
            self.none.audio_level(bus)
        }
    }
    fn actions(&self) -> &ActionFrame {
        &self.actions
    }
    fn set_hud_text(&mut self, _: EntityId, _: &str) -> HostResult<()> {
        Ok(())
    }
    fn request_screenshot(&mut self, p: &str) -> HostResult<()> {
        self.none.request_screenshot(p)
    }
    fn regenerate(&mut self, s: &str, seed: i64) -> HostResult<()> {
        self.none.regenerate(s, seed)
    }
    fn request_restart(&mut self) {
        self.restarts += 1;
    }
}

pub struct Rig {
    pub rt: ScriptRuntime,
    pub world: World,
    pub services: Services,
}

pub const STEP: f64 = 1.0 / 60.0;

impl Rig {
    pub fn new() -> Rig {
        Rig::with_config(RuntimeConfig::new(project()))
    }

    pub fn at(root: &Path) -> Rig {
        Rig::with_config(RuntimeConfig::new(root))
    }

    pub fn with_config(config: RuntimeConfig) -> Rig {
        Rig {
            rt: ScriptRuntime::new(config).expect("the runtime starts"),
            world: World::new(),
            services: Services::default(),
        }
    }

    pub fn budget(mut config: RuntimeConfig, ms: u64) -> RuntimeConfig {
        config.cpu_budget = Duration::from_millis(ms);
        config
    }

    /// Runs `source` in the game context and returns its first result as JSON.
    pub fn exec(&mut self, source: &str) -> Result<serde_json::Value, Vec<Diagnostic>> {
        self.exec_in(ContextKind::Game, source)
    }

    pub fn exec_in(
        &mut self,
        kind: ContextKind,
        source: &str,
    ) -> Result<serde_json::Value, Vec<Diagnostic>> {
        let Rig {
            rt,
            world,
            services,
        } = self;
        let mut host = WorldHost { world, services };
        rt.exec(&mut host, kind, "probe", source)
    }

    /// The single error `source` must raise.
    pub fn exec_err(&mut self, source: &str) -> Diagnostic {
        let mut d = self.exec(source).expect_err("the snippet must fail");
        assert_eq!(d.len(), 1, "{d:#?}");
        let d = d.remove(0);
        println!("{}: {}", d.code, d.message);
        d
    }

    pub fn run(&mut self, path: &str) -> Vec<Diagnostic> {
        let Rig {
            rt,
            world,
            services,
        } = self;
        let mut host = WorldHost { world, services };
        rt.run_script(&mut host, path, ContextKind::Game, None)
    }

    pub fn phase(&mut self, phase: Phase, dt: f64) -> Vec<Diagnostic> {
        let Rig {
            rt,
            world,
            services,
        } = self;
        let mut host = WorldHost { world, services };
        rt.run_phase(&mut host, phase, dt)
    }

    /// One frame with one fixed step: PreRender, PreSimulation, PostSimulation.
    pub fn step(&mut self, dt: f64) -> Vec<Diagnostic> {
        let mut d = self.phase(Phase::PreRender, dt);
        d.extend(self.phase(Phase::PreSimulation, dt));
        d.extend(self.phase(Phase::PostSimulation, dt));
        d
    }

    /// Steps until `seconds` of simulation time passed; panics on any error.
    pub fn run_for(&mut self, seconds: f64, dt: f64) {
        let mut t = 0.0;
        while t < seconds - 1e-9 {
            let d = self.step(dt);
            assert!(d.is_empty(), "{d:#?}");
            t += dt;
        }
    }

    pub fn output(&mut self) -> Vec<String> {
        self.rt.take_output().into_iter().map(|l| l.text).collect()
    }
}
