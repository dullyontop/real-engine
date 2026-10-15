//! Machine-readable description of every registered component type: what an
//! editor inspector, a Luau binding generator or an agent reads to know what
//! exists. Also emitted as JSON Schema (2020-12) for scene files, so tools can
//! validate a scene without the engine (ADR-0016).
//!
//! [`ComponentSchemaDoc`] is the serialisable form of a [`ComponentSchema`],
//! used by a scene file's `script_components` list.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};

use crate::doc::Doc;
use crate::error::ReflectError;
use crate::record::RecordId;
use crate::registry::{ComponentOrigin, ComponentSchema, FieldSchema, Registry};
use crate::value::FieldType;

/// The `format` value of a scene file.
pub const SCENE_FORMAT: &str = "real-engine/scene";
/// The scene file version this build writes.
pub const SCENE_VERSION: u32 = 1;
/// The `format` value of [`Registry::describe`]'s output.
pub const DESCRIBE_FORMAT: &str = "real-engine/schema";

/// A field type as written in files and descriptions: `{"kind": "f32"}`,
/// `{"kind": "enum", "variants": ["A", "B"]}`, `{"kind": "list", "item":
/// {"kind": "f32"}}`, `{"kind": "map"}` or `{"kind": "record", "record": "UDim2"}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldTypeDoc {
    /// The type's name (see [`FieldType::name`]).
    pub kind: String,
    /// The variants, for `"enum"` only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// The item type, for `"list"` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Box<FieldTypeDoc>>,
    /// The record type's name, for `"record"` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
}

impl From<&FieldType> for FieldTypeDoc {
    fn from(ty: &FieldType) -> Self {
        FieldTypeDoc {
            kind: ty.name().to_string(),
            variants: match ty {
                FieldType::Enum(v) => v.iter().map(|s| s.to_string()).collect(),
                _ => Vec::new(),
            },
            item: match ty {
                FieldType::List(item) => Some(Box::new(FieldTypeDoc::from(&**item))),
                _ => None,
            },
            record: match ty {
                FieldType::Record(id) => Some(id.name().to_string()),
                _ => None,
            },
        }
    }
}

impl FieldTypeDoc {
    /// The field type this document names, checked against the nesting
    /// rules ([`FieldType::validate`]). A record must already be registered.
    pub fn to_field_type(&self) -> Result<FieldType, String> {
        let ty = match self.kind.as_str() {
            "list" => {
                let item = self
                    .item
                    .as_ref()
                    .ok_or("a list type needs `item`")?
                    .to_field_type()?;
                FieldType::List(Box::new(item))
            }
            "record" => {
                let name = self.record.as_ref().ok_or("a record type needs `record`")?;
                FieldType::Record(
                    RecordId::by_name(name).ok_or(format!("no record type named {name:?}"))?,
                )
            }
            kind => FieldType::from_name(kind, &self.variants)
                .ok_or(format!("unknown field type {kind:?}"))?,
        };
        if self.item.is_some() && !matches!(ty, FieldType::List(_)) {
            return Err(format!(
                "`item` is only valid for a list, not {:?}",
                self.kind
            ));
        }
        if self.record.is_some() && !matches!(ty, FieldType::Record(_)) {
            return Err(format!(
                "`record` is only valid for a record, not {:?}",
                self.kind
            ));
        }
        ty.validate()?;
        Ok(ty)
    }
}

/// One field of a [`ComponentSchemaDoc`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldSchemaDoc {
    /// The field's name.
    pub name: String,
    /// The field's type.
    #[serde(rename = "type")]
    pub ty: FieldTypeDoc,
    /// The default, in file form.
    pub default: Doc,
    /// Documentation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
}

/// The file form of a script-defined component's schema (a scene file's
/// `script_components` entry).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentSchemaDoc {
    /// The component's name.
    pub name: String,
    /// Documentation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub doc: String,
    /// The fields, in declaration order.
    pub fields: Vec<FieldSchemaDoc>,
}

impl From<&ComponentSchema> for ComponentSchemaDoc {
    fn from(s: &ComponentSchema) -> Self {
        let no_names = |_| None;
        ComponentSchemaDoc {
            name: s.name.clone(),
            doc: s.doc.clone(),
            fields: s
                .fields
                .iter()
                .map(|f| FieldSchemaDoc {
                    name: f.name.clone(),
                    ty: FieldTypeDoc::from(&f.ty),
                    default: Doc::from_value(&f.default, &f.ty, &no_names),
                    doc: f.doc.clone(),
                })
                .collect(),
        }
    }
}

