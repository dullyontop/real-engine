//! `Random`: a seeded generator (PCG32, XSH-RR). Sequences are this engine's
//! own and do not match Roblox's numbers for the same seed.
//!
//! `Random.new()` without a seed draws its seed from the VM's run seed, so a
//! run with the same seed creates the same generators in the same order.
//! Luau's own `math.random` starts from the run seed too, instead of the VM's
//! address and the time.

use mlua::{Function, Lua, MetaMethod, Result, Table, UserData, UserDataMethods, Value};

use super::{bad_argument, num_arg, set_lib, strict, type_name, vec_value};

const MULTIPLIER: u64 = 6_364_136_223_846_793_005;
const STREAM: u64 = 0xda3e_39cb_94b9_5bdb;

/// A PCG32 generator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Random {
    state: u64,
    inc: u64,
}

impl Random {
    /// A generator from a seed.
    pub fn new(seed: u64) -> Self {
        let mut r = Random {
            state: 0,
            inc: (STREAM << 1) | 1,
        };
        r.next_u32();
        r.state = r.state.wrapping_add(seed);
        r.next_u32();
        r
    }

    /// The next 32 random bits.
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(MULTIPLIER).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// The next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        ((self.next_u32() as u64) << 32) | self.next_u32() as u64
    }

    /// A number in `[0, 1)` with 53 random bits.
    pub fn next_number(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// An integer in `[min, max]`, uniformly (`min <= max`).
    pub fn next_integer(&mut self, min: i64, max: i64) -> i64 {
        let span = (max as i128 - min as i128) as u128 + 1;
        if span > u64::MAX as u128 {
            return self.next_u64() as i64;
        }
        let span = span as u64;
        // Rejection sampling: accept draws below the largest multiple of span.
        let zone = u64::MAX - (u64::MAX % span + 1) % span;
        loop {
            let x = self.next_u64();
            if x <= zone {
                return (min as i128 + (x % span) as i128) as i64;
            }
        }
    }

    /// A direction uniformly distributed on the unit sphere.
    pub fn next_unit_vector(&mut self) -> [f32; 3] {
        let z = 2.0 * self.next_number() - 1.0;
        let phi = std::f64::consts::TAU * self.next_number();
        let r = (1.0 - z * z).max(0.0).sqrt();
        [
            (r * libm::cos(phi)) as f32,
            (r * libm::sin(phi)) as f32,
            z as f32,
        ]
    }
}

