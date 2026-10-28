// The highlight mask for instance sets (ADR-0052): their opaque parts hide
// the highlights behind them in the mask's depth pass, as opaque items do.
#include "mask.wgsl"

@group(1) @binding(0) var<uniform> set_part: SetPart;

#include "instances.wgsl"

@vertex
fn vs_mask_set(v: VsIn, p: Placement) -> VsOut {
    var out: VsOut;
    out.clip = frame.view_proj * vec4<f32>(set_position(v.position, p), 1.0);
    out.id = 0u;
    return out;
}
