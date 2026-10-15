//! Physics for parts (WP13): parts, assemblies, joints, constraints and
//! movers, collision groups, touches and the workspace's spatial queries,
//! simulated by `re_physics`.
//!
//! The simulation of a world lives in its [`CompatPhysics`] resource
//! ([`sim`]). The runtime drives it once per frame:
//!
//! * [`step`] brings the simulation up to date with the world (parts scripts
//!   created, moved, changed or destroyed), steps it and writes the moved
//!   parts back;
//! * [`dispatch`] then fires `Touched` and `TouchEnded` and destroys the parts
//!   that fell below `Workspace.FallenPartsDestroyHeight`.
//!
//! Scripts reach it through the members [`register`] binds: the `BasePart`
//! properties and methods, `PVInstance:PivotTo`, the `Model` helpers,
//! attachments, joints and constraints, `PhysicsService` and the `WorldRoot`
//! queries.
//!
//! Other work packages use [`with_physics`] (the character controller,
//! which marks the parts it moves [`CharacterDriven`]), [`group_bits`] and
//! [`StaticCells`] (the renderer's merged static meshes, ADR-0031).

mod constraints;
mod layout;
mod parts;
mod pose;
mod query;
mod service;
mod sim;

use mlua::Lua;
use re_core::Diagnostic;
use re_physics::Physics;
use re_scene::{Entity, World};

pub use pose::{cframe_value, pose_of, world_pose};
pub use sim::{CELL_SIZE, Cell, CellId, CharacterDriven, CompatPhysics, MAX_GROUPS, StaticCells};

use crate::class::{Accessor, ClassRegistry};
use crate::error::CompatError;
use crate::runtime::{Bindings, with_world};
use sim::Sim;

/// The steps each frame is split into: four keep a free fall within half a
/// stud of the exact distance after a second.
pub const SUBSTEPS: u32 = 4;

/// The longest frame simulated in one call, in seconds (a longer one runs
/// slowly rather than unstably).
pub const MAX_FRAME: f32 = 0.1;

/// Registers the physics members: accessors and methods.
pub fn register(b: &mut Bindings) {
    for (class, prop, a) in accessors() {
        b.accessor(class, prop, a);
    }
    parts::register_methods(b);
    query::register_methods(b);
    service::register_methods(b);
}

/// The accessors of the physics classes' builtin properties, as
/// `(class, property, accessor)`, for registries built outside [`Bindings`].
pub fn accessors() -> Vec<(&'static str, &'static str, Accessor)> {
    let mut v = parts::accessors();
    v.extend(constraints::accessors());
    v.extend(service::accessors());
    v
}

/// Gives the workspace of a compat world its `Terrain` and `Camera` when it
/// lacks them. The runtime calls it when it creates a VM; [`step`] calls it
/// too.
pub fn prepare_world(world: &mut World, classes: &ClassRegistry) -> Result<(), CompatError> {
    service::prepare_world(world, classes)?;
    sim::prepare(world);
    Ok(())
}

/// Runs `f` on the world's simulation, creating it on first use. `None` when
/// the world is not a compat world with a workspace.
pub(crate) fn with_sim<R>(
    world: &mut World,
    f: impl FnOnce(&mut Sim, &mut World) -> R,
) -> Option<R> {
    let mut res = match world.ecs.remove_resource::<CompatPhysics>() {
        Some(r) => r,
        None => CompatPhysics(std::sync::Mutex::new(Sim::new(world)?)),
    };
    let out = {
        let sim = res.0.get_mut().unwrap_or_else(|e| e.into_inner());
        f(sim, world)
    };
    world.ecs.insert_resource(res);
    Some(out)
}

/// Reads the world's simulation without changing it (`None` before the first
/// sync).
pub(crate) fn read_sim<R>(world: &World, f: impl FnOnce(&Sim) -> R) -> Option<R> {
    let res = world.ecs.get_resource::<CompatPhysics>()?;
    let sim = res.0.lock().unwrap_or_else(|e| e.into_inner());
    Some(f(&sim))
}

/// Advances the simulation of `world` by `dt` seconds: syncs it with the
/// world, runs [`SUBSTEPS`] steps (with the movers' forces) and writes the
/// moved parts back. Returns the simulation's diagnostics.
pub fn step(world: &mut World, dt: f32) -> Vec<Diagnostic> {
    let dt = if dt.is_finite() {
        dt.clamp(0.0, MAX_FRAME)
    } else {
        0.0
    };
    with_sim(world, |sim, w| {
        sim.sync(w, true);
        let movers = constraints::movers(sim, w);
        let mut out = Vec::new();
        if dt > 0.0 {
            let h = dt / SUBSTEPS as f32;
            for _ in 0..SUBSTEPS {
                constraints::apply_movers(sim, &movers, h);
                if let Err(d) = sim.physics.step(h) {
                    out.push(d);
                    break;
                }
            }
        }
        sim.write_back(w);
        sim.publish_cells(w);
        out.extend(sim.physics.take_diagnostics());
        out
    })
    .unwrap_or_default()
}

/// Fires `Touched` and `TouchEnded` for the touches of the last [`step`] and
/// destroys the parts that fell below `Workspace.FallenPartsDestroyHeight`.
/// Call it with the VM bound, after [`step`].
pub fn dispatch(lua: &Lua) -> mlua::Result<()> {
    let (touches, fallen) = with_world(lua, |w| {
        with_sim(w, |sim, _| {
            (
                std::mem::take(&mut sim.touches),
                std::mem::take(&mut sim.fallen),
            )
        })
        .unwrap_or_default()
    })?;
    for t in touches {
        let event = if t.began { "Touched" } else { "TouchEnded" };
        let (a, b) = (
            crate::instance::push(lua, t.a)?,
            crate::instance::push(lua, t.b)?,
        );
        crate::signal::fire(lua, t.a, event, mlua::MultiValue::from_vec(vec![b]))?;
        crate::signal::fire(lua, t.b, event, mlua::MultiValue::from_vec(vec![a]))?;
    }
    for e in fallen {
        crate::instance::destroy(lua, e)?;
    }
    Ok(())
}

/// Runs `f` on the world's `re_physics` world (made on first use), for the
/// work packages that add their own bodies (the character controller).
pub fn with_physics<R>(world: &mut World, f: impl FnOnce(&mut Physics) -> R) -> Option<R> {
    with_sim(world, |sim, _| f(&mut sim.physics))
}

/// The groups collision group `name` collides with, one bit per group id
/// (an unknown name is `Default`).
pub fn group_bits(world: &mut World, name: &str) -> u32 {
    with_sim(world, |sim, _| sim.group_bits(name)).unwrap_or(u32::MAX)
}

/// The id of collision group `name` (0, `Default`, for an unknown name).
pub fn group_id(world: &mut World, name: &str) -> u8 {
    with_sim(world, |sim, _| sim.groups.id(name)).unwrap_or(0)
}

/// The assembly key `re_physics` gives the part `e` (`None` when it is not
/// simulated).
pub fn assembly_of(world: &World, e: Entity) -> Option<re_types::EntityId> {
    read_sim(world, |sim| sim.assembly_key(e)).flatten()
}
