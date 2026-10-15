//! `UDim`, `UDim2` and `Rect`: UI sizes and rectangles.

use mlua::{
    FromLua, Lua, MetaMethod, Result, Table, UserData, UserDataFields, UserDataMethods, Value,
};

use super::vector::Vector2;
use super::{bad_argument, fmt_f32, num_arg, set_lib, strict, type_name, ud_arg};

/// One UI dimension: a fraction of the parent's size plus pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UDim {
    /// The fraction of the parent's size.
    pub scale: f32,
    /// Pixels (whole numbers; a fraction is truncated).
    pub offset: i32,
}

impl UDim {
    /// A dimension from its parts.
    pub const fn new(scale: f32, offset: i32) -> Self {
        Self { scale, offset }
    }

    fn lerp(self, o: UDim, t: f32) -> UDim {
        UDim::new(
            self.scale + (o.scale - self.scale) * t,
            (self.offset as f32 + (o.offset - self.offset) as f32 * t) as i32,
        )
    }

    fn string(self) -> String {
        format!("{}, {}", fmt_f32(self.scale), self.offset)
    }
}

/// Converts a script number to an offset: truncated, saturating.
fn offset(x: f64) -> i32 {
    x as i32
}

/// A UI position or size: a [`UDim`] for X and one for Y.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UDim2 {
    /// Horizontal.
    pub x: UDim,
    /// Vertical.
    pub y: UDim,
}

impl UDim2 {
    /// From scales and offsets.
    pub const fn new(xs: f32, xo: i32, ys: f32, yo: i32) -> Self {
        Self {
            x: UDim::new(xs, xo),
            y: UDim::new(ys, yo),
        }
    }
}

/// An axis-aligned rectangle from `min` to `max`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    /// The top-left corner.
    pub min: Vector2,
    /// The bottom-right corner.
    pub max: Vector2,
}

impl FromLua for UDim {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        ud_arg::<UDim>(&value, 1, "UDim", "UDim")
    }
}

impl FromLua for UDim2 {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        ud_arg::<UDim2>(&value, 1, "UDim2", "UDim2")
    }
}

impl FromLua for Rect {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        ud_arg::<Rect>(&value, 1, "Rect", "Rect")
    }
}

fn arith_err(op: &str, a: &Value, b: &Value) -> mlua::Error {
    super::error(
        re_script::codes::BAD_ARGUMENT,
        format!(
            "attempt to perform arithmetic ({op}) on {} and {}",
            type_name(a),
            type_name(b)
        ),
    )
}

impl UserData for UDim {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "UDim");
        fields.add_field_method_get("Scale", |_, u| Ok(u.scale));
        fields.add_field_method_get("Offset", |_, u| Ok(u.offset));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Lerp", |_, u, (o, t): (UDim, Value)| {
            Ok(u.lerp(o, num_arg(&t, 2, "Lerp", None)? as f32))
        });
        methods.add_meta_function(MetaMethod::Add, |_, (a, b): (Value, Value)| {
            let (x, y) = (
                ud_arg::<UDim>(&a, 1, "add", "UDim").map_err(|_| arith_err("add", &a, &b))?,
                ud_arg::<UDim>(&b, 2, "add", "UDim").map_err(|_| arith_err("add", &a, &b))?,
            );
            Ok(UDim::new(
                x.scale + y.scale,
                x.offset.wrapping_add(y.offset),
            ))
        });
        methods.add_meta_function(MetaMethod::Sub, |_, (a, b): (Value, Value)| {
            let (x, y) = (
                ud_arg::<UDim>(&a, 1, "sub", "UDim").map_err(|_| arith_err("sub", &a, &b))?,
                ud_arg::<UDim>(&b, 2, "sub", "UDim").map_err(|_| arith_err("sub", &a, &b))?,
            );
            Ok(UDim::new(
                x.scale - y.scale,
                x.offset.wrapping_sub(y.offset),
            ))
        });
        methods.add_meta_method(MetaMethod::Unm, |_, u, ()| {
            Ok(UDim::new(-u.scale, u.offset.wrapping_neg()))
        });
        methods.add_meta_method(MetaMethod::Eq, |_, u, o: UDim| Ok(*u == o));
        methods.add_meta_method(MetaMethod::ToString, |_, u, ()| Ok(u.string()));
        strict(methods, "UDim");
    }
}

