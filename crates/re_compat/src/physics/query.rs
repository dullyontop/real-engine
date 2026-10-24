//! The `WorldRoot` queries: `Raycast`, `Blockcast`, `Spherecast`,
//! `Shapecast` (the nearest hit, as a `RaycastResult`), the region queries
//! `GetPartBoundsInBox`, `GetPartBoundsInRadius` and `GetPartsInPart`, and
//! `BulkMoveTo`.
//!
//! Queries sync the simulation first, so they see parts as scripts left them.
//! A part is seen when it collides or its `CanQuery` is on, and when the
//! params' filter, `CollisionGroup` and `RespectCanCollide` let it through.

use std::collections::HashSet;
use std::sync::Arc;

use glam::Vec3;
use mlua::{
    AnyUserData, Lua, MetaMethod, MultiValue, UserData, UserDataFields, UserDataMethods,
    Value as LuaValue, Vector,
};
use re_physics::{PartGeom, PartShape, PartView, Pose};
use re_scene::{Entity, World};
use re_script::codes::BAD_ARGUMENT;

use super::layout::Layout;
use super::parts::pose_of_cframe;
use super::pose::world_pose;
use super::sim::{Sim, entity_of};
use super::with_sim;
use crate::datatypes::{CFrame, OverlapParams, RaycastParams};
use crate::enums::EnumItem;
use crate::instance::methods_api::{args, ret};
use crate::instance::{self, push, push_list, tree};
use crate::runtime::{Bindings, lua_error, with_world};

/// What a ray or cast hit.
#[derive(Clone, Debug)]
pub struct RaycastResult {
    /// The part hit.
    pub instance: Entity,
    /// Where it was hit.
    pub position: Vec3,
    /// The part's surface normal there.
    pub normal: Vec3,
    /// How far the ray or shape went.
    pub distance: f32,
    /// The part's `Material` item name.
    pub material: String,
}

impl UserData for RaycastResult {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "RaycastResult");
        fields.add_field_method_get("Instance", |lua, r| push(lua, r.instance));
        fields.add_field_method_get("Position", |_, r| Ok(vector(r.position)));
        fields.add_field_method_get("Normal", |_, r| Ok(vector(r.normal)));
        fields.add_field_method_get("Distance", |_, r| Ok(f64::from(r.distance)));
        fields.add_field_method_get("Material", |lua, r| {
            EnumItem::named("Material", &r.material).to_lua(lua)
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("RaycastResult"));
        crate::datatypes::strict(methods, "RaycastResult");
    }
}

fn vector(v: Vec3) -> Vector {
    Vector::new(v.x, v.y, v.z)
}

fn vec_of(v: Vector) -> Vec3 {
    Vec3::new(v.x(), v.y(), v.z())
}

/// The filter of one query, resolved against the world.
struct Filter {
    /// The filter instances and their descendants.
    listed: HashSet<Entity>,
    include: bool,
    /// The groups the params' group collides with, by id.
    groups: u32,
    respect_can_collide: bool,
    /// Parts never seen (the part a cast or overlap is about).
    skip: Option<Entity>,
}

impl Filter {
    fn new(
        w: &World,
        sim: &Sim,
        list: &[AnyUserData],
        include: bool,
        group: &str,
        respect_can_collide: bool,
    ) -> Filter {
        let mut listed = HashSet::new();
        for ud in list {
            if let Some(e) = instance::entity_of(ud)
                && tree::alive(w, e)
            {
                listed.insert(e);
                listed.extend(tree::descendants(w, e));
            }
        }
        Filter {
            listed,
            include,
            groups: sim.group_bits(group),
            respect_can_collide,
            skip: None,
        }
    }

    fn of_raycast(w: &World, sim: &Sim, p: Option<&RaycastParams>) -> Filter {
        match p {
            Some(p) => Filter::new(
                w,
                sim,
                &p.filter,
                p.filter_type.name() == "Include",
                &p.collision_group,
                p.respect_can_collide,
            ),
            None => Filter::new(w, sim, &[], false, "Default", false),
        }
    }