impl ComponentSchemaDoc {
    /// The script-origin [`ComponentSchema`] this document declares.
    pub fn to_schema(&self) -> Result<ComponentSchema, ReflectError> {
        let mut fields = Vec::with_capacity(self.fields.len());
        for f in &self.fields {
            let ty = f.ty.to_field_type().map_err(|e| {
                ReflectError::InvalidSchema(format!("{}.{}: {e}", self.name, f.name))
            })?;
            let default =
                f.default
                    .to_value(&ty, &|_| None)
                    .map_err(|error| ReflectError::Value {
                        path: format!("{}.{}", self.name, f.name),
                        error,
                    })?;
            fields.push(FieldSchema {
                name: f.name.clone(),
                ty,
                doc: f.doc.clone(),
                default,
            });
        }
        Ok(ComponentSchema {
            name: self.name.clone(),
            doc: self.doc.clone(),
            fields,
            origin: ComponentOrigin::Script,
        })
    }
}

fn field_json_schema(ty: &FieldType) -> Json {
    let num_array = |n: usize| json!({ "type": "array", "items": { "type": "number" }, "minItems": n, "maxItems": n });
    match ty {
        FieldType::Bool => json!({ "type": "boolean" }),
        FieldType::I32 => json!({ "type": "integer", "minimum": i32::MIN, "maximum": i32::MAX }),
        FieldType::I64 => json!({ "type": "integer" }),
        FieldType::F32 | FieldType::F64 => json!({ "type": "number" }),
        FieldType::Vec3 => num_array(3),
        FieldType::Quat => num_array(4),
        FieldType::Color => {
            json!({ "type": "array", "items": { "type": "number" }, "minItems": 3, "maxItems": 4 })
        }
        FieldType::String => json!({ "type": "string" }),
        FieldType::Entity => {
            json!({ "type": ["string", "null"], "description": "stable id of another entity" })
        }
        FieldType::Enum(variants) => json!({ "enum": variants }),
        FieldType::List(item) => json!({ "type": "array", "items": field_json_schema(item) }),
        FieldType::Map => json!({
            "type": "object",
            "description": "string keys; values are a bool, a number, a string or an object tagged with `$type`",
            "additionalProperties": dynamic_json_schema()
        }),
        FieldType::Record(id) => record_json_schema(*id),
    }
}

fn record_json_schema(id: RecordId) -> Json {
    let schema = id.schema();
    let props: Map<String, Json> = schema
        .fields
        .iter()
        .map(|f| (f.name.clone(), field_json_schema(&f.ty)))
        .collect();
    let names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
    let items: Vec<Json> = schema
        .fields
        .iter()
        .map(|f| field_json_schema(&f.ty))
        .collect();
    let n = schema.fields.len();
    json!({
        "description": format!("record {}", schema.name),
        "oneOf": [
            { "type": "object", "properties": props, "required": names, "additionalProperties": false },
            { "type": "array", "prefixItems": items, "minItems": n, "maxItems": n }
        ]
    })
}

/// The schema of a map value: plain JSON for bools, numbers and strings, a
/// `$type`-tagged object for everything else.
fn dynamic_json_schema() -> Json {
    json!({
        "oneOf": [
            { "type": ["boolean", "number", "string"] },
            {
                "type": "object",
                "required": ["$type", "value"],
                "properties": {
                    "$type": { "enum": ["vec3", "quat", "color", "list", "record"] },
                    "name": { "type": "string" },
                    "value": {}
                },
                "additionalProperties": false
            }
        ]
    })
}

fn doc_json(doc: &Doc) -> Json {
    serde_json::to_value(doc).unwrap_or(Json::Null)
}

