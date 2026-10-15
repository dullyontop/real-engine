// Composites highlights over the finished image from the mask: the fill where
// a highlighted item is, the outline in a band around it.
#include "common.wgsl"

struct Highlights {
    // Fill of highlight i at 2 i, its outline at 2 i + 1 (linear RGB, opacity).
    colors: array<vec4<f32>, 62>,
    // x: outline radius in pixels
    params: vec4<f32>,
};

@group(0) @binding(0) var<uniform> highlights: Highlights;
@group(0) @binding(1) var mask: texture_2d<u32>;

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
}

// The straight colour and opacity at `position` (zero for none).
fn highlight(position: vec4<f32>) -> vec4<f32> {
    let p = vec2<i32>(floor(position.xy));
    let size = vec2<i32>(textureDimensions(mask));
    let id = textureLoad(mask, p, 0).r;
    if (id > 0u) {
        return highlights.colors[2u * (id - 1u)];
    }
    let r = i32(highlights.params.x);
    var best = 0u;
    var best_d = 1000000;
    for (var dy = -r; dy <= r; dy++) {
        for (var dx = -r; dx <= r; dx++) {
            let d = dx * dx + dy * dy;
            if (d > r * r || d >= best_d) {
                continue;
            }
            let q = clamp(p + vec2<i32>(dx, dy), vec2<i32>(0), size - vec2<i32>(1));
            let k = textureLoad(mask, q, 0).r;
            if (k > 0u) {
                best = k;
                best_d = d;
            }
        }
    }
    if (best == 0u) {
        return vec4<f32>(0.0);
    }
    return highlights.colors[2u * (best - 1u) + 1u];
}

@fragment
fn fs_highlight_linear(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    return highlight(position);
}

@fragment
fn fs_highlight_gamma(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let c = highlight(position);
    return vec4<f32>(srgb_from_linear(c.rgb), c.a);
}
