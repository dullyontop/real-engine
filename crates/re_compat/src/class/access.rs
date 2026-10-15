//! Registering the class layer's components in a world, and reading and
//! writing properties of instances.

use re_reflect::{ComponentOrigin, ComponentSchema, FieldSchema, Value};
use re_scene::{Entity, World};

use super::{ClassDef, ClassMarker, ClassRegistry, Member, Storage};
use crate::error::CompatError;
use crate::scene::ServiceTag;

/// The name of a class's own component: `Compat_<Class>`.
pub fn own_component_name(class: &str) -> String {
    format!("Compat_{class}")
}

fn register_native<T: re_reflect::Reflect>(world: &mut World) {
    if world.registry.key_of::<T>().is_none() {
        world.registry.register::<T>(&mut world.ecs);
    }
}

impl ClassRegistry {
    /// The schema of `class`'s own component, if it declares own properties:
    /// one field per own property, in declaration order.
    pub fn own_component_schema(&self, class: &ClassDef) -> Option<ComponentSchema> {
        let name = class.own_component()?;
        let fields = class
            .properties
            .iter()
            .filter_map(|p| match &p.storage {
                Storage::Own { ty, default, .. } => Some(FieldSchema {
                    name: p.name.clone(),
                    ty: ty.clone(),
                    doc: p.doc.clone(),
                    default: default.clone(),
                }),
                _ => None,
            })
            .collect();
        Some(ComponentSchema {
            name,
            doc: format!(
                "The {} class's own properties (compatibility layer).",
                class.name
            ),
            fields,
            origin: ComponentOrigin::Host,
        })
    }

    /// Registers [`ClassMarker`], [`ServiceTag`] and every class's own
    /// component in `world`, as host components
    /// ([`ComponentOrigin::Host`]): scene files store their field values but
    /// not their schemas, so adding an own property, an enum item or a class
    /// never invalidates a saved scene (a field the file lacks takes its
    /// default). Call it before loading a compat scene.
    ///
    /// Idempotent: a component already registered under an own component's
    /// name must be a host component of the same shape. Also checks that every
    /// alias names a registered component and field and that every class's
    /// native components are registered. Everything is checked before anything
    /// is registered.
    pub fn register_components(&self, world: &mut World) -> Result<(), CompatError> {
        let mut new = Vec::new();
        for class in &self.classes {
            for c in &class.components {
                if world.registry.try_key(c).is_none() {
                    return Err(CompatError::InvalidData(format!(
                        "class {} adds the unknown component {c}",
                        class.name
                    )));
                }
            }
            for p in &class.properties {
                if let Storage::Alias {
                    component, field, ..
                } = &p.storage
                {
                    let ok = world
                        .registry
                        .try_key(component)
                        .is_some_and(|k| world.registry.schema(k).field_index(field).is_ok());
                    if !ok {
                        return Err(CompatError::InvalidData(format!(
                            "{}.{} aliases {component}.{field}, which is not a registered field",
                            class.name, p.name
                        )));
                    }
                }
            }
            let Some(schema) = self.own_component_schema(class) else {
                continue;
            };
            match world.registry.try_key(&schema.name) {
                Some(key) => {
                    let existing = world.registry.schema(key);
                    if existing.origin != ComponentOrigin::Host || !existing.same_shape(&schema) {
                        return Err(CompatError::Scene(format!(
                            "component {} is already registered with a different shape",
                            schema.name
                        )));
                    }
                }
                None => new.push(schema),
            }
        }
        register_native::<ClassMarker>(world);
        register_native::<ServiceTag>(world);
        for schema in new {
            world.registry.register_host(&mut world.ecs, schema)?;
        }
        Ok(())
    }

    /// The components an instance of `class` carries besides its
    /// [`ClassMarker`]: each class's native components and own component, from
    /// `Instance` down to `class`, each once.
    pub fn instance_components(&self, class: &str) -> Result<Vec<String>, CompatError> {
        let chain = self.chain(class)?;
        let mut out: Vec<String> = Vec::new();
        for c in chain.iter().rev() {
            for name in c.components.iter().cloned().chain(c.own_component()) {
                if !out.contains(&name) {
                    out.push(name);
                }
            }
        }
        Ok(out)
    }

    /// The class of an instance (from its [`ClassMarker`]); `None` if the
    /// entity is not an instance or its class is unknown.
    pub fn class_of(&self, world: &World, entity: Entity) -> Option<&ClassDef> {
        let marker = world.ecs.get::<ClassMarker>(entity)?;
        self.try_class(&marker.class)
    }

