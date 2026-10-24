//! Ragdolls in the running game (ADR-0040): a `Ragdoll` on a `SkinnedMesh`
//! becomes a physics ragdoll on the skin's skeleton, `physics.ragdoll` switches
//! it, `physics.ragdoll_state` reads it back, and the skin shows the simulated
//! pose blended in. Headless: no GPU needed.

use std::path::Path;

use glam::{Mat4, Vec3};
use re_app::{App, AppConfig};
use re_scene::entity_id;

/// The humanoid's joints: name, parent (an earlier index) and offset from it.
const JOINTS: [(&str, Option<usize>, [f32; 3]); 17] = [
    ("hips", None, [0.0, 1.0, 0.0]),
    ("spine", Some(0), [0.0, 0.15, 0.0]),
    ("chest", Some(1), [0.0, 0.2, 0.0]),
    ("head", Some(2), [0.0, 0.3, 0.0]),
    ("head_end", Some(3), [0.0, 0.2, 0.0]),
    ("upper_arm_l", Some(2), [0.2, 0.15, 0.0]),
    ("forearm_l", Some(5), [0.28, 0.0, 0.0]),
    ("hand_l", Some(6), [0.26, 0.0, 0.0]),
    ("upper_arm_r", Some(2), [-0.2, 0.15, 0.0]),
    ("forearm_r", Some(8), [-0.28, 0.0, 0.0]),
    ("hand_r", Some(9), [-0.26, 0.0, 0.0]),
    ("thigh_l", Some(0), [0.1, -0.05, 0.0]),
    ("shin_l", Some(11), [0.0, -0.43, 0.0]),
    ("foot_l", Some(12), [0.0, -0.44, 0.0]),
    ("thigh_r", Some(0), [-0.1, -0.05, 0.0]),
    ("shin_r", Some(14), [0.0, -0.43, 0.0]),
    ("foot_r", Some(15), [0.0, -0.44, 0.0]),
];

/// A one-triangle mesh skinned to the humanoid's skeleton (skin `Rig`); with
/// `renamed`, the joints are called `bone_0`, `bone_1` and so on.
fn man_glb(renamed: bool) -> Vec<u8> {
    let mut bin: Vec<u8> = Vec::new();
    let mut views = Vec::new();
    let mut accessors = Vec::new();
    let mut push = |bytes: &[u8], component: u32, count: usize, ty: &str, extra: &str| {
        while !bin.len().is_multiple_of(4) {
            bin.push(0);
        }
        views.push(format!(
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{}}}"#,
            bin.len(),
            bytes.len()
        ));
        bin.extend_from_slice(bytes);
        accessors.push(format!(
            r#"{{"bufferView":{},"componentType":{component},"count":{count},"type":"{ty}"{extra}}}"#,
            views.len() - 1
        ));
        accessors.len() - 1
    };
    let floats = |v: &[f32]| -> Vec<u8> { v.iter().flat_map(|f| f.to_le_bytes()).collect() };
    let p = push(
        &floats(&[0.0, 0.9, 0.0, 0.1, 1.1, 0.0, -0.1, 1.1, 0.0]),
        5126,
        3,
        "VEC3",
        r#","min":[-0.1,0.9,0],"max":[0.1,1.1,0]"#,
    );
    let n = push(&floats(&[0.0, 0.0, 1.0].repeat(3)), 5126, 3, "VEC3", "");
    let j: Vec<u8> = [0u16; 12].iter().flat_map(|j| j.to_le_bytes()).collect();
    let j = push(&j, 5123, 3, "VEC4", "");
    let w = push(
        &floats(&[1.0, 0.0, 0.0, 0.0].repeat(3)),
        5126,
        3,
        "VEC4",
        "",
    );
    let mut model = Vec::new();
    for (_, parent, offset) in JOINTS {
        let at = parent.map_or(Vec3::ZERO, |p: usize| model[p]) + Vec3::from(offset);
        model.push(at);
    }
    let ibm: Vec<f32> = model
        .iter()
        .flat_map(|at| Mat4::from_translation(-*at).to_cols_array())
        .collect();
    let ibm = push(&floats(&ibm), 5126, JOINTS.len(), "MAT4", "");
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let nodes: Vec<String> = JOINTS
        .iter()
        .enumerate()
        .map(|(i, (name, _, offset))| {
            let children: Vec<String> = JOINTS
                .iter()
                .enumerate()
                .filter(|(_, (_, p, _))| *p == Some(i))
                .map(|(c, _)| c.to_string())
                .collect();
            let name = if renamed {
                format!("bone_{i}")
            } else {
                (*name).to_string()
            };
            format!(
                r#"{{"name":"{name}","translation":[{},{},{}],"children":[{}]}}"#,
                offset[0],
                offset[1],
                offset[2],
                children.join(",")
            )
        })
        .chain([r#"{"name":"Body","mesh":0,"skin":0}"#.to_string()])
        .collect();
    let joints: Vec<String> = (0..JOINTS.len()).map(|i| i.to_string()).collect();
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0,{}]}}],
"nodes":[{}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":{p},"NORMAL":{n},"JOINTS_0":{j},"WEIGHTS_0":{w}}}}}]}}],
"skins":[{{"name":"Rig","joints":[{}],"inverseBindMatrices":{ibm}}}],
"accessors":[{}],"bufferViews":[{}],"buffers":[{{"byteLength":{}}}]}}"#,
        JOINTS.len(),
        nodes.join(","),
        joints.join(","),
        accessors.join(","),
        views.join(","),
        bin.len()
    );
    let mut j = json.into_bytes();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&((12 + 8 + j.len() + 8 + bin.len()) as u32).to_le_bytes());
    out.extend_from_slice(&(j.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&j);
    out.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    out
}

