//! `re_reflect` tests: by-name access, coercion and its error texts (the core
//! spike report's §5 cheat-sheet, verbatim), the f32 text round trip, the
//! registry description and equality-checked writes.
//!
//! The components here are local test components with the spike's shapes
//! (its four-field `RigidBody`), so the cheat-sheet texts are reproduced
//! exactly.

use bevy_ecs::entity::Entity;
use bevy_ecs::world::World;
use glam::{Quat, Vec3};
use re_reflect::{
    Color, ComponentKey, Doc, FieldType, ReflectError, Registry, Value, component,
    f32_from_parsed_f64, field_enum,
};

component! {
    /// Position, rotation and scale relative to the parent entity (or to the
    /// world for a root entity).
    pub struct Transform {
        /// Offset from the parent's origin, in metres.
        pub translation: Vec3 = Vec3::ZERO,
        /// Orientation relative to the parent. Normalised on write.
        pub rotation: Quat = Quat::IDENTITY,
        /// Per-axis scale relative to the parent.
        pub scale: Vec3 = Vec3::ONE,
    }
}

field_enum! {
    /// How the physics engine treats a body.
    pub enum BodyKind { Static, Dynamic, Kinematic }
}

component! {
    /// Makes the entity a physics body.
    pub struct RigidBody {
        /// Static, dynamic or kinematic.
        pub kind: BodyKind = BodyKind::Static,
        /// Mass in kilograms (dynamic bodies only).
        pub mass: f32 = 1.0,
        /// Friction coefficient.
        pub friction: f32 = 0.5,
        /// Bounciness.
        pub restitution: f32 = 0.0,
    }
}

component! {
    /// Omnidirectional light.
    pub struct PointLight {
        /// The colour.
        pub color: Color = Color::WHITE,
        /// Luminous intensity in candela.
        pub intensity: f32 = 800.0,
        /// Distance in metres beyond which the light has no effect.
        pub range: f32 = 20.0,
        /// Whether it casts shadows.
        pub shadows: bool = false,
    }
}

component! {
    /// Rigidly attaches this body to another entity.
    pub struct Weld {
        /// The entity this one is welded to.
        pub other: Option<Entity> = None,
    }
}

struct Fixture {
    world: World,
    registry: Registry,
    names: Vec<(String, Entity)>,
}

impl Fixture {
    fn new() -> Self {
        let mut world = World::new();
        let mut registry = Registry::default();
        registry.register::<Transform>(&mut world);
        registry.register::<RigidBody>(&mut world);
        registry.register::<PointLight>(&mut world);
        registry.register::<Weld>(&mut world);
        Fixture {
            world,
            registry,
            names: Vec::new(),
        }
    }

    fn spawn(&mut self, name: &str, components: &[(&str, &[(&str, Doc)])]) -> Entity {
        let e = self.world.spawn_empty().id();
        self.names.push((name.to_string(), e));
        for (cname, fields) in components {
            let key = self.registry.key(cname).unwrap();
            let defaults = self.registry.schema(key).defaults();
            self.registry
                .insert(&mut self.world, e, key, defaults)
                .unwrap();
            for (f, v) in *fields {
                self.set(name, cname, f, v.clone()).unwrap();
            }
        }
        e
    }

    fn entity(&self, name: &str) -> Entity {
        self.names.iter().find(|(n, _)| n == name).unwrap().1
    }

    fn key_index(
        &self,
        component: &str,
        field: &str,
    ) -> Result<(ComponentKey, usize), ReflectError> {
        let key = self.registry.key(component)?;
        let index = self.registry.schema(key).field_index(field)?;
        Ok((key, index))
    }

    fn get(&self, name: &str, component: &str, field: &str) -> Result<Value, ReflectError> {
        let (key, index) = self.key_index(component, field)?;
        Ok(self
            .registry
            .get_field(&self.world, self.entity(name), key, index)
            .unwrap())
    }

    fn get_doc(&self, name: &str, component: &str, field: &str) -> Doc {
        let (key, index) = self.key_index(component, field).unwrap();
        let v = self.get(name, component, field).unwrap();
        let ty = &self.registry.schema(key).fields[index].ty;
        Doc::from_value(&v, ty, &|e| {
            self.names
                .iter()
                .find(|(_, x)| *x == e)
                .map(|(n, _)| n.clone())
        })
    }

    /// Sets a field from text-boundary data, as a command would.
    fn set(
        &mut self,
        name: &str,
        component: &str,
        field: &str,
        value: Doc,
    ) -> Result<(), ReflectError> {
        let (key, index) = self.key_index(component, field)?;
        let schema = self.registry.schema(key);
        let names = &self.names;
        let value = value
            .to_value(&schema.fields[index].ty, &|id| {
                names.iter().find(|(n, _)| n == id).map(|(_, e)| *e)
            })
            .map_err(|error| ReflectError::Value {
                path: format!("{component}.{field}"),
                error,
            })?;
        let e = self.entity(name);
        self.registry
            .set_field(&mut self.world, e, key, index, value)
    }
}