    fn of_overlap(w: &World, sim: &Sim, p: Option<&OverlapParams>) -> (Filter, usize) {
        match p {
            Some(p) => (
                Filter::new(
                    w,
                    sim,
                    &p.filter,
                    p.filter_type.name() == "Include",
                    &p.collision_group,
                    p.respect_can_collide,
                ),
                if p.max_parts > 0 {
                    p.max_parts as usize
                } else {
                    usize::MAX
                },
            ),
            None => (
                Filter::new(w, sim, &[], false, "Default", false),
                usize::MAX,
            ),
        }
    }

    fn accept(&self, v: &PartView) -> bool {
        let Some(e) = entity_of(v.id) else {
            return false;
        };
        if Some(e) == self.skip {
            return false;
        }
        if self.groups & (1 << v.group) == 0 {
            return false;
        }
        if self.respect_can_collide && !v.can_collide {
            return false;
        }
        self.listed.contains(&e) == self.include
    }
}

/// Runs `f` on the simulation after syncing it with the world.
fn synced<R>(lua: &Lua, f: impl FnOnce(&mut Sim, &mut World) -> R) -> mlua::Result<Option<R>> {
    with_world(lua, |w| {
        with_sim(w, |sim, w| {
            sim.sync(w, false);
            f(sim, w)
        })
    })
}

/// The first part a ray from `origin` along `direction` (its length is the
/// reach) enters, and how far along it is, with `Workspace:Raycast`'s
/// default filter. For the UI's pointer on surfaces.
pub(crate) fn first_hit(w: &mut World, origin: Vec3, direction: Vec3) -> Option<(Entity, f32)> {
    with_sim(w, |sim, w| {
        sim.sync(w, false);
        let f = Filter::of_raycast(w, sim, None);
        let hit = sim
            .physics
            .raycast_parts(origin, direction, &mut |v: &PartView| f.accept(v))?;
        Some((entity_of(hit.part)?, hit.distance))
    })
    .flatten()
}

fn material(w: &World, e: Entity) -> String {
    Layout::find(w)
        .and_then(|l| l.material.str(w, e))
        .unwrap_or_else(|| "Plastic".into())
}

fn result(lua: &Lua, hit: Option<RaycastResult>) -> mlua::Result<MultiValue> {
    match hit {
        Some(r) => ret(lua, lua.create_userdata(r)?),
        None => ret(lua, LuaValue::Nil),
    }
}

fn raycast_params(v: &LuaValue, what: &str) -> mlua::Result<Option<RaycastParams>> {
    match v {
        LuaValue::Nil => Ok(None),
        LuaValue::UserData(ud) => match ud.borrow::<RaycastParams>() {
            Ok(p) => Ok(Some(p.clone())),
            Err(_) => Err(lua_error(
                BAD_ARGUMENT,
                format!("bad argument to {what}: RaycastParams expected"),
            )),
        },
        _ => Err(lua_error(
            BAD_ARGUMENT,
            format!("bad argument to {what}: RaycastParams expected"),
        )),
    }
}

fn overlap_params(v: &LuaValue, what: &str) -> mlua::Result<Option<OverlapParams>> {
    match v {
        LuaValue::Nil => Ok(None),
        LuaValue::UserData(ud) => match ud.borrow::<OverlapParams>() {
            Ok(p) => Ok(Some(p.clone())),
            Err(_) => Err(lua_error(
                BAD_ARGUMENT,
                format!("bad argument to {what}: OverlapParams expected"),
            )),
        },
        _ => Err(lua_error(
            BAD_ARGUMENT,
            format!("bad argument to {what}: OverlapParams expected"),
        )),
    }
}

