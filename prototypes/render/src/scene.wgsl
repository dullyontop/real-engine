// Lit scene shader for the render spike: Blinn-Phong + hemisphere ambient,
// procedural checker on the ground plane.

struct Globals {
    view_proj: mat4x4<f32>,
    camera_pos: vec4<f32>,
    // xyz: direction towards the light (normalized), w: unused
    light_dir: vec4<f32>,
    // rgb: light color, w: ambient strength
    light_color: vec4<f32>,
    // xyz: point light position, w: point light intensity
    point_light: vec4<f32>,
};

struct Object {
    model: mat4x4<f32>,
    // inverse-transpose of `model`; only the upper 3x3 is used
    normal_matrix: mat4x4<f32>,
    // rgb: tint, w: 1.0 = procedural checker
    tint: vec4<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var<uniform> object: Object;

struct VsIn {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec3<f32>,
};

@vertex
fn vs_main(v: VsIn) -> VsOut {
    let world = object.model * vec4<f32>(v.position, 1.0);
    var out: VsOut;
    out.clip = globals.view_proj * world;
    out.world_pos = world.xyz;
    out.normal = (object.normal_matrix * vec4<f32>(v.normal, 0.0)).xyz;
    out.color = v.color * object.tint.rgb;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let n = normalize(in.normal);
    let l = normalize(globals.light_dir.xyz);
    let v = normalize(globals.camera_pos.xyz - in.world_pos);
    let h = normalize(l + v);

    var albedo = in.color;
    if (object.tint.w > 0.5) {
        let cell = abs(i32(floor(in.world_pos.x)) + i32(floor(in.world_pos.z))) % 2;
        albedo = albedo * select(0.65, 1.0, cell == 0);
    }

    let diffuse = max(dot(n, l), 0.0);
    let spec = pow(max(dot(n, h), 0.0), 48.0) * 0.35 * f32(diffuse > 0.0);

    // Warm point light with inverse-square falloff: gives per-pixel gradients.
    let to_p = globals.point_light.xyz - in.world_pos;
    let dist2 = dot(to_p, to_p);
    let lp = to_p * inverseSqrt(dist2);
    let atten = globals.point_light.w / (1.0 + dist2);
    let point = vec3<f32>(1.0, 0.55, 0.25) * max(dot(n, lp), 0.0) * atten;

    let hemi = mix(vec3<f32>(0.20, 0.17, 0.15), vec3<f32>(0.40, 0.48, 0.62), n.y * 0.5 + 0.5);
    let ambient = hemi * globals.light_color.w;
    let lit = albedo * (ambient + globals.light_color.rgb * diffuse * 0.8 + point)
        + globals.light_color.rgb * spec;
    return vec4<f32>(lit, 1.0);
}
