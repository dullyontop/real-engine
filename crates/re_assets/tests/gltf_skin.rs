//! glTF skin import (ADR-0039): joints, inverse bind matrices, weights and the
//! RE7060–RE7063 diagnostics.

mod common;

use common::skin_glb::Glb;
use glam::{Mat4, Quat, Vec3};
use re_assets::{AssetId, ImportedGltf, codes, import_gltf};
use re_core::Severity;
use re_types::mesh::Aabb;

const POS: [[f32; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [0.0, 1.5, 0.0],
    [0.0, 2.0, 0.0],
];

fn armature() -> Mat4 {
    Mat4::from_rotation_translation(Quat::from_rotation_x(0.5), Vec3::new(0.0, 0.0, 1.0))
}

/// Joint globals at bind: Root at the armature, Tip one unit up from Root.
fn bind_globals() -> (Mat4, Mat4) {
    let root = armature();
    (root, root * Mat4::from_translation(Vec3::Y))
}

struct Options {
    /// Leave out WEIGHTS_0.
    no_weights_0: bool,
    /// Inverse bind matrices to write (normally 2).
    ibm_count: usize,
    /// A joint index for vertex 3's first influence (normally 0, the Tip in file order).
    bad_joint: Option<u16>,
    /// Call the Tip joint "Root" as well.
    duplicate_names: bool,
    /// Give the mesh node a skin but the mesh no weights.
    unweighted: bool,
    /// Add a second root joint "Prop", listed last, and make it the skin's
    /// `skeleton`.
    prop_skeleton: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            no_weights_0: false,
            ibm_count: 2,
            bad_joint: None,
            duplicate_names: false,
            unweighted: false,
            prop_skeleton: false,
        }
    }
}

