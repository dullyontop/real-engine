//! The class layer (ADR-0012): Roblox-style classes as data over the native
//! entity model.
//!
//! An instance is an entity with a [`ClassMarker`] naming its class. A class
//! has a superclass (only `Instance` has none), adds a set of native
//! components, and declares properties, methods and events, each with a
//! [`Status`]. A property's value lives in one of three places
//! ([`Storage`]):
//! * an alias to a native component field (`Position` → `Transform.translation`),
//!   optionally through a named [`Converter`];
//! * the class's own runtime component, `Compat_<Class>`, registered with
//!   `Registry::register_host` so it is saved, undone and reflected like any
//!   other component (scene files store its values by field name, not its
//!   schema, so the class data can grow without breaking saved scenes);
//! * a builtin [`Accessor`] written in Rust (`Name`, `Parent`, `ClassName`).
//!
//! [`ClassRegistry`] is built once from the class, enum and library files
//! ([`ClassRegistryBuilder`]), plus the converters and accessors the
//! implementing work packages register, and is read-only afterwards.
//!
//! The own component is named `Compat_<Class>` rather than `Compat.<Class>`,
//! because component names are identifiers (ADR-0032).
//!
//! Each class has a number, its [`ClassId`], and the registry keeps each
//! class's ancestors as a bitset, so [`ClassRegistry::is_a`] is two lookups
//! and a bit test, with no allocation. [`ClassIndex`] finds every instance
//! of a class without walking the world.

pub(crate) mod access;
mod index;
mod trace;

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use re_reflect::{
    ComponentOrigin, ComponentSchema, FieldSchema, FieldType, RecordId, Reflect, Value, suggest,
};
use re_scene::ecs as bevy_ecs;
use re_scene::{Entity, World};

use crate::data::{
    ClassDoc, ClassFile, DataSet, EnumDoc, EnumFile, EventDoc, LibraryDoc, LibraryFile, MethodDoc,
    ParamDoc, Status, StorageDoc, merge_classes,
};
use crate::error::CompatError;

pub use access::own_component_name;
pub use index::ClassIndex;
pub use trace::TRACE_ENV as MEMBER_TRACE_ENV;

/// Marks an entity as an instance of a Roblox-style class.
///
/// Immutable, as an instance's class never changes (`ClassName` is
/// read-only): every write replaces it, so its hooks keep the
/// [`ClassIndex`] exact across spawn, despawn, scene load and fork.
#[derive(bevy_ecs::component::Component, Clone, Debug, PartialEq, Default)]
#[component(
    immutable,
    on_insert = index::marker_inserted,
    on_discard = index::marker_discarded
)]
pub struct ClassMarker {
    /// The class name (`Part`, `Folder`, `Script`).
    pub class: String,
}

impl Reflect for ClassMarker {
    const NAME: &'static str = "ClassMarker";

    fn schema() -> ComponentSchema {
        ComponentSchema {
            name: Self::NAME.to_string(),
            doc: "Marks an entity as an instance of a Roblox-style class.".to_string(),
            fields: vec![FieldSchema {
                name: "class".to_string(),
                ty: FieldType::String,
                doc: "The class name (`Part`, `Folder`, `Script`).".to_string(),
                default: Value::Str(String::new()),
            }],
            origin: ComponentOrigin::Native,
        }
    }

    fn get_field(&self, index: usize) -> Value {
        assert_eq!(index, 0, "ClassMarker: field index {index} out of range");
        Value::Str(self.class.clone())
    }

    fn set_field(&mut self, index: usize, value: &Value) {
        assert_eq!(index, 0, "ClassMarker: field index {index} out of range");
        let Value::Str(class) = value else {
            panic!("value was coerced to the field type");
        };
        self.class.clone_from(class);
    }
}

/// A class's number in its [`ClassRegistry`]: its place in name order. It
/// means nothing to another registry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClassId(u16);

