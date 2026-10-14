// Debug lines: world-space segments with a colour, depth-tested, not depth-written.
#include "common.wgsl"

@group(0) @binding(0) var<uniform> frame: Frame;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_line(v: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = frame.view_proj * vec4<f32>(v.position, 1.0);
    out.color = v.color;
    return out;
}

@fragment
fn fs_line_linear(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}

@fragment
fn fs_line_gamma(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(srgb_from_linear(in.color.rgb), in.color.a);
}
