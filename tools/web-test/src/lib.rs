//! Smoke checks of the C++ the engine links, Luau (through mlua) and Jolt (through
//! `re_physics`), built for `wasm32-unknown-unknown` with wasi-sdk (ADR-0049 §1, §2).
//!
//! The same functions run natively (`src/main.rs`) and in wasm under Node
//! (`tools/web-test/smoke.mjs`), and CI compares their output line by line. Luau's checks
//! cover what the browser build depends on: C++ exceptions crossing `pcall` and Rust
//! callbacks, coroutines, vectors, bytecode, and 32-bit integers. The physics check drops a
//! pile of bodies and hashes their final poses, which match natively only when Jolt is
//! built cross-platform deterministic (ADR-0036).

use mlua::{Lua, Value};
use re_core::TaskPool;
use re_physics::{Physics, PhysicsConfig};
use re_types::EntityId;
use re_types::glam::{Quat, Vec3};
use re_types::physics::{BodyDesc, BodyKind, PhysicsCmd, ShapeDesc};

/// Luau snippets and what each returns, rendered with `{:?}` on mlua's `Value`.
pub const LUAU_CHECKS: &[(&str, &str)] = &[
    ("arithmetic", "return 1 + 2"),
    (
        "pcall",
        "local ok, e = pcall(function() error('boom') end) return tostring(ok) .. ' ' .. e",
    ),
    (
        "rust_error",
        "local ok, e = pcall(rust_fail, 'from rust') return tostring(ok) .. ' ' .. tostring(e)",
    ),
    ("rust_call", "return rust_add(40, 2)"),
    (
        "coroutine",
        "local co = coroutine.wrap(function() for i = 1, 3 do coroutine.yield(i) end end) \
         return co() + co() + co()",
    ),
    (
        "vector",
        "local v = vector.create(1, 2, 3) * 2 return v.x + v.y + v.z",
    ),
    ("big_integer", "return 3000000000 + 1"),
    (
        "maths",
        "local t = 0 for i = 1, 1000 do t = t + math.sqrt(i) * math.floor(i / 7) end return t",
    ),
];

fn lua() -> mlua::Result<Lua> {
    let lua = Lua::new();
    let add = lua.create_function(|_, (a, b): (f64, f64)| Ok(a + b))?;
    let fail = lua
        .create_function(|_, msg: String| -> mlua::Result<()> { Err(mlua::Error::runtime(msg)) })?;
    lua.globals().set("rust_add", add)?;
    lua.globals().set("rust_fail", fail)?;
    lua.sandbox(true)?;
    Ok(lua)
}

#[allow(
    clippy::useless_conversion,
    reason = "mlua::Integer is i64 here but i32 on wasm32"
)]
fn integer(i: mlua::Integer) -> f64 {
    i64::from(i) as f64
}

/// Runs `src` in a fresh sandboxed state and renders its result.
fn eval(src: &[u8], binary: bool) -> mlua::Result<String> {
    let lua = lua()?;
    let mode = if binary {
        mlua::chunk::ChunkMode::Binary
    } else {
        mlua::chunk::ChunkMode::Text
    };
    let value: Value = lua.load(src).set_name("=check").set_mode(mode).eval()?;
    // Rendered while `lua` is alive: a string value borrows from the state. Numbers print
    // the same whichever of Integer and Number mlua chose: its `Integer` is i64 natively
    // and i32 on wasm32.
    Ok(match value {
        Value::Integer(i) => format!("number {}", integer(i)),
        Value::Number(n) => format!("number {n}"),
        other => format!("{other:?}"),
    })
}

/// Runs every [`LUAU_CHECKS`] entry, a compile error, an uncaught error and a bytecode
/// chunk, one `name: result` line each.
pub fn luau_report() -> String {
    let mut out = String::new();
    let mut line = |name: &str, result: mlua::Result<String>| {
        let text = result.unwrap_or_else(|e| {
            let e = e.to_string();
            format!("error: {}", e.lines().next().unwrap_or(""))
        });
        out += &format!("{name}: {text}\n");
    };
    for (name, src) in LUAU_CHECKS {
        line(name, eval(src.as_bytes(), false));
    }
    line("compile_error", eval(b"return +", false));
    line("uncaught", eval(b"error('uncaught')", false));
    line(
        "bytecode",
        mlua::chunk::Compiler::new()
            .compile("return 40 + 2")
            .and_then(|bytecode| eval(&bytecode, true)),
    );
    out
}

