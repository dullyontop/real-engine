// Depth-only pass into one cascade of the sun's shadow map.
#include "common.wgsl"

// Must match `CascadeUniform` in src/renderer.rs.
struct Cascade {
    light_view_proj: mat4x4<f32>,
};

// At a dynamic offset: the cascade being drawn.
@group(0) @binding(0) var<uniform> cascade: Cascade;

struct VsIn {
    @location(0) position: vec3<f32>,
};

// The vertex stage, shared with the skinned entry point (shadow_skinned.wgsl).
fn shadow_vertex(position: vec3<f32>, inst: Instance) -> vec4<f32> {
    let world = instance_model(inst) * vec4<f32>(position, 1.0);
    return cascade.light_view_proj * world;
}

@vertex
fn vs_shadow(v: VsIn, inst: Instance) -> @builtin(position) vec4<f32> {
    return shadow_vertex(v.position, inst);
}
