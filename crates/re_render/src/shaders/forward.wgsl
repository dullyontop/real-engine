// Forward PBR-lite pass: base colour, texture, metallic and roughness, a normal
// map, one shadowed sun, up to eight point and eight spot lights, hemisphere
// ambient, emissive, distance fog and colour correction.
#include "common.wgsl"
#include "fx.wgsl"

@group(0) @binding(0) var<uniform> frame: Frame;
@group(0) @binding(1) var shadow_map: texture_depth_2d;
@group(0) @binding(2) var shadow_sampler: sampler_comparison;

// Must match `MaterialUniform` in src/renderer.rs.
struct Material {
    // Linear RGBA base colour.
    base_color: vec4<f32>,
    // rgb: emissive * emissive_strength.
    emissive: vec4<f32>,
    // x: metallic, y: roughness, z: alpha cutoff (negative: no masking), w: 1 when unlit
    params: vec4<f32>,
    // x: 1 with a normal map, y: 1 when double-sided, z: 1 when alpha-blended
    flags: vec4<f32>,
};

@group(1) @binding(0) var<uniform> material: Material;
@group(1) @binding(1) var base_texture: texture_2d<f32>;
@group(1) @binding(2) var metallic_roughness_texture: texture_2d<f32>;
@group(1) @binding(3) var normal_texture: texture_2d<f32>;
@group(1) @binding(4) var material_sampler: sampler;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) uv: vec2<f32>,
    @location(3) color: vec4<f32>,
    @location(4) flags: vec4<f32>,
};

// The vertex stage, shared with the skinned entry point (forward_skinned.wgsl).
fn forward_vertex(v: VsIn, inst: Instance) -> VsOut {
    let world = instance_model(inst) * vec4<f32>(v.position, 1.0);
    var out: VsOut;
    out.clip = frame.view_proj * world;
    out.world_pos = world.xyz;
    out.normal = instance_normal(inst) * v.normal;
    out.uv = v.uv;
    out.color = v.color;
    out.flags = inst.flags;
    return out;
}

@vertex
fn vs_forward(v: VsIn, inst: Instance) -> VsOut {
    return forward_vertex(v, inst);
}

fn d_ggx(n_dot_h: f32, a: f32) -> f32 {
    let a2 = a * a;
    let f = n_dot_h * n_dot_h * (a2 - 1.0) + 1.0;
    return a2 / (PI * f * f);
}

fn v_smith_ggx_correlated(n_dot_v: f32, n_dot_l: f32, a: f32) -> f32 {
    let a2 = a * a;
    let gv = n_dot_l * sqrt(n_dot_v * n_dot_v * (1.0 - a2) + a2);
    let gl = n_dot_v * sqrt(n_dot_l * n_dot_l * (1.0 - a2) + a2);
    return 0.5 / max(gv + gl, 1e-5);
}

fn f_schlick(f0: vec3<f32>, v_dot_h: f32) -> vec3<f32> {
    return f0 + (vec3<f32>(1.0) - f0) * pow(1.0 - v_dot_h, 5.0);
}

// Light reflected towards the eye from one light. The diffuse term is not divided
// by pi (and the specular term is scaled by pi to match), so an illuminance of 1
// on a white diffuse surface gives 1.
fn direct(n: vec3<f32>, v: vec3<f32>, l: vec3<f32>, radiance: vec3<f32>,
          diffuse_color: vec3<f32>, f0: vec3<f32>, a: f32) -> vec3<f32> {
    let n_dot_l = dot(n, l);
    if (n_dot_l <= 0.0) {
        return vec3<f32>(0.0);
    }
    let h = normalize(l + v);
    let n_dot_v = max(dot(n, v), 1e-4);
    let n_dot_h = max(dot(n, h), 0.0);
    let v_dot_h = max(dot(v, h), 0.0);
    let spec = d_ggx(n_dot_h, a) * v_smith_ggx_correlated(n_dot_v, n_dot_l, a) * f_schlick(f0, v_dot_h);
    return (diffuse_color + PI * spec) * radiance * n_dot_l;
}

// Split-sum environment BRDF approximation for the ambient specular term.
fn env_brdf(f0: vec3<f32>, roughness: f32, n_dot_v: f32) -> vec3<f32> {
    let c0 = vec4<f32>(-1.0, -0.0275, -0.572, 0.022);
    let c1 = vec4<f32>(1.0, 0.0425, 1.04, -0.04);
    let r = roughness * c0 + c1;
    let a004 = min(r.x * r.x, exp2(-9.28 * n_dot_v)) * r.x + r.y;
    let ab = vec2<f32>(-1.04, 1.04) * a004 + r.zw;
    return f0 * ab.x + ab.y;
}

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

