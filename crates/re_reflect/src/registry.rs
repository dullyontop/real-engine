//! The component registry: names, fields, types, defaults and docs for every
//! component the engine can address by name (ADR-0032).
//!
//! Native components are ordinary Rust structs declared with
//! [`component!`](crate::component); their data lives in typed bevy_ecs
//! columns. Script components are declared at run time (from Luau, an agent or
//! a scene file's `script_components`) and get a real bevy_ecs `ComponentId`
//! whose storage holds a `Vec<Value>`, so they are queryable, change-tracked,
//! saved and undone like native ones.

use std::alloc::Layout;
use std::collections::{HashMap, HashSet};
use std::marker::PhantomData;

use bevy_ecs::component::{
    Component, ComponentCloneBehavior, ComponentDescriptor, ComponentId, Immutable, Mutable,
    StorageType,
};
use bevy_ecs::entity::Entity;
use bevy_ecs::ptr::OwningPtr;
use bevy_ecs::world::World;

use crate::error::{ReflectError, suggest};
use crate::value::{FieldType, Value};

/// One field of a component: its name, type, doc and default.
#[derive(Clone, Debug, PartialEq)]
pub struct FieldSchema {
    /// The field's name (an identifier).
    pub name: String,
    /// The field's type.
    pub ty: FieldType,
    /// Documentation shown by `describe`, inspectors and agents.
    pub doc: String,
    /// The value a new component gets when the field is not given.
    pub default: Value,
}

/// Where a component type was declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComponentOrigin {
    /// A Rust struct declared with [`component!`](crate::component).
    Native,
    /// Declared at run time with [`Registry::register_script`].
    Script,
    /// Declared at run time by the host from its own data with
    /// [`Registry::register_host`] (the compatibility layer's per-class
    /// components). Unlike script components, scene files do not record their
    /// schemas: the host registers them before loading, and a file names their
    /// fields like a native component's, so a missing field takes its default.
    Host,
}

impl ComponentOrigin {
    /// `"native"`, `"script"` or `"host"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ComponentOrigin::Native => "native",
            ComponentOrigin::Script => "script",
            ComponentOrigin::Host => "host",
        }
    }
}

/// The description of one component type.
#[derive(Clone, Debug, PartialEq)]
pub struct ComponentSchema {
    /// The component's name (an identifier, unique in the registry).
    pub name: String,
    /// Documentation.
    pub doc: String,
    /// The fields, in declaration order.
    pub fields: Vec<FieldSchema>,
    /// Native or script.
    pub origin: ComponentOrigin,
}

impl ComponentSchema {
    /// The index of the field named `field`, or an error with a suggestion and
    /// the list of valid fields.
    pub fn field_index(&self, field: &str) -> Result<usize, ReflectError> {
        self.fields
            .iter()
            .position(|f| f.name == field)
            .ok_or_else(|| ReflectError::UnknownField {
                component: self.name.clone(),
                field: field.to_string(),
                suggestion: suggest(field, self.fields.iter().map(|f| f.name.as_str())),
                valid: self.fields.iter().map(|f| f.name.clone()).collect(),
            })
    }

    /// Every field's default, in declaration order.
    pub fn defaults(&self) -> Vec<Value> {
        self.fields.iter().map(|f| f.default.clone()).collect()
    }

    /// True if `other` declares the same name, field names, types and defaults
    /// (docs and origin are not compared).
    pub fn same_shape(&self, other: &ComponentSchema) -> bool {
        self.name == other.name
            && self.fields.len() == other.fields.len()
            && self
                .fields
                .iter()
                .zip(&other.fields)
                .all(|(a, b)| a.name == b.name && a.ty == b.ty && a.default == b.default)
    }
}

/// How the registry writes a field of a native component: in place for a
/// mutable component, or by re-inserting a changed copy for an immutable one
/// (so its `on_insert` and `on_discard` hooks see every change, which is what
/// keeps hook-maintained indexes such as `re_scene`'s tag index exact).
/// Implemented for bevy's [`Mutable`] and [`Immutable`] only.
pub trait WriteMode: bevy_ecs::component::ComponentMutability {
    /// Writes field `index` of `T` on `entity` unless it already equals
    /// `value`. Returns false if the entity lacks the component.
    fn write_field<T: Reflect + Component<Mutability = Self>>(
        world: &mut World,
        entity: Entity,
        index: usize,
        value: &Value,
    ) -> bool;
}