fn v3(x: f32, y: f32, z: f32) -> Doc {
    Doc::List(vec![Doc::F32(x), Doc::F32(y), Doc::F32(z)])
}

fn sample() -> Fixture {
    let mut f = Fixture::new();
    f.spawn(
        "crate",
        &[
            ("Transform", &[("translation", v3(0.0, 5.0, 0.0))]),
            (
                "RigidBody",
                &[("kind", Doc::Str("Dynamic".into())), ("mass", Doc::Int(20))],
            ),
        ],
    );
    f.spawn("weld", &[("Weld", &[("other", Doc::Str("crate".into()))])]);
    f
}

#[test]
fn by_name_access_and_coercion() {
    let mut f = sample();
    assert_eq!(
        f.get("crate", "RigidBody", "mass").unwrap(),
        Value::Float(20.0)
    );
    assert_eq!(
        f.get("crate", "RigidBody", "kind").unwrap(),
        Value::Enum("Dynamic".into())
    );
    let crate_e = f.entity("crate");
    assert_eq!(
        f.get("weld", "Weld", "other").unwrap(),
        Value::Entity(Some(crate_e))
    );
    assert_eq!(f.get_doc("weld", "Weld", "other"), Doc::Str("crate".into()));

    // Typo'd names produce suggestions.
    let err = f.get("crate", "RigidBdy", "mass").unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"unknown component "RigidBdy" (did you mean "RigidBody"?)"#
    );
    let err = f.get("crate", "RigidBody", "mas").unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"component RigidBody has no field "mas" (did you mean "mass"?); fields are: kind, mass, friction, restitution"#
    );
    assert_eq!(err.code().to_string(), "RE1002");

    // f64 from a script rounds to f32; non-integral floats are rejected for ints.
    f.set("crate", "RigidBody", "mass", Doc::F64(0.1)).unwrap();
    assert_eq!(
        f.get("crate", "RigidBody", "mass").unwrap(),
        Value::Float(0.1f32 as f64)
    );
    assert_eq!(f.get_doc("crate", "RigidBody", "mass"), Doc::F32(0.1));
    let err = f
        .set("crate", "RigidBody", "mass", Doc::Str("heavy".into()))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "RigidBody.mass: expected f32, found string \"heavy\""
    );
    assert_eq!(err.code().to_string(), "RE1003");
    let err = f
        .set("crate", "RigidBody", "kind", Doc::Str("Floaty".into()))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "RigidBody.kind: unknown variant \"Floaty\", expected one of Static, Dynamic, Kinematic"
    );
    assert_eq!(err.code().to_string(), "RE1007");

    // Wrong vector arity.
    let err = f
        .set(
            "crate",
            "Transform",
            "translation",
            Doc::List(vec![Doc::Int(0), Doc::Int(1)]),
        )
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "Transform.translation: expected vec3 as [x, y, z], found list of 2"
    );

    // Vectors also accept {x, y, z}; quaternions are normalised.
    let map = Doc::Map(
        [("x", 1), ("y", 2), ("z", 3)]
            .map(|(k, v)| (k.into(), Doc::Int(v)))
            .into(),
    );
    f.set("crate", "Transform", "translation", map).unwrap();
    assert_eq!(
        f.get("crate", "Transform", "translation").unwrap(),
        Value::Vec3(Vec3::new(1.0, 2.0, 3.0))
    );
    f.set(
        "crate",
        "Transform",
        "rotation",
        Doc::List(vec![Doc::Int(0), Doc::Int(0), Doc::Int(0), Doc::Int(2)]),
    )
    .unwrap();
    assert_eq!(
        f.get("crate", "Transform", "rotation").unwrap(),
        Value::Quat(Quat::IDENTITY)
    );

    // Out of range and non-finite.
    let err = f
        .set("crate", "RigidBody", "mass", Doc::F64(1e300))
        .unwrap_err();
    assert_eq!(err.code().to_string(), "RE1004");
    let err = f
        .set("crate", "RigidBody", "mass", Doc::F64(f64::NAN))
        .unwrap_err();
    assert_eq!(err.code().to_string(), "RE1006");
}

