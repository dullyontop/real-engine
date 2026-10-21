// The shadow probe (`Renderer::probe_sun_shadow`): one point per pixel of a
// one-row target, each writing the sun's shadow at its world position as the
// last frame's cascades give it.
#include "lighting.wgsl"

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
};

@vertex
fn vs_probe(
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    // The pixel's centre in clip space.
    @location(2) x: f32,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(x, 0.0, 0.0, 1.0);
    out.world_pos = position;
    out.normal = normal;
    return out;
}

// r: visibility, g: the cascade plus one (0 outside the shadows), b: the blend
// towards the next cascade.
@fragment
fn fs_probe(in: VsOut) -> @location(0) vec4<f32> {
    if (frame.sun_color.w < 0.5) {
        return vec4<f32>(1.0, 0.0, 0.0, 1.0);
    }
    let s = sun_shadow(in.world_pos, normalize(in.normal));
    return vec4<f32>(s.x, s.y + 1.0, s.z, 1.0);
}
