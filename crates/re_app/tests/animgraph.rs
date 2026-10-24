//! Animation graphs in the running game (ADR-0039): `Animator`s posing a
//! generated 20-joint figure, the three-pose golden, root motion (for a plain
//! entity and for a character), `BoneAttachment`s, the Luau `anim` library,
//! the `animgraph.*` and `anim.set_param` commands, and the graph state in
//! the run's state hash. The tests that draw need a Vulkan or GL adapter.

use std::path::{Path, PathBuf};

use glam::{Mat4, Quat, Vec3};
use re_app::{App, AppConfig};
use re_command::{AllowAll, Origin};
use re_reflect::Value;
use re_render::Image;
use re_render::imgdiff::{self, Tolerance};
use serde_json::json;

// --- The figure -------------------------------------------------------------

/// The figure's joints: name, parent, offset from the parent, and the box
/// drawn for it (centre and half size, in the joint's space).
#[allow(clippy::type_complexity)]
const JOINTS: [(&str, Option<usize>, [f32; 3], Option<([f32; 3], [f32; 3])>); 20] = [
    ("root", None, [0.0, 0.0, 0.0], None),
    (
        "hips",
        Some(0),
        [0.0, 0.95, 0.0],
        Some(([0.0, 0.05, 0.0], [0.14, 0.08, 0.09])),
    ),
    (
        "spine",
        Some(1),
        [0.0, 0.15, 0.0],
        Some(([0.0, 0.1, 0.0], [0.12, 0.1, 0.08])),
    ),
    (
        "chest",
        Some(2),
        [0.0, 0.2, 0.0],
        Some(([0.0, 0.1, 0.0], [0.16, 0.12, 0.09])),
    ),
    (
        "neck",
        Some(3),
        [0.0, 0.2, 0.0],
        Some(([0.0, 0.05, 0.0], [0.04, 0.05, 0.04])),
    ),
    (
        "head",
        Some(4),
        [0.0, 0.1, 0.0],
        Some(([0.0, 0.12, 0.02], [0.09, 0.11, 0.1])),
    ),
    ("shoulder_l", Some(3), [0.12, 0.15, 0.0], None),
    (
        "arm_l",
        Some(6),
        [0.08, 0.0, 0.0],
        Some(([0.0, -0.14, 0.0], [0.045, 0.14, 0.045])),
    ),
    (
        "forearm_l",
        Some(7),
        [0.0, -0.28, 0.0],
        Some(([0.0, -0.125, 0.0], [0.04, 0.125, 0.04])),
    ),
    (
        "hand_l",
        Some(8),
        [0.0, -0.25, 0.0],
        Some(([0.0, -0.06, 0.0], [0.035, 0.06, 0.02])),
    ),
    ("shoulder_r", Some(3), [-0.12, 0.15, 0.0], None),
    (
        "arm_r",
        Some(10),
        [-0.08, 0.0, 0.0],
        Some(([0.0, -0.14, 0.0], [0.045, 0.14, 0.045])),
    ),
    (
        "forearm_r",
        Some(11),
        [0.0, -0.28, 0.0],
        Some(([0.0, -0.125, 0.0], [0.04, 0.125, 0.04])),
    ),
    (
        "hand_r",
        Some(12),
        [0.0, -0.25, 0.0],
        Some(([0.0, -0.06, 0.0], [0.035, 0.06, 0.02])),
    ),
    (
        "thigh_l",
        Some(1),
        [0.09, -0.05, 0.0],
        Some(([0.0, -0.21, 0.0], [0.065, 0.21, 0.065])),
    ),
    (
        "shin_l",
        Some(14),
        [0.0, -0.42, 0.0],
        Some(([0.0, -0.21, 0.0], [0.05, 0.21, 0.05])),
    ),
    (
        "foot_l",
        Some(15),
        [0.0, -0.42, 0.0],
        Some(([0.0, -0.04, 0.06], [0.05, 0.04, 0.11])),
    ),
    (
        "thigh_r",
        Some(1),
        [-0.09, -0.05, 0.0],
        Some(([0.0, -0.21, 0.0], [0.065, 0.21, 0.065])),
    ),
    (
        "shin_r",
        Some(17),
        [0.0, -0.42, 0.0],
        Some(([0.0, -0.21, 0.0], [0.05, 0.21, 0.05])),
    ),
    (
        "foot_r",
        Some(18),
        [0.0, -0.42, 0.0],
        Some(([0.0, -0.04, 0.06], [0.05, 0.04, 0.11])),
    ),
];

