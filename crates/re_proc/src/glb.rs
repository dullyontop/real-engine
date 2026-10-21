//! A minimal glTF binary writer for baked meshes (ADR-0043 §2): one mesh,
//! one triangle primitive with POSITION, NORMAL, TEXCOORD_0, COLOR_0 (all
//! f32) and u32 indices, drawn by one node of the default scene. The output
//! loads through the engine's glTF importer, which gives the same vertices
//! bit for bit, and opens in other tools.

use re_types::mesh::MeshData;
use serde_json::json;

const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;
const FLOAT: u32 = 5126;
const UNSIGNED_INT: u32 = 5125;

/// Appends `bytes` to `bin` aligned to four bytes and returns its
/// `(offset, length)`.
fn push(bin: &mut Vec<u8>, bytes: &[u8]) -> (usize, usize) {
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let offset = bin.len();
    bin.extend_from_slice(bytes);
    (offset, bytes.len())
}

fn floats<const N: usize>(items: impl Iterator<Item = [f32; N]>) -> Vec<u8> {
    items
        .flat_map(|a| a.into_iter().flat_map(f32::to_le_bytes))
        .collect()
}

/// `mesh` as a `.glb` file. Deterministic: the same mesh gives the same
/// bytes.
///
/// # Errors
///
/// A mesh with no vertices or triangles, or one that is not well formed (an
/// index past the vertices, or a partial triangle).
pub fn write_glb(mesh: &MeshData, name: &str) -> Result<Vec<u8>, String> {
    if mesh.vertices.is_empty() || mesh.indices.is_empty() {
        return Err("the mesh is empty".into());
    }
    if !mesh.is_well_formed() {
        return Err("the mesh is not a well-formed triangle list".into());
    }
    let n = mesh.vertices.len();
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for v in &mesh.vertices {
        for i in 0..3 {
            min[i] = min[i].min(v.pos[i]);
            max[i] = max[i].max(v.pos[i]);
        }
    }
    let mut bin = Vec::new();
    let views = [
        (
            push(&mut bin, &floats(mesh.vertices.iter().map(|v| v.pos))),
            ARRAY_BUFFER,
        ),
        (
            push(&mut bin, &floats(mesh.vertices.iter().map(|v| v.normal))),
            ARRAY_BUFFER,
        ),
        (
            push(&mut bin, &floats(mesh.vertices.iter().map(|v| v.uv))),
            ARRAY_BUFFER,
        ),
        (
            push(&mut bin, &floats(mesh.vertices.iter().map(|v| v.color))),
            ARRAY_BUFFER,
        ),
        (
            push(
                &mut bin,
                &mesh
                    .indices
                    .iter()
                    .flat_map(|i| i.to_le_bytes())
                    .collect::<Vec<_>>(),
            ),
            ELEMENT_ARRAY_BUFFER,
        ),
    ];
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let buffer_views: Vec<_> = views
        .iter()
        .map(|((offset, len), target)| {
            json!({ "buffer": 0, "byteOffset": offset, "byteLength": len, "target": target })
        })
        .collect();
    let accessor = |view: usize, kind: &str, component: u32, count: usize| json!({ "bufferView": view, "componentType": component, "count": count, "type": kind });
    let mut position = accessor(0, "VEC3", FLOAT, n);
    position["min"] = json!(min);
    position["max"] = json!(max);
    let doc = json!({
        "asset": { "version": "2.0", "generator": "real-engine bake" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0, "name": name }],
        "meshes": [{
            "name": name,
            "primitives": [{
                "attributes": { "POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2, "COLOR_0": 3 },
                "indices": 4,
                "mode": 4
            }]
        }],
        "accessors": [
            position,
            accessor(1, "VEC3", FLOAT, n),
            accessor(2, "VEC2", FLOAT, n),
            accessor(3, "VEC4", FLOAT, n),
            accessor(4, "SCALAR", UNSIGNED_INT, mesh.indices.len()),
        ],
        "bufferViews": buffer_views,
        "buffers": [{ "byteLength": bin.len() }],
    });
    let mut text = serde_json::to_vec(&doc).map_err(|e| e.to_string())?;
    while !text.len().is_multiple_of(4) {
        text.push(b' ');
    }
    let total = 12 + 8 + text.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(text.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&text);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    Ok(out)
}