/// How a cast moves.
enum Cast {
    Ray(Vec3),
    Solid(PartGeom, Pose),
}

fn cast(
    lua: &Lua,
    how: Cast,
    direction: Vec3,
    params: Option<RaycastParams>,
    skip: Option<Entity>,
) -> mlua::Result<MultiValue> {
    if !direction.is_finite() {
        return Err(lua_error(BAD_ARGUMENT, "the direction is not finite"));
    }
    let hit = synced(lua, |sim, w| {
        let mut f = Filter::of_raycast(w, sim, params.as_ref());
        f.skip = skip;
        let mut accept = |v: &PartView| f.accept(v);
        let hit = match how {
            Cast::Ray(origin) => sim.physics.raycast_parts(origin, direction, &mut accept),
            Cast::Solid(g, from) => sim.physics.shapecast_parts(g, from, direction, &mut accept),
        }?;
        let e = entity_of(hit.part)?;
        Some(RaycastResult {
            instance: e,
            position: hit.position,
            normal: hit.normal,
            distance: hit.distance,
            material: material(w, e),
        })
    })?
    .flatten();
    result(lua, hit)
}

fn raycast(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (origin, direction, params): (Vector, Vector, LuaValue) = args(lua, a, "Raycast")?;
    let params = raycast_params(&params, "Raycast")?;
    let origin = vec_of(origin);
    if !origin.is_finite() {
        return Err(lua_error(BAD_ARGUMENT, "Raycast: the origin is not finite"));
    }
    cast(lua, Cast::Ray(origin), vec_of(direction), params, None)
}

fn blockcast(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (cf, size, direction, params): (CFrame, Vector, Vector, LuaValue) =
        args(lua, a, "Blockcast")?;
    let params = raycast_params(&params, "Blockcast")?;
    let geom = PartGeom::new(PartShape::Block, vec_of(size).abs()).sanitized();
    cast(
        lua,
        Cast::Solid(geom, pose_of_cframe(&cf)),
        vec_of(direction),
        params,
        None,
    )
}

fn spherecast(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (pos, radius, direction, params): (Vector, f32, Vector, LuaValue) =
        args(lua, a, "Spherecast")?;
    let params = raycast_params(&params, "Spherecast")?;
    if !(radius.is_finite() && radius > 0.0) {
        return Err(lua_error(
            BAD_ARGUMENT,
            "Spherecast: the radius must be positive",
        ));
    }
    let geom = PartGeom::new(PartShape::Ball, Vec3::splat(radius * 2.0)).sanitized();
    cast(
        lua,
        Cast::Solid(geom, Pose::new(vec_of(pos), glam::Quat::IDENTITY)),
        vec_of(direction),
        params,
        None,
    )
}

/// A part's shape and world pose.
fn part_solid(lua: &Lua, part: &LuaValue, what: &str) -> mlua::Result<(Entity, PartGeom, Pose)> {
    let e = instance::arg_instance(part, what)?;
    with_world(lua, |w| {
        let l = Layout::find(w)?;
        if !l.is_part(w, e) {
            return None;
        }
        let class = tree::class(w, e).unwrap_or("Part").to_string();
        Some((e, l.geom(w, e, &class), world_pose(w, e)?))
    })?
    .ok_or_else(|| {
        lua_error(
            BAD_ARGUMENT,
            format!("bad argument to {what}: BasePart expected"),
        )
    })
}

fn shapecast(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (part, direction, params): (LuaValue, Vector, LuaValue) = args(lua, a, "Shapecast")?;
    let params = raycast_params(&params, "Shapecast")?;
    let (e, geom, pose) = part_solid(lua, &part, "Shapecast")?;
    cast(
        lua,
        Cast::Solid(geom, pose),
        vec_of(direction),
        params,
        Some(e),
    )
}

enum Region {
    Box(Pose, Vec3),
    Sphere(Vec3, f32),
    Solid(PartGeom, Pose),
}

