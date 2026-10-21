//! Canonical forms and hashes: canonical JSON (keys sorted at every level),
//! the hash of a graph document, `hash64` of a node id, and the hash of a
//! [`SceneFragment`].

use re_reflect::Value;
use re_scene::SceneFragment;
use re_types::graph::GraphDoc;
use serde_json::Value as Json;

/// `value` with the keys of every object sorted, written compactly. Two
/// JSON values that differ only in key order give the same text.
pub fn canonical_json(value: &Json) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Json, out: &mut String) {
    match value {
        Json::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Json::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
        Json::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// `value` with every object's keys sorted (a value, not text).
pub fn sorted(value: &Json) -> Json {
    match value {
        Json::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Json::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), sorted(&map[k])))
                    .collect(),
            )
        }
        Json::Array(items) => Json::Array(items.iter().map(sorted).collect()),
        other => other.clone(),
    }
}

/// The first eight bytes of `blake3(text)`, little-endian. Used for node ids
/// in `Pcg32::fork(hash64(node_id))` (ARCHITECTURE §5).
pub fn hash64(text: &str) -> u64 {
    let h = blake3::hash(text.as_bytes());
    let mut b = [0u8; 8];
    b.copy_from_slice(&h.as_bytes()[..8]);
    u64::from_le_bytes(b)
}

/// The hash of a graph document's canonical JSON, without its `layout`:
/// moving a node on the canvas regenerates nothing (ADR-0044).
pub fn doc_hash(doc: &GraphDoc) -> blake3::Hash {
    let mut json = serde_json::to_value(doc).unwrap_or(Json::Null);
    if let Some(obj) = json.as_object_mut() {
        obj.shift_remove("layout");
    }
    blake3::hash(canonical_json(&json).as_bytes())
}

/// The version stamp of a graph document (ADR-0043): what decides which
/// code produces its output. It hashes the `graph` generator's name and
/// version and, for each node in document order, its id, kind, recorded
/// version and its kind's implementation hash (all zeros for Rust kinds).
/// Parameters, seeds and layout are not part of it. Streaming (WP23) keys
/// cells by it, and the `graph` generator's fingerprint includes it, so
/// changing a Luau node's scripts regenerates the sources that use it.
pub fn version_stamp(doc: &GraphDoc, registry: &crate::NodeRegistry) -> blake3::Hash {
    stamp(
        doc.nodes
            .iter()
            .map(|n| (n.id.as_str(), n.kind.as_str(), n.version)),
        registry,
    )
}

pub(crate) fn stamp<'a>(
    nodes: impl Iterator<Item = (&'a str, &'a str, u32)>,
    registry: &crate::NodeRegistry,
) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    let s = |h: &mut blake3::Hasher, t: &str| {
        h.update(&(t.len() as u64).to_le_bytes());
        h.update(t.as_bytes());
    };
    h.update(b"re_proc version stamp v1");
    s(&mut h, crate::GENERATOR);
    h.update(&crate::GENERATOR_VERSION.to_le_bytes());
    for (id, kind, version) in nodes {
        s(&mut h, id);
        s(&mut h, kind);
        h.update(&version.to_le_bytes());
        h.update(&registry.get(kind).map_or([0; 32], |d| d.implementation));
    }
    h.finalize()
}

/// A hash of a fragment's exact content: keys, names, parents, and every
/// component and field value bit for bit, in order. Equal fragments give
/// equal hashes on every platform.
pub fn fragment_hash(fragment: &SceneFragment) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    let s = |h: &mut blake3::Hasher, t: &str| {
        h.update(&(t.len() as u64).to_le_bytes());
        h.update(t.as_bytes());
    };
    h.update(&(fragment.len() as u64).to_le_bytes());
    for node in fragment {
        s(&mut h, &node.key);
        s(&mut h, &node.name);
        match &node.parent_key {
            Some(p) => {
                h.update(&[1]);
                s(&mut h, p);
            }
            None => {
                h.update(&[0]);
            }
        }
        h.update(&(node.components.len() as u64).to_le_bytes());
        for (component, fields) in &node.components {
            s(&mut h, component);
            h.update(&(fields.len() as u64).to_le_bytes());
            for (field, value) in fields {
                s(&mut h, field);
                hash_value(&mut h, value);
            }
        }
    }
    h.finalize()
}

fn hash_value(h: &mut blake3::Hasher, v: &Value) {
    let f = |h: &mut blake3::Hasher, x: f32| {
        h.update(&x.to_bits().to_le_bytes());
    };
    match v {
        Value::Bool(b) => {
            h.update(&[0, u8::from(*b)]);
        }
        Value::Int(i) => {
            h.update(&[1]);
            h.update(&i.to_le_bytes());
        }
        Value::Float(x) => {
            h.update(&[2]);
            h.update(&x.to_bits().to_le_bytes());
        }
        Value::Vec3(v) => {
            h.update(&[3]);
            for x in v.to_array() {
                f(h, x);
            }
        }
        Value::Quat(q) => {
            h.update(&[4]);
            for x in q.to_array() {
                f(h, x);
            }
        }
        Value::Color(c) => {
            h.update(&[5]);
            h.update(format!("{c:?}").as_bytes());
        }
        Value::Str(t) => {
            h.update(&[6]);
            h.update(&(t.len() as u64).to_le_bytes());
            h.update(t.as_bytes());
        }
        Value::Entity(e) => {
            h.update(&[7]);
            h.update(&e.map_or(u64::MAX, |e| e.to_bits()).to_le_bytes());
        }
        Value::Enum(t) => {
            h.update(&[8]);
            h.update(&(t.len() as u64).to_le_bytes());
            h.update(t.as_bytes());
        }
        Value::List(items) => {
            h.update(&[9]);
            h.update(&(items.len() as u64).to_le_bytes());
            for item in items {
                hash_value(h, item);
            }
        }
        Value::Map(map) => {
            h.update(&[10]);
            h.update(&(map.len() as u64).to_le_bytes());
            for (k, v) in map {
                h.update(&(k.len() as u64).to_le_bytes());
                h.update(k.as_bytes());
                hash_value(h, v);
            }
        }
        Value::Record(id, values) => {
            h.update(&[11]);
            let name = id.name();
            h.update(&(name.len() as u64).to_le_bytes());
            h.update(name.as_bytes());
            for v in values {
                hash_value(h, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys() {
        let a: Json = serde_json::from_str(r#"{"b": 1, "a": {"y": [1, 2], "x": null}}"#).unwrap();
        let b: Json = serde_json::from_str(r#"{"a": {"x": null, "y": [1, 2]}, "b": 1}"#).unwrap();
        assert_eq!(canonical_json(&a), canonical_json(&b));
        assert_eq!(canonical_json(&a), r#"{"a":{"x":null,"y":[1,2]},"b":1}"#);
        assert_eq!(canonical_json(&sorted(&a)), canonical_json(&a));
    }
}