impl Registry {
    /// The registry description: every component with its origin, doc and
    /// fields (name, type, default, doc), in registration order.
    pub fn describe(&self) -> Json {
        let no_names = |_| None;
        let components: Vec<Json> = self
            .schemas()
            .map(|s| {
                let fields: Vec<Json> = s
                    .fields
                    .iter()
                    .map(|f| {
                        json!({
                            "name": f.name,
                            "type": FieldTypeDoc::from(&f.ty),
                            "default": doc_json(&Doc::from_value(&f.default, &f.ty, &no_names)),
                            "doc": f.doc,
                        })
                    })
                    .collect();
                json!({
                    "name": s.name,
                    "origin": s.origin.as_str(),
                    "doc": s.doc,
                    "fields": fields,
                })
            })
            .collect();
        fn collect(ty: &FieldType, out: &mut Vec<RecordId>) {
            match ty {
                FieldType::Record(id) if !out.contains(id) => out.push(*id),
                FieldType::List(item) => collect(item, out),
                _ => {}
            }
        }
        // Only the records this registry's fields use, by name, so the output
        // does not depend on what else the process registered.
        let mut used = Vec::new();
        for s in self.schemas() {
            for f in &s.fields {
                collect(&f.ty, &mut used);
            }
        }
        used.sort_by_key(|id| id.name());
        let records: Vec<Json> = used
            .into_iter()
            .map(|id| {
                let r = id.schema();
                json!({
                    "name": r.name,
                    "doc": r.doc,
                    "fields": r.fields.iter().map(|f| json!({
                        "name": f.name,
                        "type": FieldTypeDoc::from(&f.ty),
                        "doc": f.doc,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({
            "format": DESCRIBE_FORMAT,
            "version": 1,
            "components": components,
            "records": records,
        })
    }

    /// JSON Schema (2020-12) for a scene file with the currently registered
    /// components.
    pub fn scene_json_schema(&self) -> Json {
        let no_names = |_| None;
        let mut defs = Map::new();
        for s in self.schemas() {
            let mut props = Map::new();
            for f in &s.fields {
                let mut p = field_json_schema(&f.ty);
                if let Some(obj) = p.as_object_mut() {
                    obj.insert(
                        "default".into(),
                        doc_json(&Doc::from_value(&f.default, &f.ty, &no_names)),
                    );
                    if !f.doc.is_empty() {
                        obj.insert("description".into(), f.doc.clone().into());
                    }
                }
                props.insert(f.name.clone(), p);
            }
            defs.insert(
                s.name.clone(),
                json!({
                    "type": "object",
                    "description": s.doc,
                    "properties": props,
                    "additionalProperties": false
                }),
            );
        }
        let component_props: Map<String, Json> = defs
            .keys()
            .map(|k| (k.clone(), json!({ "$ref": format!("#/$defs/{k}") })))
            .collect();
        let field_doc = json!({
            "type": "object",
            "required": ["name", "type", "default"],
            "additionalProperties": false,
            "properties": {
                "name": { "type": "string" },
                "type": {
                    "type": "object",
                    "required": ["kind"],
                    "additionalProperties": false,
                    "properties": {
                        "kind": { "enum": ["bool", "i32", "i64", "f32", "f64", "vec3", "quat", "color", "string", "entity", "enum", "list", "map", "record"] },
                        "variants": { "type": "array", "items": { "type": "string" } },
                        "item": { "type": "object" },
                        "record": { "type": "string" }
                    }
                },
                "default": {},
                "doc": { "type": "string" }
            }
        });
        let script_components = json!({
            "type": "array",
            "description": "Schemas of the script-defined components used in this file.",
            "items": {
                "type": "object",
                "required": ["name", "fields"],
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string", "pattern": "^[A-Za-z_][A-Za-z0-9_]*$" },
                    "doc": { "type": "string" },
                    "fields": { "type": "array", "items": field_doc }
                }
            }
        });
        let field_override = json!({
            "type": "object",
            "required": ["target", "component", "field", "value"],
            "additionalProperties": false,
            "properties": {
                "target": { "type": "string" },
                "component": { "type": "string" },
                "field": { "type": "string" },
                "value": {}
            }
        });
        let deletion_override = json!({
            "type": "object",
            "required": ["target", "deleted"],
            "additionalProperties": false,
            "properties": {
                "target": { "type": "string" },
                "deleted": { "const": true }
            }
        });
        let entity = json!({
            "type": "object",
            "required": ["id"],
            "additionalProperties": false,
            "properties": {
                "id": { "type": "string", "minLength": 1, "pattern": "^[^/]+$" },
                "name": { "type": "string" },
                "parent": { "type": "string" },
                "components": {
                    "type": "object",
                    "properties": component_props,
                    "additionalProperties": false
                },
                "overrides": {
                    "type": "array",
                    "items": { "oneOf": [field_override, deletion_override] }
                }
            }
        });
        json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "Real Engine scene",
            "type": "object",
            "required": ["format", "version", "entities"],
            "additionalProperties": false,
            "properties": {
                "$schema": { "type": "string" },
                "format": { "const": SCENE_FORMAT },
                "version": { "const": SCENE_VERSION },
                "script_components": script_components,
                "entities": { "type": "array", "items": entity }
            },
            "$defs": defs,
        })
    }
}
