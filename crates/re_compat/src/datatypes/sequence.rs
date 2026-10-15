//! `NumberRange`, `NumberSequence` and `NumberSequenceKeypoint`, and the
//! keypoint rules `ColorSequence` shares.

use mlua::{
    FromLua, Lua, MetaMethod, Result, Table, UserData, UserDataFields, UserDataMethods, Value,
};

use super::{bad_argument, fmt_f32, num_arg, set_lib, strict, type_name};

/// The most keypoints a sequence may have, as in Roblox.
pub const MAX_KEYPOINTS: usize = 20;

/// A closed range of `f32`, `min <= max`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NumberRange {
    /// The low end.
    pub min: f32,
    /// The high end.
    pub max: f32,
}

impl UserData for NumberRange {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "NumberRange");
        fields.add_field_method_get("Min", |_, r| Ok(r.min));
        fields.add_field_method_get("Max", |_, r| Ok(r.max));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, r, o: Value| {
            Ok(matches!(&o, Value::UserData(u) if u.borrow::<NumberRange>().is_ok_and(|o| *r == *o)))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, r, ()| {
            Ok(format!("{} {} ", fmt_f32(r.min), fmt_f32(r.max)))
        });
        strict(methods, "NumberRange");
    }
}

/// One keypoint of a [`NumberSequence`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct NumberSequenceKeypoint {
    /// Where it sits, in `[0, 1]`.
    pub time: f32,
    /// The value there.
    pub value: f32,
    /// How far a particle's value may vary from `value`, either way.
    pub envelope: f32,
}

impl UserData for NumberSequenceKeypoint {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "NumberSequenceKeypoint");
        fields.add_field_method_get("Time", |_, k| Ok(k.time));
        fields.add_field_method_get("Value", |_, k| Ok(k.value));
        fields.add_field_method_get("Envelope", |_, k| Ok(k.envelope));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, k, o: Value| {
            Ok(matches!(&o, Value::UserData(u) if u.borrow::<NumberSequenceKeypoint>().is_ok_and(|o| *k == *o)))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, k, ()| Ok(keypoint_string(k)));
        strict(methods, "NumberSequenceKeypoint");
    }
}

fn keypoint_string(k: &NumberSequenceKeypoint) -> String {
    format!(
        "{} {} {} ",
        fmt_f32(k.time),
        fmt_f32(k.value),
        fmt_f32(k.envelope)
    )
}

/// A curve over `[0, 1]`: at least two keypoints, the first at time 0 and the
/// last at time 1, in increasing time.
#[derive(Clone, Debug, PartialEq)]
pub struct NumberSequence {
    /// The keypoints.
    pub keypoints: Vec<NumberSequenceKeypoint>,
}

impl NumberSequence {
    /// A constant sequence.
    pub fn constant(v: f32) -> Self {
        Self::between(v, v)
    }

    /// From `a` at time 0 to `b` at time 1.
    pub fn between(a: f32, b: f32) -> Self {
        Self {
            keypoints: vec![
                NumberSequenceKeypoint {
                    time: 0.0,
                    value: a,
                    envelope: 0.0,
                },
                NumberSequenceKeypoint {
                    time: 1.0,
                    value: b,
                    envelope: 0.0,
                },
            ],
        }
    }

