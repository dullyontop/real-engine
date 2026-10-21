// The highlight mask for skinned meshes.
#include "mask.wgsl"

@group(1) @binding(0) var<storage, read> palette: array<mat4x4<f32>>;

#include "skin.wgsl"

@vertex
fn vs_mask_skinned(v: VsIn, inst: Instance, s: Skin) -> VsOut {
    let m = skin_matrix(s, inst);
    return mask_vertex((m * vec4<f32>(v.position, 1.0)).xyz, inst);
}
