// The shadow pass for skinned meshes.
#include "shadow.wgsl"

@group(1) @binding(0) var<storage, read> palette: array<mat4x4<f32>>;

#include "skin.wgsl"

@vertex
fn vs_shadow_skinned(v: VsIn, inst: Instance, s: Skin) -> @builtin(position) vec4<f32> {
    let m = skin_matrix(s, inst);
    return shadow_vertex((m * vec4<f32>(v.position, 1.0)).xyz, inst);
}
