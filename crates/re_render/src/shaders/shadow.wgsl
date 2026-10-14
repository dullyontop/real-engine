// Depth-only pass into the sun's shadow map.
#include "common.wgsl"

@group(0) @binding(0) var<uniform> frame: Frame;

struct VsIn {
    @location(0) position: vec3<f32>,
};

@vertex
fn vs_shadow(v: VsIn, inst: Instance) -> @builtin(position) vec4<f32> {
    let world = instance_model(inst) * vec4<f32>(v.position, 1.0);
    return frame.light_view_proj * world;
}
