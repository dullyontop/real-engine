//! Luau's transcendental maths on the `libm` crate (ADR-0036, D2 determinism).
//!
//! Luau's `math` library and `vector.angle` call the platform's C library, which
//! rounds differently on Linux, Windows, macOS and the web, so the same script
//! would leave a different `state_hash` on each. Every VM replaces those functions
//! with the ones below before the sandbox freezes the library tables, and
//! [`crate::compile::compiler`] disables the matching builtins so a call always
//! reaches the table instead of Luau's fastcall (which would call the C library
//! again, and would also constant-fold calls with constant arguments).
//!
//! The `^` operator still uses the platform's `pow`, except for the exponents 2,
//! 0.5 and 3, which Luau compiles to a multiplication or a square root.

use mlua::{Lua, Result, Table, Vector};

type Unary = fn(f64) -> f64;

/// The one-argument `math` functions that are replaced.
const UNARY: [(&str, Unary); 11] = [
    ("sin", libm::sin),
    ("cos", libm::cos),
    ("tan", libm::tan),
    ("asin", libm::asin),
    ("acos", libm::acos),
    ("atan", libm::atan),
    ("sinh", libm::sinh),
    ("cosh", libm::cosh),
    ("tanh", libm::tanh),
    ("exp", libm::exp),
    ("log10", libm::log10),
];

/// Builtins the compiler must not fastcall or fold (see the module docs).
pub(crate) const DISABLED_BUILTINS: [&str; 15] = [
    "math.sin",
    "math.cos",
    "math.tan",
    "math.asin",
    "math.acos",
    "math.atan",
    "math.atan2",
    "math.sinh",
    "math.cosh",
    "math.tanh",
    "math.exp",
    "math.log",
    "math.log10",
    "math.pow",
    "vector.angle",
];

/// Replaces the transcendental functions of `math` and `vector` in `lua`'s
/// globals. Call before `sandbox(true)` freezes the library tables.
pub(crate) fn install(lua: &Lua) -> Result<()> {
    let globals = lua.globals();
    let math: Table = globals.get("math")?;
    for (name, f) in UNARY {
        math.raw_set(name, lua.create_function(move |_, x: f64| Ok(f(x)))?)?;
    }
    math.raw_set(
        "atan2",
        lua.create_function(|_, (y, x): (f64, f64)| Ok(libm::atan2(y, x)))?,
    )?;
    math.raw_set(
        "pow",
        lua.create_function(|_, (x, y): (f64, f64)| Ok(libm::pow(x, y)))?,
    )?;
    // As Luau's math_log: base 2 and 10 use log2 and log10.
    math.raw_set(
        "log",
        lua.create_function(|_, (x, base): (f64, Option<f64>)| {
            Ok(match base {
                None => libm::log(x),
                Some(2.0) => libm::log2(x),
                Some(10.0) => libm::log10(x),
                Some(b) => libm::log(x) / libm::log(b),
            })
        })?,
    )?;
    let vector: Table = globals.get("vector")?;
    vector.raw_set(
        "angle",
        lua.create_function(|_, args: (Vector, Vector, Option<Vector>)| Ok(angle(args)))?,
    )?;
    Ok(())
}

/// Luau's `vector.angle(a, b, axis?)`: the angle between `a` and `b`, negative
/// when `axis` is given and points against `a × b`. The products are in `f32`
/// as in Luau; the sine is a correctly rounded square root.
fn angle((a, b, axis): (Vector, Vector, Option<Vector>)) -> f64 {
    let cross = [
        a.y() * b.z() - a.z() * b.y(),
        a.z() * b.x() - a.x() * b.z(),
        a.x() * b.y() - a.y() * b.x(),
    ];
    let sin = f64::from(cross[0] * cross[0] + cross[1] * cross[1] + cross[2] * cross[2]).sqrt();
    let cos = f64::from(a.x() * b.x() + a.y() * b.y() + a.z() * b.z());
    let angle = libm::atan2(sin, cos);
    match axis {
        Some(n) if cross[0] * n.x() + cross[1] * n.y() + cross[2] * n.z() < 0.0 => -angle,
        _ => angle,
    }
}
