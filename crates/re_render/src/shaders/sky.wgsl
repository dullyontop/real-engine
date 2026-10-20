// The sky gradient behind the scene, drawn first over the whole target. Its
// colours are unlit: exposure does not change them.
#include "common.wgsl"

@group(0) @binding(0) var<uniform> frame: Frame;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) ndc: vec2<f32>,
};

@vertex
fn vs_sky(@builtin(vertex_index) index: u32) -> VsOut {
    // One triangle that covers the screen.
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    let ndc = uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0);
    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.ndc = ndc;
    return out;
}

fn sky(in: VsOut) -> vec3<f32> {
    let far = frame.inv_view_proj * vec4<f32>(in.ndc, 1.0, 1.0);
    let dir = normalize(far.xyz / far.w - frame.camera_pos.xyz);
    var c: vec3<f32>;
    if (dir.y >= 0.0) {
        c = mix(frame.sky_horizon.rgb, frame.sky_zenith.rgb, sqrt(dir.y));
    } else {
        c = mix(frame.sky_horizon.rgb, frame.sky_ground.rgb, sqrt(-dir.y));
    }
    if (frame.sky_sun_dir.w > 0.5) {
        let cos_angle = dot(dir, normalize(frame.sky_sun_dir.xyz));
        let disc = smoothstep(0.9985, 0.9992, cos_angle);
        let glow = pow(max(cos_angle, 0.0), 64.0) * 0.25;
        c += frame.sky_sun_color.rgb * (disc + glow);
    }
    return c;
}

@fragment
fn fs_sky(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(sky(in), 0.0);
}