impl WriteMode for Mutable {
    fn write_field<T: Reflect + Component<Mutability = Self>>(
        world: &mut World,
        entity: Entity,
        index: usize,
        value: &Value,
    ) -> bool {
        let Some(mut c) = world.get_mut::<T>(entity) else {
            return false;
        };
        // Read through `Deref` first so an equal write does not trip change detection.
        if c.get_field(index) != *value {
            c.set_field(index, value);
        }
        true
    }
}

impl WriteMode for Immutable {
    fn write_field<T: Reflect + Component<Mutability = Self>>(
        world: &mut World,
        entity: Entity,
        index: usize,
        value: &Value,
    ) -> bool {
        let Some(c) = world.get::<T>(entity) else {
            return false;
        };
        let mut copy = c.clone();
        copy.set_field(index, value);
        // Compared after the write, because a component may normalise what it
        // stores (`Tags` drops repeats).
        if copy.get_field(index) != c.get_field(index) {
            world.entity_mut(entity).insert(copy);
        }
        true
    }
}

/// Implemented by [`component!`](crate::component) for native components.
///
/// Most components are mutable. An immutable component (bevy's
/// `#[component(immutable)]`) implements this trait by hand and is written by
/// replacement ([`WriteMode`]).
pub trait Reflect: Component<Mutability: WriteMode> + Clone + Default {
    /// The component's name in the registry and in files.
    const NAME: &'static str;
    /// The component's schema.
    fn schema() -> ComponentSchema;
    /// The value of field `index`.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of range.
    fn get_field(&self, index: usize) -> Value;
    /// Sets field `index`; `value` has already been coerced to the field's type.
    ///
    /// # Panics
    ///
    /// Panics if `index` is out of range or `value` has the wrong variant.
    fn set_field(&mut self, index: usize, value: &Value);
}

/// Type-erased access to one registered component type.
trait ComponentOps: Send + Sync {
    fn get_field(&self, world: &World, entity: Entity, index: usize) -> Option<Value>;
    fn get_all(&self, world: &World, entity: Entity) -> Option<Vec<Value>>;
    /// Returns false if the entity lacks the component. Does not mark the
    /// component changed when the value is equal.
    fn set_field(&self, world: &mut World, entity: Entity, index: usize, value: &Value) -> bool;
    fn insert(&self, world: &mut World, entity: Entity, values: Vec<Value>);
    fn remove(&self, world: &mut World, entity: Entity);
    /// Registers the same component type in another world.
    fn register_in(
        &self,
        schema: &ComponentSchema,
        world: &mut World,
    ) -> (ComponentId, Box<dyn ComponentOps>);
}

struct NativeOps<T>(PhantomData<fn() -> T>);

impl<T: Reflect> ComponentOps for NativeOps<T> {
    fn get_field(&self, world: &World, entity: Entity, index: usize) -> Option<Value> {
        world.get::<T>(entity).map(|c| c.get_field(index))
    }
    fn get_all(&self, world: &World, entity: Entity) -> Option<Vec<Value>> {
        let c = world.get::<T>(entity)?;
        let n = T::schema().fields.len();
        Some((0..n).map(|i| c.get_field(i)).collect())
    }
    fn set_field(&self, world: &mut World, entity: Entity, index: usize, value: &Value) -> bool {
        <T::Mutability as WriteMode>::write_field::<T>(world, entity, index, value)
    }
    fn insert(&self, world: &mut World, entity: Entity, values: Vec<Value>) {
        let mut c = T::default();
        for (i, v) in values.iter().enumerate() {
            c.set_field(i, v);
        }
        world.entity_mut(entity).insert(c);
    }
    fn remove(&self, world: &mut World, entity: Entity) {
        world.entity_mut(entity).remove::<T>();
    }
    fn register_in(
        &self,
        _schema: &ComponentSchema,
        world: &mut World,
    ) -> (ComponentId, Box<dyn ComponentOps>) {
        (
            world.register_component::<T>(),
            Box::new(NativeOps::<T>(PhantomData)),
        )
    }
}

