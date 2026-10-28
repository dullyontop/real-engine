//! What the runtime reaches while scripts run: the [`World`], the
//! [`HostServices`] of the running game, and (in the `tools` context) the command
//! registry.
//!
//! Every [`ScriptRuntime`](crate::ScriptRuntime) entry point takes a
//! `&mut dyn ScriptHost` and holds it only for the duration of that call. Engine
//! functions called from Luau borrow it briefly through `with_host` and never
//! call back into Luau while they hold it.

use std::cell::Cell;
use std::ptr::NonNull;

use mlua::Lua;
use re_command::{CommandOutcome, CommandRegistry, EditTarget, Origin, PolicyCheck};
use re_scene::{StreamCell, World};
use re_types::anim::AnimValue;
use re_types::audio::{AudioCmd, BusLevel};
use re_types::host::{HostError, HostResult, HostServices};
use re_types::input::ActionFrame;
use re_types::physics::{
    CharacterState, GroupMask, RagdollMode, RagdollState, RayHit, VehicleInput, VehicleState,
};
use re_types::{EntityId, glam::Vec3};
use serde_json::Value as Json;

use crate::codes;

/// What a script context needs from the engine while it runs.
///
/// `re_app` implements it for its `App`. [`WorldHost`] and [`CommandHost`] are
/// ready-made implementations for tools, tests and builder runs.
pub trait ScriptHost {
    /// The world scripts read and write.
    fn world(&mut self) -> &mut World;

    /// The runtime services (physics queries, input, audio, HUD, regeneration).
    fn services(&mut self) -> &mut dyn HostServices;

    /// Dispatches a command for a `tools` script, with `origin`
    /// ([`Origin::Script`] and the script's path). `None` means this host has no
    /// command registry; scripts then get an error naming the command.
    fn run_command(
        &mut self,
        name: &str,
        args: Json,
        dry_run: bool,
        origin: Origin,
    ) -> Option<CommandOutcome> {
        let _ = (name, args, dry_run, origin);
        None
    }

    /// World streaming (ADR-0042), for the `stream` library; `None` when
    /// this host does not stream (builder runs, tools without a game).
    fn stream(&mut self) -> Option<&'static dyn StreamAccess> {
        None
    }
}

/// World streaming as scripts see it: `re_app` implements it over
/// `re_proc::stream`, whose state lives in the world's resources.
pub trait StreamAccess {
    /// The streaming tick that last ran, with the cells it unloaded and
    /// loaded (each in key order); `None` before the first.
    fn events(&self, world: &World) -> Option<(u64, Vec<StreamCell>, Vec<StreamCell>)>;

    /// The loaded cells, in key order.
    fn cells(&self, world: &World) -> Vec<StreamCell>;

    /// True if cell `key` is loaded.
    fn is_loaded(&self, world: &World, key: &str) -> bool;

    /// Keeps cell `key` loaded whatever the focuses (`on`), or releases it.
    fn pin(&self, world: &mut World, key: &str, on: bool);
}

/// A host made of a world and a set of services, without commands.
pub struct WorldHost<'a> {
    /// The world.
    pub world: &'a mut World,
    /// The services.
    pub services: &'a mut dyn HostServices,
}

impl ScriptHost for WorldHost<'_> {
    fn world(&mut self) -> &mut World {
        self.world
    }
    fn services(&mut self) -> &mut dyn HostServices {
        self.services
    }
}

/// A host whose world is an [`EditTarget`]'s, with commands dispatched through a
/// [`CommandRegistry`] under a [`PolicyCheck`].
pub struct CommandHost<'a, C: EditTarget> {
    /// The target commands edit; its world is the scripts' world.
    pub target: &'a mut C,
    /// The command registry.
    pub registry: &'a CommandRegistry<C>,
    /// The policy every dispatch is checked against.
    pub policy: &'a dyn PolicyCheck,
    /// The services.
    pub services: &'a mut dyn HostServices,
}

