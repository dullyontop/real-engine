// The forward pass for skinned meshes: skins the position and normal, then
// runs the forward vertex stage and the forward fragment entry points unchanged.
#include "forward.wgsl"

@group(2) @binding(0) var<storage, read> palette: array<mat4x4<f32>>;

#include "skin.wgsl"

@vertex
fn vs_forward_skinned(v: VsIn, inst: Instance, s: Skin) -> VsOut {
    let m = skin_matrix(s, inst);
    var skinned = v;
    skinned.position = (m * vec4<f32>(v.position, 1.0)).xyz;
    skinned.normal = skin_linear(m) * v.normal;
    return forward_vertex(skinned, inst);
}
