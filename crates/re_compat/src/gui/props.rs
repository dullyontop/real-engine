//! Typed reads and change-only writes of UI properties.

use glam::Vec2;
use re_reflect::{RecordId, Value};
use re_scene::{Entity, World};

use crate::ClassRegistry;
use crate::instance::tree;
use crate::tween::set_own;

/// A `UDim`: a fraction of a length plus pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct UDim {
    pub scale: f32,
    pub offset: f32,
}

impl UDim {
    pub fn of(self, length: f32) -> f32 {
        self.scale * length + self.offset
    }
}

/// A `UDim2`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct UDim2 {
    pub x: UDim,
    pub y: UDim,
}

impl UDim2 {
    pub fn of(self, size: Vec2) -> Vec2 {
        Vec2::new(self.x.of(size.x), self.y.of(size.y))
    }
}

/// An sRGB colour with alpha, channels 0 to 1 as scripts give them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Rgba(pub [f32; 4]);

impl Rgba {
    pub const WHITE: Rgba = Rgba([1.0, 1.0, 1.0, 1.0]);

    pub fn with_transparency(self, t: f32) -> Rgba {
        let mut c = self.0;
        c[3] *= (1.0 - t).clamp(0.0, 1.0);
        Rgba(c)
    }

    pub fn mul(self, o: Rgba) -> Rgba {
        let (a, b) = (self.0, o.0);
        Rgba([a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]])
    }

    pub fn egui(self) -> egui::Color32 {
        let c = self.0.map(|x| (x.clamp(0.0, 1.0) * 255.0).round() as u8);
        egui::Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3])
    }
}

/// Reads of one instance's properties, with fallbacks for values of the
/// wrong form (which the class layer does not let scripts write).
pub(crate) struct Props<'a> {
    pub w: &'a World,
    pub classes: &'a ClassRegistry,
    pub e: Entity,
}

fn num(v: &Value) -> Option<f64> {
    match v {
        Value::Float(f) => Some(*f),
        Value::Int(i) => Some(*i as f64),
        _ => None,
    }
}

fn fields(v: &Value, record: &str) -> Option<Vec<f64>> {
    match v {
        Value::Record(id, fs) if id.name() == record => fs.iter().map(num).collect(),
        _ => None,
    }
}

impl Props<'_> {
    pub fn get(&self, name: &str) -> Option<Value> {
        self.classes.get_property(self.w, self.e, name).ok()
    }

    pub fn f32(&self, name: &str, or: f32) -> f32 {
        self.get(name)
            .as_ref()
            .and_then(num)
            .map_or(or, |x| x as f32)
    }

    pub fn int(&self, name: &str, or: i64) -> i64 {
        match self.get(name) {
            Some(Value::Int(i)) => i,
            Some(Value::Float(f)) => f as i64,
            _ => or,
        }
    }

    pub fn bool(&self, name: &str, or: bool) -> bool {
        match self.get(name) {
            Some(Value::Bool(b)) => b,
            _ => or,
        }
    }

    pub fn string(&self, name: &str) -> String {
        match self.get(name) {
            Some(Value::Str(s)) => s,
            _ => String::new(),
        }
    }

    pub fn item(&self, name: &str, or: &str) -> String {
        match self.get(name) {
            Some(Value::Enum(s)) => s,
            _ => or.to_string(),
        }
    }

    pub fn entity(&self, name: &str) -> Option<Entity> {
        match self.get(name) {
            Some(Value::Entity(e)) => e,
            _ => None,
        }
    }

    pub fn vec2(&self, name: &str, or: Vec2) -> Vec2 {
        self.get(name)
            .and_then(|v| fields(&v, "Vector2"))
            .map_or(or, |f| Vec2::new(f[0] as f32, f[1] as f32))
    }

    pub fn vec3(&self, name: &str) -> glam::Vec3 {
        match self.get(name) {
            Some(Value::Vec3(v)) => v,
            _ => glam::Vec3::ZERO,
        }
    }

    pub fn udim(&self, name: &str) -> UDim {
        self.get(name)
            .and_then(|v| fields(&v, "UDim"))
            .map_or(UDim::default(), |f| UDim {
                scale: f[0] as f32,
                offset: f[1] as f32,
            })
    }

    pub fn udim2(&self, name: &str) -> UDim2 {
        self.get(name)
            .and_then(|v| fields(&v, "UDim2"))
            .map_or(UDim2::default(), |f| UDim2 {
                x: UDim {
                    scale: f[0] as f32,
                    offset: f[1] as f32,
                },
                y: UDim {
                    scale: f[2] as f32,
                    offset: f[3] as f32,
                },
            })
    }

    pub fn color(&self, name: &str, or: Rgba) -> Rgba {
        match self.get(name) {
            Some(Value::Color(c)) => Rgba([c.r, c.g, c.b, 1.0]),
            _ => or,
        }
    }

    /// A `NumberSequence` or `ColorSequence` as `(time, value)` keypoints,
    /// the value being the number, or the colour's channels.
    pub fn sequence(&self, name: &str) -> Vec<(f32, [f32; 3])> {
        let Some(Value::List(ks)) = self.get(name) else {
            return Vec::new();
        };
        ks.iter()
            .filter_map(|k| match k {
                Value::Record(_, fs) if fs.len() >= 2 => {
                    let t = num(&fs[0])? as f32;
                    let v = match &fs[1] {
                        Value::Color(c) => [c.r, c.g, c.b],
                        other => [num(other)? as f32; 3],
                    };
                    Some((t, v))
                }
                _ => None,
            })
            .collect()
    }

    /// The font face (family stem, weight, italic).
    pub fn font_face(&self) -> (String, String, bool) {
        match self.get("FontFace") {
            Some(Value::Record(_, fs)) if fs.len() == 3 => {
                let family = match &fs[0] {
                    Value::Str(s) => s.clone(),
                    _ => String::new(),
                };
                let weight = match &fs[1] {
                    Value::Enum(s) => s.clone(),
                    _ => "Regular".into(),
                };
                let italic = matches!(&fs[2], Value::Enum(s) if s == "Italic");
                (family, weight, italic)
            }
            _ => (String::new(), "Regular".into(), false),
        }
    }
}

/// A `Vector2` value.
pub(crate) fn vec2_value(v: Vec2) -> Value {
    let id = RecordId::by_name("Vector2").expect("the Vector2 record is registered");
    Value::Record(id, vec![Value::Float(v.x as f64), Value::Float(v.y as f64)])
}

/// Writes own property `name` (read-only ones included) when its value
/// changed, so Changed fires only for real changes. Returns whether it wrote.
pub(crate) fn write(
    w: &mut World,
    classes: &ClassRegistry,
    e: Entity,
    name: &str,
    v: Value,
) -> bool {
    if classes.get_property(w, e, name).ok().as_ref() == Some(&v) {
        return false;
    }
    let Some(class) = tree::class(w, e).and_then(|c| {
        classes
            .ancestry(c)
            .into_iter()
            .find(|d| d.properties.iter().any(|p| p.name == name))
            .map(|d| d.name.clone())
    }) else {
        return false;
    };
    set_own(w, e, &class, name, v).is_ok()
}