impl ClassId {
    /// The number, as an index into tables kept per class.
    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// One property of a class.
#[derive(Clone, Debug, PartialEq)]
pub struct PropertyDef {
    /// The name.
    pub name: String,
    /// The type as the data file writes it (`bool`, `Vector3`, `Enum.Material`).
    pub ty: String,
    /// Where the value lives.
    pub storage: Storage,
    /// True if scripts may only read it.
    pub read_only: bool,
    /// What to use instead, when deprecated.
    pub deprecated: Option<String>,
    /// How far it is implemented.
    pub status: Status,
    /// The gap, for `partial`.
    pub note: String,
    /// What it means.
    pub doc: String,
}

/// Where a property's value lives.
#[derive(Clone, Debug, PartialEq)]
pub enum Storage {
    /// A field of a native component, optionally through a converter.
    Alias {
        /// The native component.
        component: String,
        /// Its field.
        field: String,
        /// The converter's name.
        via: Option<String>,
    },
    /// A field of the declaring class's own component `Compat_<Class>`.
    Own {
        /// The field's index in the own component.
        index: usize,
        /// The field's type.
        ty: FieldType,
        /// The field's default.
        default: Value,
    },
    /// A Rust accessor.
    Builtin,
}

/// One method of a class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodDef {
    /// The name.
    pub name: String,
    /// The parameters after the instance itself.
    pub params: Vec<ParamDoc>,
    /// The result types.
    pub returns: Vec<String>,
    /// True if the call can yield.
    pub yields: bool,
    /// What to use instead, when deprecated.
    pub deprecated: Option<String>,
    /// How far it is implemented.
    pub status: Status,
    /// The gap, for `partial`.
    pub note: String,
    /// What it does.
    pub doc: String,
}

/// One event of a class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventDef {
    /// The name.
    pub name: String,
    /// The values each firing passes.
    pub params: Vec<ParamDoc>,
    /// What to use instead, when deprecated.
    pub deprecated: Option<String>,
    /// How far it is implemented.
    pub status: Status,
    /// The gap, for `partial`.
    pub note: String,
    /// When it fires.
    pub doc: String,
}

/// One class.
#[derive(Clone, Debug, PartialEq)]
pub struct ClassDef {
    /// The name.
    pub name: String,
    /// The superclass; `None` only for `Instance`.
    pub superclass: Option<String>,
    /// Creatable with `Instance.new`.
    pub creatable: bool,
    /// A service: one instance under `game`.
    pub service: bool,
    /// The native components the class adds (its superclasses add theirs).
    pub components: Vec<String>,
    /// How far the class itself is implemented.
    pub status: Status,
    /// The gap, for `partial`.
    pub note: String,
    /// What it is.
    pub doc: String,
    /// Properties declared on this class.
    pub properties: Vec<PropertyDef>,
    /// Methods declared on this class.
    pub methods: Vec<MethodDef>,
    /// Events declared on this class.
    pub events: Vec<EventDef>,
}

impl ClassDef {
    /// The name of this class's own component, if it declares a property with
    /// own storage.
    pub fn own_component(&self) -> Option<String> {
        self.properties
            .iter()
            .any(|p| matches!(p.storage, Storage::Own { .. }))
            .then(|| own_component_name(&self.name))
    }
}

/// A member found by [`ClassRegistry::member`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Member<'a> {
    /// A property.
    Property(&'a PropertyDef),
    /// A method.
    Method(&'a MethodDef),
    /// An event.
    Event(&'a EventDef),
}

impl<'a> Member<'a> {
    /// The member's name.
    pub fn name(&self) -> &'a str {
        match self {
            Member::Property(p) => &p.name,
            Member::Method(m) => &m.name,
            Member::Event(e) => &e.name,
        }
    }

    /// The member's status.
    pub fn status(&self) -> Status {
        match self {
            Member::Property(p) => p.status,
            Member::Method(m) => m.status,
            Member::Event(e) => e.status,
        }
    }

    /// What to use instead, when deprecated.
    pub fn deprecated(&self) -> Option<&'a str> {
        match self {
            Member::Property(p) => p.deprecated.as_deref(),
            Member::Method(m) => m.deprecated.as_deref(),
            Member::Event(e) => e.deprecated.as_deref(),
        }
    }

    /// `property`, `method` or `event`.
    pub fn kind(&self) -> &'static str {
        match self {
            Member::Property(_) => "property",
            Member::Method(_) => "method",
            Member::Event(_) => "event",
        }
    }
}