/// Drops `bodies` spheres and boxes on a floor for `steps` steps at 60 Hz and returns an
/// FNV-1a hash of every body's final position and rotation bits, as 16 hex digits.
pub fn pile_hash(bodies: u32, steps: u32) -> Result<String, String> {
    let fail = |d: re_core::Diagnostic| d.render_human();
    let mut physics = Physics::new(PhysicsConfig::default(), TaskPool::global()).map_err(fail)?;
    let mut cmds = vec![PhysicsCmd::AddBody {
        id: EntityId(0),
        desc: BodyDesc {
            shape: ShapeDesc::Box {
                half_extents: Vec3::new(30.0, 0.5, 30.0),
            },
            pos: Vec3::new(0.0, -0.5, 0.0),
            ..BodyDesc::default()
        },
    }];
    let side = libm::cbrt(f64::from(bodies)).ceil() as u32;
    for i in 0..bodies {
        let (x, y, z) = (i % side, i / (side * side), (i / side) % side);
        let shape = if i % 2 == 0 {
            ShapeDesc::Sphere { radius: 0.4 }
        } else {
            ShapeDesc::Box {
                half_extents: Vec3::splat(0.4),
            }
        };
        cmds.push(PhysicsCmd::AddBody {
            id: EntityId(1 + u64::from(i)),
            desc: BodyDesc {
                kind: BodyKind::Dynamic,
                shape,
                pos: Vec3::new(
                    x as f32 - side as f32 * 0.5,
                    1.0 + y as f32,
                    z as f32 - side as f32 * 0.5,
                ),
                // A quarter turn per eight bodies, built without trigonometry so the start
                // is the same on every platform.
                rot: if i % 8 == 0 {
                    Quat::from_xyzw(0.0, 0.707_106_77, 0.0, 0.707_106_77)
                } else {
                    Quat::IDENTITY
                },
                ..BodyDesc::default()
            },
        });
    }
    physics.apply(&mut cmds);
    for _ in 0..steps {
        physics.step(1.0 / 60.0).map_err(fail)?;
    }
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for i in 0..=u64::from(bodies) {
        let (pos, rot) = physics
            .pose(EntityId(i))
            .ok_or_else(|| format!("body {i} is missing"))?;
        for v in pos.to_array().into_iter().chain(rot.to_array()) {
            for b in v.to_bits().to_le_bytes() {
                hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
            }
        }
    }
    Ok(format!("{hash:016x}"))
}

/// The bodies and steps CI uses.
pub const PILE: (u32, u32) = (300, 240);

/// Everything CI compares: the Luau report and the pile hash.
pub fn report() -> String {
    let (bodies, steps) = PILE;
    let pile = pile_hash(bodies, steps).unwrap_or_else(|e| format!("error: {e}"));
    format!("{}pile {bodies}x{steps}: {pile}\n", luau_report())
}

#[cfg(target_arch = "wasm32")]
#[expect(
    unsafe_code,
    reason = "the module runs the C++ static constructors itself, once"
)]
mod wasm {
    use wasm_bindgen::prelude::*;

    unsafe extern "C" {
        fn __wasm_call_ctors();
    }

    /// Runs the C++ static constructors (Luau's flag registry) once. wasm-ld runs them
    /// before every export call unless the module calls them itself.
    #[wasm_bindgen]
    pub fn init() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        // SAFETY: wasm-ld exports `__wasm_call_ctors` for a module that runs its
        // constructors itself; `ONCE` runs them once, before any C++ code.
        ONCE.call_once(|| unsafe { __wasm_call_ctors() });
    }

    /// [`super::report`], for `smoke.mjs`.
    #[wasm_bindgen]
    pub fn report() -> String {
        super::report()
    }
}
