//! `Color3`, `ColorSequence` and `ColorSequenceKeypoint`.
//!
//! A `Color3` holds three `f32` channels exactly as the script gave them
//! (`fromRGB` divides by 255); channels are not clamped, as in Roblox. They
//! are the usual sRGB-encoded values: converting to the engine's linear
//! colours happens only where a property aliases a native colour field.

use mlua::{
    FromLua, Lua, MetaMethod, Result, Table, UserData, UserDataFields, UserDataMethods, Value,
};

use super::sequence::{check_keypoint_times, keypoint_list};
use super::{bad_argument, fmt_f32, num_arg, set_lib, strict, type_name};

/// An RGB colour of three `f32` channels, usually in `[0, 1]`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Color3 {
    /// Red.
    pub r: f32,
    /// Green.
    pub g: f32,
    /// Blue.
    pub b: f32,
}

impl Color3 {
    /// A colour from its channels.
    pub const fn new(r: f32, g: f32, b: f32) -> Self {
        Self { r, g, b }
    }

    /// A colour from 0–255 channels.
    pub fn from_rgb(r: f64, g: f64, b: f64) -> Self {
        Self::new((r / 255.0) as f32, (g / 255.0) as f32, (b / 255.0) as f32)
    }

    /// A colour from hue, saturation and value, each in `[0, 1]` (hue wraps).
    pub fn from_hsv(h: f64, s: f64, v: f64) -> Self {
        let h = (h - h.floor()) * 6.0;
        let i = h.floor();
        let f = h - i;
        let p = v * (1.0 - s);
        let q = v * (1.0 - s * f);
        let t = v * (1.0 - s * (1.0 - f));
        let (r, g, b) = match i as i64 % 6 {
            0 => (v, t, p),
            1 => (q, v, p),
            2 => (p, v, t),
            3 => (p, q, v),
            4 => (t, p, v),
            _ => (v, p, q),
        };
        Self::new(r as f32, g as f32, b as f32)
    }

    /// Hue, saturation and value, each in `[0, 1]`.
    pub fn to_hsv(self) -> (f64, f64, f64) {
        let (r, g, b) = (self.r as f64, self.g as f64, self.b as f64);
        let max = r.max(g).max(b);
        let min = r.min(g).min(b);
        let d = max - min;
        let s = if max == 0.0 { 0.0 } else { d / max };
        let h = if d == 0.0 {
            0.0
        } else if max == r {
            ((g - b) / d).rem_euclid(6.0) / 6.0
        } else if max == g {
            ((b - r) / d + 2.0) / 6.0
        } else {
            ((r - g) / d + 4.0) / 6.0
        };
        (h, s, max)
    }

    /// A colour from `"#RRGGBB"`, `"RRGGBB"`, `"#RGB"` or `"RGB"`.
    pub fn from_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#').unwrap_or(s);
        let digits: Vec<u8> = s
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<_>>()?;
        let (r, g, b) = match digits.as_slice() {
            [r, g, b] => (r * 17, g * 17, b * 17),
            [r1, r2, g1, g2, b1, b2] => (r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2),
            _ => return None,
        };
        Some(Self::from_rgb(r as f64, g as f64, b as f64))
    }

    /// `"RRGGBB"`, uppercase, channels clamped to `[0, 1]` and rounded.
    pub fn to_hex(self) -> String {
        let c = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
        format!("{:02X}{:02X}{:02X}", c(self.r), c(self.g), c(self.b))
    }

    /// Linear interpolation per channel.
    pub fn lerp(self, o: Color3, t: f32) -> Self {
        Self::new(
            self.r + (o.r - self.r) * t,
            self.g + (o.g - self.g) * t,
            self.b + (o.b - self.b) * t,
        )
    }
}

impl FromLua for Color3 {
    fn from_lua(value: Value, _: &Lua) -> Result<Self> {
        match &value {
            Value::UserData(ud) if ud.is::<Color3>() => Ok(*ud.borrow::<Color3>()?),
            _ => Err(bad_argument(
                1,
                "Color3",
                format!("Color3 expected, got {}", type_name(&value)),
            )),
        }
    }
}

