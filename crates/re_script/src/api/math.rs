//! Luau's native `vector` and the `quat` and `color` userdata.
//!
//! Vectors are Luau's builtin value type, made with `vector.create(x, y, z)`
//! (ADR-0011); [`V3`] converts them to and from `glam::Vec3`. A newtype is needed
//! because both types are foreign (the orphan rule).

use glam::{EulerRot, Quat, Vec3};
use mlua::{
    FromLua, IntoLua, Lua, MetaMethod, Result, UserData, UserDataFields, UserDataMethods, Value,
    Vector,
};
use re_reflect::Color;

use super::{Lib, conversion_error};

/// A Luau `vector` as a `glam::Vec3`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct V3(pub Vec3);

impl FromLua for V3 {
    fn from_lua(value: Value, _lua: &Lua) -> Result<Self> {
        match value {
            Value::Vector(v) => Ok(V3(Vec3::new(v.x(), v.y(), v.z()))),
            other => Err(conversion_error(
                &other,
                "vector",
                "expected a vector, e.g. vector.create(x, y, z)",
            )),
        }
    }
}

impl IntoLua for V3 {
    fn into_lua(self, _lua: &Lua) -> Result<Value> {
        Ok(Value::Vector(Vector::new(self.0.x, self.0.y, self.0.z)))
    }
}

/// A rotation (`quat` in Luau). Immutable; operations return new values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LQuat(pub Quat);

impl FromLua for LQuat {
    fn from_lua(value: Value, _lua: &Lua) -> Result<Self> {
        match &value {
            Value::UserData(ud) => ud.borrow::<LQuat>().map(|q| *q).map_err(|_| {
                conversion_error(&value, "Quat", "expected a quat, e.g. quat.identity()")
            }),
            _ => Err(conversion_error(
                &value,
                "Quat",
                "expected a quat, e.g. quat.identity()",
            )),
        }
    }
}

impl UserData for LQuat {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("x", |_, q| Ok(q.0.x));
        fields.add_field_method_get("y", |_, q| Ok(q.0.y));
        fields.add_field_method_get("z", |_, q| Ok(q.0.z));
        fields.add_field_method_get("w", |_, q| Ok(q.0.w));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("inverse", |_, q, ()| Ok(LQuat(q.0.inverse())));
        methods.add_method("rotate", |_, q, v: V3| Ok(V3(q.0 * v.0)));
        methods.add_method("to_euler", |_, q, ()| {
            let (y, x, z) = q.0.to_euler(EulerRot::YXZ);
            Ok((x, y, z))
        });
        methods.add_meta_method(MetaMethod::Mul, |lua, q, rhs: Value| match rhs {
            Value::Vector(v) => V3(q.0 * Vec3::new(v.x(), v.y(), v.z())).into_lua(lua),
            other => {
                let r = LQuat::from_lua(other, lua)?;
                lua.create_userdata(LQuat((q.0 * r.0).normalize()))
                    .map(Value::UserData)
            }
        });
        methods.add_meta_method(MetaMethod::Eq, |_, q, other: LQuat| Ok(q.0 == other.0));
        methods.add_meta_method(MetaMethod::ToString, |_, q, ()| {
            Ok(format!("quat({}, {}, {}, {})", q.0.x, q.0.y, q.0.z, q.0.w))
        });
        super::strict_userdata(methods, "Quat");
    }
}

/// A linear RGBA colour (`Color` in Luau).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LColor(pub Color);

impl FromLua for LColor {
    fn from_lua(value: Value, _lua: &Lua) -> Result<Self> {
        match &value {
            Value::UserData(ud) => ud.borrow::<LColor>().map(|c| *c).map_err(|_| {
                conversion_error(&value, "Color", "expected a color, e.g. color.new(1, 0, 0)")
            }),
            _ => Err(conversion_error(
                &value,
                "Color",
                "expected a color, e.g. color.new(1, 0, 0)",
            )),
        }
    }
}

