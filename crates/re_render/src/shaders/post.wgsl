// The post chain (ADR-0038): sun rays, bloom, blur, then colour grading, tone
// mapping and output encoding into the caller's target. Every pass draws one
// full-screen triangle and reads `source` (and for some passes `extra`).
#include "common.wgsl"
#include "tonemap.wgsl"

// Must match `PostUniform` in src/post.rs.
struct Post {
    // xy: 1 / size of `source`, zw: 1 / size of the pass's target
    texel: vec4<f32>,
    // Per pass. Sun rays: xy sun position in UV, z intensity, w spread.
    // Bloom prefilter: x threshold, y knee. Blur: xy direction in source texels,
    // z sigma in texels, w taps on each side. Final: x bloom intensity (0: none),
    // y 1 when `extra` holds the blurred image, z output alpha with no coverage,
    // w tone mapping operator.
    params: vec4<f32>,
    // x: brightness, y: contrast, z: saturation, w: 1 with colour grading
    grading: vec4<f32>,
    // rgb: tint; w: bloom intensity for the first blur pass
    tint: vec4<f32>,
    // rgb: the clear colour, put behind `source` where its coverage is below 1
    // (zero once composited, and with a sky). w: 1 in a final pass that changes
    // nothing where the scene drew nothing; it leaves those pixels to the
    // target's clear colour, which is exact.
    background: vec4<f32>,
};

@group(0) @binding(0) var<uniform> post: Post;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var extra: texture_2d<f32>;
@group(0) @binding(3) var linear_clamp: sampler;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) index: u32) -> VsOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VsOut;
    out.clip = vec4<f32>(uv * vec2<f32>(2.0, -2.0) + vec2<f32>(-1.0, 1.0), 0.0, 1.0);
    out.uv = uv;
    return out;
}

// The scene is drawn over transparent black, so the clear colour is exact
// (Rgba16Float would round it); it goes behind the image here.
fn behind(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb + post.background.rgb * (1.0 - clamp(c.a, 0.0, 1.0)), c.a);
}

fn sample_source(uv: vec2<f32>) -> vec4<f32> {
    return behind(textureSampleLevel(source, linear_clamp, uv, 0.0));
}

fn load_source(position: vec4<f32>) -> vec4<f32> {
    return behind(textureLoad(source, vec2<i32>(floor(position.xy)), 0));
}

// Sun rays: each pixel marches to the sun's position on screen and gathers
// the sky (where coverage in alpha is below 1) within a disc around the sun,
// so unblocked lines carry the sun's light out and blocked ones leave shafts.
// The disc's radius grows with spread; the light falls off about as one over
// the distance from the sun.
@fragment
fn fs_sun_rays(in: VsOut) -> @location(0) vec4<f32> {
    let base = load_source(in.clip);
    let sun = post.params.xy;
    let to_sun = sun - in.uv;
    // UV distances in units of the height.
    let aspect = vec2<f32>(post.texel.y / post.texel.x, 1.0);
    let radius = 0.04 + 0.2 * clamp(post.params.w, 0.0, 1.0);
    let samples = 48;
    var rays = vec3<f32>(0.0);
    for (var i = 0; i < samples; i++) {
        let p = in.uv + to_sun * ((f32(i) + 0.5) / f32(samples));
        let s = sample_source(p);
        let near = 1.0 - smoothstep(0.0, radius, length((p - sun) * aspect));
        rays += s.rgb * (1.0 - clamp(s.a, 0.0, 1.0)) * near;
    }
    return vec4<f32>(base.rgb + rays / f32(samples) * post.params.z, base.a);
}

// The 13-tap downsample of Jimenez 2014 (Call of Duty: Advanced Warfare).
fn downsample13(uv: vec2<f32>) -> vec3<f32> {
    let t = post.texel.xy;
    let a = sample_source(uv + t * vec2<f32>(-2.0, -2.0)).rgb;
    let b = sample_source(uv + t * vec2<f32>(0.0, -2.0)).rgb;
    let c = sample_source(uv + t * vec2<f32>(2.0, -2.0)).rgb;
    let d = sample_source(uv + t * vec2<f32>(-2.0, 0.0)).rgb;
    let e = sample_source(uv).rgb;
    let f = sample_source(uv + t * vec2<f32>(2.0, 0.0)).rgb;
    let g = sample_source(uv + t * vec2<f32>(-2.0, 2.0)).rgb;
    let h = sample_source(uv + t * vec2<f32>(0.0, 2.0)).rgb;
    let i = sample_source(uv + t * vec2<f32>(2.0, 2.0)).rgb;
    let j = sample_source(uv + t * vec2<f32>(-1.0, -1.0)).rgb;
    let k = sample_source(uv + t * vec2<f32>(1.0, -1.0)).rgb;
    let l = sample_source(uv + t * vec2<f32>(-1.0, 1.0)).rgb;
    let m = sample_source(uv + t * vec2<f32>(1.0, 1.0)).rgb;
    return e * 0.125 + (a + c + g + i) * 0.03125 + (b + d + f + h) * 0.0625 + (j + k + l + m) * 0.125;
}

