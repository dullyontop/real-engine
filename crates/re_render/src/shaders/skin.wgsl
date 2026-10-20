// Linear blend skinning (ADR-0039). The including file declares the joint
// palette first, as `var<storage, read> palette: array<mat4x4<f32>>` in its
// own bind group; this file reads it by name.
//
// A skinned item's first palette matrix is its instance's flags.w (an exact
// integer in an f32, below 2^24).

// The skin vertex stream (vertex buffer slot 2), 16 bytes per vertex.
struct Skin {
    @location(12) joints: vec4<u32>,
    // unorm16 weights that sum to one.
    @location(13) weights: vec4<f32>,
};

fn skin_matrix(s: Skin, inst: Instance) -> mat4x4<f32> {
    let base = u32(inst.flags.w + 0.5);
    return palette[base + s.joints.x] * s.weights.x
        + palette[base + s.joints.y] * s.weights.y
        + palette[base + s.joints.z] * s.weights.z
        + palette[base + s.joints.w] * s.weights.w;
}

// The upper 3x3 of a skin matrix, for normals. Joints with non-uniform scale
// shade slightly wrong, as usual for linear blend skinning.
fn skin_linear(m: mat4x4<f32>) -> mat3x3<f32> {
    return mat3x3<f32>(m[0].xyz, m[1].xyz, m[2].xyz);
}