impl<C: EditTarget> ScriptHost for CommandHost<'_, C> {
    fn world(&mut self) -> &mut World {
        self.target.world()
    }
    fn services(&mut self) -> &mut dyn HostServices {
        self.services
    }
    fn run_command(
        &mut self,
        name: &str,
        args: Json,
        dry_run: bool,
        origin: Origin,
    ) -> Option<CommandOutcome> {
        Some(
            self.registry
                .dispatch_as(self.target, name, args, dry_run, self.policy, origin),
        )
    }
}

/// [`HostServices`] for contexts with no running game (builder scripts, tests,
/// tools run from the CLI): queries find nothing, input is idle, and every action
/// fails with `RE3015`.
#[derive(Debug, Default)]
pub struct NoServices {
    actions: ActionFrame,
}

impl NoServices {
    fn unavailable<T>(what: &str) -> HostResult<T> {
        Err(HostError::new(
            codes::SERVICE_UNAVAILABLE_STR,
            format!("{what} is not available here: no game is running"),
        ))
    }
}

impl HostServices for NoServices {
    fn raycast(&self, _: Vec3, _: Vec3, _: f32, _: GroupMask) -> Option<RayHit> {
        None
    }
    fn apply_impulse(&mut self, _: EntityId, _: Vec3) -> HostResult<()> {
        Self::unavailable("physics.impulse")
    }
    fn character_move(&mut self, _: EntityId, _: Vec3, _: bool) -> HostResult<()> {
        Self::unavailable("physics.move_character")
    }
    fn character_state(&self, _: EntityId) -> HostResult<CharacterState> {
        Self::unavailable("physics.character")
    }
    fn body_velocity(&self, _: EntityId) -> HostResult<(Vec3, Vec3)> {
        Self::unavailable("physics.velocity")
    }
    fn set_body_velocity(&mut self, _: EntityId, _: Vec3, _: Vec3) -> HostResult<()> {
        Self::unavailable("physics.set_velocity")
    }
    fn vehicle_drive(&mut self, _: EntityId, _: VehicleInput) -> HostResult<()> {
        Self::unavailable("physics.drive")
    }
    fn vehicle_state(&self, _: EntityId) -> HostResult<VehicleState> {
        Self::unavailable("physics.vehicle")
    }
    fn ragdoll_mode(&mut self, _: EntityId, _: RagdollMode) -> HostResult<()> {
        Self::unavailable("physics.ragdoll")
    }
    fn ragdoll_state(&self, _: EntityId) -> HostResult<RagdollState> {
        Self::unavailable("physics.ragdoll_state")
    }
    fn ragdoll_impulse(&mut self, _: EntityId, _: &str, _: Vec3) -> HostResult<()> {
        Self::unavailable("physics.ragdoll_impulse")
    }
    fn play_sound(&mut self, _: &str, _: Option<Vec3>, _: f32) -> HostResult<()> {
        Self::unavailable("audio.play")
    }
    fn audio_mix(&mut self, cmd: AudioCmd) -> HostResult<()> {
        Self::unavailable(match cmd {
            AudioCmd::SetBusVolume { .. } => "audio.set_bus_volume",
            AudioCmd::SetEffectParam { .. } => "audio.set_effect",
            AudioCmd::ApplySnapshot { .. } => "audio.snapshot",
            _ => "audio.play",
        })
    }
    fn audio_level(&self, _: &str) -> HostResult<BusLevel> {
        Self::unavailable("audio.levels")
    }
    fn actions(&self) -> &ActionFrame {
        &self.actions
    }
    fn set_hud_text(&mut self, _: EntityId, _: &str) -> HostResult<()> {
        Self::unavailable("ui.set_text")
    }
    fn request_screenshot(&mut self, _: &str) -> HostResult<()> {
        Self::unavailable("screenshot")
    }
    fn regenerate(&mut self, _: &str, _: i64) -> HostResult<()> {
        Self::unavailable("proc.regenerate")
    }
    fn request_restart(&mut self) {}
    fn set_time_scale(&mut self, _: f64) -> HostResult<()> {
        Self::unavailable("time.set_scale")
    }
    fn time_scale(&self) -> f64 {
        1.0
    }
    fn anim_set(&mut self, _: EntityId, _: &str, _: AnimValue) -> HostResult<()> {
        Self::unavailable("anim.set")
    }
    fn anim_get(&self, _: EntityId, _: &str) -> HostResult<AnimValue> {
        Self::unavailable("anim.get")
    }
    fn anim_trigger(&mut self, _: EntityId, _: &str) -> HostResult<()> {
        Self::unavailable("anim.trigger")
    }
    fn anim_state(&self, _: EntityId, _: Option<&str>) -> HostResult<String> {
        Self::unavailable("anim.state")
    }
    fn anim_joint(&self, _: EntityId, _: &str) -> HostResult<(Vec3, re_types::glam::Quat)> {
        Self::unavailable("anim.joint")
    }
}