fn joint(name: &str) -> usize {
    JOINTS.iter().position(|j| j.0 == name).unwrap()
}

/// Each joint's rest position in the skin's space.
fn rest_positions() -> Vec<Vec3> {
    let mut out: Vec<Vec3> = Vec::new();
    for (_, parent, offset, _) in JOINTS {
        let p = parent.map_or(Vec3::ZERO, |p| out[p]);
        out.push(p + Vec3::from(offset));
    }
    out
}

struct Bin {
    bin: Vec<u8>,
    views: Vec<String>,
    accessors: Vec<String>,
}

impl Bin {
    fn push(&mut self, bytes: &[u8], component: u32, count: usize, ty: &str, extra: &str) -> usize {
        while !self.bin.len().is_multiple_of(4) {
            self.bin.push(0);
        }
        self.views.push(format!(
            r#"{{"buffer":0,"byteOffset":{},"byteLength":{}}}"#,
            self.bin.len(),
            bytes.len()
        ));
        self.bin.extend_from_slice(bytes);
        self.accessors.push(format!(
            r#"{{"bufferView":{},"componentType":{component},"count":{count},"type":"{ty}"{extra}}}"#,
            self.views.len() - 1
        ));
        self.accessors.len() - 1
    }

    fn floats(&mut self, v: &[f32], per: usize, ty: &str, extra: &str) -> usize {
        let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.push(&bytes, 5126, v.len() / per, ty, extra)
    }
}

