// Shared declarations of the forward renderer. Must match `FrameUniform` and
// `InstanceRaw` in src/frame.rs.

const PI: f32 = 3.14159265;
const MAX_POINT_LIGHTS: u32 = 8u;
const MAX_SPOT_LIGHTS: u32 = 8u;

struct PointLight {
    // xyz: world position, w: range
    position_range: vec4<f32>,
    // rgb: colour * intensity / (4 pi) * exposure, w: unused
    color: vec4<f32>,
};

struct SpotLight {
    // xyz: world position, w: range
    position_range: vec4<f32>,
    // rgb: colour * intensity / (4 pi) * exposure, w: cosine where the fade ends
    color: vec4<f32>,
    // xyz: the direction it shines, w: cosine of the half angle
    direction: vec4<f32>,
};

struct Frame {
    view_proj: mat4x4<f32>,
    // Projection from world space into the shadow map (uv in xy, depth in z).
    light_view_proj: mat4x4<f32>,
    // Clip space back to world space (the sky pass).
    inv_view_proj: mat4x4<f32>,
    // xyz: camera position
    camera_pos: vec4<f32>,
    // xyz: direction towards the sun, w: 1 when there is a sun
    sun_dir: vec4<f32>,
    // rgb: sun colour * intensity * exposure, w: 1 when the sun casts shadows
    sun_color: vec4<f32>,
    // rgb: sky ambient * exposure
    ambient_sky: vec4<f32>,
    // rgb: ground ambient * exposure
    ambient_ground: vec4<f32>,
    // x: shadow map texel size in uv, y: normal offset in world units,
    // z: point light count, w: exposure (applied to emissive here)
    params: vec4<f32>,
    // rgb: fog colour (linear, unlit), w: fog start
    fog_color: vec4<f32>,
    // x: fog end, y: 1 with fog, z: spot light count
    fog_params: vec4<f32>,
    // x: brightness, y: contrast, z: saturation, w: 1 with colour correction
    grading: vec4<f32>,
    // rgb: tint
    tint: vec4<f32>,
    // rgb: sky colours, w of zenith: 1 with a sky
    sky_zenith: vec4<f32>,
    sky_horizon: vec4<f32>,
    sky_ground: vec4<f32>,
    // xyz: towards the sun disc, w: 1 with a disc
    sky_sun_dir: vec4<f32>,
    sky_sun_color: vec4<f32>,
    points: array<PointLight, 8>,
    spots: array<SpotLight, 8>,
};


// One drawn instance (vertex buffer slot 1).
struct Instance {
    @location(4) model_0: vec4<f32>,
    @location(5) model_1: vec4<f32>,
    @location(6) model_2: vec4<f32>,
    @location(7) model_3: vec4<f32>,
    @location(8) normal_0: vec4<f32>,
    @location(9) normal_1: vec4<f32>,
    @location(10) normal_2: vec4<f32>,
    // x: receives shadows, y: selected
    @location(11) flags: vec4<f32>,
};

fn instance_model(i: Instance) -> mat4x4<f32> {
    return mat4x4<f32>(i.model_0, i.model_1, i.model_2, i.model_3);
}

fn instance_normal(i: Instance) -> mat3x3<f32> {
    return mat3x3<f32>(i.normal_0.xyz, i.normal_1.xyz, i.normal_2.xyz);
}

fn srgb_from_linear(c: vec3<f32>) -> vec3<f32> {
    let x = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lower = x * 12.92;
    let higher = 1.055 * pow(x, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(higher, lower, x <= vec3<f32>(0.0031308));
}

fn linear_from_srgb(c: vec3<f32>) -> vec3<f32> {
    let lower = c / 12.92;
    let higher = pow((c + 0.055) / 1.055, vec3<f32>(2.4));
    return select(higher, lower, c <= vec3<f32>(0.04045));
}