fn shade(in: VsOut, front_facing: bool) -> vec4<f32> {
    // Samples and derivatives first, while control flow is uniform.
    let base_sample = textureSample(base_texture, material_sampler, in.uv);
    let mr_sample = textureSample(metallic_roughness_texture, material_sampler, in.uv);
    let normal_sample = textureSample(normal_texture, material_sampler, in.uv).xyz;
    let dp_dx = dpdx(in.world_pos);
    let dp_dy = dpdy(in.world_pos);
    let duv_dx = dpdx(in.uv);
    let duv_dy = dpdy(in.uv);

    let base = material.base_color * in.color * base_sample;
    if (material.params.z >= 0.0 && base.a < material.params.z) {
        discard;
    }
    let alpha = select(1.0, base.a, material.flags.z > 0.5);
    if (material.params.w > 0.5) {
        let unlit = mix(base.rgb, frame.fog_color.rgb, fog_amount(distance(frame.camera_pos.xyz, in.world_pos)));
        return vec4<f32>(grade(unlit), alpha);
    }

    var n = normalize(in.normal);
    if (!front_facing && material.flags.y > 0.5) {
        n = -n;
    }
    // Normal map through a cotangent frame built from screen-space derivatives
    // (the vertex format has no tangents). glTF normal maps have +Y towards the
    // top of the image, which is towards -v.
    let dp2perp = cross(dp_dy, n);
    let dp1perp = cross(n, dp_dx);
    let t = dp2perp * duv_dx.x + dp1perp * duv_dy.x;
    let b = -(dp2perp * duv_dx.y + dp1perp * duv_dy.y);
    let scale = max(dot(t, t), dot(b, b));
    if (material.flags.x > 0.5 && scale > 1e-20) {
        let s = inverseSqrt(scale);
        let tangent_n = normal_sample * 2.0 - vec3<f32>(1.0);
        n = normalize(mat3x3<f32>(t * s, b * s, n) * tangent_n);
    }

    let metallic = clamp(material.params.x * mr_sample.b, 0.0, 1.0);
    let roughness = clamp(material.params.y * mr_sample.g, 0.045, 1.0);
    let a = roughness * roughness;
    let diffuse_color = base.rgb * (1.0 - metallic);
    let f0 = mix(vec3<f32>(0.04), base.rgb, metallic);
    let v = normalize(frame.camera_pos.xyz - in.world_pos);
    let n_dot_v = max(dot(n, v), 1e-4);

    var color = vec3<f32>(0.0);
    if (frame.sun_dir.w > 0.5) {
        let visible = sun_visibility(in.world_pos, normalize(in.normal), in.flags.x);
        color += direct(n, v, frame.sun_dir.xyz, frame.sun_color.rgb * visible, diffuse_color, f0, a);
    }
    let count = u32(frame.params.z);
    for (var i = 0u; i < count; i++) {
        let light = frame.points[i];
        let to_light = light.position_range.xyz - in.world_pos;
        let d2 = max(dot(to_light, to_light), 1e-4);
        let range = light.position_range.w;
        let ratio = d2 / (range * range);
        let window = clamp(1.0 - ratio * ratio, 0.0, 1.0);
        let attenuation = window * window / d2;
        color += direct(n, v, to_light * inverseSqrt(d2), light.color.rgb * attenuation, diffuse_color, f0, a);
    }
    let spot_count = u32(frame.fog_params.z);
    for (var i = 0u; i < spot_count; i++) {
        let light = frame.spots[i];
        let to_light = light.position_range.xyz - in.world_pos;
        let d2 = max(dot(to_light, to_light), 1e-4);
        let range = light.position_range.w;
        let ratio = d2 / (range * range);
        let window = clamp(1.0 - ratio * ratio, 0.0, 1.0);
        let l = to_light * inverseSqrt(d2);
        let cone = smoothstep(light.direction.w, light.color.w, dot(-l, light.direction.xyz));
        let attenuation = window * window / d2 * cone;
        color += direct(n, v, l, light.color.rgb * attenuation, diffuse_color, f0, a);
    }
    let r = reflect(-v, n);
    let ambient_spec = mix(hemisphere(r), hemisphere(n), roughness) * env_brdf(f0, roughness, n_dot_v);
    color += hemisphere(n) * diffuse_color + ambient_spec;
    color += material.emissive.rgb * frame.params.w;
    if (in.flags.y > 0.5) {
        color = mix(color, vec3<f32>(1.0, 0.55, 0.1), 0.35);
    }
    color = mix(color, frame.fog_color.rgb, fog_amount(distance(frame.camera_pos.xyz, in.world_pos)));
    return vec4<f32>(grade(color), alpha);
}

@fragment
fn fs_forward_linear(in: VsOut, @builtin(front_facing) front_facing: bool) -> @location(0) vec4<f32> {
    return shade(in, front_facing);
}

@fragment
fn fs_forward_gamma(in: VsOut, @builtin(front_facing) front_facing: bool) -> @location(0) vec4<f32> {
    let c = shade(in, front_facing);
    return vec4<f32>(srgb_from_linear(c.rgb), c.a);
}
