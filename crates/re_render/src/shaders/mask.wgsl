// The highlight mask: each highlighted draw item writes its highlight's number
// (instance flags.z) into an R8Uint target. A depth-only pass of the opaque
// scene first hides what is behind other geometry.
#include "common.wgsl"

@group(0) @binding(0) var<uniform> frame: Frame;

struct VsIn {
    @location(0) position: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) @interpolate(flat) id: u32,
};

@vertex
fn vs_mask(v: VsIn, inst: Instance) -> VsOut {
    var out: VsOut;
    out.clip = frame.view_proj * instance_model(inst) * vec4<f32>(v.position, 1.0);
    out.id = u32(inst.flags.z + 0.5);
    return out;
}

@fragment
fn fs_mask(in: VsOut) -> @location(0) u32 {
    return in.id;
}