/// Storage of a script-defined component: one value per schema field.
struct ScriptData(Vec<Value>);

struct ScriptOps {
    id: ComponentId,
}

impl ScriptOps {
    fn register(name: &str, world: &mut World) -> ComponentId {
        // SAFETY: the layout and drop function are those of `ScriptData`, which
        // is Send + Sync; script components are not relationships, and
        // `summary_tick` is false (bevy asserts it is only set for Table storage).
        let descriptor = unsafe {
            ComponentDescriptor::new_with_layout(
                name.to_string(),
                StorageType::Table,
                Layout::new::<ScriptData>(),
                Some(drop_script_data),
                true,
                false,
                ComponentCloneBehavior::Ignore,
                None,
            )
        };
        world.register_component_with_descriptor(descriptor)
    }
}

impl ComponentOps for ScriptOps {
    fn get_field(&self, world: &World, entity: Entity, index: usize) -> Option<Value> {
        let ptr = world.get_by_id(entity, self.id)?;
        // SAFETY: `self.id` was registered with the layout and drop of `ScriptData`.
        let data = unsafe { ptr.deref::<ScriptData>() };
        data.0.get(index).cloned()
    }
    fn get_all(&self, world: &World, entity: Entity) -> Option<Vec<Value>> {
        let ptr = world.get_by_id(entity, self.id)?;
        // SAFETY: as above.
        Some(unsafe { ptr.deref::<ScriptData>() }.0.clone())
    }
    fn set_field(&self, world: &mut World, entity: Entity, index: usize, value: &Value) -> bool {
        let Some(m) = world.get_mut_by_id(entity, self.id) else {
            return false;
        };
        // SAFETY: as above.
        let unchanged = unsafe { m.as_ref().deref::<ScriptData>() }.0[index] == *value;
        if !unchanged {
            // `into_inner` marks the component changed.
            // SAFETY: as above.
            let data = unsafe { m.into_inner().deref_mut::<ScriptData>() };
            data.0[index] = value.clone();
        }
        true
    }
    fn insert(&self, world: &mut World, entity: Entity, values: Vec<Value>) {
        OwningPtr::make(ScriptData(values), |ptr| {
            // SAFETY: `ptr` holds a `ScriptData`, matching the registered layout.
            unsafe {
                world.entity_mut(entity).insert_by_id(self.id, ptr);
            }
        });
    }
    fn remove(&self, world: &mut World, entity: Entity) {
        world.entity_mut(entity).remove_by_id(self.id);
    }
    fn register_in(
        &self,
        schema: &ComponentSchema,
        world: &mut World,
    ) -> (ComponentId, Box<dyn ComponentOps>) {
        let id = ScriptOps::register(&schema.name, world);
        (id, Box::new(ScriptOps { id }))
    }
}

/// # Safety
///
/// `ptr` must point to a `ScriptData`.
unsafe fn drop_script_data(ptr: OwningPtr<'_>) {
    // SAFETY: only called by bevy_ecs on values inserted by `ScriptOps::insert`.
    unsafe { ptr.drop_as::<ScriptData>() }
}

struct Entry {
    schema: ComponentSchema,
    id: ComponentId,
    ops: Box<dyn ComponentOps>,
}

/// Handle to a registered component type, cheap to copy. Keys are indices in
/// registration order, so a registry rebuilt with
/// [`Registry::rebuild_for`] hands out the same keys.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ComponentKey(usize);

impl ComponentKey {
    /// The registration index.
    pub fn index(self) -> usize {
        self.0
    }
}

/// Called after a registry write changed a component: [`Registry::set_field`]
/// with a different value, [`Registry::insert`] or [`Registry::remove`]. A
/// world that keeps a journal of changes installs one with
/// [`Registry::set_change_hook`]; the registry knows nothing else about it.
pub type ChangeHook = fn(&mut World, Entity, ComponentKey);

/// What a field holds beyond its type, for tools (ADR-0064 §9). Hints are
/// kept in a side table on the [`Registry`] ([`Registry::set_field_hint`]),
/// so that no [`FieldSchema`] changes; [`Registry::describe`] shows them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FieldHint {
    /// The kind of asset a string field names, as the editor's asset index
    /// knows them (ADR-0064 §7): `material`, `prefab`, `scene`, `graph`,
    /// `animgraph`, `script`, `texture`, `model`, `clip`, `skin` or `audio`.
    pub asset: Option<&'static str>,
}