fn project(ragdoll: &str, script: Option<&str>, renamed: bool) -> tempfile::TempDir {
    let script_ref = if script.is_some() {
        r#""ScriptRef": { "path": "scripts/fall.luau" },"#
    } else {
        ""
    };
    let scene = format!(
        r#"{{
  "format": "real-engine/scene",
  "version": 1,
  "entities": [
    {{ "id": "ground", "components": {{
        "Collider": {{ "shape": "Box", "half_extents": [50.0, 0.5, 50.0] }},
        "Transform": {{ "translation": [0.0, -0.5, 0.0] }} }} }},
    {{ "id": "man", "components": {{
        "SkinnedMesh": {{ "asset": "assets/man.glb#Rig" }},
        "Ragdoll": {ragdoll},
        {script_ref}
        "Transform": {{ "translation": [0.0, 0.3, 0.0] }} }} }}
  ]
}}
"#
    );
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    for d in ["scenes", "assets", "scripts"] {
        std::fs::create_dir_all(p.join(d)).unwrap();
    }
    std::fs::write(p.join("real.toml"), "[project]\nname = \"ragdoll\"\n").unwrap();
    std::fs::write(p.join("scenes/main.scene.json"), scene).unwrap();
    std::fs::write(p.join("assets/man.glb"), man_glb(renamed)).unwrap();
    if let Some(s) = script {
        std::fs::write(p.join("scripts/fall.luau"), s).unwrap();
    }
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn man(app: &App) -> re_types::EntityId {
    entity_id(app.world().entity("man").unwrap())
}

/// The hips' model-space height in the skin's shown pose.
fn hips_y(app: &App) -> f32 {
    app.skin_sync().joint_matrices(man(app)).unwrap()[0]
        .w_axis
        .y
}