/// A member and the class that declares it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemberRef<'a> {
    /// The declaring class (the asked-for class or one of its superclasses).
    pub class: &'a ClassDef,
    /// The member.
    pub member: Member<'a>,
}

impl MemberRef<'_> {
    /// `Class.Member`, naming the declaring class.
    pub fn path(&self) -> String {
        format!("{}.{}", self.class.name, self.member.name())
    }

    /// The `RE9101` warning for a deprecated member, naming its replacement.
    pub fn deprecation(&self) -> Option<re_core::Diagnostic> {
        self.member.deprecated().map(|instead| {
            re_core::Diagnostic::warning(
                crate::codes::DEPRECATED_MEMBER,
                format!("{} is deprecated", self.path()),
            )
            .with_hint(format!("use {instead}"))
        })
    }

    /// [`Self::require_implemented`] for a member a script reached (an index,
    /// an assignment, `GetPropertyChangedSignal`, a tween goal). It also
    /// records the member for the member-call check when tracing is on.
    pub(crate) fn require_implemented_by_script(&self) -> Result<(), CompatError> {
        self.require_implemented()?;
        trace::record(&self.class.name, self.member.name());
        Ok(())
    }

    /// `Ok` if the member is `implemented` or `partial`; the `RE9001` error
    /// naming it otherwise.
    pub fn require_implemented(&self) -> Result<(), CompatError> {
        match self.member.status() {
            Status::Implemented | Status::Partial => Ok(()),
            status => Err(CompatError::NotImplemented {
                class: self.class.name.clone(),
                member: self.member.name().to_string(),
                status,
            }),
        }
    }
}

/// A converter's write direction: property value → field value.
pub type ToFieldFn = dyn Fn(&Value) -> Result<Value, CompatError> + Send + Sync;
/// A converter's read direction: field value → property value.
pub type FromFieldFn = dyn Fn(&Value) -> Value + Send + Sync;
/// An accessor's read.
pub type GetFn = dyn Fn(&World, Entity) -> Result<Value, CompatError> + Send + Sync;
/// An accessor's write.
pub type SetFn = dyn Fn(&mut World, Entity, Value) -> Result<(), CompatError> + Send + Sync;

/// Converts between a property's value and the native field it aliases
/// (degrees to radians, a `BrickColor` to a colour, ...).
#[derive(Clone)]
pub struct Converter {
    /// Property value → field value, for writes.
    pub to_field: Arc<ToFieldFn>,
    /// Field value → property value, for reads.
    pub from_field: Arc<FromFieldFn>,
}

impl fmt::Debug for Converter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Converter")
    }
}

/// Reads (and optionally writes) a builtin property in Rust.
#[derive(Clone)]
pub struct Accessor {
    /// Reads the property of an instance.
    pub get: Arc<GetFn>,
    /// Writes it; `None` for a property only the engine changes.
    pub set: Option<Arc<SetFn>>,
}

impl fmt::Debug for Accessor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Accessor")
    }
}

/// Collects the data files, converters and accessors, then builds a
/// [`ClassRegistry`].
#[derive(Default)]
pub struct ClassRegistryBuilder {
    data: DataSet,
    converters: HashMap<String, Converter>,
    accessors: HashMap<(String, String), Accessor>,
}

impl ClassRegistryBuilder {
    /// A builder with no data.
    pub fn new() -> Self {
        Self::default()
    }