/// The component registry. It belongs to one bevy `World` (component ids are per world).
#[derive(Default)]
pub struct Registry {
    entries: Vec<Entry>,
    by_name: HashMap<String, usize>,
    by_id: HashMap<ComponentId, usize>,
    change_hook: Option<ChangeHook>,
    /// Field hints by (entry, field index).
    hints: HashMap<(usize, usize), FieldHint>,
}

impl Registry {
    /// Registers a native component (declared with [`component!`](crate::component)).
    /// Registering the same type again returns its existing key.
    ///
    /// # Panics
    ///
    /// Panics if another component with the same name is already registered,
    /// or if a field type breaks the nesting rules ([`FieldType::validate`])
    /// (programming errors at start-up).
    pub fn register<T: Reflect>(&mut self, world: &mut World) -> ComponentKey {
        let schema = T::schema();
        debug_assert_eq!(schema.name, T::NAME);
        for f in &schema.fields {
            if let Err(e) = f.ty.validate() {
                panic!("{}.{}: {e}", schema.name, f.name);
            }
        }
        let id = world.register_component::<T>();
        if let Some(&i) = self.by_name.get(&schema.name) {
            assert_eq!(
                self.entries[i].id, id,
                "component name {:?} is already registered by another type",
                schema.name
            );
            return ComponentKey(i);
        }
        self.push(schema, id, Box::new(NativeOps::<T>(PhantomData)))
    }

    /// Declares a component type at run time. Names must be identifiers, and
    /// field defaults are coerced to their declared types. The schema's origin
    /// is set to [`ComponentOrigin::Script`].
    pub fn register_script(
        &mut self,
        world: &mut World,
        schema: ComponentSchema,
    ) -> Result<ComponentKey, ReflectError> {
        self.register_dynamic(world, schema, ComponentOrigin::Script)
    }

    /// Declares a host component type at run time, checked like
    /// [`Registry::register_script`]; its origin is [`ComponentOrigin::Host`].
    pub fn register_host(
        &mut self,
        world: &mut World,
        schema: ComponentSchema,
    ) -> Result<ComponentKey, ReflectError> {
        self.register_dynamic(world, schema, ComponentOrigin::Host)
    }

    fn register_dynamic(
        &mut self,
        world: &mut World,
        mut schema: ComponentSchema,
        origin: ComponentOrigin,
    ) -> Result<ComponentKey, ReflectError> {
        if self.by_name.contains_key(&schema.name) {
            return Err(ReflectError::DuplicateComponent(schema.name));
        }
        if !is_identifier(&schema.name) {
            return Err(ReflectError::InvalidSchema(format!(
                "component name {:?} must be an identifier",
                schema.name
            )));
        }
        let mut seen = HashSet::new();
        for f in &mut schema.fields {
            if !is_identifier(&f.name) {
                return Err(ReflectError::InvalidSchema(format!(
                    "field name {:?} must be an identifier",
                    f.name
                )));
            }
            if let Err(e) = f.ty.validate() {
                return Err(ReflectError::InvalidSchema(format!(
                    "{}.{}: {e}",
                    schema.name, f.name
                )));
            }
            if !seen.insert(f.name.clone()) {
                return Err(ReflectError::InvalidSchema(format!(
                    "{} declares field {:?} twice",
                    schema.name, f.name
                )));
            }
            f.default =
                f.ty.coerce(f.default.clone())
                    .map_err(|e| ReflectError::Value {
                        path: format!("{}.{}", schema.name, f.name),
                        error: e,
                    })?;
            if f.default != Value::Entity(None) && f.ty == FieldType::Entity {
                return Err(ReflectError::InvalidSchema(format!(
                    "{}.{}: the default of an entity field must be null",
                    schema.name, f.name
                )));
            }
        }
        schema.origin = origin;
        let id = ScriptOps::register(&schema.name, world);
        Ok(self.push(schema, id, Box::new(ScriptOps { id })))
    }

