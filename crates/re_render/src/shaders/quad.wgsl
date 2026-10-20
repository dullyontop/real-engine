// World quads (`RenderSnapshot::quads`): a texture with premultiplied alpha on a
// rectangle in the world, unlit or lit like a matte surface. Depth-tested quads
// are drawn into the HDR scene; on-top quads over the finished image.
#include "common.wgsl"
#include "lighting.wgsl"
#include "fx.wgsl"

// Must match `QuadUniform` in src/renderer.rs.
struct Quad {
    // Local -0.5..0.5 in X and Y (scaled to the quad's size) to world.
    world: mat4x4<f32>,
    // x: light influence, y: brightness, z: 1 for fog (depth-tested quads)
    params: vec4<f32>,
};

@group(1) @binding(0) var<uniform> quad: Quad;
@group(1) @binding(1) var quad_texture: texture_2d<f32>;
@group(1) @binding(2) var quad_sampler: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) world_pos: vec3<f32>,
    @location(1) uv: vec2<f32>,
};

@vertex
fn vs_quad(@builtin(vertex_index) index: u32) -> VsOut {
    // Two triangles: corners 0 1 2, 2 1 3 of the unit square.
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[index];
    let world = quad.world * vec4<f32>(c.x - 0.5, c.y - 0.5, 0.0, 1.0);
    var out: VsOut;
    out.clip = frame.view_proj * world;
    out.world_pos = world.xyz;
    // The texture's top row is at local +Y.
    out.uv = vec2<f32>(c.x, 1.0 - c.y);
    return out;
}

fn quad_color(in: VsOut) -> vec4<f32> {
    let texel = textureSample(quad_texture, quad_sampler, in.uv);
    var n = normalize((quad.world * vec4<f32>(0.0, 0.0, 1.0, 0.0)).xyz);
    if (dot(n, frame.camera_pos.xyz - in.world_pos) < 0.0) {
        n = -n;
    }
    let influence = clamp(quad.params.x, 0.0, 1.0);
    var light = vec3<f32>(quad.params.y);
    if (influence > 0.0) {
        light = mix(light, diffuse_light(in.world_pos, n, 1.0), influence);
    }
    var rgb = texel.rgb * light;
    if (quad.params.z > 0.5) {
        let fog = fog_amount(distance(frame.camera_pos.xyz, in.world_pos));
        rgb = mix(rgb, frame.fog_color.rgb * texel.a, fog);
    }
    return vec4<f32>(rgb, texel.a);
}

@fragment
fn fs_quad_linear(in: VsOut) -> @location(0) vec4<f32> {
    return quad_color(in);
}

// For gamma-storing targets: encoded straight colour, premultiplied again.
@fragment
fn fs_quad_gamma(in: VsOut) -> @location(0) vec4<f32> {
    let c = quad_color(in);
    if (c.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    let straight = clamp(c.rgb / c.a, vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(srgb_from_linear(straight) * c.a, c.a);
}
