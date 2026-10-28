// Instance sets (ADR-0052): one part of a set drawn at each of its
// placements. The including file declares the (set, part) entry first, as
// `var<uniform> set_part: SetPart` at a dynamic offset in its own bind group;
// this file reads it by name.

// Must match `SetPartUniform` in src/instances/mod.rs.
struct SetPart {
    // From the set's space to the render origin's.
    set_matrix: mat4x4<f32>,
    // From a placement's root to the part.
    part_matrix: mat4x4<f32>,
    // Their normal matrices (inverse transposes), as padded columns.
    set_normal_0: vec4<f32>,
    set_normal_1: vec4<f32>,
    set_normal_2: vec4<f32>,
    part_normal_0: vec4<f32>,
    part_normal_1: vec4<f32>,
    part_normal_2: vec4<f32>,
    // x: receives shadows, y: selected
    flags: vec4<f32>,
    reserved: vec4<f32>,
};

// One placement (vertex buffer slot 1): `Placement` in re_types, of whose 32
// bytes the vertex stage reads the first 28.
struct Placement {
    // xyz: the copy's root from the set's origin, w: its uniform scale
    @location(4) position_scale: vec4<f32>,
    // Its rotation, a unit quaternion (snorm16).
    @location(5) rotation: vec4<f32>,
    // rgb: its tint (unorm8), multiplied into the base colour
    @location(6) tint: vec4<f32>,
};

// `v` turned by the unit quaternion `q`.
fn quat_rotate(q: vec4<f32>, v: vec3<f32>) -> vec3<f32> {
    let t = 2.0 * cross(q.xyz, v);
    return v + q.w * t + cross(q.xyz, t);
}

// A position in the part's mesh, in the render origin's space:
// set * (root + rotate(q, scale * part * position)).
fn set_position(position: vec3<f32>, p: Placement) -> vec3<f32> {
    let local = (set_part.part_matrix * vec4<f32>(position, 1.0)).xyz * p.position_scale.w;
    let root = p.position_scale.xyz + quat_rotate(normalize(p.rotation), local);
    return (set_part.set_matrix * vec4<f32>(root, 1.0)).xyz;
}

// A normal of the part's mesh, in the render origin's space (not unit length).
fn set_normal(normal: vec3<f32>, p: Placement) -> vec3<f32> {
    let part_n = mat3x3<f32>(set_part.part_normal_0.xyz, set_part.part_normal_1.xyz, set_part.part_normal_2.xyz);
    let set_n = mat3x3<f32>(set_part.set_normal_0.xyz, set_part.set_normal_1.xyz, set_part.set_normal_2.xyz);
    return set_n * quat_rotate(normalize(p.rotation), part_n * normal);
}