    fn push(
        &mut self,
        schema: ComponentSchema,
        id: ComponentId,
        ops: Box<dyn ComponentOps>,
    ) -> ComponentKey {
        let index = self.entries.len();
        self.by_name.insert(schema.name.clone(), index);
        self.by_id.insert(id, index);
        self.entries.push(Entry { schema, id, ops });
        ComponentKey(index)
    }

    /// A registry for another (empty) bevy `World` with the same component
    /// types, native and script, registered in the same order, so every
    /// [`ComponentKey`] means the same component in both. Used by
    /// `re_scene::World::fork`.
    pub fn rebuild_for(&self, world: &mut World) -> Registry {
        let mut out = Registry::default();
        for e in &self.entries {
            let (id, ops) = e.ops.register_in(&e.schema, world);
            out.push(e.schema.clone(), id, ops);
        }
        out.hints.clone_from(&self.hints);
        out
    }

    /// Gives one field of a registered component a hint for tools
    /// (ADR-0064 §9), replacing the one it had.
    ///
    /// # Errors
    ///
    /// The component is not registered, or has no such field.
    pub fn set_field_hint(
        &mut self,
        component: &str,
        field: &str,
        hint: FieldHint,
    ) -> Result<(), ReflectError> {
        let key = self.key(component)?;
        let index = self.schema(key).field_index(field)?;
        self.hints.insert((key.0, index), hint);
        Ok(())
    }

    /// The hint of field `index` of the component `key` (no hint when none
    /// was given).
    pub fn field_hint(&self, key: ComponentKey, index: usize) -> FieldHint {
        self.hints.get(&(key.0, index)).copied().unwrap_or_default()
    }

    /// The key of the component named `name`, or an error with a suggestion.
    pub fn key(&self, name: &str) -> Result<ComponentKey, ReflectError> {
        self.by_name
            .get(name)
            .map(|&i| ComponentKey(i))
            .ok_or_else(|| ReflectError::UnknownComponent {
                name: name.to_string(),
                suggestion: suggest(name, self.by_name.keys().map(String::as_str)),
            })
    }

    /// The key of the component named `name`, if it is registered.
    pub fn try_key(&self, name: &str) -> Option<ComponentKey> {
        self.by_name.get(name).map(|&i| ComponentKey(i))
    }

    /// The key of the native component `T`, if it is registered.
    pub fn key_of<T: Reflect>(&self) -> Option<ComponentKey> {
        self.try_key(T::NAME)
    }

    /// The key registered for a bevy `ComponentId`, if any.
    pub fn key_for_id(&self, id: ComponentId) -> Option<ComponentKey> {
        self.by_id.get(&id).map(|&i| ComponentKey(i))
    }

    /// The schema of a registered component.
    ///
    /// # Panics
    ///
    /// Panics if `key` comes from another registry with fewer entries.
    pub fn schema(&self, key: ComponentKey) -> &ComponentSchema {
        &self.entries[key.0].schema
    }

    /// The bevy `ComponentId` of a registered component.
    pub fn component_id(&self, key: ComponentKey) -> ComponentId {
        self.entries[key.0].id
    }

    /// Every schema, in registration order.
    pub fn schemas(&self) -> impl Iterator<Item = &ComponentSchema> {
        self.entries.iter().map(|e| &e.schema)
    }

    /// Every key, in registration order.
    pub fn keys(&self) -> impl Iterator<Item = ComponentKey> + use<> {
        (0..self.entries.len()).map(ComponentKey)
    }

    /// The number of registered component types.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Registered components present on `entity`, sorted by name.
    pub fn components_of(&self, world: &World, entity: Entity) -> Vec<ComponentKey> {
        let Ok(e) = world.get_entity(entity) else {
            return Vec::new();
        };
        let mut keys: Vec<ComponentKey> = e
            .archetype()
            .iter_components()
            .filter_map(|id| self.by_id.get(&id).map(|&i| ComponentKey(i)))
            .collect();
        keys.sort_by(|a, b| self.schema(*a).name.cmp(&self.schema(*b).name));
        keys
    }

