// Tone mapping operators (ADR-0038) for linear scene colour, with results in
// 0..1 linear display colour.

// Khronos PBR Neutral: base colours below about 0.76 come out unchanged
// (less the 4% Fresnel lift); brighter values are compressed towards white.
fn tonemap_pbr_neutral(color_in: vec3<f32>) -> vec3<f32> {
    let start = 0.8 - 0.04;
    let desaturation = 0.15;
    var color = color_in;
    let x = min(color.r, min(color.g, color.b));
    let offset = select(0.04, x - 6.25 * x * x, x < 0.08);
    color -= vec3<f32>(offset);
    let peak = max(color.r, max(color.g, color.b));
    if (peak < start) {
        return color;
    }
    let d = 1.0 - start;
    let new_peak = 1.0 - d * d / (peak + d - start);
    color *= new_peak / peak;
    let g = 1.0 - 1.0 / (desaturation * (peak - new_peak) + 1.0);
    return mix(color, vec3<f32>(new_peak), g);
}

// The ACES RRT and ODT fit by Stephen Hill. The matrices are listed by rows,
// so they multiply from the left of a row vector.
fn tonemap_aces(color: vec3<f32>) -> vec3<f32> {
    let input = mat3x3<f32>(
        vec3<f32>(0.59719, 0.35458, 0.04823),
        vec3<f32>(0.07600, 0.90834, 0.01566),
        vec3<f32>(0.02840, 0.13383, 0.83777),
    );
    let output = mat3x3<f32>(
        vec3<f32>(1.60475, -0.53108, -0.07367),
        vec3<f32>(-0.10208, 1.10813, -0.00605),
        vec3<f32>(-0.00327, -0.07276, 1.07602),
    );
    let v = color * input;
    let a = v * (v + 0.0245786) - 0.000090537;
    let b = v * (0.983729 * v + 0.4329510) + 0.238081;
    return clamp((a / b) * output, vec3<f32>(0.0), vec3<f32>(1.0));
}

// AgX, base look (Troy Sobotka's curve as in Blender, with the polynomial
// sigmoid fit of Benjamin Wrensch).
fn tonemap_agx(color: vec3<f32>) -> vec3<f32> {
    let inset = mat3x3<f32>(
        vec3<f32>(0.842479062253094, 0.0423282422610123, 0.0423756549057051),
        vec3<f32>(0.0784335999999992, 0.878468636469772, 0.0784336),
        vec3<f32>(0.0792237451477643, 0.0791661274605434, 0.879142973793104),
    );
    let outset = mat3x3<f32>(
        vec3<f32>(1.19687900512017, -0.0528968517574562, -0.0529716355144438),
        vec3<f32>(-0.0980208811401368, 1.15190312990417, -0.0980434501171241),
        vec3<f32>(-0.0990297440797205, -0.0989611768448433, 1.15107367264116),
    );
    let min_ev = -12.47393;
    let max_ev = 4.026069;
    var x = inset * max(color, vec3<f32>(1e-10));
    x = clamp(log2(max(x, vec3<f32>(1e-10))), vec3<f32>(min_ev), vec3<f32>(max_ev));
    x = (x - min_ev) / (max_ev - min_ev);
    let x2 = x * x;
    let x4 = x2 * x2;
    x = 15.5 * x4 * x2 - 40.14 * x4 * x + 31.96 * x4 - 6.868 * x2 * x + 0.4298 * x2 + 0.1191 * x - 0.00232;
    x = outset * x;
    return clamp(pow(max(x, vec3<f32>(0.0)), vec3<f32>(2.2)), vec3<f32>(0.0), vec3<f32>(1.0));
}

// Operator 0: none (clamp), 1: PBR Neutral, 2: ACES, 3: AgX.
fn tonemap(color: vec3<f32>, op: u32) -> vec3<f32> {
    switch op {
        case 1u: { return clamp(tonemap_pbr_neutral(color), vec3<f32>(0.0), vec3<f32>(1.0)); }
        case 2u: { return tonemap_aces(color); }
        case 3u: { return tonemap_agx(color); }
        default: { return clamp(color, vec3<f32>(0.0), vec3<f32>(1.0)); }
    }
}