/// One clip: `(joint, times, rotations)` channels plus an optional root
/// translation channel.
struct ClipDef {
    name: &'static str,
    rotations: Vec<(&'static str, Vec<f32>, Vec<Quat>)>,
    root: Option<(Vec<f32>, Vec<Vec3>)>,
}

fn clips() -> Vec<ClipDef> {
    let x = Quat::from_rotation_x;
    let z = Quat::from_rotation_z;
    let swing = |a: f32| vec![x(a), x(-a), x(a)];
    vec![
        ClipDef {
            name: "idle",
            rotations: vec![
                ("chest", vec![0.0, 1.0, 2.0], vec![x(0.0), x(-0.06), x(0.0)]),
                (
                    "arm_l",
                    vec![0.0, 1.0, 2.0],
                    vec![z(0.12), z(0.18), z(0.12)],
                ),
                (
                    "arm_r",
                    vec![0.0, 1.0, 2.0],
                    vec![z(-0.12), z(-0.18), z(-0.12)],
                ),
                ("head", vec![0.0, 1.0, 2.0], vec![x(0.0), x(0.05), x(0.0)]),
            ],
            root: None,
        },
        ClipDef {
            name: "walk",
            rotations: vec![
                ("thigh_l", vec![0.0, 0.5, 1.0], swing(-0.5)),
                ("thigh_r", vec![0.0, 0.5, 1.0], swing(0.5)),
                (
                    "shin_l",
                    vec![0.0, 0.25, 0.5, 0.75, 1.0],
                    vec![x(0.1), x(0.7), x(0.1), x(0.1), x(0.1)],
                ),
                (
                    "shin_r",
                    vec![0.0, 0.25, 0.5, 0.75, 1.0],
                    vec![x(0.1), x(0.1), x(0.1), x(0.7), x(0.1)],
                ),
                ("arm_l", vec![0.0, 0.5, 1.0], swing(0.45)),
                ("arm_r", vec![0.0, 0.5, 1.0], swing(-0.45)),
                ("forearm_l", vec![0.0, 1.0], vec![x(-0.4), x(-0.4)]),
                ("forearm_r", vec![0.0, 1.0], vec![x(-0.4), x(-0.4)]),
                ("spine", vec![0.0, 1.0], vec![x(0.08), x(0.08)]),
            ],
            root: Some((
                vec![0.0, 0.5, 1.0],
                vec![
                    Vec3::ZERO,
                    Vec3::new(0.0, 0.03, 0.7),
                    Vec3::new(0.0, 0.0, 1.4),
                ],
            )),
        },
        ClipDef {
            name: "wave",
            rotations: vec![
                ("arm_r", vec![0.0, 1.0], vec![z(-2.5), z(-2.5)]),
                (
                    "forearm_r",
                    vec![0.0, 0.5, 1.0],
                    vec![z(-0.5), z(0.4), z(-0.5)],
                ),
                ("hand_r", vec![0.0, 1.0], vec![z(0.0), z(0.0)]),
            ],
            root: None,
        },
    ]
}

/// The figure as a binary glTF: skin `Figure` over the 20 joints, one box per
/// bone rigidly weighted to its joint, and clips `idle`, `walk` (moving the
/// root 1.4 m forward per second) and `wave` (the right arm).
fn figure_glb() -> Vec<u8> {
    let mut b = Bin {
        bin: Vec::new(),
        views: Vec::new(),
        accessors: Vec::new(),
    };
    let rest = rest_positions();
    let (mut pos, mut nrm, mut jnt, mut wgt, mut idx) = (
        Vec::<f32>::new(),
        Vec::<f32>::new(),
        Vec::<u8>::new(),
        Vec::<f32>::new(),
        Vec::<u16>::new(),
    );
    let faces = [
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
    ];
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for (j, (_, _, _, shape)) in JOINTS.iter().enumerate() {
        let Some((centre, half)) = shape else {
            continue;
        };
        let (c, h) = (rest[j] + Vec3::from(*centre), Vec3::from(*half));
        for (n, u, v) in faces {
            let base = (pos.len() / 3) as u16;
            for (su, sv) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
                let p = c + (n + u * su + v * sv) * h;
                lo = lo.min(p);
                hi = hi.max(p);
                pos.extend_from_slice(&p.to_array());
                nrm.extend_from_slice(&n.to_array());
                for k in [j as u16, 0, 0, 0] {
                    jnt.extend_from_slice(&k.to_le_bytes());
                }
                wgt.extend_from_slice(&[1.0, 0.0, 0.0, 0.0]);
            }
            idx.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    let count = pos.len() / 3;
    let p = b.floats(
        &pos,
        3,
        "VEC3",
        &format!(
            r#","min":[{},{},{}],"max":[{},{},{}]"#,
            lo.x, lo.y, lo.z, hi.x, hi.y, hi.z
        ),
    );
    let n = b.floats(&nrm, 3, "VEC3", "");
    let j = b.push(&jnt, 5123, count, "VEC4", "");
    let w = b.floats(&wgt, 4, "VEC4", "");
    let idx_bytes: Vec<u8> = idx.iter().flat_map(|i| i.to_le_bytes()).collect();
    let i = b.push(&idx_bytes, 5123, idx.len(), "SCALAR", "");
    let ibm: Vec<f32> = rest
        .iter()
        .flat_map(|p| Mat4::from_translation(-*p).to_cols_array())
        .collect();
    let ibm = b.floats(&ibm, 16, "MAT4", "");

    let mut animations = Vec::new();
    for clip in clips() {
        let mut samplers = Vec::new();
        let mut channels = Vec::new();
        for (name, times, rots) in &clip.rotations {
            let t = b.floats(
                times,
                1,
                "SCALAR",
                &format!(
                    r#","min":[{}],"max":[{}]"#,
                    times[0],
                    times[times.len() - 1]
                ),
            );
            let values: Vec<f32> = rots.iter().flat_map(|q| q.to_array()).collect();
            let v = b.floats(&values, 4, "VEC4", "");
            channels.push(format!(
                r#"{{"sampler":{},"target":{{"node":{},"path":"rotation"}}}}"#,
                samplers.len(),
                joint(name)
            ));
            samplers.push(format!(r#"{{"input":{t},"output":{v}}}"#));
        }
        if let Some((times, values)) = &clip.root {
            let t = b.floats(
                times,
                1,
                "SCALAR",
                &format!(
                    r#","min":[{}],"max":[{}]"#,
                    times[0],
                    times[times.len() - 1]
                ),
            );
            let values: Vec<f32> = values.iter().flat_map(|v| v.to_array()).collect();
            let v = b.floats(&values, 3, "VEC3", "");
            channels.push(format!(
                r#"{{"sampler":{},"target":{{"node":0,"path":"translation"}}}}"#,
                samplers.len()
            ));
            samplers.push(format!(r#"{{"input":{t},"output":{v}}}"#));
        }
        animations.push(format!(
            r#"{{"name":"{}","samplers":[{}],"channels":[{}]}}"#,
            clip.name,
            samplers.join(","),
            channels.join(",")
        ));
    }
    while !b.bin.len().is_multiple_of(4) {
        b.bin.push(0);
    }
    let nodes: Vec<String> = JOINTS
        .iter()
        .enumerate()
        .map(|(k, (name, _, offset, _))| {
            let children: Vec<String> = JOINTS
                .iter()
                .enumerate()
                .filter(|(_, c)| c.1 == Some(k))
                .map(|(c, _)| c.to_string())
                .collect();
            let children = if children.is_empty() {
                String::new()
            } else {
                format!(r#","children":[{}]"#, children.join(","))
            };
            format!(
                r#"{{"name":"{name}","translation":[{},{},{}]{children}}}"#,
                offset[0], offset[1], offset[2]
            )
        })
        .chain([r#"{"name":"Body","mesh":0,"skin":0}"#.to_string()])
        .collect();
    let joints: Vec<String> = (0..JOINTS.len()).map(|k| k.to_string()).collect();
    let json = format!(
        r#"{{"asset":{{"version":"2.0"}},"scene":0,"scenes":[{{"nodes":[0,{body}]}}],
"nodes":[{nodes}],
"meshes":[{{"primitives":[{{"attributes":{{"POSITION":{p},"NORMAL":{n},"JOINTS_0":{j},"WEIGHTS_0":{w}}},"indices":{i}}}]}}],
"skins":[{{"name":"Figure","skeleton":0,"joints":[{joints}],"inverseBindMatrices":{ibm}}}],
"animations":[{animations}],
"accessors":[{acc}],"bufferViews":[{views}],"buffers":[{{"byteLength":{len}}}]}}"#,
        body = JOINTS.len(),
        nodes = nodes.join(","),
        joints = joints.join(","),
        animations = animations.join(","),
        acc = b.accessors.join(","),
        views = b.views.join(","),
        len = b.bin.len()
    );
    let mut j = json.into_bytes();
    while !j.len().is_multiple_of(4) {
        j.push(b' ');
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&((12 + 8 + j.len() + 8 + b.bin.len()) as u32).to_le_bytes());
    out.extend_from_slice(&(j.len() as u32).to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&j);
    out.extend_from_slice(&(b.bin.len() as u32).to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&b.bin);
    out
}

/// Speed blends idle into walk; `waving` crossfades the right arm (masked
/// from its shoulder down) into `wave`; `jump` is a trigger that nothing
/// tests but scripts may set.
const GRAPH: &str = r#"{
  "format": "real-engine/animgraph", "version": 1,
  "params": [
    { "id": "speed", "type": "float" },
    { "id": "waving", "type": "bool" },
    { "id": "jump", "type": "trigger" }
  ],
  "nodes": [
    { "id": "idle", "kind": "clip", "clip": "assets/figure.glb#idle" },
    { "id": "walk", "kind": "clip", "clip": "assets/figure.glb#walk", "root_motion": true },
    { "id": "move", "kind": "blend1d", "param": "speed",
      "children": [{ "node": "idle", "threshold": 0.0 }, { "node": "walk", "threshold": 1.4 }] },
    { "id": "wave", "kind": "clip", "clip": "assets/figure.glb#wave" }
  ],
  "layers": [
    { "id": "base", "entry": "move", "states": [{ "id": "move", "node": "move" }] },
    { "id": "arm", "mask": "right_arm", "entry": "down",
      "states": [{ "id": "down", "node": "move" }, { "id": "up", "node": "wave" }],
      "transitions": [
        { "id": "raise", "from": "down", "to": "up", "duration": 0.2,
          "conditions": [{ "test": "bool", "param": "waving", "value": true }] },
        { "id": "lower", "from": "up", "to": "down", "duration": 0.2,
          "conditions": [{ "test": "bool", "param": "waving", "value": false }] }
      ] }
  ],
  "masks": [{ "id": "right_arm", "joints": [{ "joint": "shoulder_r", "subtree": true }] }]
}
"#;

fn figure_entity(id: &str, x: f32, extra: &str) -> String {
    format!(
        r#"{{ "id": "{id}", "components": {{
        "SkinnedMesh": {{ "asset": "assets/figure.glb#Figure" }},
        "Animator": {{ "graph": "anims/figure.animgraph.json" }},
        "MaterialRef": {{ "asset": "materials/skin.material.json" }},
        "Transform": {{ "translation": [{x}, 0.0, 0.0] }} {extra} }} }}"#
    )
}

fn project(entities: &[String], scripts: &[(&str, &str)]) -> tempfile::TempDir {
    let mut all = vec![
        r#"{ "id": "camera", "components": {
        "Camera": { "active": true, "fov_y_deg": 40.0 },
        "Transform": { "translation": [0.0, 1.0, 5.5] } } }"#
            .to_string(),
        r#"{ "id": "sun", "components": {
        "Light": { "kind": "Directional", "intensity": 0.8 },
        "Transform": { "rotation": [-0.3, 0.2, 0.0, 0.93] } } }"#
            .to_string(),
    ];
    all.extend(entities.iter().cloned());
    let scene = format!(
        "{{\n  \"format\": \"real-engine/scene\",\n  \"version\": 1,\n  \"entities\": [\n{}\n  ]\n}}\n",
        all.join(",\n")
    );
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    for d in ["scenes", "materials", "assets", "anims", "scripts"] {
        std::fs::create_dir_all(p.join(d)).unwrap();
    }
    std::fs::write(
        p.join("real.toml"),
        "[project]\nname = \"figures\"\n\n[render]\nwidth = 320\nheight = 200\n",
    )
    .unwrap();
    std::fs::write(p.join("scenes/main.scene.json"), scene).unwrap();
    std::fs::write(
        p.join("materials/skin.material.json"),
        r#"{ "base_color": [0.25, 0.32, 0.55, 1.0], "roughness": 0.7 }"#,
    )
    .unwrap();
    std::fs::write(p.join("assets/figure.glb"), figure_glb()).unwrap();
    std::fs::write(p.join("anims/figure.animgraph.json"), GRAPH).unwrap();
    for (path, text) in scripts {
        std::fs::write(p.join(path), text).unwrap();
    }
    dir
}