    /// Every (entity, component, field index) whose entity-typed field points
    /// at an entity in `targets`. Walks only archetypes that contain a
    /// component with entity fields.
    pub fn references_to(
        &self,
        world: &World,
        targets: &HashSet<Entity>,
    ) -> Vec<(Entity, ComponentKey, usize)> {
        let mut out = Vec::new();
        for (ki, entry) in self.entries.iter().enumerate() {
            let fields: Vec<usize> = entry
                .schema
                .fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.ty == FieldType::Entity)
                .map(|(i, _)| i)
                .collect();
            if fields.is_empty() {
                continue;
            }
            for archetype in world.archetypes().iter().filter(|a| a.contains(entry.id)) {
                for e in archetype.entities() {
                    for &fi in &fields {
                        if let Some(Value::Entity(Some(t))) = entry.ops.get_field(world, e.id(), fi)
                            && targets.contains(&t)
                        {
                            out.push((e.id(), ComponentKey(ki), fi));
                        }
                    }
                }
            }
        }
        out
    }

    /// True if `entity` has the component.
    pub fn has(&self, world: &World, entity: Entity, key: ComponentKey) -> bool {
        world
            .get_entity(entity)
            .is_ok_and(|e| e.contains_id(self.entries[key.0].id))
    }

    /// One field's value, or `None` if the entity lacks the component.
    pub fn get_field(
        &self,
        world: &World,
        entity: Entity,
        key: ComponentKey,
        index: usize,
    ) -> Option<Value> {
        self.entries[key.0].ops.get_field(world, entity, index)
    }

    /// Every field's value, or `None` if the entity lacks the component.
    pub fn get_all(&self, world: &World, entity: Entity, key: ComponentKey) -> Option<Vec<Value>> {
        self.entries[key.0].ops.get_all(world, entity)
    }

    /// Sets one field by index; `value` is coerced to the field type first.
    /// Writing a value equal to the current one does not mark the component changed.
    pub fn set_field(
        &self,
        world: &mut World,
        entity: Entity,
        key: ComponentKey,
        index: usize,
        value: Value,
    ) -> Result<(), ReflectError> {
        let schema = self.schema(key);
        let field = &schema.fields[index];
        let value = field.ty.coerce(value).map_err(|e| ReflectError::Value {
            path: format!("{}.{}", schema.name, field.name),
            error: e,
        })?;
        if let Some(hook) = self.change_hook {
            return self.set_field_hooked(world, entity, key, index, &value, hook);
        }
        if self.entries[key.0]
            .ops
            .set_field(world, entity, index, &value)
        {
            Ok(())
        } else {
            Err(ReflectError::MissingComponent {
                component: schema.name.clone(),
            })
        }
    }

    /// [`Registry::set_field`] with a change hook: the value before is read
    /// so an equal write reports nothing. Kept apart so a registry without a
    /// hook writes exactly as before.
    #[cold]
    fn set_field_hooked(
        &self,
        world: &mut World,
        entity: Entity,
        key: ComponentKey,
        index: usize,
        value: &Value,
        hook: ChangeHook,
    ) -> Result<(), ReflectError> {
        let ops = &self.entries[key.0].ops;
        let before = ops.get_field(world, entity, index);
        if !ops.set_field(world, entity, index, value) {
            return Err(ReflectError::MissingComponent {
                component: self.schema(key).name.clone(),
            });
        }
        if before != ops.get_field(world, entity, index) {
            hook(world, entity, key);
        }
        Ok(())
    }

    /// Inserts (or replaces) a component from a full list of field values,
    /// coercing each.
    pub fn insert(
        &self,
        world: &mut World,
        entity: Entity,
        key: ComponentKey,
        values: Vec<Value>,
    ) -> Result<(), ReflectError> {
        let schema = self.schema(key);
        if values.len() != schema.fields.len() {
            return Err(ReflectError::InvalidSchema(format!(
                "{} has {} fields, got {} values",
                schema.name,
                schema.fields.len(),
                values.len()
            )));
        }
        let mut coerced = Vec::with_capacity(values.len());
        for (f, v) in schema.fields.iter().zip(values) {
            coerced.push(f.ty.coerce(v).map_err(|e| ReflectError::Value {
                path: format!("{}.{}", schema.name, f.name),
                error: e,
            })?);
        }
        self.entries[key.0].ops.insert(world, entity, coerced);
        if let Some(hook) = self.change_hook {
            hook(world, entity, key);
        }
        Ok(())
    }

    /// Removes the component (a no-op if the entity lacks it).
    pub fn remove(&self, world: &mut World, entity: Entity, key: ComponentKey) {
        let had = self.change_hook.is_some() && self.has(world, entity, key);
        self.entries[key.0].ops.remove(world, entity);
        if let Some(hook) = self.change_hook.filter(|_| had) {
            hook(world, entity, key);
        }
    }

    /// Installs (or with `None` removes) the hook told of every change a
    /// registry write makes.
    pub fn set_change_hook(&mut self, hook: Option<ChangeHook>) {
        self.change_hook = hook;
    }

    /// The installed change hook.
    pub fn change_hook(&self) -> Option<ChangeHook> {
        self.change_hook
    }
}

