// Distance fog, shared by the passes that bind `frame`.
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