fn open(dir: &Path) -> App {
    App::open(dir, AppConfig::load(dir).unwrap()).unwrap()
}

fn dispatch(app: &mut App, name: &str, args: serde_json::Value) -> re_command::CommandOutcome {
    app.dispatch(name, args, false, &AllowAll, Origin::User)
}

fn translation(app: &App, id: &str) -> Vec3 {
    match app.world().get(id, "Transform", "translation").unwrap() {
        Value::Vec3(v) => v,
        other => panic!("{other:?}"),
    }
}

fn joint_matrix(app: &App, id: &str, name: &str) -> Mat4 {
    let e = re_scene::entity_id(app.world().entity(id).unwrap());
    app.skin_sync().joint_matrices(e).unwrap()[joint(name)]
}

// --- Goldens ------------------------------------------------------------------

fn golden_set(app: &App) -> Option<&'static str> {
    match app.graphics()?.backend() {
        "vulkan" => Some("vulkan"),
        "gl" => Some("gl"),
        _ => None,
    }
}

/// Compares `image` with `tests/goldens/<set>/<name>.png` at the M1 tolerance,
/// as `re_render`'s goldens do (Linux only). The actual image goes to
/// `out/re_app/<set>/`; `RE_UPDATE_GOLDENS=1` rewrites the golden.
fn check_golden(app: &App, name: &str, image: &Image) {
    let Some(set) = golden_set(app).filter(|_| cfg!(target_os = "linux")) else {
        println!("no golden set here; skipping the comparison of {name}");
        return;
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = root.join("../../out/re_app").join(set);
    image.save_png(&out.join(format!("{name}.png"))).unwrap();
    let golden_path = root
        .join("tests/goldens")
        .join(set)
        .join(format!("{name}.png"));
    if std::env::var_os("RE_UPDATE_GOLDENS").is_some_and(|v| !v.is_empty()) {
        image.save_png(&golden_path).unwrap();
        println!("updated {}", golden_path.display());
        return;
    }
    let golden = Image::load_png(&golden_path).unwrap_or_else(|d| {
        panic!(
            "{}missing golden: run with RE_UPDATE_GOLDENS=1 and review the image",
            d.render_human()
        )
    });
    let report = imgdiff::compare(image, &golden, Tolerance::default())
        .unwrap_or_else(|d| panic!("{}", d.render_human()));
    println!("{name} ({set}): {}", report.summary());
    if !report.matches() {
        let diff = imgdiff::diff_image(image, &golden, Tolerance::default()).unwrap();
        diff.save_png(&out.join(format!("{name}.diff.png")))
            .unwrap();
        panic!(
            "{name} does not match its {set} golden: {} (actual and diff in {})",
            report.summary(),
            out.display()
        );
    }
}

#[test]
fn a_graph_poses_the_figure_in_three_ways() {
    let dir = project(
        &[
            figure_entity("standing", -1.1, ""),
            figure_entity("walking", 0.0, ""),
            figure_entity("waving", 1.1, ""),
        ],
        &[],
    );
    let mut app = open(dir.path());
    app.ensure_graphics().unwrap();
    // The graphs bind in the first fixed step.
    let report = app.run_headless(Some(1), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    for (id, param, value) in [
        ("walking", "speed", json!(1.4)),
        ("waving", "speed", json!(0.7)),
        ("waving", "waving", json!(true)),
    ] {
        let o = dispatch(
            &mut app,
            "anim.set_param",
            json!({ "entity": id, "param": param, "value": value }),
        );
        assert!(o.ok, "{:?}", o.diagnostics);
    }
    let report = app.run_headless(Some(26), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);

    let e = |id: &str| re_scene::entity_id(app.world().entity(id).unwrap());
    let skins = app.skin_sync();
    assert_eq!(
        skins.player(e("standing")).unwrap().state(Some("arm")),
        Some("down")
    );
    assert_eq!(
        skins.player(e("waving")).unwrap().state(Some("arm")),
        Some("up")
    );
    assert_eq!(app.snapshot().skinned.len(), 3);

    // Root motion moved the walkers forward (+Z), the faster one further;
    // the standing figure stayed put.
    let (s, w, v) = (
        translation(&app, "standing"),
        translation(&app, "walking"),
        translation(&app, "waving"),
    );
    assert_eq!(s, Vec3::new(-1.1, 0.0, 0.0));
    assert!(w.z > 0.4 && w.x == 0.0, "{w}");
    assert!(v.z > 0.1 && v.z < w.z, "{v}");
    // The waving arm is raised: the right hand is above the head.
    let hand = joint_matrix(&app, "waving", "hand_r").w_axis.y;
    let head = joint_matrix(&app, "waving", "head").w_axis.y;
    assert!(hand > head, "hand {hand}, head {head}");
    let standing_hand = joint_matrix(&app, "standing", "hand_r").w_axis.y;
    assert!(standing_hand < 1.0, "{standing_hand}");

    let image = app.screenshot(None).unwrap();
    check_golden(&app, "figure_three_poses", &image);
}

// --- Without a GPU ------------------------------------------------------------

#[test]
fn root_motion_moves_an_entity_and_bone_attachments_follow() {
    let dir = project(
        &[
            figure_entity("hero", 0.0, ""),
            r#"{ "id": "sword", "parent": "hero", "components": {
            "BoneAttachment": { "joint": "hand_r" }, "Transform": {} } }"#
                .to_string(),
            r#"{ "id": "lost", "parent": "hero", "components": {
            "BoneAttachment": { "joint": "tail" }, "Transform": {} } }"#
                .to_string(),
        ],
        &[],
    );
    let mut app = open(dir.path());
    let first = app.run_headless(Some(1), None).unwrap();
    let o = dispatch(
        &mut app,
        "anim.set_param",
        json!({ "entity": "hero", "param": "speed", "value": 1.4 }),
    );
    assert!(o.ok, "{:?}", o.diagnostics);
    let report = app.run_headless(Some(60), None).unwrap();
    // One warning, on the first step, for the attachment whose joint is
    // missing.
    let missing: Vec<_> = first
        .diagnostics
        .iter()
        .chain(&report.diagnostics)
        .filter(|d| d.message.contains("`tail`"))
        .collect();
    assert_eq!(missing.len(), 1, "{:?}", report.diagnostics);

    // Walk covers 1.4 m a second at full weight: 60 steps of 1/60 s.
    let z = translation(&app, "hero").z;
    assert!((z - 1.4).abs() < 0.03, "{z}");
    // The pose keeps the root at its first frame's horizontal place.
    let root = joint_matrix(&app, "hero", "root").w_axis;
    assert!(root.x.abs() < 1e-5 && root.z.abs() < 1e-5, "{root}");

    let hand = joint_matrix(&app, "hero", "hand_r");
    let sword = translation(&app, "sword");
    assert!(
        sword.abs_diff_eq(hand.w_axis.truncate(), 1e-5),
        "{sword} vs {hand}"
    );
}