fn region(
    lua: &Lua,
    r: Region,
    params: Option<OverlapParams>,
    skip: Option<Entity>,
) -> mlua::Result<MultiValue> {
    let list = synced(lua, |sim, w| {
        let (mut f, max) = Filter::of_overlap(w, sim, params.as_ref());
        f.skip = skip;
        let mut accept = |v: &PartView| f.accept(v);
        let ids = match r {
            Region::Box(p, s) => sim.physics.parts_in_box(p, s, &mut accept, max),
            Region::Sphere(c, rad) => sim.physics.parts_in_radius(c, rad, &mut accept, max),
            Region::Solid(g, p) => sim.physics.parts_in_solid(g, p, &mut accept, max),
        };
        ids.into_iter().filter_map(entity_of).collect::<Vec<_>>()
    })?
    .unwrap_or_default();
    ret(lua, push_list(lua, &list)?)
}

fn get_part_bounds_in_box(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (cf, size, params): (CFrame, Vector, LuaValue) = args(lua, a, "GetPartBoundsInBox")?;
    let params = overlap_params(&params, "GetPartBoundsInBox")?;
    region(
        lua,
        Region::Box(pose_of_cframe(&cf), vec_of(size)),
        params,
        None,
    )
}

fn get_part_bounds_in_radius(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (pos, radius, params): (Vector, f32, LuaValue) = args(lua, a, "GetPartBoundsInRadius")?;
    let params = overlap_params(&params, "GetPartBoundsInRadius")?;
    region(lua, Region::Sphere(vec_of(pos), radius), params, None)
}

fn get_parts_in_part(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (part, params): (LuaValue, LuaValue) = args(lua, a, "GetPartsInPart")?;
    let params = overlap_params(&params, "GetPartsInPart")?;
    let (e, geom, pose) = part_solid(lua, &part, "GetPartsInPart")?;
    region(lua, Region::Solid(geom, pose), params, Some(e))
}

fn bulk_move_to(lua: &Lua, _e: Entity, a: MultiValue) -> mlua::Result<MultiValue> {
    let (parts, frames, _mode): (mlua::Table, mlua::Table, LuaValue) = args(lua, a, "BulkMoveTo")?;
    let parts: Vec<LuaValue> = parts.sequence_values().collect::<mlua::Result<_>>()?;
    let frames: Vec<CFrame> = frames.sequence_values().collect::<mlua::Result<_>>()?;
    if parts.len() != frames.len() {
        return Err(lua_error(
            BAD_ARGUMENT,
            "BulkMoveTo: partList and cframeList must be the same length",
        ));
    }
    let mut moves = Vec::with_capacity(parts.len());
    for (p, cf) in parts.iter().zip(&frames) {
        moves.push((instance::arg_instance(p, "BulkMoveTo")?, pose_of_cframe(cf)));
    }
    with_world(lua, |w| {
        for (e, pose) in moves {
            super::pose::set_world_pose(w, e, pose)?;
        }
        Ok(())
    })?
    .map_err(crate::runtime::compat_error)?;
    ret(lua, ())
}

type Imp = fn(&Lua, Entity, MultiValue) -> mlua::Result<MultiValue>;

/// Registers the `WorldRoot` queries and `BulkMoveTo`.
pub(crate) fn register_methods(b: &mut Bindings) {
    let methods: &[(&str, Imp)] = &[
        ("Raycast", raycast),
        ("Blockcast", blockcast),
        ("Spherecast", spherecast),
        ("Shapecast", shapecast),
        ("GetPartBoundsInBox", get_part_bounds_in_box),
        ("GetPartBoundsInRadius", get_part_bounds_in_radius),
        ("GetPartsInPart", get_parts_in_part),
        ("BulkMoveTo", bulk_move_to),
    ];
    for (name, f) in methods {
        b.method("WorldRoot", name, Arc::new(*f));
    }
}