#[test]
fn double_rounding_f32_is_recovered() {
    // 7.038531e-26 and its negative are the two finite f32 values whose
    // shortest text does not survive text -> f64 -> f32 (found by the
    // f32_exhaustive example).
    for bits in [0x15ae43fd_u32, 0x95ae43fd] {
        let f = f32::from_bits(bits);
        let text = serde_json::to_string(&f).unwrap();
        assert_eq!(text.trim_start_matches('-'), "7.038531e-26");
        let back: f64 = serde_json::from_str(&text).unwrap();
        assert_ne!((back as f32).to_bits(), f.to_bits());
        assert_eq!(f32_from_parsed_f64(back).to_bits(), f.to_bits());

        // Through the Doc path a field gets the right value.
        let mut fx = Fixture::new();
        fx.spawn("a", &[("PointLight", &[("range", Doc::F64(back))])]);
        assert_eq!(
            fx.get("a", "PointLight", "range").unwrap(),
            Value::Float(f as f64)
        );
        let doc: Doc = serde_json::from_str(&text).unwrap();
        let v = doc.to_value(&FieldType::F32, &|_| None).unwrap();
        assert_eq!(v, Value::Float(f as f64));
    }
}

#[test]
fn integers_into_f32_round_once() {
    // 2^60 + 2^36 + 1: i64 -> f64 -> f32 rounds twice and lands one ulp
    // below the correctly rounded `i as f32`.
    let i: i64 = (1 << 60) + (1 << 36) + 1;
    assert_ne!((i as f64) as f32, i as f32);
    let want = Value::Float(i as f32 as f64);
    assert_eq!(FieldType::F32.coerce(Value::Int(i)).unwrap(), want);
    assert_eq!(
        Doc::Int(i).to_value(&FieldType::F32, &|_| None).unwrap(),
        want
    );
    let mut fx = Fixture::new();
    fx.spawn("a", &[("PointLight", &[("intensity", Doc::Int(i))])]);
    assert_eq!(fx.get("a", "PointLight", "intensity").unwrap(), want);
    // Vector components take the same path.
    let v = Doc::List(vec![Doc::Int(i), Doc::Int(0), Doc::Int(0)])
        .to_value(&FieldType::Vec3, &|_| None)
        .unwrap();
    assert_eq!(v, Value::Vec3(Vec3::new(i as f32, 0.0, 0.0)));
}

#[test]
fn every_f32_survives_text_via_f64() {
    // Text is parsed as f64 then recovered as f32. Check a deterministic
    // sample of more than 1,000,000 bit patterns plus the two double-rounding
    // values (the exhaustive 2^32 run is the f32_exhaustive example).
    let mut samples: Vec<u32> = (0..=u32::MAX).step_by(4093).collect();
    samples.extend([
        0x15ae43fd,
        0x95ae43fd,
        0x7f7f_ffff,
        0x0000_0001,
        0x8000_0000,
    ]);
    assert!(samples.len() >= 1_000_000);
    let mut checked = 0usize;
    for bits in samples {
        let f = f32::from_bits(bits);
        if !f.is_finite() {
            continue;
        }
        let text = serde_json::to_string(&Doc::F32(f)).unwrap();
        let doc: Doc = serde_json::from_str(&text).unwrap();
        let Value::Float(back) = doc.to_value(&FieldType::F32, &|_| None).unwrap() else {
            panic!("{text}")
        };
        assert_eq!((back as f32).to_bits(), bits, "{f:e} -> {text}");
        checked += 1;
    }
    assert!(checked >= 1_000_000, "{checked}");
}

#[test]
fn schema_lists_components_fields_and_docs() {
    let f = Fixture::new();
    let d = f.registry.describe();
    let t = d["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "Transform")
        .unwrap();
    assert_eq!(t["fields"][0]["name"], "translation");
    assert_eq!(t["fields"][0]["type"]["kind"], "vec3");
    assert_eq!(
        t["fields"][0]["doc"],
        "Offset from the parent's origin, in metres."
    );
    assert_eq!(
        t["fields"][2]["default"],
        serde_json::json!([1.0, 1.0, 1.0])
    );
    let rb = d["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "RigidBody")
        .unwrap();
    assert_eq!(
        rb["fields"][0]["type"]["variants"],
        serde_json::json!(["Static", "Dynamic", "Kinematic"])
    );
    let js = f.registry.scene_json_schema();
    assert_eq!(
        js["$defs"]["PointLight"]["properties"]["intensity"]["type"],
        "number"
    );
    assert_eq!(
        js["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
}

#[test]
fn equal_writes_do_not_trigger_change_detection() {
    use bevy_ecs::query::Changed;
    let mut f = sample();
    f.world.clear_trackers();
    let mut q = f.world.query_filtered::<Entity, Changed<Transform>>();
    assert_eq!(q.iter(&f.world).count(), 0);
    f.set("crate", "Transform", "translation", v3(0.0, 5.0, 0.0))
        .unwrap();
    assert_eq!(q.iter(&f.world).count(), 0, "same value: not changed");
    f.set("crate", "Transform", "translation", v3(0.0, 6.0, 0.0))
        .unwrap();
    assert_eq!(q.iter(&f.world).count(), 1);
}

#[test]
fn diagnostics_catalogue_is_complete() {
    let problems = re_core::validate_catalog(re_reflect::DIAGNOSTICS, re_reflect::CODE_RANGE);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(re_reflect::DIAGNOSTICS.len(), 11);
}
