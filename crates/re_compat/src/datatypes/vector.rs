//! `Vector3` (Luau's native vector with a type metatable, see `vector.luau`)
//! and `Vector2` (userdata).

use mlua::{
    FromLua, Lua, MetaMethod, Result, Table, UserData, UserDataFields, UserDataMethods, Value,
    Variadic,
};

use super::{bad_argument, fmt_f32, num_arg, set_lib, strict, type_name};

const VECTOR_LUAU: &str = include_str!("vector.luau");

/// Installs `Vector3`, the vector type metatable and `Vector2`.
pub(crate) fn install(lua: &Lua, globals: &Table) -> Result<()> {
    let tostr = lua.create_function(|_, v: mlua::Vector| {
        Ok(format!(
            "{}, {}, {}",
            fmt_f32(v.x()),
            fmt_f32(v.y()),
            fmt_f32(v.z())
        ))
    })?;
    let (lib, mt): (Table, Table) = lua.load(VECTOR_LUAU).set_name("=Vector3").call(tostr)?;
    lua.set_type_metatable::<mlua::Vector>(Some(mt));
    globals.raw_set("Vector3", lib)?;

    let new = lua.create_function(|_, (x, y): (Value, Value)| {
        Ok(Vector2::new(
            num_arg(&x, 1, "new", Some(0.0))? as f32,
            num_arg(&y, 2, "new", Some(0.0))? as f32,
        ))
    })?;
    let c = |x: f32, y: f32| -> Result<Value> {
        lua.create_userdata(Vector2::new(x, y)).map(Value::UserData)
    };
    set_lib(
        lua,
        globals,
        "Vector2",
        vec![
            ("new", Value::Function(new)),
            ("zero", c(0.0, 0.0)?),
            ("one", c(1.0, 1.0)?),
            ("xAxis", c(1.0, 0.0)?),
            ("yAxis", c(0.0, 1.0)?),
        ],
    )
}

/// A 2D vector of `f32` (`Vector2`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Vector2 {
    /// X.
    pub x: f32,
    /// Y.
    pub y: f32,
}

impl Vector2 {
    /// A vector from its components.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    /// The length.
    pub fn magnitude(self) -> f32 {
        (self.x * self.x + self.y * self.y).sqrt()
    }

    /// The vector divided by its length (NaN for the zero vector, as in
    /// Roblox).
    pub fn unit(self) -> Self {
        let m = self.magnitude();
        Self::new(self.x / m, self.y / m)
    }

    /// The dot product.
    pub fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y
    }

    /// The 2D cross product (the Z of the 3D cross product).
    pub fn cross(self, o: Self) -> f32 {
        self.x * o.y - self.y * o.x
    }

    fn map(self, f: impl Fn(f32) -> f32) -> Self {
        Self::new(f(self.x), f(self.y))
    }

    fn zip(self, o: Self, f: impl Fn(f32, f32) -> f32) -> Self {
        Self::new(f(self.x, o.x), f(self.y, o.y))
    }
}

fn sign(x: f32) -> f32 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

impl FromLua for Vector2 {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        match &value {
            Value::UserData(ud) if ud.is::<Vector2>() => Ok(*ud.borrow::<Vector2>()?),
            _ => Err(bad_argument(
                1,
                "Vector2",
                format!("Vector2 expected, got {}", type_name(&value)),
            )),
        }
    }
}

/// An operand of Vector2 arithmetic: a Vector2 or a number.
enum Operand {
    V(Vector2),
    N(f32),
}

fn operand(v: &Value, op: &str) -> Result<Operand> {
    match v {
        Value::Number(_) | Value::Integer(_) => Ok(Operand::N(num_arg(v, 1, op, None)? as f32)),
        Value::UserData(ud) if ud.is::<Vector2>() => Ok(Operand::V(*ud.borrow::<Vector2>()?)),
        other => Err(super::error(
            re_script::codes::BAD_ARGUMENT,
            format!(
                "attempt to perform arithmetic ({op}) on Vector2 and {}",
                type_name(other)
            ),
        )),
    }
}