    /// A builder with the data files built into the crate.
    pub fn builtin() -> Result<Self, CompatError> {
        Ok(Self::from_data(DataSet::builtin()?))
    }

    /// A builder with the given data files.
    pub fn from_data(data: DataSet) -> Self {
        Self {
            data,
            ..Self::default()
        }
    }

    /// Adds a class file; `file` names it in errors.
    pub fn classes(mut self, file: &str, f: ClassFile) -> Self {
        self.data.classes.push((file.to_string(), f));
        self
    }

    /// Adds an enum file.
    pub fn enums(mut self, file: &str, f: EnumFile) -> Self {
        self.data.enums.push((file.to_string(), f));
        self
    }

    /// Adds a library file.
    pub fn libraries(mut self, file: &str, f: LibraryFile) -> Self {
        self.data.libraries.push((file.to_string(), f));
        self
    }

    /// Registers the converter that `"via": name` properties use.
    pub fn converter(mut self, name: &str, c: Converter) -> Self {
        self.converters.insert(name.to_string(), c);
        self
    }

    /// Registers the accessor of a builtin property.
    pub fn accessor(mut self, class: &str, property: &str, a: Accessor) -> Self {
        self.accessors
            .insert((class.to_string(), property.to_string()), a);
        self
    }

    /// Builds the registry. Checks, besides what [`merge_classes`] checks:
    /// every superclass exists and there is no cycle; only `Instance` has no
    /// superclass; a member does not reuse the name of an inherited member of
    /// a different kind; every `via` names a registered converter; every own
    /// property's type resolves to a field type and its default fits; every
    /// `implemented` or `partial` builtin property has an accessor; and every
    /// accessor names a builtin property.
    pub fn build(self) -> Result<ClassRegistry, CompatError> {
        let reg = self.build_data_only()?;
        reg.check_bindings()?;
        Ok(reg)
    }

    /// Builds the registry checking the data only, not that converters and
    /// accessors are registered: for tools that read the tables
    /// (`cargo xtask coverage`) without the implementations.
    pub fn build_data_only(self) -> Result<ClassRegistry, CompatError> {
        let docs = merge_classes(&self.data)?;
        let mut enums = BTreeMap::new();
        for (file, f) in &self.data.enums {
            for e in &f.enums {
                if enums.insert(e.name.clone(), e.clone()).is_some() {
                    return Err(bad(format!(
                        "{file}: enum {} is declared in two files",
                        e.name
                    )));
                }
            }
        }
        let mut libraries = BTreeMap::new();
        for (file, f) in &self.data.libraries {
            for l in &f.libraries {
                if libraries.insert(l.name.clone(), l.clone()).is_some() {
                    return Err(bad(format!(
                        "{file}: library {} is declared in two files",
                        l.name
                    )));
                }
            }
        }
        let names: HashMap<String, usize> = docs
            .iter()
            .enumerate()
            .map(|(i, c)| (c.name.clone(), i))
            .collect();
        register_records(&libraries, &enums)?;
        let mut classes = Vec::with_capacity(docs.len());
        for doc in &docs {
            classes.push(class_def(doc, &names, &enums)?);
        }
        let tree = Tree::new(&classes, &names)?;
        let reg = ClassRegistry {
            by_name: names,
            classes,
            tree,
            enums,
            libraries,
            converters: self.converters,
            accessors: self.accessors,
        };
        reg.check_data()?;
        Ok(reg)
    }
}

fn bad(msg: String) -> CompatError {
    CompatError::InvalidData(msg)
}

/// The inheritance tree by [`ClassId`], worked out once when the registry is
/// built.
#[derive(Debug)]
struct Tree {
    /// Each class's chain: the class, then its superclasses up to `Instance`.
    chains: Vec<Box<[ClassId]>>,
    /// Each class's ancestors, itself included, as `words` bits per class.
    ancestors: Vec<u64>,
    words: usize,
    /// Each class and the classes that inherit from it, in id order.
    families: Vec<Box<[ClassId]>>,
}

