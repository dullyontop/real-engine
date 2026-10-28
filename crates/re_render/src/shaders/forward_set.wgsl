// The forward pass for instance sets (ADR-0052): places each copy of a part
// from its placement, then shades it with the forward fragment entry point
// unchanged.
#include "forward.wgsl"

@group(2) @binding(0) var<uniform> set_part: SetPart;

#include "instances.wgsl"

@vertex
fn vs_forward_set(v: VsIn, p: Placement) -> VsOut {
    let world = set_position(v.position, p);
    var out: VsOut;
    out.clip = frame.view_proj * vec4<f32>(world, 1.0);
    out.world_pos = world;
    out.normal = set_normal(v.normal, p);
    out.uv = v.uv;
    out.color = v.color * vec4<f32>(p.tint.rgb, 1.0);
    out.flags = set_part.flags;
    return out;
}