    fn property<'a>(
        &'a self,
        world: &World,
        entity: Entity,
        name: &str,
    ) -> Result<(&'a ClassDef, &'a super::PropertyDef), CompatError> {
        let Some(class) = self.class_of(world, entity) else {
            return Err(CompatError::Scene(format!(
                "entity {} is not an instance of a known class",
                world.id_of(entity).unwrap_or("(no id)")
            )));
        };
        let m = self.member(&class.name, name)?;
        let Member::Property(p) = m.member else {
            return Err(CompatError::NotAProperty {
                class: class.name.clone(),
                member: name.to_string(),
                kind: m.member.kind(),
            });
        };
        m.require_implemented()?;
        Ok((m.class, p))
    }

    /// Reads property `name` of an instance. Fails with `RE9001` for an
    /// unknown member or one that is not implemented.
    pub fn get_property(
        &self,
        world: &World,
        entity: Entity,
        name: &str,
    ) -> Result<Value, CompatError> {
        let (class, p) = self.property(world, entity, name)?;
        match &p.storage {
            Storage::Alias {
                component,
                field,
                via,
            } => {
                let v = read_field(world, entity, component, field)?;
                match via {
                    None => Ok(v),
                    Some(name) => Ok((self.require_converter(class, p, name)?.from_field)(&v)),
                }
            }
            Storage::Own { .. } => {
                read_field(world, entity, &own_component_name(&class.name), &p.name)
            }
            Storage::Builtin => match self.accessor(&class.name, &p.name) {
                Some(a) => (a.get)(world, entity),
                None => Err(CompatError::NotImplemented {
                    class: class.name.clone(),
                    member: p.name.clone(),
                    status: p.status,
                }),
            },
        }
    }

    /// Writes property `name` of an instance. Fails with `RE9001` for an
    /// unknown or unimplemented member, `RE9005` for a read-only one, and the
    /// reflection error when the value does not fit.
    pub fn set_property(
        &self,
        world: &mut World,
        entity: Entity,
        name: &str,
        value: Value,
    ) -> Result<(), CompatError> {
        let (class, p) = self.property(world, entity, name)?;
        let read_only = || CompatError::ReadOnly {
            class: class.name.clone(),
            property: p.name.clone(),
        };
        if p.read_only {
            return Err(read_only());
        }
        match &p.storage {
            Storage::Alias {
                component,
                field,
                via,
            } => {
                let v = match via {
                    None => value,
                    Some(name) => (self.require_converter(class, p, name)?.to_field)(&value)?,
                };
                write_field(world, entity, component, field, v)
            }
            Storage::Own { .. } => write_field(
                world,
                entity,
                &own_component_name(&class.name),
                &p.name,
                saturate(value),
            ),
            Storage::Builtin => match self.accessor(&class.name, &p.name) {
                Some(a) => match &a.set {
                    Some(set) => set(world, entity, value),
                    None => Err(read_only()),
                },
                None => Err(CompatError::NotImplemented {
                    class: class.name.clone(),
                    member: p.name.clone(),
                    status: p.status,
                }),
            },
        }
    }
}

impl ClassRegistry {
    /// The converter `name`, or `RE9001` naming the property when it is not
    /// registered (a registry built with `build_data_only`).
    fn require_converter(
        &self,
        class: &ClassDef,
        p: &super::PropertyDef,
        name: &str,
    ) -> Result<&super::Converter, CompatError> {
        self.converter(name)
            .ok_or_else(|| CompatError::NotImplemented {
                class: class.name.clone(),
                member: p.name.clone(),
                status: p.status,
            })
    }
}

fn read_field(
    world: &World,
    entity: Entity,
    component: &str,
    field: &str,
) -> Result<Value, CompatError> {
    let key = world.registry.key(component)?;
    let index = world.registry.schema(key).field_index(field)?;
    world
        .registry
        .get_field(&world.ecs, entity, key, index)
        .ok_or_else(|| {
            re_reflect::ReflectError::MissingComponent {
                component: component.to_string(),
            }
            .into()
        })
}

/// `value` with every infinite number held to the largest finite `f32`, so
/// the common `math.huge` (a `MaxSize`, a `MaxDistance`) is stored as the
/// largest number an own field keeps rather than refused; NaN still is.
fn saturate(value: Value) -> Value {
    const MAX: f64 = f32::MAX as f64;
    match value {
        Value::Float(f) if f.is_infinite() => Value::Float(MAX.copysign(f)),
        Value::Vec3(v) => Value::Vec3(v.clamp(
            glam::Vec3::splat(-f32::MAX),
            glam::Vec3::splat(f32::MAX),
        )),
        Value::Record(id, fields) => Value::Record(id, fields.into_iter().map(saturate).collect()),
        Value::List(items) => Value::List(items.into_iter().map(saturate).collect()),
        other => other,
    }
}

fn write_field(
    world: &mut World,
    entity: Entity,
    component: &str,
    field: &str,
    value: Value,
) -> Result<(), CompatError> {
    let key = world.registry.key(component)?;
    let index = world.registry.schema(key).field_index(field)?;
    Ok(world
        .registry
        .set_field(&mut world.ecs, entity, key, index, value)?)
}
