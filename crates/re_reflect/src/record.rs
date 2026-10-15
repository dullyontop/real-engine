//! Records: named, fixed tuples of scalar fields, for small datatypes such as
//! a 2D size with scale and offset, a number range or a sequence keypoint
//! (ADR-0032 amendment).
//!
//! A record type is registered once per process with
//! [`RecordSchema::register`] and is then addressed by its [`RecordId`] in
//! [`FieldType::Record`] and [`Value::Record`]. Registration is idempotent for
//! an identical schema and refuses a different schema under a taken name, so
//! a record's shape never changes after it is registered and the set of field
//! types stays closed.
//!
//! Record fields are scalars only: `bool`, the integer and float types,
//! `vec3`, `quat`, `color`, `string` and `enum`. A record holds no entity
//! reference, list, map or nested record.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use crate::error::ReflectError;
use crate::registry::is_identifier;
use crate::value::{FieldType, Value};

/// One field of a [`RecordSchema`].
#[derive(Clone, Debug, PartialEq)]
pub struct RecordField {
    /// The field's name (an identifier).
    pub name: String,
    /// The field's type (a scalar type).
    pub ty: FieldType,
    /// Documentation.
    pub doc: String,
}

impl RecordField {
    /// A field with no doc.
    pub fn new(name: &str, ty: FieldType) -> Self {
        Self {
            name: name.to_string(),
            ty,
            doc: String::new(),
        }
    }
}

/// The shape of a record type.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordSchema {
    /// The record's name (an identifier, unique in the process).
    pub name: String,
    /// Documentation.
    pub doc: String,
    /// The fields, in order. A record value holds one value per field.
    pub fields: Vec<RecordField>,
}

/// Handle to a registered record type. Cheap to copy; valid for the whole
/// process. Files and descriptions name records by name, never by id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordId(u32);

/// Field-type names a record may not take, because they name the built-in
/// kinds in files (`{"kind": "vec3"}`) and tagged map values (`{"$type": "list"}`).
const RESERVED: &[&str] = &[
    "bool", "i32", "i64", "f32", "f64", "vec3", "quat", "color", "string", "entity", "enum",
    "list", "map", "record",
];

#[derive(Default)]
struct Table {
    schemas: Vec<&'static RecordSchema>,
    by_name: HashMap<String, RecordId>,
}

fn table() -> &'static RwLock<Table> {
    static TABLE: OnceLock<RwLock<Table>> = OnceLock::new();
    TABLE.get_or_init(|| RwLock::new(Table::default()))
}

impl RecordSchema {
    /// A record schema from its name and fields, with no doc.
    pub fn new(name: &str, fields: Vec<RecordField>) -> Self {
        Self {
            name: name.to_string(),
            doc: String::new(),
            fields,
        }
    }

    /// Registers the record type for this process. Registering an identical
    /// schema again (docs aside) returns the same id; a different schema under
    /// a registered name is refused.
    pub fn register(self) -> Result<RecordId, ReflectError> {
        if !is_identifier(&self.name) || RESERVED.contains(&self.name.as_str()) {
            return Err(ReflectError::InvalidSchema(format!(
                "record name {:?} must be an identifier other than a built-in type name",
                self.name
            )));
        }
        if self.fields.is_empty() {
            return Err(ReflectError::InvalidSchema(format!(
                "record {} needs at least one field",
                self.name
            )));
        }
        for (i, f) in self.fields.iter().enumerate() {
            if !is_identifier(&f.name) {
                return Err(ReflectError::InvalidSchema(format!(
                    "record {}: field name {:?} must be an identifier",
                    self.name, f.name
                )));
            }
            if self.fields[..i].iter().any(|g| g.name == f.name) {
                return Err(ReflectError::InvalidSchema(format!(
                    "record {} declares field {:?} twice",
                    self.name, f.name
                )));
            }
            if !f.ty.is_scalar() {
                return Err(ReflectError::InvalidSchema(format!(
                    "record {}.{}: a record field must be a scalar type, not {}",
                    self.name,
                    f.name,
                    f.ty.name()
                )));
            }
        }
        let mut t = table().write().unwrap_or_else(|e| e.into_inner());
        if let Some(&id) = t.by_name.get(&self.name) {
            let old = t.schemas[id.0 as usize];
            let same = old.fields.len() == self.fields.len()
                && old
                    .fields
                    .iter()
                    .zip(&self.fields)
                    .all(|(a, b)| a.name == b.name && a.ty == b.ty);
            return if same {
                Ok(id)
            } else {
                Err(ReflectError::InvalidSchema(format!(
                    "record {} is already registered with different fields",
                    self.name
                )))
            };
        }
        let id = RecordId(t.schemas.len() as u32);
        t.by_name.insert(self.name.clone(), id);
        t.schemas.push(Box::leak(Box::new(self)));
        Ok(id)
    }

    /// The index of the field named `name`.
    pub fn field_index(&self, name: &str) -> Option<usize> {
        self.fields.iter().position(|f| f.name == name)
    }

    /// Every field's zero value ([`FieldType::default_value`]), in order.
    pub fn default_values(&self) -> Vec<Value> {
        self.fields.iter().map(|f| f.ty.default_value()).collect()
    }
}

impl RecordId {
    /// The record's schema.
    pub fn schema(self) -> &'static RecordSchema {
        let t = table().read().unwrap_or_else(|e| e.into_inner());
        t.schemas[self.0 as usize]
    }

    /// The record's name.
    pub fn name(self) -> &'static str {
        &self.schema().name
    }

    /// The record type registered under `name`, if any.
    pub fn by_name(name: &str) -> Option<RecordId> {
        let t = table().read().unwrap_or_else(|e| e.into_inner());
        t.by_name.get(name).copied()
    }

    /// Every registered record type, in registration order.
    pub fn all() -> Vec<RecordId> {
        let t = table().read().unwrap_or_else(|e| e.into_inner());
        (0..t.schemas.len() as u32).map(RecordId).collect()
    }
}
