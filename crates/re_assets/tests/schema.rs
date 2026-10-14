//! With the `schemars` feature, `MaterialFile` derives a JSON Schema that follows
//! its serde shape (`cargo test -p re_assets --features schemars`).

#![cfg(feature = "schemars")]

use re_assets::MaterialFile;
use serde_json::Value;

#[test]
fn material_schema_follows_serde() {
    let s = serde_json::to_value(schemars::schema_for!(MaterialFile)).unwrap();
    let props = s["properties"].as_object().unwrap();
    // The `$schema` key is renamed, as in the file.
    assert!(props.contains_key("$schema"), "{s:#}");
    assert!(!props.contains_key("schema"), "{s:#}");
    for key in [
        "base_color",
        "base_color_texture",
        "metallic",
        "roughness",
        "metallic_roughness_texture",
        "normal_texture",
        "emissive",
        "emissive_strength",
        "alpha_mode",
        "alpha_cutoff",
        "double_sided",
        "unlit",
    ] {
        assert!(props.contains_key(key), "{key}: {s:#}");
    }
    // Unknown keys are errors, and every key is optional.
    assert_eq!(s["additionalProperties"], Value::Bool(false), "{s:#}");
    assert!(s.get("required").is_none(), "{s:#}");
    // Alpha modes are the snake-case names.
    let text = s.to_string();
    for mode in ["\"opaque\"", "\"mask\"", "\"blend\""] {
        assert!(text.contains(mode), "{mode}: {s:#}");
    }
}