// The first bloom level: the soft-knee threshold applied after downsampling.
@fragment
fn fs_bloom_prefilter(in: VsOut) -> @location(0) vec4<f32> {
    // Infinite or NaN scene values would spread over the whole chain.
    let c = clamp(downsample13(in.uv), vec3<f32>(0.0), vec3<f32>(65000.0));
    let threshold = post.params.x;
    let knee = threshold * post.params.y + 1e-5;
    let brightness = max(c.r, max(c.g, c.b));
    var soft = clamp(brightness - threshold + knee, 0.0, 2.0 * knee);
    soft = soft * soft / (4.0 * knee);
    let contribution = max(soft, brightness - threshold) / max(brightness, 1e-5);
    return vec4<f32>(c * contribution, 1.0);
}

@fragment
fn fs_bloom_down(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(downsample13(in.uv), 1.0);
}

// A 3x3 tent upsample, added onto the next larger level by the blend state.
@fragment
fn fs_bloom_up(in: VsOut) -> @location(0) vec4<f32> {
    let t = post.texel.xy;
    var c = sample_source(in.uv).rgb * 4.0;
    c += (sample_source(in.uv + vec2<f32>(-t.x, 0.0)).rgb + sample_source(in.uv + vec2<f32>(t.x, 0.0)).rgb
        + sample_source(in.uv + vec2<f32>(0.0, -t.y)).rgb + sample_source(in.uv + vec2<f32>(0.0, t.y)).rgb) * 2.0;
    c += sample_source(in.uv + vec2<f32>(-t.x, -t.y)).rgb + sample_source(in.uv + vec2<f32>(t.x, -t.y)).rgb
        + sample_source(in.uv + vec2<f32>(-t.x, t.y)).rgb + sample_source(in.uv + vec2<f32>(t.x, t.y)).rgb;
    return vec4<f32>(c / 16.0, 1.0);
}

// Separable Gaussian blur. The first (horizontal) pass reads the full-size
// image into a half-size target, adding bloom from `extra` in fs_blur_bloom.
fn gaussian(in: VsOut) -> vec4<f32> {
    let dir = post.params.xy * post.texel.xy;
    let sigma = max(post.params.z, 1e-3);
    let taps = i32(post.params.w);
    var sum = vec4<f32>(0.0);
    var total = 0.0;
    for (var i = -taps; i <= taps; i++) {
        let x = f32(i);
        let w = exp(-0.5 * x * x / (sigma * sigma));
        sum += sample_source(in.uv + dir * x) * w;
        total += w;
    }
    return sum / total;
}

@fragment
fn fs_blur(in: VsOut) -> @location(0) vec4<f32> {
    return gaussian(in);
}

@fragment
fn fs_blur_bloom(in: VsOut) -> @location(0) vec4<f32> {
    let c = gaussian(in);
    let bloom = textureSampleLevel(extra, linear_clamp, in.uv, 0.0).rgb;
    return vec4<f32>(c.rgb + bloom * post.tint.w, c.a);
}

// Colour correction in scene-linear values.
fn grade(c: vec3<f32>) -> vec3<f32> {
    if (post.grading.w < 0.5) {
        return c;
    }
    var x = c + vec3<f32>(post.grading.x);
    x = (x - vec3<f32>(0.5)) * (1.0 + post.grading.y) + vec3<f32>(0.5);
    let luma = dot(x, vec3<f32>(0.2126, 0.7152, 0.0722));
    x = mix(vec3<f32>(luma), x, 1.0 + post.grading.z);
    return max(x * post.tint.rgb, vec3<f32>(0.0));
}

fn finish(in: VsOut) -> vec4<f32> {
    let raw = textureLoad(source, vec2<i32>(floor(in.clip.xy)), 0);
    if (post.background.w > 0.5 && all(raw == vec4<f32>(0.0))) {
        discard;
    }
    let here = behind(raw);
    var rgb = here.rgb;
    let other = textureSampleLevel(extra, linear_clamp, in.uv, 0.0).rgb;
    if (post.params.y > 0.5) {
        rgb = other;
    } else if (post.params.x > 0.0) {
        rgb += other * post.params.x;
    }
    rgb = tonemap(grade(rgb), u32(post.params.w));
    // Coverage over the clear alpha, as blending onto the cleared target gave.
    let coverage = clamp(here.a, 0.0, 1.0);
    return vec4<f32>(rgb, post.params.z + (1.0 - post.params.z) * coverage);
}

@fragment
fn fs_final_linear(in: VsOut) -> @location(0) vec4<f32> {
    return finish(in);
}

@fragment
fn fs_final_gamma(in: VsOut) -> @location(0) vec4<f32> {
    let c = finish(in);
    return vec4<f32>(srgb_from_linear(c.rgb), c.a);
}
