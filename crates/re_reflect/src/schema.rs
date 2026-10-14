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
use crate::registry::{ComponentOrigin, ComponentSchema, FieldSchema, Registry};
use crate::value::FieldType;

/// The `format` value of a scene file.
pub const SCENE_FORMAT: &str = "real-engine/scene";
/// The scene file version this build writes.
pub const SCENE_VERSION: u32 = 1;
/// The `format` value of [`Registry::describe`]'s output.
pub const DESCRIBE_FORMAT: &str = "real-engine/schema";

/// A field type as written in files and descriptions:
/// `{"kind": "f32"}` or `{"kind": "enum", "variants": ["A", "B"]}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FieldTypeDoc {
    /// The type's name (see [`FieldType::name`]).
    pub kind: String,
    /// The variants, for `"enum"` only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
}

impl From<&FieldType> for FieldTypeDoc {
    fn from(ty: &FieldType) -> Self {
        FieldTypeDoc {
            kind: ty.name().to_string(),
            variants: match ty {
                FieldType::Enum(v) => v.iter().map(|s| s.to_string()).collect(),
                _ => Vec::new(),
            },
        }
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
            let ty = FieldType::from_name(&f.ty.kind, &f.ty.variants).ok_or_else(|| {
                ReflectError::InvalidSchema(format!(
                    "{}.{}: unknown field type {:?}",
                    self.name, f.name, f.ty.kind
                ))
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
    }
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
        json!({
            "format": DESCRIBE_FORMAT,
            "version": 1,
            "components": components,
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
                        "kind": { "enum": ["bool", "i32", "i64", "f32", "f64", "vec3", "quat", "color", "string", "entity", "enum"] },
                        "variants": { "type": "array", "items": { "type": "string" } }
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
