//! Prefab files (`prefabs/*.prefab.json`): reusable entity trees in the scene
//! file format with `"format": "real-engine/prefab"`. A prefab is instantiated
//! through the procedural fragment model (ADR-0017): [`PrefabFile::instantiate`]
//! turns it into a [`SceneFragment`] whose keys are
//! `<key_prefix>/<prefab entity id>`, which a generator returns as part of its
//! output.

use std::collections::HashMap;

use re_reflect::{ComponentSchemaDoc, FieldType, Registry, Value};
use serde::{Deserialize, Serialize};

use crate::error::SceneError;
use crate::procedural::{GenNode, SceneFragment};
use crate::scene::{EntityDoc, VERSION};

/// The `format` value of a prefab file.
pub const PREFAB_FORMAT: &str = "real-engine/prefab";

/// A prefab file: the scene document with `"format": "real-engine/prefab"`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PrefabFile {
    /// The optional `$schema` key, kept as written.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Always [`PREFAB_FORMAT`].
    pub format: String,
    /// The file version (the scene file version).
    pub version: u32,
    /// Schemas of the script-defined components used in the file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_components: Vec<ComponentSchemaDoc>,
    /// The entities, parents before children; ids are local to the prefab.
    pub entities: Vec<EntityDoc>,
}

impl PrefabFile {
    /// Reads a prefab file and checks its format and version.
    pub fn from_json(text: &str) -> Result<PrefabFile, SceneError> {
        let p: PrefabFile =
            serde_json::from_str(text).map_err(|e| SceneError::Parse(e.to_string()))?;
        if p.format != PREFAB_FORMAT {
            return Err(SceneError::UnsupportedFormat(format!(
                "expected format {PREFAB_FORMAT:?}, found {:?}",
                p.format
            )));
        }
        if p.version != VERSION {
            return Err(SceneError::UnsupportedFormat(format!(
                "unsupported prefab version {} (this build reads {VERSION})",
                p.version
            )));
        }
        Ok(p)
    }

    /// Writes the prefab in the scene file style.
    pub fn to_json(&self) -> String {
        crate::scene::to_pretty_json(self)
    }

    /// The fragment this prefab instantiates into: one [`GenNode`] per entity,
    /// keyed `<key_prefix>/<prefab entity id>`, with parents mapped the same
    /// way (the prefab's roots have no parent key, so they become children of
    /// the procedural source unless the caller re-parents them).
    ///
    /// Component values are converted against `registry` (which must already
    /// know the prefab's script components; see
    /// [`World::declare_script_components`](crate::World::declare_script_components)).
    /// Entity-typed fields must be null: a fragment cannot reference other
    /// entities in M1.
    pub fn instantiate(
        &self,
        registry: &Registry,
        key_prefix: &str,
    ) -> Result<SceneFragment, SceneError> {
        let mut out = Vec::with_capacity(self.entities.len());
        let mut known: HashMap<&str, ()> = HashMap::new();
        for (i, d) in self.entities.iter().enumerate() {
            let at = format!("entities[{i}] (id {:?})", d.id);
            if d.id.is_empty() || d.id.contains('/') {
                return Err(SceneError::ParseAt {
                    at,
                    error: Box::new(SceneError::InvalidId(d.id.clone())),
                });
            }
            if known.insert(d.id.as_str(), ()).is_some() {
                return Err(SceneError::DuplicateId(d.id.clone()));
            }
            let parent_key = match &d.parent {
                Some(p) if known.contains_key(p.as_str()) && p != &d.id => {
                    Some(format!("{key_prefix}/{p}"))
                }
                Some(p) => {
                    return Err(SceneError::ForwardParent {
                        at,
                        parent: p.clone(),
                    });
                }
                None => None,
            };
            let mut components = Vec::with_capacity(d.components.len());
            for (cname, fields) in &d.components {
                let wrap = |e: SceneError| SceneError::ParseAt {
                    at: at.clone(),
                    error: Box::new(e),
                };
                let key = registry.key(cname).map_err(|e| wrap(e.into()))?;
                let schema = registry.schema(key);
                let mut values = Vec::with_capacity(fields.len());
                for (fname, doc) in fields {
                    let index = schema.field_index(fname).map_err(|e| wrap(e.into()))?;
                    let ty = &schema.fields[index].ty;
                    if *ty == FieldType::Entity && *doc != re_reflect::Doc::Null {
                        return Err(wrap(SceneError::Invalid(format!(
                            "{cname}.{fname}: entity references are not supported in prefabs"
                        ))));
                    }
                    let v: Value = doc.to_value(ty, &|_| None).map_err(|error| {
                        wrap(
                            re_reflect::ReflectError::Value {
                                path: format!("{cname}.{fname}"),
                                error,
                            }
                            .into(),
                        )
                    })?;
                    values.push((fname.clone(), v));
                }
                components.push((cname.clone(), values));
            }
            out.push(GenNode {
                key: format!("{key_prefix}/{}", d.id),
                name: d.name.clone(),
                parent_key,
                components,
            });
        }
        Ok(out)
    }
}
