//! `real schema [--out DIR]`: every JSON Schema the engine defines, generated
//! from the engine itself (`cargo xtask schemas` calls this and checks the
//! committed `schemas/` against it).

use std::path::Path;

use re_core::Diagnostic;
use serde_json::Value as Json;

use crate::codes;

/// The schemas, as `(file name, schema)`, in file-name order.
pub fn all() -> Vec<(&'static str, Json)> {
    let world = re_scene::World::new();
    let scene = world.scene_json_schema();
    let mut prefab = scene.clone();
    if let Some(obj) = prefab.as_object_mut() {
        obj.insert("title".into(), Json::from("Real Engine prefab"));
        if let Some(format) = obj.get_mut("properties").and_then(|p| p.get_mut("format")) {
            *format = serde_json::json!({ "const": re_scene::PREFAB_FORMAT });
        }
    }
    let to_json = |s: schemars::Schema| serde_json::to_value(s).unwrap_or(Json::Null);
    let mut out = vec![
        (
            "graph.schema.json",
            to_json(schemars::schema_for!(re_types::GraphDoc)),
        ),
        (
            "material.schema.json",
            to_json(schemars::schema_for!(re_assets::MaterialFile)),
        ),
        ("prefab.schema.json", prefab),
        (
            "replay.schema.json",
            to_json(schemars::schema_for!(re_types::ReplayDoc)),
        ),
        ("scene.schema.json", scene),
        (
            "verify-report.schema.json",
            to_json(schemars::schema_for!(re_app::VerifyReport)),
        ),
    ];
    out.sort_by_key(|(name, _)| *name);
    out
}

/// Pretty JSON with a final newline (stable output for committed files).
pub fn to_text(schema: &Json) -> String {
    let mut s = serde_json::to_string_pretty(schema).unwrap_or_default();
    s.push('\n');
    s
}

/// Writes every schema into `dir` (created if missing).
///
/// # Errors
///
/// `RE0902` when the directory or a file cannot be written.
pub fn write_all(dir: &Path) -> Result<Vec<String>, Diagnostic> {
    let fail = |what: String, e: std::io::Error| {
        Diagnostic::error(codes::WRITE_FAILED, format!("cannot write {what}: {e}"))
    };
    std::fs::create_dir_all(dir).map_err(|e| fail(dir.display().to_string(), e))?;
    let mut written = Vec::new();
    for (name, schema) in all() {
        let path = dir.join(name);
        std::fs::write(&path, to_text(&schema)).map_err(|e| fail(path.display().to_string(), e))?;
        written.push(name.to_string());
    }
    Ok(written)
}
