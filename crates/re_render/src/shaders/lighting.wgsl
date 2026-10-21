// The frame's lights and the sun's cascaded shadow map, shared by the lit scene
// passes.
#include "common.wgsl"

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var shadow_map: texture_depth_2d_array;
@group(0) @binding(2) var shadow_sampler: sampler_comparison;

fn hemisphere(dir: vec3<f32>) -> vec3<f32> {
    return mix(frame.ambient_ground.rgb, frame.ambient_sky.rgb, clamp(dir.y * 0.5 + 0.5, 0.0, 1.0));
}

// The sun's visibility at `world_pos` in cascade `c`: a 3x3 hardware-compare
// filter, with the point moved along the normal by the cascade's offset.
fn cascade_visibility(c: i32, world_pos: vec3<f32>, n: vec3<f32>) -> f32 {
    let p = frame.cascades[c] * vec4<f32>(world_pos + n * frame.cascade_offset[c], 1.0);
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
            lit += textureSampleCompareLevel(shadow_map, shadow_sampler, uv + offset, c, ndc.z);
        }
    }
    return lit / 9.0;
}

// x: the sun's visibility, y: the cascade (-1 outside the shadow distance),
// z: how far into the blend band towards the next cascade (0 to 1).
fn sun_shadow(world_pos: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
    let count = i32(frame.shadow_params.x);
    let depth = dot(world_pos - frame.camera_pos.xyz, frame.camera_forward.xyz);
    var c = 0;
    while (c < count && depth > frame.cascade_far[c]) {
        c++;
    }
    if (c >= count) {
        return vec3<f32>(1.0, -1.0, 0.0);
    }
    let start = select(0.0, frame.cascade_far[max(c - 1, 0)], c > 0);
    let end = frame.cascade_far[c];
    let band = frame.shadow_params.y * (end - start);
    let t = select(0.0, clamp((depth - (end - band)) / band, 0.0, 1.0), band > 0.0);
    var visible = cascade_visibility(c, world_pos, n);
    if (t > 0.0) {
        // The last cascade fades out to unshadowed at the shadow distance.
        let next = select(1.0, cascade_visibility(c + 1, world_pos, n), c + 1 < count);
        visible = mix(visible, next, t);
    }
    return vec3<f32>(visible, f32(c), t);
}

fn sun_visibility(world_pos: vec3<f32>, n: vec3<f32>, receives: f32) -> f32 {
    if (frame.sun_color.w < 0.5 || receives < 0.5) {
        return 1.0;
    }
    return sun_shadow(world_pos, n).x;
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