impl Tree {
    /// Fails on a superclass cycle, or more classes than a [`ClassId`] can
    /// number.
    fn new(classes: &[ClassDef], names: &HashMap<String, usize>) -> Result<Tree, CompatError> {
        if classes.len() > usize::from(u16::MAX) + 1 {
            return Err(bad(format!(
                "{} classes are more than the class layer can number",
                classes.len()
            )));
        }
        let id = |i: usize| ClassId(i as u16);
        let mut chains = Vec::with_capacity(classes.len());
        for (i, c) in classes.iter().enumerate() {
            let mut chain = vec![id(i)];
            let mut cur = c.superclass.as_deref().and_then(|s| names.get(s));
            while let Some(&s) = cur {
                if chain.contains(&id(s)) {
                    return Err(bad(format!("class {}'s superclasses form a cycle", c.name)));
                }
                chain.push(id(s));
                cur = classes[s].superclass.as_deref().and_then(|s| names.get(s));
            }
            chains.push(chain.into_boxed_slice());
        }
        let words = classes.len().div_ceil(64);
        let mut ancestors = vec![0u64; classes.len() * words];
        let mut families: Vec<Vec<ClassId>> = vec![Vec::new(); classes.len()];
        for (i, chain) in chains.iter().enumerate() {
            for a in chain.iter().map(|a| a.index()) {
                ancestors[i * words + a / 64] |= 1 << (a % 64);
                families[a].push(id(i));
            }
        }
        Ok(Tree {
            chains,
            ancestors,
            words,
            families: families.into_iter().map(Vec::into_boxed_slice).collect(),
        })
    }

    fn is_a(&self, class: ClassId, ancestor: ClassId) -> bool {
        let a = ancestor.index();
        self.ancestors[class.index() * self.words + a / 64] & (1 << (a % 64)) != 0
    }
}

fn class_def(
    doc: &ClassDoc,
    names: &HashMap<String, usize>,
    enums: &BTreeMap<String, EnumDoc>,
) -> Result<ClassDef, CompatError> {
    match (&doc.superclass, doc.name.as_str()) {
        (None, "Instance") => {}
        (None, _) => return Err(bad(format!("class {} has no superclass", doc.name))),
        (Some(_), "Instance") => return Err(bad("class Instance has a superclass".into())),
        (Some(s), _) if !names.contains_key(s) => {
            return Err(bad(format!(
                "class {} names the unknown superclass {s}",
                doc.name
            )));
        }
        _ => {}
    }
    let mut own = 0usize;
    let mut properties = Vec::with_capacity(doc.properties.len());
    for p in &doc.properties {
        let storage = match &p.storage {
            StorageDoc::Alias {
                component,
                field,
                via,
            } => Storage::Alias {
                component: component.clone(),
                field: field.clone(),
                via: via.clone(),
            },
            StorageDoc::Builtin => Storage::Builtin,
            StorageDoc::Own => {
                let ty = field_type(&format!("{}.{}", doc.name, p.name), &p.ty, enums, true)?;
                let default = match &p.default {
                    None => ty.default_value(),
                    Some(json) => re_reflect::Doc::from(json)
                        .to_value(&ty, &|_| None)
                        .map_err(|e| bad(format!("{}.{}: default: {e}", doc.name, p.name)))?,
                };
                own += 1;
                Storage::Own {
                    index: own - 1,
                    ty,
                    default,
                }
            }
        };
        properties.push(PropertyDef {
            name: p.name.clone(),
            ty: p.ty.clone(),
            storage,
            read_only: p.read_only,
            deprecated: p.deprecated.clone(),
            status: p.status,
            note: p.note.clone(),
            doc: p.doc.clone(),
        });
    }
    Ok(ClassDef {
        name: doc.name.clone(),
        superclass: doc.superclass.clone(),
        creatable: doc.creatable,
        service: doc.service,
        components: doc.components.clone(),
        status: doc.status.unwrap_or(Status::Missing),
        note: doc.note.clone(),
        doc: doc.doc.clone(),
        properties,
        methods: doc.methods.iter().map(method_def).collect(),
        events: doc.events.iter().map(event_def).collect(),
    })
}

