//! Typed reads and change-only writes of UI properties.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use glam::Vec2;
use re_reflect::{ComponentKey, RecordId, Value};
use re_scene::{Entity, World};

use crate::ClassRegistry;
use crate::class::{ClassMarker, Storage, own_component_name};
use crate::data::Status;
use crate::instance::tree;

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

/// A fast hasher for the per-frame caches (the keys are small numbers and
/// short names; nothing here faces untrusted input that could collide them
/// on purpose to any effect but a slower frame).
#[derive(Default, Clone, Copy)]
pub(crate) struct Fx(u64);

impl std::hash::Hasher for Fx {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut b = [0u8; 8];
            b[..chunk.len()].copy_from_slice(chunk);
            self.write_u64(u64::from_le_bytes(b));
        }
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = (self.0.rotate_left(5) ^ n).wrapping_mul(0x51_7c_c1_b7_27_22_0a_95);
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    fn write_u32(&mut self, n: u32) {
        self.write_u64(n as u64);
    }

    fn write_u8(&mut self, n: u8) {
        self.write_u64(n as u64);
    }
}

pub(crate) type FxMap<K, V> = HashMap<K, V, std::hash::BuildHasherDefault<Fx>>;

/// Where an own field lives: its component and its index in it.
pub(crate) type FieldAt = (ComponentKey, usize);

/// One frame's view of the world, with the lookups the UI repeats for every
/// element (where a class keeps a property, what it inherits from) cached.
pub(crate) struct Scope<'a> {
    pub w: &'a World,
    pub classes: &'a ClassRegistry,
    /// Class names to small numbers, so the caches below key on a number.
    ids: RefCell<FxMap<String, usize>>,
    ents: RefCell<FxMap<Entity, usize>>,
    fields: RefCell<FxMap<(usize, usize), Option<FieldAt>>>,
    isa: RefCell<FxMap<(usize, usize), bool>>,
    decor: RefCell<FxMap<Entity, Rc<Decor>>>,
}

/// An element's children as the UI sees them: its `GuiObject` children in
/// child order, and the first of each modifier.
#[derive(Debug, Default)]
pub(crate) struct Decor {
    pub kids: Vec<Entity>,
    pub layout: Option<Entity>,
    pub padding: Option<Entity>,
    pub scale: Option<Entity>,
    pub size: Option<Entity>,
    pub aspect: Option<Entity>,
    pub corner: Option<Entity>,
    pub gradient: Option<Entity>,
    pub strokes: Vec<Entity>,
}

impl<'a> Scope<'a> {
    pub fn new(w: &'a World, classes: &'a ClassRegistry) -> Scope<'a> {
        Scope {
            w,
            classes,
            ids: RefCell::default(),
            ents: RefCell::default(),
            fields: RefCell::default(),
            isa: RefCell::default(),
            decor: RefCell::default(),
        }
    }

    /// The UI children of `e`, gathered once a frame.
    pub fn decor(&self, e: Entity) -> Rc<Decor> {
        if let Some(d) = self.decor.borrow().get(&e) {
            return d.clone();
        }
        let mut d = Decor::default();
        for c in tree::children(self.w, e) {
            let first = |slot: &mut Option<Entity>| {
                if slot.is_none() {
                    *slot = Some(c);
                }
            };
            if self.is_a(c, "GuiObject") {
                d.kids.push(c);
            } else if self.is_a(c, "UILayout") {
                first(&mut d.layout);
            } else if self.is_a(c, "UIPadding") {
                first(&mut d.padding);
            } else if self.is_a(c, "UIScale") {
                first(&mut d.scale);
            } else if self.is_a(c, "UISizeConstraint") {
                first(&mut d.size);
            } else if self.is_a(c, "UIAspectRatioConstraint") {
                first(&mut d.aspect);
            } else if self.is_a(c, "UICorner") {
                first(&mut d.corner);
            } else if self.is_a(c, "UIGradient") {
                first(&mut d.gradient);
            } else if self.is_a(c, "UIStroke") {
                d.strokes.push(c);
            }
        }
        let d = Rc::new(d);
        self.decor.borrow_mut().insert(e, d.clone());
        d
    }

    pub fn props(&'a self, e: Entity) -> Props<'a> {
        let c = self.class_of(e);
        Props { s: self, e, c }
    }

    /// The class of `e`, as a number and a name.
    fn class_of(&self, e: Entity) -> Option<(usize, &'a str)> {
        let c = self.class(e)?;
        if let Some(&i) = self.ents.borrow().get(&e) {
            return Some((i, c));
        }
        let i = self.class_id(c);
        self.ents.borrow_mut().insert(e, i);
        Some((i, c))
    }

    fn class(&self, e: Entity) -> Option<&'a str> {
        self.w.ecs.get::<ClassMarker>(e).map(|m| m.class.as_str())
    }

    fn class_id(&self, class: &str) -> usize {
        if let Some(&i) = self.ids.borrow().get(class) {
            return i;
        }
        let mut ids = self.ids.borrow_mut();
        let i = ids.len();
        ids.insert(class.to_string(), i);
        i
    }

    /// True if `e` is an instance of `class` or a subclass.
    pub fn is_a(&self, e: Entity, class: &'static str) -> bool {
        let Some((i, c)) = self.class_of(e) else {
            return false;
        };
        let key = (i, class.as_ptr() as usize);
        self.isa_with(key, c, class)
    }

    fn isa_with(&self, key: (usize, usize), c: &str, class: &'static str) -> bool {
        if let Some(&b) = self.isa.borrow().get(&key) {
            return b;
        }
        let b = self.classes.is_a(c, class);
        self.isa.borrow_mut().insert(key, b);
        b
    }

    /// Where `class` keeps own property `name`: its component and field,
    /// `None` when it is not an implemented own property.
    fn field(&self, cid: usize, class: &str, name: &'static str) -> Option<FieldAt> {
        let key = (cid, name.as_ptr() as usize);
        if let Some(&f) = self.fields.borrow().get(&key) {
            return f;
        }
        let f = self.classes.ancestry(class).into_iter().find_map(|d| {
            let p = d.properties.iter().find(|p| p.name == name)?;
            if !matches!(p.storage, Storage::Own { .. })
                || !matches!(p.status, Status::Implemented | Status::Partial)
            {
                return None;
            }
            let k = self.w.registry.try_key(&own_component_name(&d.name))?;
            let i = self.w.registry.schema(k).field_index(name).ok()?;
            Some((k, i))
        });
        self.fields.borrow_mut().insert(key, f);
        f
    }

    /// Property `name` of `e`, whose class is `c` (number and name).
    fn get(&self, e: Entity, c: (usize, &str), name: &'static str) -> Option<Value> {
        match self.field(c.0, c.1, name) {
            Some((k, i)) => self.w.registry.get_field(&self.w.ecs, e, k, i),
            None => self.classes.get_property(self.w, e, name).ok(),
        }
    }

    /// Where a write of own property `name` of `e` goes.
    pub fn target(&self, e: Entity, name: &'static str) -> Option<FieldAt> {
        let (i, c) = self.class_of(e)?;
        self.field(i, c, name)
    }
}