/// Applies `f` to the two operands of a binary metamethod, either of which
/// may be the number.
fn arith(
    a: Value,
    b: Value,
    op: &str,
    f: impl Fn(f32, f32) -> f32,
    numbers: bool,
) -> Result<Vector2> {
    match (operand(&a, op)?, operand(&b, op)?) {
        (Operand::V(a), Operand::V(b)) => Ok(a.zip(b, f)),
        (Operand::V(a), Operand::N(n)) if numbers => Ok(a.map(|x| f(x, n))),
        (Operand::N(n), Operand::V(b)) if numbers => Ok(b.map(|x| f(n, x))),
        _ => Err(super::error(
            re_script::codes::BAD_ARGUMENT,
            format!("attempt to perform arithmetic ({op}) on Vector2 and number"),
        )),
    }
}

impl UserData for Vector2 {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "Vector2");
        fields.add_field_method_get("X", |_, v| Ok(v.x));
        fields.add_field_method_get("Y", |_, v| Ok(v.y));
        fields.add_field_method_get("x", |_, v| Ok(v.x));
        fields.add_field_method_get("y", |_, v| Ok(v.y));
        fields.add_field_method_get("Magnitude", |_, v| Ok(v.magnitude()));
        fields.add_field_method_get("magnitude", |_, v| Ok(v.magnitude()));
        fields.add_field_method_get("Unit", |_, v| Ok(v.unit()));
        fields.add_field_method_get("unit", |_, v| Ok(v.unit()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Dot", |_, v, o: Vector2| Ok(v.dot(o)));
        methods.add_method("Cross", |_, v, o: Vector2| Ok(v.cross(o)));
        methods.add_method("Lerp", |_, v, (o, t): (Vector2, Value)| {
            let t = num_arg(&t, 2, "Lerp", None)? as f32;
            Ok(Vector2::new(v.x + (o.x - v.x) * t, v.y + (o.y - v.y) * t))
        });
        methods.add_method("lerp", |_, v, (o, t): (Vector2, Value)| {
            let t = num_arg(&t, 2, "lerp", None)? as f32;
            Ok(Vector2::new(v.x + (o.x - v.x) * t, v.y + (o.y - v.y) * t))
        });
        methods.add_method("Abs", |_, v, ()| Ok(v.map(f32::abs)));
        methods.add_method("Floor", |_, v, ()| Ok(v.map(f32::floor)));
        methods.add_method("Ceil", |_, v, ()| Ok(v.map(f32::ceil)));
        methods.add_method("Sign", |_, v, ()| Ok(v.map(sign)));
        methods.add_method("Min", |_, v, others: Variadic<Vector2>| {
            Ok(others.iter().fold(*v, |a, b| a.zip(*b, f32::min)))
        });
        methods.add_method("Max", |_, v, others: Variadic<Vector2>| {
            Ok(others.iter().fold(*v, |a, b| a.zip(*b, f32::max)))
        });
        methods.add_method("Angle", |_, v, (o, signed): (Vector2, Option<bool>)| {
            let a = libm::atan2(v.cross(o) as f64, v.dot(o) as f64);
            Ok(if signed.unwrap_or(false) { a } else { a.abs() })
        });
        methods.add_method("FuzzyEq", |_, v, (o, eps): (Vector2, Option<f64>)| {
            let e = eps.unwrap_or(1e-5) as f32;
            Ok((v.x - o.x).abs() <= e && (v.y - o.y).abs() <= e)
        });
        methods.add_meta_function(MetaMethod::Add, |_, (a, b): (Value, Value)| {
            arith(a, b, "add", |x, y| x + y, false)
        });
        methods.add_meta_function(MetaMethod::Sub, |_, (a, b): (Value, Value)| {
            arith(a, b, "sub", |x, y| x - y, false)
        });
        methods.add_meta_function(MetaMethod::Mul, |_, (a, b): (Value, Value)| {
            arith(a, b, "mul", |x, y| x * y, true)
        });
        methods.add_meta_function(MetaMethod::Div, |_, (a, b): (Value, Value)| {
            arith(a, b, "div", |x, y| x / y, true)
        });
        methods.add_meta_function(MetaMethod::IDiv, |_, (a, b): (Value, Value)| {
            arith(a, b, "idiv", |x, y| (x / y).floor(), true)
        });
        methods.add_meta_method(MetaMethod::Unm, |_, v, ()| Ok(v.map(|x| -x)));
        methods.add_meta_method(MetaMethod::Eq, |_, v, o: Vector2| Ok(*v == o));
        methods.add_meta_method(MetaMethod::ToString, |_, v, ()| {
            Ok(format!("{}, {}", fmt_f32(v.x), fmt_f32(v.y)))
        });
        strict(methods, "Vector2");
    }
}