/// True if `s` is an ASCII identifier (`[A-Za-z_][A-Za-z0-9_]*`).
pub fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Declares a native component with field docs and defaults, and implements
/// [`Reflect`] for it.
///
/// ```
/// use re_reflect::component;
///
/// component! {
///     /// Hit points.
///     pub struct Health {
///         /// Current hit points.
///         pub current: f32 = 100.0,
///         pub max: f32 = 100.0,
///     }
/// }
/// assert_eq!(<Health as re_reflect::Reflect>::schema().fields[0].doc, "Current hit points.");
/// ```
///
/// A `requires(A, B)` clause after the name adds bevy required components
/// (`pub struct Transform requires(GlobalTransform) { ... }`); it is written
/// as a clause rather than an attribute to avoid a `macro_rules` ambiguity.
///
/// The macro expands to `#[derive(Component)]`, whose generated code names
/// `bevy_ecs::...`. A crate that does not depend on `bevy_ecs` itself puts
/// `use re_scene::ecs as bevy_ecs;` in the module that invokes the macro.
#[macro_export]
macro_rules! component {
    (
        $(#[doc = $doc:literal])*
        $vis:vis struct $name:ident $(requires($($req:ty),* $(,)?))? {
            $( $(#[doc = $fdoc:literal])* $fvis:vis $field:ident : $fty:ty = $default:expr ),* $(,)?
        }
    ) => {
        $(#[doc = $doc])*
        #[derive($crate::__private::Component, Clone, Debug, PartialEq)]
        $(#[require($($req),*)])?
        $vis struct $name { $( $(#[doc = $fdoc])* $fvis $field: $fty ),* }

        impl Default for $name {
            fn default() -> Self {
                Self { $($field: $default),* }
            }
        }

        impl $crate::registry::Reflect for $name {
            const NAME: &'static str = stringify!($name);

            fn schema() -> $crate::registry::ComponentSchema {
                let default = Self::default();
                $crate::registry::ComponentSchema {
                    name: stringify!($name).to_string(),
                    doc: $crate::registry::join_doc(&[$($doc),*]),
                    fields: vec![$(
                        $crate::registry::FieldSchema {
                            name: stringify!($field).to_string(),
                            ty: <$fty as $crate::value::FieldValue>::field_type(),
                            doc: $crate::registry::join_doc(&[$($fdoc),*]),
                            default: $crate::value::FieldValue::to_value(&default.$field),
                        }
                    ),*],
                    origin: $crate::registry::ComponentOrigin::Native,
                }
            }

            #[allow(unused_assignments, unused_mut, unused_variables)]
            fn get_field(&self, index: usize) -> $crate::value::Value {
                let mut i = 0usize;
                $(
                    if index == i {
                        return $crate::value::FieldValue::to_value(&self.$field);
                    }
                    i += 1;
                )*
                panic!("{}: field index {index} out of range", stringify!($name))
            }

            #[allow(unused_assignments, unused_mut, unused_variables)]
            fn set_field(&mut self, index: usize, value: &$crate::value::Value) {
                let mut i = 0usize;
                $(
                    if index == i {
                        self.$field = <$fty as $crate::value::FieldValue>::from_value(value)
                            .expect("value was coerced to the field type");
                        return;
                    }
                    i += 1;
                )*
                panic!("{}: field index {index} out of range", stringify!($name))
            }
        }
    };
}

/// Joins `///` doc lines into one string (used by [`component!`](crate::component)).
#[doc(hidden)]
pub fn join_doc(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|l| l.strip_prefix(' ').unwrap_or(l))
        .collect::<Vec<_>>()
        .join("\n")
}