fn method_def(m: &MethodDoc) -> MethodDef {
    MethodDef {
        name: m.name.clone(),
        params: m.params.clone(),
        returns: m.returns.clone(),
        yields: m.yields,
        deprecated: m.deprecated.clone(),
        status: m.status,
        note: m.note.clone(),
        doc: m.doc.clone(),
    }
}

fn event_def(e: &EventDoc) -> EventDef {
    EventDef {
        name: e.name.clone(),
        params: e.params.clone(),
        deprecated: e.deprecated.clone(),
        status: e.status,
        note: e.note.clone(),
        doc: e.doc.clone(),
    }
}

/// The field type that stores a value of data-file type `ty`:
/// * `bool`, `number` (f64), `int` (i64), `string`, `Vector3` (vec3),
///   `Color3` (color);
/// * `Instance` or `Instance<Class>` (entity), where `allow_entity`;
/// * `Enum.<Name>` (an enum of the item names of a declared enum type);
/// * `NumberSequence` and `ColorSequence` (a list of their keypoint
///   records);
/// * a record type declared by a library's `record` (or otherwise registered);
/// * a reflection type name (`f32`, `quat`, ...), for record fields that need
///   one.
///
/// `what` names the row in errors.
pub(crate) fn field_type(
    what: &str,
    ty: &str,
    enums: &BTreeMap<String, EnumDoc>,
    allow_entity: bool,
) -> Result<FieldType, CompatError> {
    let entity = ty == "Instance" || (ty.starts_with("Instance<") && ty.ends_with('>'));
    Ok(match ty {
        "number" => FieldType::F64,
        "int" => FieldType::I64,
        "Vector3" => FieldType::Vec3,
        "Color3" => FieldType::Color,
        "NumberSequence" | "ColorSequence" => {
            let keypoint = format!("{ty}Keypoint");
            let Some(id) = RecordId::by_name(&keypoint) else {
                return Err(bad(format!(
                    "{what}: type {ty} needs the {keypoint} record, which no library declares"
                )));
            };
            FieldType::List(Box::new(FieldType::Record(id)))
        }
        _ if entity && allow_entity => FieldType::Entity,
        _ => {
            if let Some(name) = ty.strip_prefix("Enum.") {
                let Some(e) = enums.get(name) else {
                    return Err(bad(format!("{what}: unknown enum type {ty}")));
                };
                let items: Vec<String> = e.items.iter().map(|i| i.name.clone()).collect();
                FieldType::from_name("enum", &items).expect("an enum type has items")
            } else if let Some(t) = FieldType::from_name(ty, &[]).filter(FieldType::is_scalar) {
                t
            } else if let Some(id) = RecordId::by_name(ty) {
                FieldType::Record(id)
            } else {
                return Err(bad(format!(
                    "{what}: type {ty} has no field type here (a record type must be declared \
                     by a library's `record`)"
                )));
            }
        }
    })
}

/// Registers the record types the libraries declare (`record`), in name order.
fn register_records(
    libraries: &BTreeMap<String, LibraryDoc>,
    enums: &BTreeMap<String, EnumDoc>,
) -> Result<(), CompatError> {
    for l in libraries.values().filter(|l| !l.record.is_empty()) {
        let mut fields = Vec::with_capacity(l.record.len());
        for f in &l.record {
            let ty = field_type(&format!("{}.{}", l.name, f.name), &f.ty, enums, false)?;
            fields.push(re_reflect::RecordField::new(&f.name, ty));
        }
        re_reflect::RecordSchema::new(&l.name, fields)
            .register()
            .map_err(|e| bad(format!("record {}: {e}", l.name)))?;
    }
    Ok(())
}