impl UserData for UDim2 {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "UDim2");
        fields.add_field_method_get("X", |_, u| Ok(u.x));
        fields.add_field_method_get("Y", |_, u| Ok(u.y));
        fields.add_field_method_get("Width", |_, u| Ok(u.x));
        fields.add_field_method_get("Height", |_, u| Ok(u.y));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Lerp", |_, u, (o, t): (UDim2, Value)| {
            let t = num_arg(&t, 2, "Lerp", None)? as f32;
            Ok(UDim2 {
                x: u.x.lerp(o.x, t),
                y: u.y.lerp(o.y, t),
            })
        });
        methods.add_meta_function(MetaMethod::Add, |_, (a, b): (Value, Value)| {
            let (p, q) = (
                ud_arg::<UDim2>(&a, 1, "add", "UDim2").map_err(|_| arith_err("add", &a, &b))?,
                ud_arg::<UDim2>(&b, 2, "add", "UDim2").map_err(|_| arith_err("add", &a, &b))?,
            );
            Ok(UDim2::new(
                p.x.scale + q.x.scale,
                p.x.offset.wrapping_add(q.x.offset),
                p.y.scale + q.y.scale,
                p.y.offset.wrapping_add(q.y.offset),
            ))
        });
        methods.add_meta_function(MetaMethod::Sub, |_, (a, b): (Value, Value)| {
            let (p, q) = (
                ud_arg::<UDim2>(&a, 1, "sub", "UDim2").map_err(|_| arith_err("sub", &a, &b))?,
                ud_arg::<UDim2>(&b, 2, "sub", "UDim2").map_err(|_| arith_err("sub", &a, &b))?,
            );
            Ok(UDim2::new(
                p.x.scale - q.x.scale,
                p.x.offset.wrapping_sub(q.x.offset),
                p.y.scale - q.y.scale,
                p.y.offset.wrapping_sub(q.y.offset),
            ))
        });
        methods.add_meta_method(MetaMethod::Unm, |_, u, ()| {
            Ok(UDim2::new(
                -u.x.scale,
                u.x.offset.wrapping_neg(),
                -u.y.scale,
                u.y.offset.wrapping_neg(),
            ))
        });
        methods.add_meta_method(MetaMethod::Eq, |_, u, o: UDim2| Ok(*u == o));
        methods.add_meta_method(MetaMethod::ToString, |_, u, ()| {
            Ok(format!("{{{}}}, {{{}}}", u.x.string(), u.y.string()))
        });
        strict(methods, "UDim2");
    }
}

impl UserData for Rect {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "Rect");
        fields.add_field_method_get("Min", |_, r| Ok(r.min));
        fields.add_field_method_get("Max", |_, r| Ok(r.max));
        fields.add_field_method_get("Width", |_, r| Ok(r.max.x - r.min.x));
        fields.add_field_method_get("Height", |_, r| Ok(r.max.y - r.min.y));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, r, o: Rect| Ok(*r == o));
        methods.add_meta_method(MetaMethod::ToString, |_, r, ()| {
            Ok(format!(
                "{}, {}, {}, {}",
                fmt_f32(r.min.x),
                fmt_f32(r.min.y),
                fmt_f32(r.max.x),
                fmt_f32(r.max.y)
            ))
        });
        strict(methods, "Rect");
    }
}

/// Installs `UDim`, `UDim2` and `Rect`.
pub(crate) fn install(lua: &Lua, globals: &Table) -> Result<()> {
    let udim_new = lua.create_function(|_, (s, o): (Value, Value)| {
        Ok(UDim::new(
            num_arg(&s, 1, "new", Some(0.0))? as f32,
            offset(num_arg(&o, 2, "new", Some(0.0))?),
        ))
    })?;
    set_lib(
        lua,
        globals,
        "UDim",
        vec![("new", Value::Function(udim_new))],
    )?;

    let udim2_new = lua.create_function(|_, (a, b, c, d): (Value, Value, Value, Value)| {
        if let Value::UserData(_) = a {
            let x = ud_arg::<UDim>(&a, 1, "new", "UDim")?;
            let y = ud_arg::<UDim>(&b, 2, "new", "UDim")?;
            return Ok(UDim2 { x, y });
        }
        Ok(UDim2::new(
            num_arg(&a, 1, "new", Some(0.0))? as f32,
            offset(num_arg(&b, 2, "new", Some(0.0))?),
            num_arg(&c, 3, "new", Some(0.0))? as f32,
            offset(num_arg(&d, 4, "new", Some(0.0))?),
        ))
    })?;
    let from_scale = lua.create_function(|_, (x, y): (Value, Value)| {
        Ok(UDim2::new(
            num_arg(&x, 1, "fromScale", Some(0.0))? as f32,
            0,
            num_arg(&y, 2, "fromScale", Some(0.0))? as f32,
            0,
        ))
    })?;
    let from_offset = lua.create_function(|_, (x, y): (Value, Value)| {
        Ok(UDim2::new(
            0.0,
            offset(num_arg(&x, 1, "fromOffset", Some(0.0))?),
            0.0,
            offset(num_arg(&y, 2, "fromOffset", Some(0.0))?),
        ))
    })?;
    set_lib(
        lua,
        globals,
        "UDim2",
        vec![
            ("new", Value::Function(udim2_new)),
            ("fromScale", Value::Function(from_scale)),
            ("fromOffset", Value::Function(from_offset)),
        ],
    )?;

    let rect_new = lua.create_function(|_, (a, b, c, d): (Value, Value, Value, Value)| {
        if let Value::UserData(_) = a {
            let min = ud_arg::<Vector2>(&a, 1, "new", "Vector2")?;
            let max = ud_arg::<Vector2>(&b, 2, "new", "Vector2")?;
            return Ok(Rect { min, max });
        }
        if !matches!(a, Value::Nil) && matches!(c, Value::Nil) {
            return Err(bad_argument(3, "new", "Rect.new takes 0, 2 or 4 arguments"));
        }
        Ok(Rect {
            min: Vector2::new(
                num_arg(&a, 1, "new", Some(0.0))? as f32,
                num_arg(&b, 2, "new", Some(0.0))? as f32,
            ),
            max: Vector2::new(
                num_arg(&c, 3, "new", Some(0.0))? as f32,
                num_arg(&d, 4, "new", Some(0.0))? as f32,
            ),
        })
    })?;
    set_lib(
        lua,
        globals,
        "Rect",
        vec![("new", Value::Function(rect_new))],
    )
}