/// A two-joint bar. Skin joints are listed child first (Tip, Root) to check the
/// reordering. Node 0 "Armature" (rotated, moved) holds joint node 1 "Root"
/// (child: node 2 "Tip") and the mesh node 3 "Bar".
fn bar(o: &Options) -> Vec<u8> {
    let mut g = Glb::default();
    let pos = g.positions(&POS);
    let idx: Vec<u8> = [0u16, 1, 2, 2, 1, 3]
        .iter()
        .flat_map(|i| i.to_le_bytes())
        .collect();
    let idx = g.accessor(
        &idx,
        common::skin_glb::UNSIGNED_SHORT,
        6,
        "SCALAR",
        false,
        "",
    );
    // File joint order: 0 = Tip, 1 = Root.
    let mut joints: Vec<[u16; 4]> = vec![[1, 0, 0, 0], [1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 0, 0]];
    if let Some(j) = o.bad_joint {
        joints[3][0] = j;
    }
    let j0 = g.joints_u16(&joints);
    // v0 all Root, v1 Root .5 Tip .5, v2 Tip .75 Root .25, v3 all Tip.
    let w0 = g.weights_u8(&[
        [255, 0, 0, 0],
        [128, 127, 0, 0],
        [191, 64, 0, 0],
        [255, 0, 0, 0],
    ]);
    // An all-zero second set (u8 joints, float weights) changes nothing.
    let j1 = g.joints_u8(&[[0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0], [0, 0, 0, 0]]);
    let w1 = g.weights_f32(&[[0.0; 4], [0.0; 4], [0.0; 4], [0.0; 4]]);
    let (root, tip) = bind_globals();
    let ibms: Vec<Mat4> = [tip.inverse(), root.inverse()]
        .into_iter()
        .cycle()
        .take(o.ibm_count)
        .collect();
    let ibm = g.mat4s(&ibms);
    let t = g.times(&[0.0, 1.0]);
    let r = g.floats(
        &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.70710677, 0.70710677],
        4,
        "VEC4",
    );
    let attributes = if o.unweighted {
        format!(r#"{{"POSITION":{pos}}}"#)
    } else if o.no_weights_0 {
        format!(r#"{{"POSITION":{pos},"JOINTS_0":{j0}}}"#)
    } else {
        format!(
            r#"{{"POSITION":{pos},"JOINTS_0":{j0},"WEIGHTS_0":{w0},"JOINTS_1":{j1},"WEIGHTS_1":{w1}}}"#
        )
    };
    let a = armature();
    let (s, q, tr) = a.to_scale_rotation_translation();
    let tip_name = if o.duplicate_names { "Root" } else { "Tip" };
    let body = format!(
        r#""scene":0,"scenes":[{{"nodes":[0]}}],
"nodes":[
 {{"name":"Armature","children":[1,3,4],"translation":{tr:?},"rotation":{q:?},"scale":{s:?}}},
 {{"name":"Root","children":[2]}},
 {{"name":"{tip_name}","translation":[0,1,0]}},
 {{"name":"Bar","mesh":0,"skin":0}},
 {{"name":"Prop","translation":[1,0,0]}}
],
"meshes":[{{"name":"bar","primitives":[{{"attributes":{attributes},"indices":{idx}}}]}}],
"skins":[{{"name":"rig","joints":{skin_joints},"inverseBindMatrices":{ibm},"skeleton":{skeleton}}}],
"animations":[{{"name":"bend","samplers":[{{"input":{t},"output":{r}}}],"channels":[{{"sampler":0,"target":{{"node":2,"path":"rotation"}}}}]}}]"#,
        skin_joints = if o.prop_skeleton { "[2,1,4]" } else { "[2,1]" },
        skeleton = if o.prop_skeleton { 4 } else { 1 },
        tr = tr.to_array(),
        q = q.to_array(),
        s = s.to_array(),
    );
    g.finish(&body)
}

fn import(bytes: &[u8]) -> Result<ImportedGltf, re_core::Diagnostic> {
    import_gltf(bytes, None, AssetId::new_source(), "assets/bar.glb")
}

#[test]
fn imports_the_skin_in_parent_first_order() {
    let g = import(&bar(&Options::default())).unwrap();
    assert!(g.warnings.is_empty(), "{:#?}", g.warnings);
    assert_eq!(g.skins.len(), 1);
    let skin = g.skin("rig").unwrap();
    assert!(skin.is_well_formed());
    let names: Vec<&str> = skin.joints.iter().map(|j| j.name.as_str()).collect();
    assert_eq!(
        names,
        ["Root", "Tip"],
        "reordered so the parent comes first"
    );
    assert_eq!(skin.joints[0].parent, None);
    assert_eq!(skin.joints[1].parent, Some(0));
    assert_eq!(skin.joints[1].rest.translation, Vec3::Y);
    assert!(skin.joints[1].parent_offset.is_none());
    let offset = skin.joints[0]
        .parent_offset
        .expect("the armature's transform");
    assert!(offset.abs_diff_eq(armature(), 1e-6));

    // The inverse bind matrices follow the new order.
    let (root, tip) = bind_globals();
    assert!(skin.inverse_bind[0].abs_diff_eq(root.inverse(), 1e-6));
    assert!(skin.inverse_bind[1].abs_diff_eq(tip.inverse(), 1e-6));

    // The node and mesh are tied to the skin; weights are remapped to the new
    // order (Root = 0, Tip = 1) and sum to one.
    assert_eq!(g.nodes[3].skin, Some(0));
    assert_eq!(g.skinned_meshes(0), [0]);
    let w = g.meshes[0].skin_weights.as_ref().unwrap();
    assert!(w.is_well_formed(&g.meshes[0].mesh, 2));
    assert_eq!(w.joints[0], [0, 0, 0, 0]);
    assert_eq!(w.weights[0], [65535, 0, 0, 0]);
    assert_eq!(w.joints[1][..2], [0, 1]);
    assert_eq!(w.joints[2][..2], [1, 0], "Tip has the larger weight");
    assert_eq!(w.joints[3], [1, 0, 0, 0]);
    assert_eq!(w.weights[3], [65535, 0, 0, 0]);

    // Joint bounds cover the bind-space vertices each joint influences.
    let at = |y0: f32, y1: f32| Some(Aabb::new(Vec3::new(0.0, y0, 0.0), Vec3::new(0.0, y1, 0.0)));
    assert_eq!(skin.joint_bounds, [at(0.0, 1.5), at(1.0, 2.0)]);

    // The clip targets the joint by name.
    assert_eq!(g.clips[0].targets[0].target, "Tip");
}

#[test]
fn the_skeleton_root_becomes_joint_0() {
    let g = import(&bar(&Options {
        prop_skeleton: true,
        ibm_count: 3,
        ..Options::default()
    }))
    .unwrap();
    let skin = g.skin("rig").unwrap();
    assert!(skin.is_well_formed());
    let names: Vec<&str> = skin.joints.iter().map(|j| j.name.as_str()).collect();
    assert_eq!(
        names,
        ["Prop", "Root", "Tip"],
        "the skeleton root comes first"
    );
    assert_eq!(skin.joints[2].parent, Some(1));
    // Weights follow the new order: v0 is all Root.
    let w = g.meshes[0].skin_weights.as_ref().unwrap();
    assert_eq!(w.joints[0], [1, 0, 0, 0]);
}

#[test]
fn skinned_palette_reproduces_the_bind_pose() {
    // model(joint) * inverse_bind is the identity at the rest pose, so the bind
    // pose draws the mesh where it was modelled.
    let g = import(&bar(&Options::default())).unwrap();
    let skin = &g.skins[0];
    let mut model: Vec<Mat4> = Vec::new();
    for j in &skin.joints {
        let parent = j.parent.map_or(Mat4::IDENTITY, |p| model[p as usize]);
        let offset = j.parent_offset.unwrap_or(Mat4::IDENTITY);
        model.push(parent * offset * j.rest.to_mat4());
    }
    for (m, ibm) in model.iter().zip(&skin.inverse_bind) {
        assert!(
            (*m * *ibm).abs_diff_eq(Mat4::IDENTITY, 1e-5),
            "{}",
            *m * *ibm
        );
    }
}

#[test]
fn more_than_four_influences_keep_the_largest() {
    let mut g = Glb::default();
    let pos = g.positions(&POS[..3]);
    let j0 = g.joints_u16(&[[0, 1, 2, 3]; 3]);
    let w0 = g.weights_f32(&[[0.4, 0.2, 0.1, 0.1], [1.0, 0.0, 0.0, 0.0], [0.25; 4]]);
    let j1 = g.joints_u16(&[[4, 5, 0, 0]; 3]);
    let w1 = g.weights_f32(&[[0.15, 0.05, 0.0, 0.0], [0.0; 4], [0.0; 4]]);
    let ibm = g.mat4s(&[Mat4::IDENTITY; 6]);
    let body = format!(
        r#""scene":0,"scenes":[{{"nodes":[0,1,2,3,4,5,6]}}],
"nodes":[{{"name":"a"}},{{"name":"b"}},{{"name":"c"}},{{"name":"d"}},{{"name":"e"}},{{"name":"f"}},{{"name":"m","mesh":0,"skin":0}}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":{pos},"JOINTS_0":{j0},"WEIGHTS_0":{w0},"JOINTS_1":{j1},"WEIGHTS_1":{w1}}}}}]}}],
"skins":[{{"joints":[0,1,2,3,4,5],"inverseBindMatrices":{ibm}}}]"#
    );
    let g = import(&g.finish(&body)).unwrap();
    let warn: Vec<_> = g.warnings.iter().map(|d| (d.code, d.severity)).collect();
    assert_eq!(warn, [(codes::SKIN_TOO_MANY_INFLUENCES, Severity::Warning)]);
    assert!(
        g.warnings[0].message.contains("1 vertices"),
        "{}",
        g.warnings[0].message
    );
    let w = g.meshes[0].skin_weights.as_ref().unwrap();
    // 0.4, 0.2, 0.15 and one of the 0.1s (joint 2, the lower index) survive.
    assert_eq!(w.joints[0], [0, 1, 4, 2]);
    let sum = 0.4 + 0.2 + 0.15 + 0.1;
    let expect = [0.4, 0.2, 0.15, 0.1].map(|x: f32| (x / sum * 65535.0).round() as i32);
    for (got, want) in w.weights[0].iter().zip(expect).skip(1) {
        assert_eq!(i32::from(*got), want);
    }
    assert_eq!(
        w.weights[0].iter().map(|&x| u32::from(x)).sum::<u32>(),
        65535
    );
    assert_eq!(g.skins[0].name, "skin_0");
}

#[test]
fn malformed_skins_are_errors() {
    let cases = [
        (
            Options {
                no_weights_0: true,
                ..Options::default()
            },
            "JOINTS_0 and WEIGHTS_0 must come together",
        ),
        (
            Options {
                ibm_count: 1,
                ..Options::default()
            },
            "it has 2 joints but 1 inverse bind matrices",
        ),
        (
            Options {
                bad_joint: Some(9),
                ..Options::default()
            },
            "uses joint 9 but skin `rig` has 2 joints",
        ),
    ];
    for (options, message) in cases {
        let e = import(&bar(&options)).unwrap_err();
        assert_eq!(e.code, codes::SKIN_INVALID, "{}", e.message);
        assert!(e.message.contains(message), "{}", e.message);
    }
}

#[test]
fn duplicate_joint_names_are_renamed_and_clips_follow() {
    let g = import(&bar(&Options {
        duplicate_names: true,
        ..Options::default()
    }))
    .unwrap();
    assert_eq!(g.warnings.len(), 1, "{:#?}", g.warnings);
    assert_eq!(g.warnings[0].code, codes::SKIN_DUPLICATE_JOINT_NAME);
    let names: Vec<&str> = g.skins[0].joints.iter().map(|j| j.name.as_str()).collect();
    assert_eq!(names, ["Root#1", "Root#2"]);
    assert_eq!(g.nodes[2].name, "Root#2");
    assert_eq!(g.clips[0].targets[0].target, "Root#2");
}

#[test]
fn a_skinned_node_without_weights_draws_unskinned() {
    let g = import(&bar(&Options {
        unweighted: true,
        ..Options::default()
    }))
    .unwrap();
    assert_eq!(g.warnings.len(), 1);
    assert_eq!(g.warnings[0].code, codes::SKIN_UNWEIGHTED_MESH);
    assert!(g.meshes[0].skin_weights.is_none());
    assert!(g.skinned_meshes(0).is_empty());
    assert_eq!(g.skins[0].joint_bounds, [None, None]);
}

#[test]
fn two_skins_sharing_a_mesh_get_their_own_copy() {
    let mut g = Glb::default();
    let pos = g.positions(&POS[..3]);
    let j0 = g.joints_u16(&[[0, 0, 0, 0], [1, 0, 0, 0], [1, 0, 0, 0]]);
    let w0 = g.weights_f32(&[[1.0, 0.0, 0.0, 0.0]; 3]);
    let ibm = g.mat4s(&[Mat4::IDENTITY; 2]);
    // Skin 0 lists (a, b) with b a child of a: no reorder. Skin 1 lists (b2, a2)
    // with b2 a child of a2: reordered, so its copy's joint indices swap.
    let body = format!(
        r#""scene":0,"scenes":[{{"nodes":[0,2,4,5]}}],
"nodes":[{{"name":"a","children":[1]}},{{"name":"b"}},{{"name":"a2","children":[3]}},{{"name":"b2"}},{{"name":"m1","mesh":0,"skin":0}},{{"name":"m2","mesh":0,"skin":1}}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":{pos},"JOINTS_0":{j0},"WEIGHTS_0":{w0}}}}}]}}],
"skins":[{{"joints":[0,1],"inverseBindMatrices":{ibm}}},{{"joints":[3,2],"inverseBindMatrices":{ibm}}}]"#
    );
    let g = import(&g.finish(&body)).unwrap();
    assert!(g.warnings.is_empty(), "{:#?}", g.warnings);
    assert_eq!(g.meshes.len(), 2, "the mesh is copied for the second skin");
    assert_eq!(g.skinned_meshes(0), [0]);
    assert_eq!(g.skinned_meshes(1), [1]);
    let first = g.meshes[0].skin_weights.as_ref().unwrap();
    let second = g.meshes[1].skin_weights.as_ref().unwrap();
    assert_eq!(first.joints[1][0], 1);
    assert_eq!(
        second.joints[1][0], 0,
        "b2 is index 1 in the file, 0 after a2 moves first"
    );
    assert_eq!(second.joints[0][0], 1);
}

/// The copy for a second skin starts from the weights as imported, not from the
/// first skin's remapped ones, and nodes of one skin share its copy.
#[test]
fn shared_mesh_copies_start_from_the_imported_weights() {
    let mut g = Glb::default();
    let pos = g.positions(&POS[..3]);
    let j0 = g.joints_u16(&[[0, 0, 0, 0], [1, 0, 0, 0], [1, 0, 0, 0]]);
    let w0 = g.weights_f32(&[[1.0, 0.0, 0.0, 0.0]; 3]);
    let ibm = g.mat4s(&[Mat4::IDENTITY; 2]);
    // Skin 0 lists (b, a), child first: reordered. Skin 1 lists (a2, b2): kept.
    let body = format!(
        r#""scene":0,"scenes":[{{"nodes":[0,2,4,5,6]}}],
"nodes":[{{"name":"a","children":[1]}},{{"name":"b"}},{{"name":"a2","children":[3]}},{{"name":"b2"}},{{"name":"m1","mesh":0,"skin":0}},{{"name":"m2","mesh":0,"skin":1}},{{"name":"m3","mesh":0,"skin":1}}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":{pos},"JOINTS_0":{j0},"WEIGHTS_0":{w0}}}}}]}}],
"skins":[{{"joints":[1,0],"inverseBindMatrices":{ibm}}},{{"joints":[2,3],"inverseBindMatrices":{ibm}}}]"#
    );
    let g = import(&g.finish(&body)).unwrap();
    assert!(g.warnings.is_empty(), "{:#?}", g.warnings);
    assert_eq!(
        g.meshes.len(),
        2,
        "one copy for skin 1, shared by m2 and m3"
    );
    let first = g.meshes[0].skin_weights.as_ref().unwrap();
    let second = g.meshes[1].skin_weights.as_ref().unwrap();
    assert_eq!(
        (first.joints[0][0], first.joints[1][0]),
        (1, 0),
        "skin 0 swaps"
    );
    assert_eq!(
        (second.joints[0][0], second.joints[1][0]),
        (0, 1),
        "skin 1 keeps the file's indices"
    );
    assert_eq!(g.nodes[5].meshes, g.nodes[6].meshes);
}

/// gltf accepts cyclic node graphs; a skin over one is an error, not a hang.
#[test]
fn cyclic_joint_hierarchies_are_errors() {
    for (nodes, joints) in [
        // A joint that is its own child.
        (
            r#"{"name":"a","children":[0]},{"name":"m","mesh":0,"skin":0}"#,
            "[0]",
        ),
        // Two joints that are each other's child.
        (
            r#"{"name":"a","children":[1]},{"name":"b","children":[0]},{"name":"m","mesh":0,"skin":0}"#,
            "[0,1]",
        ),
        // A cycle through a non-joint node above the joint.
        (
            r#"{"name":"x","children":[1]},{"name":"y","children":[0,2]},{"name":"a"},{"name":"m","mesh":0,"skin":0}"#,
            "[2]",
        ),
    ] {
        let mut g = Glb::default();
        let pos = g.positions(&POS[..3]);
        let j0 = g.joints_u16(&[[0, 0, 0, 0]; 3]);
        let w0 = g.weights_f32(&[[1.0, 0.0, 0.0, 0.0]; 3]);
        let mesh_node = nodes.matches("\"name\"").count() - 1;
        let body = format!(
            r#""scene":0,"scenes":[{{"nodes":[{mesh_node}]}}],"nodes":[{nodes}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":{pos},"JOINTS_0":{j0},"WEIGHTS_0":{w0}}}}}]}}],
"skins":[{{"joints":{joints}}}]"#
        );
        let err = import(&g.finish(&body)).unwrap_err();
        assert_eq!(err.code, re_assets::codes::SKIN_INVALID, "{nodes}: {err}");
        assert!(err.message.contains("cycle"), "{nodes}: {err}");
    }
}