#[test]
fn a_simulated_ragdoll_falls_and_the_skin_shows_it() {
    let dir = project(
        r#"{ "mode": "Simulated", "blend_seconds": 0.25 }"#,
        None,
        false,
    );
    let mut app = open(dir.path());
    let report = app.run_headless(Some(2), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let id = man(&app);
    let state = app.physics().ragdoll(id).expect("the ragdoll was built");
    assert_eq!(state.mode, re_types::physics::RagdollMode::Simulated);
    // The generated humanoid: hips, spine, head, both upper arms, forearms,
    // thighs and shins.
    assert_eq!(app.physics().ragdoll_parts(id).unwrap().len(), 11);
    assert!(
        app.ragdoll_sync().weight(id).unwrap() < 0.5,
        "the blend ramps up"
    );
    let report = app.run_headless(Some(240), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(app.ragdoll_sync().weight(id), Some(1.0));
    // Rest height 1.0 in model space; the ragdoll lies on the ground 0.3 below
    // the entity.
    assert!(hips_y(&app) < 0.0, "hips at {}", hips_y(&app));
    let parts = app.physics().ragdoll_parts(id).unwrap();
    assert!((parts[0].1.pos.y - (hips_y(&app) + 0.3)).abs() < 0.02);
    // The entity itself stays where it was: the skin carries the fall.
    match app.world().get("man", "Transform", "translation").unwrap() {
        re_reflect::Value::Vec3(v) => assert_eq!(v, Vec3::new(0.0, 0.3, 0.0)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_animated_ragdoll_shows_the_animation() {
    let dir = project("{}", None, false);
    let mut app = open(dir.path());
    let report = app.run_headless(Some(30), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let id = man(&app);
    assert_eq!(app.ragdoll_sync().weight(id), Some(0.0));
    assert_eq!(hips_y(&app), 1.0);
    // The parts follow the skeleton at the entity's place.
    let parts = app.physics().ragdoll_parts(id).unwrap();
    assert!((parts[0].1.pos - Vec3::new(0.0, 1.3, 0.0)).length() < 1e-3);
}

const FALL: &str = r#"local steps = 0
time.on_step(function()
	steps += 1
	if steps == 20 then
		physics.ragdoll(script.entity, "powered", 0.5)
		print(physics.ragdoll_state(script.entity).mode)
	elseif steps == 21 then
		print(physics.ragdoll_state(script.entity).mode)
		physics.ragdoll(script.entity, "simulated")
		physics.ragdoll_impulse(script.entity, "head", vector.create(0, 0, 20))
	elseif steps == 600 then
		local s = physics.ragdoll_state(script.entity)
		print(s.mode, s.calm, s.root_position.y < 0.4)
	end
end)
"#;

#[test]
fn scripts_switch_push_and_read_a_ragdoll() {
    let dir = project("{}", Some(FALL), false);
    let mut app = open(dir.path());
    let report = app.run_headless(Some(605), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(
        report.output,
        [
            "[game] animated",
            "[game] powered",
            "[game] simulated\ttrue\ttrue"
        ]
    );
}

#[test]
fn ragdoll_runs_are_deterministic() {
    let dir = project("{}", Some(FALL), false);
    let a = open(dir.path()).run_headless(Some(120), None).unwrap();
    let b = open(dir.path()).run_headless(Some(120), None).unwrap();
    assert!(a.ok(), "{:?}", a.diagnostics);
    assert_eq!(a.state_hash, b.state_hash);
}

#[test]
fn a_skeleton_without_humanoid_names_is_reported_once() {
    let dir = project("{}", None, true);
    let mut app = open(dir.path());
    let report = app.run_headless(Some(10), None).unwrap();
    let codes: Vec<_> = report
        .diagnostics
        .iter()
        .map(|d| d.code.to_string())
        .collect();
    assert_eq!(
        codes.iter().filter(|c| *c == "RE0529").count(),
        1,
        "{codes:?}"
    );
    assert!(app.physics().ragdoll(man(&app)).is_none());
    // Naming the joints builds it.
    app.world_mut()
        .set(
            "man",
            "Ragdoll",
            "joints",
            re_reflect::Value::List(
                ["bone_0", "bone_1", "bone_11", "bone_12"]
                    .map(|s| re_reflect::Value::Str(s.into()))
                    .to_vec(),
            ),
        )
        .unwrap();
    let report = app.run_headless(Some(2), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(app.physics().ragdoll_parts(man(&app)).unwrap().len(), 4);
}

#[test]
fn a_turning_ragdoll_is_not_rebuilt() {
    let dir = project(r#"{ "mode": "Simulated" }"#, None, false);
    let mut app = open(dir.path());
    let report = app.run_headless(Some(240), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let id = man(&app);
    for i in 0..24 {
        let turn = glam::Quat::from_rotation_y(0.27 * i as f32);
        app.world_mut()
            .set(
                "man",
                "Transform",
                "rotation",
                re_reflect::Value::Quat(turn),
            )
            .unwrap();
        let report = app.run_headless(Some(1), None).unwrap();
        assert!(report.ok(), "{:?}", report.diagnostics);
        // A rebuild would start animated again, blended out.
        assert_eq!(app.ragdoll_sync().weight(id), Some(1.0), "turn {i}");
    }
    // Still lying on the ground: a rebuilt ragdoll would stand up first.
    let hips = app.physics().ragdoll_parts(id).unwrap()[0].1.pos.y;
    assert!(hips < 0.4, "hips at {hips}");
}

#[test]
fn a_ragdoll_physics_refuses_is_not_retried_every_step() {
    // A body already holds the entity, so the ragdoll is refused once.
    let dir = project(
        r#"{}, "RigidBody": { "kind": "Static" }, "Collider": {}"#,
        None,
        false,
    );
    let mut app = open(dir.path());
    let report = app.run_headless(Some(30), None).unwrap();
    let ragdoll_errors = report
        .diagnostics
        .iter()
        .filter(|d| d.message.contains("ragdoll"))
        .count();
    assert_eq!(ragdoll_errors, 1, "{:?}", report.diagnostics);
    assert!(!app.ragdoll_sync().has(man(&app)));
}

#[test]
fn a_copy_of_the_game_keeps_a_fallen_ragdoll() {
    let dir = project(r#"{ "mode": "Simulated" }"#, None, false);
    let mut app = open(dir.path());
    let report = app.run_headless(Some(240), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let (report, _, _) = app.play_copy(5, None, &[], false).unwrap();
    assert!(report.diagnostics.is_empty(), "{:?}", report.diagnostics);
}

const LIMP: &str = r#"local steps = 0
time.on_step(function()
	steps += 1
	if steps == 2 then
		physics.ragdoll(script.entity, "simulated")
	end
end)
"#;

#[test]
fn a_rebuild_keeps_the_mode_a_script_set() {
    let dir = project("{}", Some(LIMP), false);
    let mut app = open(dir.path());
    let report = app.run_headless(Some(60), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let id = man(&app);
    assert_eq!(
        app.physics().ragdoll(id).unwrap().mode,
        re_types::physics::RagdollMode::Simulated
    );
    // A new mass rebuilds the ragdoll; it stays limp and shown.
    app.world_mut()
        .set(
            "man",
            "Ragdoll",
            "total_mass",
            re_reflect::Value::Float(80.0),
        )
        .unwrap();
    let report = app.run_headless(Some(3), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(
        app.physics().ragdoll(id).unwrap().mode,
        re_types::physics::RagdollMode::Simulated
    );
    assert_eq!(app.ragdoll_sync().weight(id), Some(1.0));
}

#[test]
fn a_nan_strength_is_reported_once() {
    let dir = project(r#"{ "mode": "Powered" }"#, None, false);
    let mut app = open(dir.path());
    // Reflection refuses a NaN; Rust code writing the component directly does not.
    let e = app.world().entity("man").unwrap();
    app.world_mut()
        .ecs
        .get_mut::<re_scene::Ragdoll>(e)
        .unwrap()
        .strength = f32::NAN;
    let report = app.run_headless(Some(30), None).unwrap();
    let refused = report
        .diagnostics
        .iter()
        .filter(|d| d.code.to_string() == "RE5019")
        .count();
    assert_eq!(refused, 1, "{:?}", report.diagnostics);
}

#[test]
fn the_state_hash_covers_ragdolls() {
    let dir = project(r#"{ "mode": "Simulated" }"#, None, false);
    let mut app = open(dir.path());
    let report = app.run_headless(Some(10), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    assert_eq!(report.state_hash, app.state_hash().to_hex().to_string());
    assert_ne!(app.state_hash(), app.world().state_hash());
    // The ragdoll falls while the scene stands still: only the ragdolls change it.
    let (world, game) = (app.world().state_hash(), app.state_hash());
    app.run_headless(Some(5), None).unwrap();
    assert_eq!(app.world().state_hash(), world);
    assert_ne!(app.state_hash(), game);
}
