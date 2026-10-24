//! The `anim` library: the parameters, states and joints of an entity's
//! animation graph (ADR-0039), through [`HostServices`](re_types::host::HostServices).

use mlua::{Lua, Result, Value};
use re_scene::entity_id;
use re_types::anim::AnimValue;

use super::Lib;
use super::libs::{entity_arg, host_err};
use super::math::{LQuat, V3};
use crate::codes;
use crate::host::with_host;

pub(crate) fn lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "set",
        "(e: Entity, param: string, value: number | boolean) -> ()",
        "Sets a float (number) or bool parameter of the graph `e`'s Animator plays, at once. An unknown parameter or a value of the wrong type is an error listing the graph's parameters.",
        |lua, (e, param, value): (Value, String, Value)| {
            let e = entity_arg(&e, "anim.set")?;
            let number = |n: f32| {
                if n.is_finite() {
                    Ok(AnimValue::Float(n))
                } else {
                    Err(super::error(
                        codes::BAD_ARGUMENT,
                        format!("anim.set: `{param}` is {n}, which is not finite"),
                    ))
                }
            };
            let value = match value {
                Value::Boolean(b) => AnimValue::Bool(b),
                Value::Number(n) => number(n as f32)?,
                Value::Integer(n) => number(crate::lua_int(n) as f32)?,
                other => {
                    return Err(super::error(
                        codes::BAD_ARGUMENT,
                        format!(
                            "anim.set: `{param}` takes a number or a boolean, not {}",
                            other.type_name()
                        ),
                    ));
                }
            };
            with_host(lua, |h| {
                h.services()
                    .anim_set(entity_id(e), &param, value)
                    .map_err(host_err)
            })
        },
    )?;
    lib.func(
        lua,
        "get",
        "(e: Entity, param: string) -> number | boolean",
        "A parameter of `e`'s graph: a number for a float parameter, a boolean for a bool, and for a trigger whether it is set.",
        |lua, (e, param): (Value, String)| {
            let e = entity_arg(&e, "anim.get")?;
            let v = with_host(lua, |h| {
                h.services().anim_get(entity_id(e), &param).map_err(host_err)
            })?;
            Ok(match v {
                AnimValue::Float(f) => Value::Number(f64::from(f)),
                AnimValue::Bool(b) => Value::Boolean(b),
            })
        },
    )?;
    lib.func(
        lua,
        "trigger",
        "(e: Entity, param: string) -> ()",
        "Sets a trigger of `e`'s graph; it stays set until a transition that tests it fires.",
        |lua, (e, param): (Value, String)| {
            let e = entity_arg(&e, "anim.trigger")?;
            with_host(lua, |h| {
                h.services()
                    .anim_trigger(entity_id(e), &param)
                    .map_err(host_err)
            })
        },
    )?;
    lib.func(
        lua,
        "state",
        "(e: Entity, layer: string?) -> string",
        "The state a layer of `e`'s graph is in, or fading into (the first layer by default).",
        |lua, (e, layer): (Value, Option<String>)| {
            let e = entity_arg(&e, "anim.state")?;
            with_host(lua, |h| {
                h.services()
                    .anim_state(entity_id(e), layer.as_deref())
                    .map_err(host_err)
            })
        },
    )?;
    lib.func(
        lua,
        "joint",
        "(e: Entity, joint: string) -> (vector, Quat)",
        "A joint of `e`'s skin in world space after the last step: its position and rotation.",
        |lua, (e, joint): (Value, String)| {
            let e = entity_arg(&e, "anim.joint")?;
            let (p, r) = with_host(lua, |h| {
                h.services()
                    .anim_joint(entity_id(e), &joint)
                    .map_err(host_err)
            })?;
            Ok((V3(p), LQuat(r)))
        },
    )?;
    Ok(())
}