    /// The value at `t`, linearly interpolated between keypoints (envelopes
    /// are ignored).
    pub fn sample(&self, t: f32) -> f32 {
        let k = &self.keypoints;
        for w in k.windows(2) {
            if t <= w[1].time {
                let span = w[1].time - w[0].time;
                let a = if span > 0.0 {
                    ((t - w[0].time) / span).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                return w[0].value + (w[1].value - w[0].value) * a;
            }
        }
        k.last().map_or(0.0, |k| k.value)
    }
}

impl UserData for NumberSequence {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "NumberSequence");
        fields.add_field_method_get("Keypoints", |lua, s| {
            let t = lua.create_table_with_capacity(s.keypoints.len(), 0)?;
            for k in &s.keypoints {
                t.raw_push(*k)?;
            }
            Ok(t)
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, s, o: Value| {
            Ok(matches!(&o, Value::UserData(u) if u.borrow::<NumberSequence>().is_ok_and(|o| *s == *o)))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, s, ()| {
            Ok(s.keypoints.iter().map(keypoint_string).collect::<String>())
        });
        strict(methods, "NumberSequence");
    }
}

/// Reads an array of keypoint userdata of type `T` (named `name`).
pub(crate) fn keypoint_list<T: Clone + 'static>(t: &Table, name: &str) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for (i, v) in t.sequence_values::<Value>().enumerate() {
        let v = v?;
        match &v {
            Value::UserData(ud) if ud.is::<T>() => out.push(ud.borrow::<T>()?.clone()),
            other => {
                return Err(bad_argument(
                    1,
                    "new",
                    format!("keypoint {} is a {}, not a {name}", i + 1, type_name(other)),
                ));
            }
        }
    }
    Ok(out)
}

/// Checks the keypoint rules: 2 to 20 keypoints, the first at time 0, the
/// last at time 1, times not decreasing.
pub(crate) fn check_keypoint_times(times: impl Iterator<Item = f32>) -> Result<()> {
    let times: Vec<f32> = times.collect();
    let bad = |m: &str| bad_argument(1, "new", m);
    if times.len() < 2 {
        return Err(bad("a sequence needs at least 2 keypoints"));
    }
    if times.len() > MAX_KEYPOINTS {
        return Err(bad("a sequence has at most 20 keypoints"));
    }
    if times[0] != 0.0 {
        return Err(bad("the first keypoint's time must be 0"));
    }
    if times[times.len() - 1] != 1.0 {
        return Err(bad("the last keypoint's time must be 1"));
    }
    if times.windows(2).any(|w| w[1] < w[0]) {
        return Err(bad("keypoints must be in increasing time order"));
    }
    Ok(())
}

impl FromLua for NumberRange {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        super::ud_arg::<NumberRange>(&value, 1, "NumberRange", "NumberRange")
    }
}

/// Installs `NumberRange`, `NumberSequence` and `NumberSequenceKeypoint`.
pub(crate) fn install(lua: &Lua, globals: &Table) -> Result<()> {
    let range_new = lua.create_function(|_, (a, b): (Value, Value)| {
        let min = num_arg(&a, 1, "new", None)?;
        let max = num_arg(&b, 2, "new", Some(min))?;
        if max < min {
            return Err(bad_argument(
                2,
                "new",
                "the maximum is less than the minimum",
            ));
        }
        Ok(NumberRange {
            min: min as f32,
            max: max as f32,
        })
    })?;
    set_lib(
        lua,
        globals,
        "NumberRange",
        vec![("new", Value::Function(range_new))],
    )?;

    let kp_new = lua.create_function(|_, (t, v, e): (Value, Value, Value)| {
        Ok(NumberSequenceKeypoint {
            time: num_arg(&t, 1, "new", None)? as f32,
            value: num_arg(&v, 2, "new", None)? as f32,
            envelope: num_arg(&e, 3, "new", Some(0.0))? as f32,
        })
    })?;
    set_lib(
        lua,
        globals,
        "NumberSequenceKeypoint",
        vec![("new", Value::Function(kp_new))],
    )?;

    let seq_new = lua.create_function(|_, (a, b): (Value, Value)| match &a {
        Value::Table(t) => {
            let ks: Vec<NumberSequenceKeypoint> = keypoint_list(t, "NumberSequenceKeypoint")?;
            check_keypoint_times(ks.iter().map(|k| k.time))?;
            Ok(NumberSequence { keypoints: ks })
        }
        _ => {
            let n0 = num_arg(&a, 1, "new", None)? as f32;
            let n1 = num_arg(&b, 2, "new", Some(n0 as f64))? as f32;
            Ok(NumberSequence::between(n0, n1))
        }
    })?;
    set_lib(
        lua,
        globals,
        "NumberSequence",
        vec![("new", Value::Function(seq_new))],
    )
}