/// Where `Random.new()` without a seed takes its seed from (VM app data).
struct SeedSource {
    seed: u64,
    count: u64,
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// The engine's own generators of a VM, by stream (VM app data).
#[derive(Default)]
struct EngineStreams(std::collections::HashMap<u64, Random>);

/// An integer in `[min, max]` from engine stream `stream` of the VM: the
/// draws the engine makes itself (`BrickColor.random`, which spawn a
/// character loads at). Each stream is a generator of its own, seeded from
/// the run seed and the stream, so a run with the same seed draws the same
/// numbers and no script's `Random` or `math.random` numbers change.
pub(crate) fn engine_integer(lua: &Lua, stream: u64, min: i64, max: i64) -> i64 {
    let seed = lua.app_data_ref::<SeedSource>().map_or(0, |s| s.seed);
    if lua.app_data_ref::<EngineStreams>().is_none() {
        lua.set_app_data(EngineStreams::default());
    }
    let mut streams = lua
        .app_data_mut::<EngineStreams>()
        .expect("the streams were just set");
    streams
        .0
        .entry(stream)
        .or_insert_with(|| Random::new(splitmix64(seed ^ splitmix64(!stream))))
        .next_integer(min, max)
}

/// The seed `math.random` starts from in a VM with run seed `seed`: the
/// 32 bits `math.randomseed` takes, from the derivation `Random.new()` uses
/// at count 0, which no generator gets.
fn math_random_seed(seed: u64) -> i32 {
    splitmix64(seed ^ splitmix64(0)) as u32 as i32
}

fn int_arg(v: &Value, n: usize, f: &str) -> Result<i64> {
    let x = num_arg(v, n, f, None)?;
    if !x.is_finite() {
        return Err(bad_argument(n, f, "number must be finite"));
    }
    Ok(x.floor() as i64)
}

impl UserData for Random {
    fn add_fields<F: mlua::UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "Random");
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method_mut("NextNumber", |_, r, (a, b): (Value, Value)| {
            let x = r.next_number();
            if a.is_nil() && b.is_nil() {
                return Ok(x);
            }
            let min = num_arg(&a, 1, "NextNumber", None)?;
            let max = num_arg(&b, 2, "NextNumber", None)?;
            Ok(min + (max - min) * x)
        });
        methods.add_method_mut("NextInteger", |_, r, (a, b): (Value, Value)| {
            let min = int_arg(&a, 1, "NextInteger")?;
            let max = int_arg(&b, 2, "NextInteger")?;
            if max < min {
                return Err(bad_argument(2, "NextInteger", "interval is empty"));
            }
            Ok(r.next_integer(min, max))
        });
        methods.add_method_mut("NextUnitVector", |_, r, ()| {
            Ok(vec_value(r.next_unit_vector()))
        });
        methods.add_method_mut("Shuffle", |_, r, t: Value| {
            let Value::Table(t) = t else {
                return Err(bad_argument(
                    1,
                    "Shuffle",
                    format!("table expected, got {}", type_name(&t)),
                ));
            };
            let n = t.raw_len();
            for i in 1..=n {
                if t.raw_get::<Value>(i)?.is_nil() {
                    return Err(bad_argument(
                        1,
                        "Shuffle",
                        "the table must be an array with no holes",
                    ));
                }
            }
            for i in (2..=n).rev() {
                let j = r.next_integer(1, i as i64) as usize;
                if i != j {
                    let a: Value = t.raw_get(i)?;
                    let b: Value = t.raw_get(j)?;
                    t.raw_set(i, b)?;
                    t.raw_set(j, a)?;
                }
            }
            Ok(())
        });
        methods.add_method("Clone", |_, r, ()| Ok(*r));
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("Random"));
        strict(methods, "Random");
    }
}

/// Installs `Random`, and seeds Luau's `math.random` from `seed` (a script's
/// `math.randomseed` still reseeds it).
pub(crate) fn install(lua: &Lua, globals: &Table, seed: u64) -> Result<()> {
    lua.set_app_data(SeedSource { seed, count: 0 });
    let randomseed = globals
        .get::<Table>("math")
        .and_then(|math| math.get::<Function>("randomseed"));
    if let Ok(randomseed) = randomseed {
        randomseed.call::<()>(math_random_seed(seed))?;
    }
    let new = lua.create_function(|lua, s: Value| {
        let seed = match s {
            Value::Nil => {
                let mut src = lua
                    .app_data_mut::<SeedSource>()
                    .ok_or_else(|| mlua::Error::runtime("Random is not installed"))?;
                src.count += 1;
                splitmix64(src.seed ^ splitmix64(src.count))
            }
            v => {
                let x = num_arg(&v, 1, "new", None)?;
                if !x.is_finite() {
                    return Err(bad_argument(1, "new", "the seed must be finite"));
                }
                x.trunc() as i64 as u64
            }
        };
        Ok(Random::new(seed))
    })?;
    set_lib(lua, globals, "Random", vec![("new", Value::Function(new))])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_and_in_range() {
        let mut a = Random::new(42);
        let mut b = Random::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
        let mut r = Random::new(7);
        let mut seen = [false; 6];
        for _ in 0..1000 {
            let x = r.next_number();
            assert!((0.0..1.0).contains(&x));
            let i = r.next_integer(-2, 3);
            assert!((-2..=3).contains(&i));
            seen[(i + 2) as usize] = true;
            let v = r.next_unit_vector();
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            assert!((l - 1.0).abs() < 1e-5);
        }
        assert!(seen.iter().all(|s| *s));
        assert_eq!(r.next_integer(5, 5), 5);
        assert_ne!(Random::new(1).next_u64(), Random::new(2).next_u64());
    }
}