/// Reads of one instance's properties, with fallbacks for values of the
/// wrong form (which the class layer does not let scripts write).
pub(crate) struct Props<'a> {
    pub s: &'a Scope<'a>,
    pub e: Entity,
    c: Option<(usize, &'a str)>,
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
    pub fn get(&self, name: &'static str) -> Option<Value> {
        self.s.get(self.e, self.c?, name)
    }

    pub fn f32(&self, name: &'static str, or: f32) -> f32 {
        self.get(name)
            .as_ref()
            .and_then(num)
            .map_or(or, |x| x as f32)
    }

    pub fn int(&self, name: &'static str, or: i64) -> i64 {
        match self.get(name) {
            Some(Value::Int(i)) => i,
            Some(Value::Float(f)) => f as i64,
            _ => or,
        }
    }

    pub fn bool(&self, name: &'static str, or: bool) -> bool {
        match self.get(name) {
            Some(Value::Bool(b)) => b,
            _ => or,
        }
    }

    pub fn string(&self, name: &'static str) -> String {
        match self.get(name) {
            Some(Value::Str(s)) => s,
            _ => String::new(),
        }
    }

    pub fn item(&self, name: &'static str, or: &str) -> String {
        match self.get(name) {
            Some(Value::Enum(s)) => s,
            _ => or.to_string(),
        }
    }

    pub fn entity(&self, name: &'static str) -> Option<Entity> {
        match self.get(name) {
            Some(Value::Entity(e)) => e,
            _ => None,
        }
    }

    pub fn vec2(&self, name: &'static str, or: Vec2) -> Vec2 {
        self.get(name)
            .and_then(|v| fields(&v, "Vector2"))
            .map_or(or, |f| Vec2::new(f[0] as f32, f[1] as f32))
    }

    pub fn vec3(&self, name: &'static str) -> glam::Vec3 {
        match self.get(name) {
            Some(Value::Vec3(v)) => v,
            _ => glam::Vec3::ZERO,
        }
    }

    pub fn udim(&self, name: &'static str) -> UDim {
        self.get(name)
            .and_then(|v| fields(&v, "UDim"))
            .map_or(UDim::default(), |f| UDim {
                scale: f[0] as f32,
                offset: f[1] as f32,
            })
    }

    pub fn udim2(&self, name: &'static str) -> UDim2 {
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

    pub fn color(&self, name: &'static str, or: Rgba) -> Rgba {
        match self.get(name) {
            Some(Value::Color(c)) => Rgba([c.r, c.g, c.b, 1.0]),
            _ => or,
        }
    }

    /// A `NumberSequence` or `ColorSequence` as `(time, value)` keypoints,
    /// the value being the number, or the colour's channels.
    pub fn sequence(&self, name: &'static str) -> Vec<(f32, [f32; 3])> {
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

/// Writes a resolved own field; an equal value changes nothing (the
/// registry does not mark it changed), so Changed fires only for real
/// changes.
pub(crate) fn write(w: &mut World, e: Entity, at: (ComponentKey, usize), v: Value) {
    if tree::alive(w, e) {
        let _ = w.registry.set_field(&mut w.ecs, e, at.0, at.1, v);
    }
}
