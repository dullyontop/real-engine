//! Interpolating stored property values ([`re_reflect::Value`]) for tweens.
//!
//! | Value | Interpolation |
//! |---|---|
//! | number | linear; an integer field rounds to the nearest whole number |
//! | `Vector3`, `Color3` | per component |
//! | `CFrame` | position linearly, rotation by quaternion slerp |
//! | `Vector2`, `UDim`, `UDim2`, `Rect`, `NumberRange` and other records of numbers | per field |
//! | `bool` | the start value until the end, then the goal |
//!
//! Enum items, strings, instances and sequences cannot be tweened.

use glam::{Mat3, Quat, Vec3};
use re_reflect::{Color, Value};

/// Whether a property of data-file type `ty` holding `v` can be tweened.
pub fn tweenable(v: &Value) -> bool {
    match v {
        Value::Float(_) | Value::Int(_) | Value::Vec3(_) | Value::Color(_) | Value::Bool(_) => true,
        Value::Record(_, fields) => fields
            .iter()
            .all(|f| matches!(f, Value::Float(_) | Value::Int(_) | Value::Vec3(_))),
        _ => false,
    }
}

fn mix(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// The value at eased progress `t` (0 is `a`, 1 is `b`; `t` may leave
/// `[0, 1]` for overshooting curves). `None` when the two values do not have
/// the same shape.
pub fn lerp(a: &Value, b: &Value, t: f64) -> Option<Value> {
    Some(match (a, b) {
        (Value::Float(a), Value::Float(b)) => Value::Float(mix(*a, *b, t)),
        (Value::Int(a), Value::Int(b)) => Value::Int(mix(*a as f64, *b as f64, t).round() as i64),
        // Scripts give numbers as floats; an integer field keeps whole values.
        (Value::Int(a), Value::Float(b)) => Value::Int(mix(*a as f64, *b, t).round() as i64),
        (Value::Float(a), Value::Int(b)) => Value::Float(mix(*a, *b as f64, t)),
        (Value::Vec3(a), Value::Vec3(b)) => Value::Vec3(a.lerp(*b, t as f32)),
        (Value::Color(a), Value::Color(b)) => {
            let t = t as f32;
            Value::Color(Color::rgba(
                a.r + (b.r - a.r) * t,
                a.g + (b.g - a.g) * t,
                a.b + (b.b - a.b) * t,
                a.a + (b.a - a.a) * t,
            ))
        }
        (Value::Bool(a), Value::Bool(b)) => Value::Bool(if t >= 1.0 { *b } else { *a }),
        (Value::Record(ia, fa), Value::Record(ib, fb)) if ia == ib && fa.len() == fb.len() => {
            if ia.name() == "CFrame" {
                return cframe(fa, fb, t).map(|f| Value::Record(*ia, f));
            }
            let fields = fa
                .iter()
                .zip(fb)
                .map(|(x, y)| lerp(x, y, t))
                .collect::<Option<Vec<_>>>()?;
            Value::Record(*ia, fields)
        }
        _ => return None,
    })
}

/// A CFrame record is `Position`, `XVector`, `YVector`, `ZVector`.
fn cframe(a: &[Value], b: &[Value], t: f64) -> Option<Vec<Value>> {
    let vecs = |f: &[Value]| -> Option<[Vec3; 4]> {
        let mut out = [Vec3::ZERO; 4];
        for (o, v) in out.iter_mut().zip(f) {
            match v {
                Value::Vec3(v) => *o = *v,
                _ => return None,
            }
        }
        Some(out)
    };
    let [pa, xa, ya, za] = vecs(a)?;
    let [pb, xb, yb, zb] = vecs(b)?;
    let qa = Quat::from_mat3(&Mat3::from_cols(xa, ya, za)).normalize();
    let qb = Quat::from_mat3(&Mat3::from_cols(xb, yb, zb)).normalize();
    let q = qa.slerp(qb, t as f32);
    let m = Mat3::from_quat(q);
    let p = pa.lerp(pb, t as f32);
    Some(vec![
        Value::Vec3(p),
        Value::Vec3(m.x_axis),
        Value::Vec3(m.y_axis),
        Value::Vec3(m.z_axis),
    ])
}
