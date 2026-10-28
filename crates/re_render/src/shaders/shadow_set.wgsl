// The shadow pass for instance sets (ADR-0052).
#include "shadow.wgsl"

@group(1) @binding(0) var<uniform> set_part: SetPart;

#include "instances.wgsl"

@vertex
fn vs_shadow_set(v: VsIn, p: Placement) -> @builtin(position) vec4<f32> {
    return cascade.light_view_proj * vec4<f32>(set_position(v.position, p), 1.0);
}
