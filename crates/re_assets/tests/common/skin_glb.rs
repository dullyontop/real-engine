//! A small GLB writer for skinned test models: accessors are appended to one
//! binary buffer and the caller writes the rest of the JSON.
#![allow(dead_code)] // each test binary uses a different subset

/// glTF component types.
pub const FLOAT: u32 = 5126;
pub const UNSIGNED_BYTE: u32 = 5121;
pub const UNSIGNED_SHORT: u32 = 5123;

#[derive(Default)]
pub struct Glb {
    bin: Vec<u8>,
    views: Vec<String>,
    accessors: Vec<String>,
}

impl Glb {
    /// Appends `bytes` as a buffer view with one accessor and returns the
    /// accessor's index.
    pub fn accessor(
        &mut self,
        bytes: &[u8],
        component: u32,
        count: usize,
        ty: &str,
        normalized: bool,
        extra: &str,
    ) -> usize {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let offset = self.bin.len();
        self.bin.extend_from_slice(bytes);
        self.views.push(format!(
            r#"{{"buffer":0,"byteOffset":{offset},"byteLength":{}}}"#,
            bytes.len()
        ));
        let view = self.views.len() - 1;
        self.accessors.push(format!(
            r#"{{"bufferView":{view},"componentType":{component},"count":{count},"type":"{ty}","normalized":{normalized}{extra}}}"#
        ));
        self.accessors.len() - 1
    }

    /// Float data: `per` floats per element of type `ty`.
    pub fn floats(&mut self, data: &[f32], per: usize, ty: &str) -> usize {
        let bytes: Vec<u8> = data.iter().flat_map(|x| x.to_le_bytes()).collect();
        self.accessor(&bytes, FLOAT, data.len() / per, ty, false, "")
    }

    /// Positions, with the `min` and `max` glTF requires.
    pub fn positions(&mut self, p: &[[f32; 3]]) -> usize {
        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for v in p {
            for k in 0..3 {
                min[k] = min[k].min(v[k]);
                max[k] = max[k].max(v[k]);
            }
        }
        let bytes: Vec<u8> = p
            .as_flattened()
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        let extra = format!(r#","min":{min:?},"max":{max:?}"#);
        self.accessor(&bytes, FLOAT, p.len(), "VEC3", false, &extra)
    }

    /// Keyframe times, with `min` and `max`.
    pub fn times(&mut self, t: &[f32]) -> usize {
        let bytes: Vec<u8> = t.iter().flat_map(|x| x.to_le_bytes()).collect();
        let extra = format!(
            r#","min":[{}],"max":[{}]"#,
            t.iter().copied().fold(f32::MAX, f32::min),
            t.iter().copied().fold(f32::MIN, f32::max)
        );
        self.accessor(&bytes, FLOAT, t.len(), "SCALAR", false, &extra)
    }

    /// Four u16 joint indices per vertex.
    pub fn joints_u16(&mut self, j: &[[u16; 4]]) -> usize {
        let bytes: Vec<u8> = j
            .as_flattened()
            .iter()
            .flat_map(|x| x.to_le_bytes())
            .collect();
        self.accessor(&bytes, UNSIGNED_SHORT, j.len(), "VEC4", false, "")
    }

    /// Four u8 joint indices per vertex.
    pub fn joints_u8(&mut self, j: &[[u8; 4]]) -> usize {
        self.accessor(j.as_flattened(), UNSIGNED_BYTE, j.len(), "VEC4", false, "")
    }

    /// Four normalized u8 weights per vertex.
    pub fn weights_u8(&mut self, w: &[[u8; 4]]) -> usize {
        self.accessor(w.as_flattened(), UNSIGNED_BYTE, w.len(), "VEC4", true, "")
    }

    /// Four float weights per vertex.
    pub fn weights_f32(&mut self, w: &[[f32; 4]]) -> usize {
        self.floats(w.as_flattened(), 4, "VEC4")
    }

    /// Column-major 4x4 matrices.
    pub fn mat4s(&mut self, m: &[glam::Mat4]) -> usize {
        let flat: Vec<f32> = m.iter().flat_map(|m| m.to_cols_array()).collect();
        self.floats(&flat, 16, "MAT4")
    }

    /// The GLB: `body` is the JSON object's members other than `asset`,
    /// `accessors`, `bufferViews` and `buffers`, without braces.
    pub fn finish(mut self, body: &str) -> Vec<u8> {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        let json = format!(
            r#"{{"asset":{{"version":"2.0"}},{body},"accessors":[{}],"bufferViews":[{}],"buffers":[{{"byteLength":{}}}]}}"#,
            self.accessors.join(","),
            self.views.join(","),
            self.bin.len()
        );
        super::glb(&json, &self.bin)
    }
}
