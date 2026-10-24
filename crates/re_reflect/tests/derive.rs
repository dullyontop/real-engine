//! `#[derive(Reflect)]` gives the same schema and field access as
//! `component!`, leaves `#[reflect(skip)]` fields out, and takes the
//! `re_reflect` path from `#[reflect(crate = "...")]`.

use bevy_ecs::component::Component;
use bevy_ecs::world::World;
use glam::Vec3;
use re_reflect::{FieldType, Reflect, Registry, Value, component};

component! {
    /// A wind source.
    pub struct WindMacro {
        /// Direction and strength, in metres per second.
        pub velocity: Vec3 = Vec3::new(3.0, 0.0, 0.0),
        /// Whether it gusts.
        pub gusts: bool = true,
        pub label: String = String::from("breeze"),
    }
}

/// A wind source.
#[derive(Component, Clone, Debug, PartialEq, Reflect)]
pub struct WindDerive {
    /// Direction and strength, in metres per second.
    pub velocity: Vec3,
    /// Whether it gusts.
    pub gusts: bool,
    pub label: String,
    /// Not data: rebuilt at run time.
    #[reflect(skip)]
    pub cache: Vec<f32>,
}

impl Default for WindDerive {
    fn default() -> Self {
        Self {
            velocity: Vec3::new(3.0, 0.0, 0.0),
            gusts: true,
            label: String::from("breeze"),
            cache: vec![1.0],
        }
    }
}

mod renamed {
    pub use re_reflect as engine_reflect;
}

/// A marker.
#[derive(Component, Clone, Debug, Default, PartialEq, Reflect)]
#[reflect(crate = "renamed::engine_reflect")]
pub struct Marker {}

#[test]
fn derive_matches_component_macro() {
    let a = <WindMacro as re_reflect::registry::Reflect>::schema();
    let b = <WindDerive as re_reflect::registry::Reflect>::schema();
    assert_eq!(b.name, "WindDerive");
    assert_eq!(a.doc, b.doc);
    assert_eq!(a.fields, b.fields, "same fields, docs, types and defaults");
    assert_eq!(b.fields[0].ty, FieldType::Vec3);
    assert_eq!(
        b.fields[0].doc,
        "Direction and strength, in metres per second."
    );
    assert_eq!(b.fields.len(), 3, "the skipped field is not in the schema");

    let marker = <Marker as re_reflect::registry::Reflect>::schema();
    assert!(marker.fields.is_empty());
    assert_eq!(marker.doc, "A marker.");
}

#[test]
fn derived_fields_are_read_and_written_by_name() {
    let mut world = World::new();
    let mut registry = Registry::default();
    let key = registry.register::<WindDerive>(&mut world);
    let e = world.spawn(WindDerive::default()).id();
    let gusts = registry.schema(key).field_index("gusts").unwrap();
    registry
        .set_field(&mut world, e, key, gusts, Value::Bool(false))
        .unwrap();
    let label = registry.schema(key).field_index("label").unwrap();
    registry
        .set_field(&mut world, e, key, label, Value::Str("gale".into()))
        .unwrap();
    let w = world.get::<WindDerive>(e).unwrap();
    assert!(!w.gusts);
    assert_eq!(w.label, "gale");
    assert_eq!(w.cache, vec![1.0], "skipped fields are left alone");
    assert_eq!(
        registry.get_field(&world, e, key, gusts),
        Some(Value::Bool(false))
    );
}
