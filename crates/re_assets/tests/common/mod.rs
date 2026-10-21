//! Builds small glTF 2.0 test files: a textured triangle with a material, a
//! normal map, a node tree and a translation/rotation animation.

#![allow(dead_code)]

pub mod skin_glb;

pub const POSITIONS: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
pub const NORMALS: [[f32; 3]; 3] = [[0.0, 0.0, 1.0]; 3];
pub const UVS: [[f32; 2]; 3] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
pub const INDICES: [u16; 3] = [0, 1, 2];
pub const TIMES: [f32; 2] = [0.0, 2.0];
pub const TRANSLATIONS: [[f32; 3]; 2] = [[0.0, 0.0, 0.0], [4.0, 0.0, 0.0]];
pub const ROTATIONS: [[f32; 4]; 2] = [[0.0, 0.0, 0.0, 1.0], [0.0, 0.70710677, 0.0, 0.70710677]];

/// A 2x2 RGBA PNG: red, green, blue, half-transparent white.
pub fn png_2x2() -> Vec<u8> {
    png(
        2,
        2,
        &[
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 128,
        ],
    )
}

pub fn png(w: u32, h: u32, rgba: &[u8]) -> Vec<u8> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec()).unwrap();
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

fn f32s(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

/// The binary buffer and its views: positions, normals, uvs, indices, times,
/// translations, rotations, then (if given) the PNG.
pub fn build_bin(png: Option<&[u8]>) -> (Vec<u8>, Vec<(usize, usize)>) {
    let mut bin = Vec::new();
    let mut views = Vec::new();
    let mut push = |bytes: &[u8]| {
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        views.push((bin.len(), bytes.len()));
        bin.extend_from_slice(bytes);
    };
    push(&f32s(POSITIONS.as_flattened()));
    push(&f32s(NORMALS.as_flattened()));
    push(&f32s(UVS.as_flattened()));
    push(
        &INDICES
            .iter()
            .flat_map(|i| i.to_le_bytes())
            .collect::<Vec<_>>(),
    );
    push(&f32s(&TIMES));
    push(&f32s(TRANSLATIONS.as_flattened()));
    push(&f32s(ROTATIONS.as_flattened()));
    if let Some(png) = png {
        push(png);
    }
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    (bin, views)
}

/// The glTF JSON. `buffer_uri` is `None` for a GLB; `image` is the image object.
pub fn json(
    buffer_uri: Option<&str>,
    bin_len: usize,
    views: &[(usize, usize)],
    image: &str,
) -> String {
    let uri = buffer_uri
        .map(|u| format!(r#","uri":"{u}""#))
        .unwrap_or_default();
    let view_json: Vec<String> = views
        .iter()
        .map(|(o, l)| format!(r#"{{"buffer":0,"byteOffset":{o},"byteLength":{l}}}"#))
        .collect();
    format!(
        r#"{{
  "asset": {{"version": "2.0", "generator": "re_assets test"}},
  "extensionsUsed": ["KHR_materials_emissive_strength"],
  "scene": 0,
  "scenes": [{{"nodes": [0]}}],
  "nodes": [{{"name": "Root", "children": [1], "translation": [0, 1, 0]}}, {{"mesh": 0, "name": "Lid"}}],
  "meshes": [{{"name": "tri", "primitives": [{{"attributes": {{"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}}, "indices": 3, "material": 0}}]}}],
  "materials": [{{
    "name": "red_metal",
    "pbrMetallicRoughness": {{"baseColorFactor": [1.0, 0.5, 0.25, 1.0], "metallicFactor": 0.75, "roughnessFactor": 0.2,
                              "baseColorTexture": {{"index": 0}}}},
    "emissiveFactor": [0.1, 0.2, 0.3],
    "alphaMode": "MASK", "alphaCutoff": 0.3, "doubleSided": true,
    "extensions": {{"KHR_materials_emissive_strength": {{"emissiveStrength": 4.0}}}}
  }}],
  "textures": [{{"source": 0}}],
  "images": [{image}],
  "animations": [{{
    "name": "open",
    "samplers": [{{"input": 4, "output": 5, "interpolation": "LINEAR"}}, {{"input": 4, "output": 6, "interpolation": "STEP"}}],
    "channels": [{{"sampler": 0, "target": {{"node": 1, "path": "translation"}}}}, {{"sampler": 1, "target": {{"node": 1, "path": "rotation"}}}}]
  }}],
  "accessors": [
    {{"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]}},
    {{"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC3"}},
    {{"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2"}},
    {{"bufferView": 3, "componentType": 5123, "count": 3, "type": "SCALAR"}},
    {{"bufferView": 4, "componentType": 5126, "count": 2, "type": "SCALAR", "min": [0], "max": [2]}},
    {{"bufferView": 5, "componentType": 5126, "count": 2, "type": "VEC3"}},
    {{"bufferView": 6, "componentType": 5126, "count": 2, "type": "VEC4"}}
  ],
  "bufferViews": [{views}],
  "buffers": [{{"byteLength": {bin_len}{uri}}}]
}}"#,
        views = view_json.join(", ")
    )
}

pub fn glb(json: &str, bin: &[u8]) -> Vec<u8> {
    let mut j = json.as_bytes().to_vec();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let total = 12 + 8 + j.len() + 8 + bin.len();
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(j.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&j);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(bin);
    out
}

/// The standard fixture as a GLB with the PNG in a buffer view.
pub fn fixture_glb() -> Vec<u8> {
    let png = png_2x2();
    let (bin, views) = build_bin(Some(&png));
    let doc = json(
        None,
        bin.len(),
        &views,
        r#"{"bufferView": 7, "mimeType": "image/png", "name": "albedo"}"#,
    );
    glb(&doc, &bin)
}

pub fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}