/// Every class, enum and library, built once and read-only afterwards.
#[derive(Debug)]
pub struct ClassRegistry {
    classes: Vec<ClassDef>,
    by_name: HashMap<String, usize>,
    tree: Tree,
    enums: BTreeMap<String, EnumDoc>,
    libraries: BTreeMap<String, LibraryDoc>,
    converters: HashMap<String, Converter>,
    accessors: HashMap<(String, String), Accessor>,
}

impl ClassRegistry {
    /// The registry of the data files built into the crate, checked as data
    /// only ([`ClassRegistryBuilder::build_data_only`]): it has no converters
    /// or accessors, so builtin and converted properties raise `RE9001`. The
    /// runtime builds its registry with [`ClassRegistryBuilder`] and the
    /// implementing work packages' converters and accessors.
    pub fn builtin() -> Result<ClassRegistry, CompatError> {
        ClassRegistryBuilder::builtin()?.build_data_only()
    }

    fn check_data(&self) -> Result<(), CompatError> {
        for (c, chain) in self.classes.iter().zip(&self.tree.chains) {
            // A member may override an inherited member of the same kind only.
            for anc in chain.iter().skip(1).map(|&a| self.def(a)) {
                for m in members_of(c) {
                    if let Some(inherited) = members_of(anc).find(|x| x.name() == m.name())
                        && inherited.kind() != m.kind()
                    {
                        return Err(bad(format!(
                            "{}.{} is a {} but {}.{} is a {}",
                            c.name,
                            m.name(),
                            m.kind(),
                            anc.name,
                            m.name(),
                            inherited.kind()
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn check_bindings(&self) -> Result<(), CompatError> {
        for c in &self.classes {
            for p in &c.properties {
                match &p.storage {
                    Storage::Alias { via: Some(v), .. } if !self.converters.contains_key(v) => {
                        return Err(bad(format!(
                            "{}.{}: no converter named {v} is registered",
                            c.name, p.name
                        )));
                    }
                    // A callback's function lives in the VM, not the world
                    // (`crate::instance::callback`); it needs no accessor.
                    Storage::Builtin
                        if matches!(p.status, Status::Implemented | Status::Partial)
                            && p.ty != crate::instance::CALLBACK_TYPE
                            && !self
                                .accessors
                                .contains_key(&(c.name.clone(), p.name.clone())) =>
                    {
                        return Err(bad(format!(
                            "{}.{} is {} but no accessor is registered for it",
                            c.name,
                            p.name,
                            p.status.as_str()
                        )));
                    }
                    _ => {}
                }
            }
        }
        for (class, prop) in self.accessors.keys() {
            let ok = self.try_class(class).is_some_and(|c| {
                c.properties
                    .iter()
                    .any(|p| &p.name == prop && p.storage == Storage::Builtin)
            });
            if !ok {
                return Err(bad(format!(
                    "an accessor is registered for {class}.{prop}, which is not a builtin property"
                )));
            }
        }
        Ok(())
    }

    /// The class and its superclasses, the class first, ending at
    /// `Instance`; `RE9001` for an unknown class.
    fn chain(
        &self,
        class: &str,
    ) -> Result<impl DoubleEndedIterator<Item = &ClassDef>, CompatError> {
        let id = self.id(class).ok_or_else(|| self.unknown(class))?;
        Ok(self.tree.chains[id.index()].iter().map(|&c| self.def(c)))
    }

    fn unknown(&self, name: &str) -> CompatError {
        CompatError::UnknownClass {
            name: name.to_string(),
            suggestion: suggest(name, self.classes.iter().map(|c| c.name.as_str())),
        }
    }

    /// The class named `name`, or `RE9001` with a did-you-mean.
    pub fn class(&self, name: &str) -> Result<&ClassDef, CompatError> {
        self.try_class(name).ok_or_else(|| self.unknown(name))
    }

    /// The class named `name`.
    pub fn try_class(&self, name: &str) -> Option<&ClassDef> {
        self.by_name.get(name).map(|&i| &self.classes[i])
    }

    /// The number of the class named `name`.
    pub fn id(&self, name: &str) -> Option<ClassId> {
        self.by_name.get(name).map(|&i| ClassId(i as u16))
    }

    /// The class numbered `id`.
    ///
    /// # Panics
    ///
    /// Panics if `id` is not one of this registry's.
    pub fn def(&self, id: ClassId) -> &ClassDef {
        &self.classes[id.index()]
    }

    /// Every class, sorted by name.
    pub fn classes(&self) -> impl Iterator<Item = &ClassDef> {
        self.classes.iter()
    }

    /// The class and its superclasses, the class first, ending at `Instance`.
    /// Empty for an unknown class.
    pub fn ancestry(&self, class: &str) -> Vec<&ClassDef> {
        self.chain(class).map(Iterator::collect).unwrap_or_default()
    }

    /// True if `class` is `ancestor` or inherits from it (`IsA`); false if
    /// either is unknown. Constant-time, with no allocation.
    pub fn is_a(&self, class: &str, ancestor: &str) -> bool {
        match (self.id(class), self.id(ancestor)) {
            (Some(c), Some(a)) => self.tree.is_a(c, a),
            _ => false,
        }
    }

    /// [`ClassRegistry::is_a`] by number.
    pub fn is_a_id(&self, class: ClassId, ancestor: ClassId) -> bool {
        self.tree.is_a(class, ancestor)
    }

    /// `class` and every class that inherits from it, in id order.
    pub fn family(&self, class: ClassId) -> &[ClassId] {
        &self.tree.families[class.index()]
    }

    /// The member `name` of `class`, declared on the class or inherited (the
    /// nearest declaration wins), or `RE9001` with a did-you-mean over every
    /// member the class has.
    pub fn member(&self, class: &str, name: &str) -> Result<MemberRef<'_>, CompatError> {
        for c in self.chain(class)? {
            if let Some(m) = members_of(c).find(|m| m.name() == name) {
                return Ok(MemberRef {
                    class: c,
                    member: m,
                });
            }
        }
        let all: Vec<&str> = self
            .chain(class)?
            .flat_map(|c| members_of(c).map(|m| m.name()))
            .collect();
        Err(CompatError::UnknownMember {
            class: class.to_string(),
            member: name.to_string(),
            suggestion: suggest(name, all.into_iter()),
        })
    }

    /// The enum type `Enum.<name>`.
    pub fn enum_type(&self, name: &str) -> Option<&EnumDoc> {
        self.enums.get(name)
    }

    /// Every enum type, sorted by name.
    pub fn enums(&self) -> impl Iterator<Item = &EnumDoc> {
        self.enums.values()
    }

    /// The global `name` of a library file.
    pub fn library(&self, name: &str) -> Option<&LibraryDoc> {
        self.libraries.get(name)
    }

    /// Every library global, sorted by name.
    pub fn libraries(&self) -> impl Iterator<Item = &LibraryDoc> {
        self.libraries.values()
    }

    /// The converter registered under `name`.
    pub fn converter(&self, name: &str) -> Option<&Converter> {
        self.converters.get(name)
    }

    /// The accessor of a builtin property.
    pub fn accessor(&self, class: &str, property: &str) -> Option<&Accessor> {
        self.accessors
            .get(&(class.to_string(), property.to_string()))
    }
}

fn members_of(c: &ClassDef) -> impl Iterator<Item = Member<'_>> {
    c.properties
        .iter()
        .map(Member::Property)
        .chain(c.methods.iter().map(Member::Method))
        .chain(c.events.iter().map(Member::Event))
}
