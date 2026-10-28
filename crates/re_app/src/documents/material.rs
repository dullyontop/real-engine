//! `material.create` and `material.edit` (ADR-0064 §3).
//!
//! * `material.create {path, from?, set?}` makes a `*.material.json`: a
//!   `$schema` reference, `from`'s keys and `set`. It is refused when the
//!   file exists or is open with content (`RE2051`), or the path is not a
//!   material path inside the project (`RE2052`);
//! * `material.edit {path, set?, unset?}` sets and removes keys, one
//!   `set_doc` per key.
//!
//! [`re_assets::check_material`] checks every result as the asset server's
//! loader does (unknown keys, ranges). A texture the material names (a key
//! ending in `_texture`) must be a file of the project (`RE2054`); its
//! sidecar is written by the asset server when it loads it, not here.

use re_command::{Call, CommandRegistry, Op, Tier, codes as command_codes};
use re_core::{Diagnostic, Location};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};

use super::{kind_path, open_file, open_new, parse_json, token, transact};
use crate::app::App;

/// The suffix of material files.
pub const SUFFIX: &str = ".material.json";

/// Arguments of `material.create`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateArgs {
    /// The new material, relative to the project, ending in `.material.json`
    /// (the editor puts new ones in `materials/`).
    #[schemars(extend("x-real-path" = "write"))]
    pub path: String,
    /// A material to start from: its keys are copied.
    #[serde(default)]
    #[schemars(extend("x-real-path" = "read"))]
    pub from: Option<String>,
    /// Keys to set, as a material file writes them
    /// (`{"base_color": [1, 0.5, 0.2, 1], "roughness": 0.8}`).
    #[serde(default)]
    pub set: Map<String, Json>,
}

/// Arguments of `material.edit`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EditArgs {
    /// The material, relative to the project.
    #[schemars(extend("x-real-path" = "write"))]
    pub path: String,
    /// Keys to set, as a material file writes them.
    #[serde(default)]
    pub set: Map<String, Json>,
    /// Keys to remove, so their defaults apply.
    #[serde(default)]
    pub unset: Vec<String>,
}

/// The validator of open `*.material.json` documents.
pub(super) fn validate(value: &Json) -> Result<(), String> {
    re_assets::check_material(value).map(|_| ())
}

/// The `$schema` reference of a new material at `path`: the project's
/// `schemas/material.schema.json`, relative to the material's folder.
fn schema_ref(path: &str) -> String {
    let depth = path.matches('/').count();
    format!("{}schemas/material.schema.json", "../".repeat(depth))
}

/// Checks that each texture `keys` names is a file of the project.
fn check_textures(app: &App, path: &str, keys: &Map<String, Json>) -> Result<(), Diagnostic> {
    for (key, value) in keys {
        let Some(texture) = value.as_str().filter(|_| key.ends_with("_texture")) else {
            continue;
        };
        let missing = || {
            Diagnostic::error(
                command_codes::NAMED_FILE_MISSING,
                format!("material `{key}` names `{texture}`, which does not exist"),
            )
            .at(Location::file(path))
            .with_hint(
                "texture paths are relative to the project, for example \
                 `assets/textures/rock.png`; bring a file in with asset.import first",
            )
        };
        let rel = re_assets::normalize_path(texture).map_err(|_| missing())?;
        if !app.files().is_file(&rel) {
            return Err(missing());
        }
    }
    Ok(())
}

