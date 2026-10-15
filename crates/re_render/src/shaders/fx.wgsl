// Fog and colour correction, shared by the passes that bind `frame`.
#include "common.wgsl"

// How much fog covers a point at `distance` from the camera.
fn fog_amount(distance: f32) -> f32 {
    if (frame.fog_params.y < 0.5) {
        return 0.0;
    }
    let start = frame.fog_color.w;
    let end = max(frame.fog_params.x, start + 1e-3);
    return clamp((distance - start) / (end - start), 0.0, 1.0);
}

// Colour correction of a linear colour.
fn grade(c: vec3<f32>) -> vec3<f32> {
    if (frame.grading.w < 0.5) {
        return c;
    }
    var x = c + vec3<f32>(frame.grading.x);
    x = (x - vec3<f32>(0.5)) * (1.0 + frame.grading.y) + vec3<f32>(0.5);
    let luma = dot(x, vec3<f32>(0.2126, 0.7152, 0.0722));
    x = mix(vec3<f32>(luma), x, 1.0 + frame.grading.z);
    return max(x * frame.tint.rgb, vec3<f32>(0.0));
}
