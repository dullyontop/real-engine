// Composites the egui overlay onto the target.
//
// egui paints into a gamma-space Rgba8Unorm overlay with premultiplied alpha, as
// it is designed to. This pass reads the overlay texel by texel and blends it over
// the target with premultiplied alpha. For an sRGB target it first converts the
// colour from gamma to linear by hand, because the hardware then encodes it back:
// the result is the same on every backend, with or without view formats (GL).
#include "common.wgsl"

@group(0) @binding(0) var overlay: texture_2d<f32>;

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    // One triangle that covers the screen.
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

fn overlay_texel(position: vec4<f32>) -> vec4<f32> {
    return textureLoad(overlay, vec2<i32>(floor(position.xy)), 0);
}

@fragment
fn fs_blit_linear(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let c = overlay_texel(position);
    if (c.a <= 0.0) {
        return vec4<f32>(0.0);
    }
    let straight = linear_from_srgb(clamp(c.rgb / c.a, vec3<f32>(0.0), vec3<f32>(1.0)));
    return vec4<f32>(straight * c.a, c.a);
}

@fragment
fn fs_blit_gamma(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return overlay_texel(position);
}