fn create(app: &mut App, a: CreateArgs, call: &mut Call) -> Result<Json, Diagnostic> {
    let path = kind_path(&a.path, SUFFIX, "a material")?;
    let mut content = Map::new();
    content.insert("$schema".into(), Json::String(schema_ref(&path)));
    if let Some(from) = &a.from {
        let from = kind_path(from, SUFFIX, "a material")?;
        let value = match app.world().doc(&from) {
            Some(doc) => doc.value.clone(),
            None => {
                let text = app.files().read_str(&from).map_err(|e| {
                    Diagnostic::error(
                        crate::codes::ASSET_UNAVAILABLE,
                        format!("cannot read {from}: {e}"),
                    )
                    .at(Location::file(from.clone()))
                })?;
                parse_json(&text).map_err(|e| super::invalid(&from, &e))?
            }
        };
        let Json::Object(keys) = value else {
            return Err(super::invalid(&from, "it is not a JSON object"));
        };
        content.extend(keys.into_iter().filter(|(k, _)| k != "$schema"));
    }
    check_textures(app, &path, &a.set)?;
    content.extend(a.set);
    let content = Json::Object(content);
    let opened = open_new(app, &path)?;
    transact(
        app,
        call,
        &path,
        opened,
        format!("material.create {path}"),
        vec![Op::SetDoc {
            doc: path.clone(),
            pointer: String::new(),
            value: Some(content.clone()),
        }],
    )?;
    Ok(json!({ "path": path, "material": content }))
}

fn edit(app: &mut App, a: EditArgs, call: &mut Call) -> Result<Json, Diagnostic> {
    let path = kind_path(&a.path, SUFFIX, "a material")?;
    if let Some(key) = a.unset.iter().find(|k| a.set.contains_key(*k)) {
        return Err(Diagnostic::error(
            re_command::codes::INVALID_ARGUMENTS,
            format!("`{key}` is both set and unset"),
        ));
    }
    check_textures(app, &path, &a.set)?;
    let opened = open_file(app, &path, parse_json)?;
    let current = app
        .world()
        .doc(&path)
        .and_then(|d| d.value.as_object())
        .cloned()
        .unwrap_or_default();
    let mut ops = Vec::new();
    for (key, value) in &a.set {
        if current.get(key) != Some(value) {
            ops.push(Op::SetDoc {
                doc: path.clone(),
                pointer: format!("/{}", token(key)),
                value: Some(value.clone()),
            });
        }
    }
    for key in &a.unset {
        if current.contains_key(key) {
            ops.push(Op::SetDoc {
                doc: path.clone(),
                pointer: format!("/{}", token(key)),
                value: None,
            });
        }
    }
    let count = ops.len();
    let changed = transact(
        app,
        call,
        &path,
        opened,
        format!(
            "material.edit {path} ({count} key{})",
            if count == 1 { "" } else { "s" }
        ),
        ops,
    )?;
    let material = if call.dry_run() {
        // The document as the edit would leave it.
        let mut m = current;
        for (k, v) in a.set {
            m.insert(k, v);
        }
        for k in &a.unset {
            m.remove(k);
        }
        Json::Object(m)
    } else {
        app.world()
            .doc(&path)
            .map_or(Json::Null, |d| d.value.clone())
    };
    Ok(json!({ "path": path, "changed": changed, "material": material }))
}

/// Registers `material.create` and `material.edit`.
pub fn register(registry: &mut CommandRegistry<App>) {
    registry.register_typed::<CreateArgs, _>(
        "material.create",
        "Make a material file (`*.material.json`, ADR-0064): a $schema reference, the keys of `from` (another material) and `set`, checked as the asset server loads materials. Refused when the file exists or the path is not a material path. One undo step; the project save writes the file, and undo removes it at the next save.",
        Tier::Edit,
        false,
        create,
    );
    registry.register_typed::<EditArgs, _>(
        "material.edit",
        "Set (`set`) and remove (`unset`) keys of a material file, checked as the asset server loads materials; a texture it names must exist. One undo step; the project save writes the file.",
        Tier::Edit,
        false,
        edit,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_schema_is_referenced_from_the_material_folder() {
        assert_eq!(
            schema_ref("rock.material.json"),
            "schemas/material.schema.json"
        );
        assert_eq!(
            schema_ref("materials/rock.material.json"),
            "../schemas/material.schema.json"
        );
        assert_eq!(
            schema_ref("materials/stone/rock.material.json"),
            "../../schemas/material.schema.json"
        );
    }
}
