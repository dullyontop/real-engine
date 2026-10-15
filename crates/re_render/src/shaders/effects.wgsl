// Sprites and ribbons: unlit camera-facing quads, sorted back to front on the
// CPU, depth tested but not depth written. The output is premultiplied: alpha
// is `opacity * (1 - emission)`, so emission 1 adds to what is behind.
#include "common.wgsl"
#include "fx.wgsl"

@group(0) @binding(0) var<uniform> frame: Frame;

// Must match `EffectVertex` in src/extras.rs.
struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
    // x: emission, y: 1 for a soft round fade
    @location(3) params: vec2<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) params: vec2<f32>,
    @location(3) world_pos: vec3<f32>,
};

@vertex
fn vs_effect(v: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = frame.view_proj * vec4<f32>(v.position, 1.0);
    out.uv = v.uv;
    out.color = v.color;
    out.params = v.params;
    out.world_pos = v.position;
    return out;
}

// The straight colour and the opacity of one fragment.
fn effect(in: VsOut) -> vec4<f32> {
    var alpha = clamp(in.color.a, 0.0, 1.0);
    if (in.params.y > 0.5) {
        let d = length(in.uv * 2.0 - vec2<f32>(1.0));
        alpha *= 1.0 - smoothstep(0.0, 1.0, d);
    }
    let fog = fog_amount(distance(frame.camera_pos.xyz, in.world_pos));
    // Fog hides additive light rather than tinting it.
    alpha *= 1.0 - fog * in.params.x;
    let rgb = grade(mix(in.color.rgb, frame.fog_color.rgb, fog));
    return vec4<f32>(rgb, alpha);
}

@fragment
fn fs_effect_linear(in: VsOut) -> @location(0) vec4<f32> {
    let c = effect(in);
    return vec4<f32>(c.rgb * c.a, c.a * (1.0 - in.params.x));
}

@fragment
fn fs_effect_gamma(in: VsOut) -> @location(0) vec4<f32> {
    let c = effect(in);
    return vec4<f32>(srgb_from_linear(c.rgb) * c.a, c.a * (1.0 - in.params.x));
}