#[test]
fn root_motion_moves_a_character_through_physics() {
    let dir = project(
        &[
            figure_entity(
                "hero",
                0.0,
                r#", "CharacterController": { "height": 1.8, "radius": 0.3 }"#,
            )
            .replace(
                r#""translation": [0.0, 0.0, 0.0]"#,
                r#""translation": [0.0, 1.0, 0.0]"#,
            ),
            r#"{ "id": "ground", "components": {
            "Collider": { "shape": "Box", "half_extents": [20.0, 0.5, 20.0] },
            "Transform": { "translation": [0.0, -0.5, 0.0] } } }"#
                .to_string(),
        ],
        &[],
    );
    let mut app = open(dir.path());
    app.run_headless(Some(1), None).unwrap();
    let start = translation(&app, "hero");
    dispatch(
        &mut app,
        "anim.set_param",
        json!({ "entity": "hero", "param": "speed", "value": 1.4 }),
    );
    let report = app.run_headless(Some(60), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    let moved = translation(&app, "hero") - start;
    assert!(moved.z > 1.0 && moved.z < 1.6, "{moved}");
}

const SCRIPT: &str = r#"
local hero = world.get("hero")
local steps = 0
time.on_step(function()
	steps += 1
	if steps == 2 then
		anim.set(hero, "speed", 0.7)
		anim.set(hero, "waving", true)
		anim.trigger(hero, "jump")
		local ok, err = pcall(anim.set, hero, "sped", 1)
		print(`unknown: {ok} {err}`)
		ok, err = pcall(anim.set, hero, "waving", 3)
		print(`mistyped: {ok} {err}`)
	elseif steps == 30 then
		print(`speed {anim.get(hero, "speed")} waving {anim.get(hero, "waving")} jump {anim.get(hero, "jump")}`)
		print(`states {anim.state(hero)} {anim.state(hero, "arm")}`)
		local p, r = anim.joint(hero, "hand_r")
		print(`hand above head: {p.y > anim.joint(hero, "head").y}`)
		local ok, err = pcall(anim.state, hero, "legs")
		print(`layer: {ok} {err}`)
	end
end)
"#;

