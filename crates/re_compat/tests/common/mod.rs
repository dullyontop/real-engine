//! Shared helpers for the re_compat tests.

#![allow(dead_code)]

use std::sync::Arc;

use re_compat::data::ClassFile;
use re_compat::{Accessor, ClassMarker, ClassRegistry, ClassRegistryBuilder, Converter};
use re_reflect::Value;

pub const TEST_CLASSES: &str = include_str!("../fixtures/class/test_classes.json");

/// The `double` converter: the property is twice the field.
pub fn double() -> Converter {
    Converter {
        to_field: Arc::new(|v| match v {
            Value::Vec3(v) => Ok(Value::Vec3(*v * 0.5)),
            other => Ok(other.clone()),
        }),
        from_field: Arc::new(|v| match v {
            Value::Vec3(v) => Value::Vec3(*v * 2.0),
            other => other.clone(),
        }),
    }
}

/// `ClassName` read from the class marker.
pub fn class_name() -> Accessor {
    Accessor {
        get: Arc::new(|world, e| {
            Ok(Value::Str(
                world
                    .ecs
                    .get::<ClassMarker>(e)
                    .map(|m| m.class.clone())
                    .unwrap_or_default(),
            ))
        }),
        set: None,
    }
}

/// The builder of the built-in data plus the test classes, with their
/// converter and accessor.
pub fn test_builder() -> ClassRegistryBuilder {
    instance_accessors(
        ClassRegistryBuilder::builtin()
            .unwrap()
            .classes(
                "tests/fixtures/class/test_classes.json",
                ClassFile::parse("test_classes.json", TEST_CLASSES).unwrap(),
            )
            .converter("double", double()),
    )
}

/// Registers the `Instance` accessors (`Name`, `Parent`, `ClassName`).
pub fn instance_accessors(mut b: ClassRegistryBuilder) -> ClassRegistryBuilder {
    for (prop, a) in re_compat::instance::accessors() {
        b = b.accessor("Instance", prop, a);
    }
    for (class, prop, a) in re_compat::physics::accessors() {
        b = b.accessor(class, prop, a);
    }
    b
}

pub fn test_registry() -> ClassRegistry {
    test_builder().build().unwrap()
}
