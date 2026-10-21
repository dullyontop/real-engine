// The frame's lights and the sun's shadow map, shared by the lit scene passes.
#include "common.wgsl"

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_sampler: sampler_comparison;

fn hemisphere(dir: vec3<f32>) -> vec3<f32> {
    return mix(frame.ambient_ground.rgb, frame.ambient_sky.rgb, clamp(dir.y * 0.5 + 0.5, 0.0, 1.0));
}

fn sun_visibility(world_pos: vec3<f32>, n: vec3<f32>, receives: f32) -> f32 {
    if (frame.sun_color.w < 0.5 || receives < 0.5) {
        return 1.0;
    }
    let p = frame.light_view_proj * vec4<f32>(world_pos + n * frame.params.y, 1.0);
    let ndc = p.xyz / p.w;
    let uv = ndc.xy * vec2<f32>(0.5, -0.5) + vec2<f32>(0.5);
    if (any(uv < vec2<f32>(0.0)) || any(uv > vec2<f32>(1.0)) || ndc.z > 1.0 || ndc.z < 0.0) {
        return 1.0;
    }
    let texel = frame.params.x;
    var lit = 0.0;
    for (var y = -1; y <= 1; y++) {
        for (var x = -1; x <= 1; x++) {
            let offset = vec2<f32>(f32(x), f32(y)) * texel;
            lit += textureSampleCompareLevel(shadow_map, shadow_sampler, uv + offset, ndc.z);
        }
    }
    return lit / 9.0;
}

// Light falling on a matte white surface at `pos` facing `n`: the shadowed sun,
// the point and spot lights and the hemisphere ambient, so a white diffuse
// surface shows this colour.
fn diffuse_light(pos: vec3<f32>, n: vec3<f32>, receives: f32) -> vec3<f32> {
    var light = hemisphere(n);
    if (frame.sun_dir.w > 0.5) {
        let visible = sun_visibility(pos, n, receives);
        light += frame.sun_color.rgb * visible * max(dot(n, frame.sun_dir.xyz), 0.0);
    }
    for (var i = 0u; i < u32(frame.params.z); i++) {
        let light_i = frame.points[i];
        let to_light = light_i.position_range.xyz - pos;
        let d2 = max(dot(to_light, to_light), 1e-4);
        let ratio = d2 / (light_i.position_range.w * light_i.position_range.w);
        let window = clamp(1.0 - ratio * ratio, 0.0, 1.0);
        light += light_i.color.rgb * window * window / d2 * max(dot(n, to_light * inverseSqrt(d2)), 0.0);
    }
    for (var i = 0u; i < u32(frame.fog_params.z); i++) {
        let spot = frame.spots[i];
        let to_light = spot.position_range.xyz - pos;
        let d2 = max(dot(to_light, to_light), 1e-4);
        let ratio = d2 / (spot.position_range.w * spot.position_range.w);
        let window = clamp(1.0 - ratio * ratio, 0.0, 1.0);
        let l = to_light * inverseSqrt(d2);
        let cone = smoothstep(spot.direction.w, spot.color.w, dot(-l, spot.direction.xyz));
        light += spot.color.rgb * window * window / d2 * cone * max(dot(n, l), 0.0);
    }
    return light;
}