/// The host bound to a VM for the duration of one entry point.
#[derive(Default)]
pub(crate) struct HostSlot {
    host: Cell<Option<NonNull<dyn ScriptHost + 'static>>>,
    busy: Cell<bool>,
}

/// Binds `host` to `lua` while `f` runs, so that engine functions can reach it.
#[expect(
    unsafe_code,
    reason = "the bound host is kept as a pointer with its lifetime erased"
)]
pub(crate) fn bind<R>(lua: &Lua, host: &mut dyn ScriptHost, f: impl FnOnce() -> R) -> R {
    struct Unbind<'a>(&'a Lua, Option<NonNull<dyn ScriptHost + 'static>>);
    impl Drop for Unbind<'_> {
        fn drop(&mut self) {
            if let Some(slot) = self.0.app_data_ref::<HostSlot>() {
                slot.host.set(self.1);
            }
        }
    }
    // SAFETY: the pointer is erased to `'static` only so that it can be stored in
    // the VM's app data. It is reachable only while `f` runs: the guard restores
    // the previous binding (normally none) on return and on unwind, and
    // `with_host` refuses re-entrant use, so no two `&mut` exist at once.
    let erased: NonNull<dyn ScriptHost + 'static> = unsafe {
        std::mem::transmute::<NonNull<dyn ScriptHost + '_>, NonNull<dyn ScriptHost + 'static>>(
            NonNull::from(host),
        )
    };
    let previous = {
        let slot = lua
            .app_data_ref::<HostSlot>()
            .expect("every context VM has a HostSlot");
        slot.host.replace(Some(erased))
    };
    let _unbind = Unbind(lua, previous);
    f()
}

/// Runs `f` with the bound host. Fails when no host is bound (a script called an
/// engine function outside an entry point, which cannot happen through the public
/// API) or when the host is already borrowed further up the stack.
#[expect(
    unsafe_code,
    reason = "the bound host is reached through the pointer `bind` stored"
)]
pub fn with_host<R>(
    lua: &Lua,
    f: impl FnOnce(&mut dyn ScriptHost) -> mlua::Result<R>,
) -> mlua::Result<R> {
    let slot = lua
        .app_data_ref::<HostSlot>()
        .ok_or_else(|| mlua::Error::runtime("this VM has no host slot"))?;
    let Some(mut ptr) = slot.host.get() else {
        return Err(mlua::Error::runtime(format!(
            "[{}] engine functions can only be called while the engine runs scripts",
            codes::SERVICE_UNAVAILABLE
        )));
    };
    if slot.busy.replace(true) {
        return Err(mlua::Error::runtime(
            "re-entrant engine call (an engine function called back into itself)",
        ));
    }
    struct Release<'a>(&'a Cell<bool>);
    impl Drop for Release<'_> {
        fn drop(&mut self) {
            self.0.set(false);
        }
    }
    let _release = Release(&slot.busy);
    // SAFETY: see `bind`; the pointer is live while bound, and `busy` makes this
    // the only `&mut` to it.
    let host = unsafe { ptr.as_mut() };
    f(host)
}