impl UserData for LColor {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("r", |_, c| Ok(c.0.r));
        fields.add_field_method_get("g", |_, c| Ok(c.0.g));
        fields.add_field_method_get("b", |_, c| Ok(c.0.b));
        fields.add_field_method_get("a", |_, c| Ok(c.0.a));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, c, other: LColor| Ok(c.0 == other.0));
        methods.add_meta_method(MetaMethod::ToString, |_, c, ()| {
            Ok(format!("color({}, {}, {}, {})", c.0.r, c.0.g, c.0.b, c.0.a))
        });
        super::strict_userdata(methods, "Color");
    }
}

/// The type declarations of the userdata in this module.
pub(crate) const QUAT_DECL: &str = "declare extern type Quat with
    read x: number
    read y: number
    read z: number
    read w: number
    function inverse(self): Quat
    function rotate(self, v: vector): vector
    function to_euler(self): (number, number, number)
    -- q1 * q2 composes rotations; q * v rotates a vector.
    __mul: ((Quat, Quat) -> Quat) & ((Quat, vector) -> vector)
    function __eq(self, other: Quat): boolean
    function __tostring(self): string
end";

/// The type declaration of `Color`.
pub(crate) const COLOR_DECL: &str = "declare extern type Color with
    read r: number
    read g: number
    read b: number
    read a: number
    function __eq(self, other: Color): boolean
    function __tostring(self): string
end";

/// The `quat` library.
pub(crate) fn quat_lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "identity",
        "() -> Quat",
        "The rotation that does nothing.",
        |_, ()| Ok(LQuat(Quat::IDENTITY)),
    )?;
    lib.func(
        lua,
        "from_euler",
        "(x: number, y: number, z: number) -> Quat",
        "A rotation from angles in radians: yaw `y`, then pitch `x`, then roll `z`.",
        |_, (x, y, z): (f32, f32, f32)| Ok(LQuat(Quat::from_euler(EulerRot::YXZ, y, x, z))),
    )?;
    lib.func(
        lua,
        "from_axis_angle",
        "(axis: vector, angle: number) -> Quat",
        "A rotation of `angle` radians around `axis` (normalised here).",
        |_, (axis, angle): (V3, f32)| {
            let axis = axis.0.try_normalize().ok_or_else(|| {
                mlua::Error::runtime("quat.from_axis_angle: the axis must not be zero")
            })?;
            Ok(LQuat(Quat::from_axis_angle(axis, angle)))
        },
    )?;
    lib.func(
        lua,
        "look_at",
        "(forward: vector, up: vector?) -> Quat",
        "The rotation that turns -Z towards `forward`, keeping `up` (default +Y) up.",
        |_, (forward, up): (V3, Option<V3>)| {
            let f = forward.0.try_normalize().ok_or_else(|| {
                mlua::Error::runtime("quat.look_at: the forward direction must not be zero")
            })?;
            let up = up.map_or(Vec3::Y, |u| u.0);
            let right = f.cross(up).try_normalize().unwrap_or(Vec3::X);
            let up = right.cross(f);
            Ok(LQuat(
                Quat::from_mat3(&glam::Mat3::from_cols(right, up, -f)).normalize(),
            ))
        },
    )?;
    lib.func(
        lua,
        "slerp",
        "(a: Quat, b: Quat, t: number) -> Quat",
        "Spherical interpolation from `a` (t = 0) to `b` (t = 1).",
        |_, (a, b, t): (LQuat, LQuat, f32)| Ok(LQuat(a.0.slerp(b.0, t))),
    )?;
    Ok(())
}

/// The `color` library.
pub(crate) fn color_lib(lua: &Lua, lib: &mut Lib) -> Result<()> {
    lib.func(
        lua,
        "new",
        "(r: number, g: number, b: number, a: number?) -> Color",
        "A linear colour; alpha defaults to 1.",
        |_, (r, g, b, a): (f32, f32, f32, Option<f32>)| {
            Ok(LColor(Color::rgba(r, g, b, a.unwrap_or(1.0))))
        },
    )?;
    Ok(())
}
