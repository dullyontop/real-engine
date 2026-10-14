//! With the `schemars` feature, the file-format types derive JSON Schemas that
//! follow their serde shape (`cargo test -p re_types --features schemars`).

#![cfg(feature = "schemars")]

use re_types::{GraphDoc, ReplayDoc};
use serde_json::Value;

fn schema<T: schemars::JsonSchema>() -> Value {
    serde_json::to_value(schemars::schema_for!(T)).unwrap()
}

#[test]
fn replay_schema_follows_serde() {
    let s = schema::<ReplayDoc>();
    let props = s["properties"].as_object().unwrap();
    // The `$schema` key is renamed, as in the file.
    assert!(props.contains_key("$schema"), "{s:#}");
    assert!(!props.contains_key("schema"), "{s:#}");
    // Unknown keys are errors, and defaulted fields are optional.
    assert_eq!(s["additionalProperties"], Value::Bool(false), "{s:#}");
    let required: Vec<&str> = s["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(required, ["version", "seed", "frames"], "{s:#}");
}

#[test]
fn graph_schema_follows_serde() {
    let s = schema::<GraphDoc>();
    assert_eq!(s["additionalProperties"], Value::Bool(false), "{s:#}");
    for key in ["format", "version", "name", "seed", "nodes", "edges"] {
        assert!(s["properties"].get(key).is_some(), "{key}: {s:#}");
    }
    // Edge ends are two-element arrays.
    let edge = &s["$defs"]["EdgeDoc"]["properties"]["from"];
    assert_eq!(edge["type"], "array", "{s:#}");
    assert_eq!(edge["minItems"], 2, "{s:#}");
    assert_eq!(edge["maxItems"], 2, "{s:#}");
}