impl UserData for Color3 {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "Color3");
        fields.add_field_method_get("R", |_, c| Ok(c.r));
        fields.add_field_method_get("G", |_, c| Ok(c.g));
        fields.add_field_method_get("B", |_, c| Ok(c.b));
        fields.add_field_method_get("r", |_, c| Ok(c.r));
        fields.add_field_method_get("g", |_, c| Ok(c.g));
        fields.add_field_method_get("b", |_, c| Ok(c.b));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        for name in ["Lerp", "lerp"] {
            methods.add_method(name, |_, c, (o, t): (Value, Value)| {
                let o = super::ud_arg::<Color3>(&o, 1, "Lerp", "Color3")?;
                Ok(c.lerp(o, num_arg(&t, 2, "Lerp", None)? as f32))
            });
        }
        methods.add_method("ToHSV", |_, c, ()| Ok(c.to_hsv()));
        methods.add_method("ToHex", |_, c, ()| Ok(c.to_hex()));
        methods.add_meta_method(MetaMethod::Eq, |_, c, o: Color3| Ok(*c == o));
        methods.add_meta_method(MetaMethod::ToString, |_, c, ()| {
            Ok(format!(
                "{}, {}, {}",
                fmt_f32(c.r),
                fmt_f32(c.g),
                fmt_f32(c.b)
            ))
        });
        strict(methods, "Color3");
    }
}

/// One keypoint of a [`ColorSequence`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorSequenceKeypoint {
    /// Where it sits, in `[0, 1]`.
    pub time: f32,
    /// The colour there.
    pub value: Color3,
}

impl UserData for ColorSequenceKeypoint {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "ColorSequenceKeypoint");
        fields.add_field_method_get("Time", |_, k| Ok(k.time));
        fields.add_field_method_get("Value", |_, k| Ok(k.value));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Eq, |_, k, o: Value| {
            Ok(matches!(&o, Value::UserData(u) if u.borrow::<ColorSequenceKeypoint>().is_ok_and(|o| *k == *o)))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, k, ()| Ok(keypoint_string(k)));
        strict(methods, "ColorSequenceKeypoint");
    }
}

fn keypoint_string(k: &ColorSequenceKeypoint) -> String {
    format!(
        "{} {} {} {} 0 ",
        fmt_f32(k.time),
        fmt_f32(k.value.r),
        fmt_f32(k.value.g),
        fmt_f32(k.value.b)
    )
}

/// A colour gradient over `[0, 1]`: at least two keypoints, the first at time
/// 0 and the last at time 1, in increasing time.
#[derive(Clone, Debug, PartialEq)]
pub struct ColorSequence {
    /// The keypoints.
    pub keypoints: Vec<ColorSequenceKeypoint>,
}

impl ColorSequence {
    /// The colour at `t`, linearly interpolated between keypoints.
    pub fn sample(&self, t: f32) -> Color3 {
        let k = &self.keypoints;
        for w in k.windows(2) {
            if t <= w[1].time {
                let span = w[1].time - w[0].time;
                let a = if span > 0.0 {
                    (t - w[0].time) / span
                } else {
                    0.0
                };
                return w[0].value.lerp(w[1].value, a.clamp(0.0, 1.0));
            }
        }
        k.last().map_or_else(Color3::default, |k| k.value)
    }
}

impl UserData for ColorSequence {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field("__type", "ColorSequence");
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
            Ok(matches!(&o, Value::UserData(u) if u.borrow::<ColorSequence>().is_ok_and(|o| *s == *o)))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, s, ()| {
            Ok(s.keypoints.iter().map(keypoint_string).collect::<String>())
        });
        strict(methods, "ColorSequence");
    }
}