#[test]
fn scripts_drive_graphs_through_the_anim_library() {
    let dir = project(
        &[figure_entity(
            "hero",
            0.0,
            r#", "ScriptRef": { "path": "scripts/hero.luau" }"#,
        )],
        &[("scripts/hero.luau", SCRIPT)],
    );
    let mut app = open(dir.path());
    let report = app.run_headless(Some(31), None).unwrap();
    let out = report.output.join("\n");
    assert!(
        out.contains("[RE7609] animation graph parameter `sped`: the graph has no such parameter; the graph's parameters: `speed` (float), `waving` (bool), `jump` (trigger)"),
        "{out}"
    );
    assert!(
        out.contains(
            "[RE7609] animation graph parameter `waving`: it is a bool, but was given a float"
        ),
        "{out}"
    );
    assert!(out.contains("speed 0.69999"), "{out}");
    assert!(out.contains("waving true jump true"), "{out}");
    assert!(out.contains("states move up"), "{out}");
    assert!(out.contains("hand above head: true"), "{out}");
    assert!(out.contains("layer: false"), "{out}");
    assert!(out.contains("`base`, `arm`"), "{out}");
}

#[test]
fn graph_state_is_part_of_the_state_hash() {
    let dir = project(&[figure_entity("hero", 0.0, "")], &[]);
    let run = |waving: bool| {
        let mut app = open(dir.path());
        app.run_headless(Some(1), None).unwrap();
        // `waving` changes only the arm layer's state, not the entity.
        dispatch(
            &mut app,
            "anim.set_param",
            json!({ "entity": "hero", "param": "waving", "value": waving }),
        );
        let r = app.run_headless(Some(20), None).unwrap();
        (r.state_hash, app.world().state_hash())
    };
    let (a, world_a) = run(false);
    let (b, _) = run(false);
    let (c, world_c) = run(true);
    assert_eq!(a, b, "the same run hashes the same");
    assert_eq!(world_a, world_c, "the world is the same");
    assert_ne!(a, c, "the graph state differs");
}