/// Installs `Color3`, `ColorSequence` and `ColorSequenceKeypoint`.
pub(crate) fn install(lua: &Lua, globals: &Table) -> Result<()> {
    let three = |a: &Value, b: &Value, c: &Value, f: &str| -> Result<(f64, f64, f64)> {
        Ok((
            num_arg(a, 1, f, Some(0.0))?,
            num_arg(b, 2, f, Some(0.0))?,
            num_arg(c, 3, f, Some(0.0))?,
        ))
    };
    let new = lua.create_function(move |_, (r, g, b): (Value, Value, Value)| {
        let (r, g, b) = three(&r, &g, &b, "new")?;
        Ok(Color3::new(r as f32, g as f32, b as f32))
    })?;
    let from_rgb = lua.create_function(move |_, (r, g, b): (Value, Value, Value)| {
        let (r, g, b) = three(&r, &g, &b, "fromRGB")?;
        Ok(Color3::from_rgb(r, g, b))
    })?;
    let from_hsv = lua.create_function(move |_, (h, s, v): (Value, Value, Value)| {
        let (h, s, v) = three(&h, &s, &v, "fromHSV")?;
        Ok(Color3::from_hsv(h, s, v))
    })?;
    let to_hsv = lua.create_function(|_, c: Value| {
        Ok(super::ud_arg::<Color3>(&c, 1, "toHSV", "Color3")?.to_hsv())
    })?;
    let from_hex = lua.create_function(|_, s: Value| match &s {
        Value::String(text) => Color3::from_hex(&text.to_str()?).ok_or_else(|| {
            bad_argument(
                1,
                "fromHex",
                format!("{:?} is not a hex colour", text.to_string_lossy()),
            )
        }),
        other => Err(bad_argument(
            1,
            "fromHex",
            format!("string expected, got {}", type_name(other)),
        )),
    })?;
    set_lib(
        lua,
        globals,
        "Color3",
        vec![
            ("new", Value::Function(new)),
            ("fromRGB", Value::Function(from_rgb)),
            ("fromHSV", Value::Function(from_hsv)),
            ("fromHex", Value::Function(from_hex)),
            ("toHSV", Value::Function(to_hsv)),
        ],
    )?;

    let kp_new = lua.create_function(|_, (t, c): (Value, Value)| {
        Ok(ColorSequenceKeypoint {
            time: num_arg(&t, 1, "new", None)? as f32,
            value: super::ud_arg::<Color3>(&c, 2, "new", "Color3")?,
        })
    })?;
    set_lib(
        lua,
        globals,
        "ColorSequenceKeypoint",
        vec![("new", Value::Function(kp_new))],
    )?;

    let seq_new = lua.create_function(|_, (a, b): (Value, Value)| {
        let keypoints = match (&a, &b) {
            (Value::Table(t), Value::Nil) => {
                let ks: Vec<ColorSequenceKeypoint> = keypoint_list(t, "ColorSequenceKeypoint")?;
                check_keypoint_times(ks.iter().map(|k| k.time))?;
                ks
            }
            (Value::UserData(_), _) => {
                let c0 = super::ud_arg::<Color3>(&a, 1, "new", "Color3")?;
                let c1 = match &b {
                    Value::Nil => c0,
                    _ => super::ud_arg::<Color3>(&b, 2, "new", "Color3")?,
                };
                vec![
                    ColorSequenceKeypoint {
                        time: 0.0,
                        value: c0,
                    },
                    ColorSequenceKeypoint {
                        time: 1.0,
                        value: c1,
                    },
                ]
            }
            _ => {
                return Err(bad_argument(
                    1,
                    "new",
                    format!(
                        "Color3 or table of keypoints expected, got {}",
                        type_name(&a)
                    ),
                ));
            }
        };
        Ok(ColorSequence { keypoints })
    })?;
    set_lib(
        lua,
        globals,
        "ColorSequence",
        vec![("new", Value::Function(seq_new))],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_hsv() {
        let c = Color3::from_hex("#FF8000").unwrap();
        assert_eq!(c, Color3::from_rgb(255.0, 128.0, 0.0));
        assert_eq!(c.to_hex(), "FF8000");
        assert_eq!(Color3::from_hex("f00").unwrap(), Color3::new(1.0, 0.0, 0.0));
        assert!(Color3::from_hex("xyz").is_none());
        let (h, s, v) = Color3::new(0.0, 1.0, 0.0).to_hsv();
        assert!((h - 1.0 / 3.0).abs() < 1e-9 && s == 1.0 && v == 1.0);
        let back = Color3::from_hsv(h, s, v);
        assert_eq!(back, Color3::new(0.0, 1.0, 0.0));
    }
}