#[test]
fn graphs_are_created_edited_and_validated_by_command() {
    let dir = project(&[figure_entity("hero", 0.0, "")], &[]);
    let mut app = open(dir.path());

    let o = dispatch(
        &mut app,
        "animgraph.create",
        json!({ "path": "anims/new.animgraph.json" }),
    );
    assert!(o.ok, "{:?}", o.diagnostics);
    let text = std::fs::read_to_string(dir.path().join("anims/new.animgraph.json")).unwrap();
    assert!(text.contains("real-engine/animgraph"), "{text}");
    let o = dispatch(
        &mut app,
        "animgraph.create",
        json!({ "path": "anims/new.animgraph.json" }),
    );
    assert!(!o.ok, "an existing file is refused");
    let o = dispatch(
        &mut app,
        "animgraph.create",
        json!({ "path": "anims/new.json" }),
    );
    assert!(!o.ok);

    // One batch: a parameter, two nodes, a layer, a transition added to it.
    let o = dispatch(
        &mut app,
        "animgraph.edit",
        json!({ "path": "anims/new.animgraph.json", "ops": [
            { "op": "upsert", "element": "param", "value": { "id": "go", "type": "bool" } },
            { "op": "upsert", "element": "node",
              "value": { "id": "idle", "kind": "clip", "clip": "assets/figure.glb#idle" } },
            { "op": "upsert", "element": "node",
              "value": { "id": "walk", "kind": "clip", "clip": "assets/figure.glb#walk" } },
            { "op": "upsert", "element": "layer", "value": { "id": "base", "entry": "a",
              "states": [{ "id": "a", "node": "idle" }, { "id": "b", "node": "walk" }] } },
            { "op": "upsert", "element": "transition", "layer": "base",
              "value": { "id": "t", "from": "a", "to": "b", "duration": 0.3,
                         "conditions": [{ "test": "bool", "param": "go", "value": true }] } }
        ] }),
    );
    assert!(o.ok, "{:?}", o.diagnostics);
    assert_eq!(o.changes.len(), 1, "one set_doc: {:?}", o.changes);
    let doc = &app.world().doc("anims/new.animgraph.json").unwrap().value;
    assert_eq!(doc["layers"][0]["transitions"][0]["to"], json!("b"));

    // A batch that leaves the graph invalid is refused whole, naming the
    // element.
    let o = dispatch(
        &mut app,
        "animgraph.edit",
        json!({ "path": "anims/new.animgraph.json", "ops": [
            { "op": "upsert", "element": "param", "value": { "id": "speed", "type": "float" } },
            { "op": "remove", "element": "state", "layer": "base", "id": "b" }
        ] }),
    );
    assert!(!o.ok);
    assert!(
        o.diagnostics[0]
            .message
            .contains("`layers[0].transitions[0].to` names state `b`"),
        "{:?}",
        o.diagnostics
    );
    let doc = &app.world().doc("anims/new.animgraph.json").unwrap().value;
    assert!(
        doc["params"].as_array().unwrap().len() == 1,
        "unchanged: {doc}"
    );

    // A raw set_doc is checked by the same validator.
    let o = dispatch(
        &mut app,
        "scene.apply",
        json!({ "ops": [{ "op": "set_doc", "doc": "anims/new.animgraph.json",
                          "pointer": "/layers/0/entry", "value": "zz" }] }),
    );
    assert!(!o.ok, "{:?}", o);

    // Undo takes the whole first batch back.
    let o = dispatch(&mut app, "undo", json!({}));
    assert!(o.ok, "{:?}", o.diagnostics);
    let doc = &app.world().doc("anims/new.animgraph.json").unwrap().value;
    assert!(doc.get("layers").is_none(), "{doc}");

    // Validation of the figure's graph, bound to its skin.
    let o = dispatch(
        &mut app,
        "animgraph.validate",
        json!({ "path": "anims/figure.animgraph.json", "skin": "assets/figure.glb#Figure" }),
    );
    assert!(o.ok, "{:?}", o.diagnostics);
    assert_eq!(o.data["valid"], json!(true), "{}", o.data);
    assert_eq!(o.data["warnings"], json!([]));

    // An unknown entity or parameter for anim.set_param.
    let o = dispatch(
        &mut app,
        "anim.set_param",
        json!({ "entity": "nobody", "param": "speed", "value": 1 }),
    );
    assert!(!o.ok);
}
