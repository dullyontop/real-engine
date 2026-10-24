//! Animation graphs against an f64 reference evaluator (WP21 acceptance:
//! sampling and blending match to 1e-5 on synthetic skeletons): 1D and 2D
//! blend spaces, additive nodes, masked and additive layers, crossfades and
//! their interruption, and root motion. Plus the state machine's rules
//! (exit times, triggers, `any`) and the diagnostics.

use std::collections::HashMap;
use std::sync::Arc;

use glam::{DQuat, DVec2, DVec3, Mat4, Quat, Vec3};
use re_anim::Sampler;
use re_anim::codes;
use re_anim::graph::{BoundGraph, GraphPlayer, Param, RootMotion, validate};
use re_anim::skin::Pose;
use re_core::Diagnostic;
use re_types::anim::{
    AnimGraph, Channel, ClipData, Interpolation, JointDesc, SkinData, TargetChannels, Trs,
};

const TOLERANCE: f64 = 1e-5;
const DT: f32 = 1.0 / 60.0;

struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f64 / (1u64 << 24) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f()
    }
    fn vec(&mut self, r: f64) -> Vec3 {
        Vec3::new(
            self.range(-r, r) as f32,
            self.range(-r, r) as f32,
            self.range(-r, r) as f32,
        )
    }
    fn quat(&mut self, max_angle: f64) -> Quat {
        let axis = DVec3::new(self.f() - 0.5, self.f() - 0.5, self.f() - 0.5 + 1e-3).normalize();
        DQuat::from_axis_angle(axis, self.range(-max_angle, max_angle)).as_quat()
    }
}

/// `root` with children `a`, `b`; `a` has `a1`, `a2`; `a2` has `a21`.
const PARENTS: [Option<u16>; 6] = [None, Some(0), Some(0), Some(1), Some(1), Some(4)];

fn skin() -> SkinData {
    let mut rng = Rng(7);
    let joints: Vec<JointDesc> = PARENTS
        .iter()
        .enumerate()
        .map(|(i, &parent)| JointDesc {
            name: ["root", "a", "b", "a1", "a2", "a21"][i].to_string(),
            parent,
            rest: Trs {
                translation: rng.vec(0.2) + Vec3::Y * 0.3,
                rotation: rng.quat(0.5),
                scale: Vec3::ONE,
            },
            parent_offset: None,
        })
        .collect();
    SkinData {
        name: "synthetic".into(),
        inverse_bind: vec![Mat4::IDENTITY; joints.len()],
        joint_bounds: vec![None; joints.len()],
        joints,
    }
}

/// A clip of `duration` animating every joint but the root with three linear
/// keys of random translation, rotation and scale.
fn random_clip(name: &str, duration: f32, seed: u64) -> ClipData {
    let mut rng = Rng(seed);
    let times = vec![0.0, duration * 0.4, duration];
    let targets = ["a", "b", "a1", "a2", "a21"]
        .iter()
        .map(|t| TargetChannels {
            target: t.to_string(),
            translation: Some(Channel {
                interpolation: Interpolation::Linear,
                times: times.clone(),
                values: (0..3).map(|_| rng.vec(0.3)).collect(),
            }),
            rotation: Some(Channel {
                interpolation: Interpolation::Linear,
                times: times.clone(),
                values: (0..3).map(|_| rng.quat(1.2)).collect(),
            }),
            scale: Some(Channel {
                interpolation: Interpolation::Linear,
                times: times.clone(),
                values: (0..3).map(|_| Vec3::ONE + rng.vec(0.2)).collect(),
            }),
        })
        .collect();
    ClipData {
        name: name.into(),
        duration,
        targets,
    }
}

/// A clip moving the root forward 2 m and turning it 40 degrees about +Y per
/// second (with a sideways wobble, a vertical bob and a tilt about X), over
/// one second.
fn walk_clip() -> ClipData {
    let times: Vec<f32> = (0..=4).map(|i| i as f32 * 0.25).collect();
    let translation = times
        .iter()
        .map(|&t| Vec3::new(0.1 * libm::sinf(t * 6.0), 0.05 * t, 2.0 * t))
        .collect();
    let rotation = times
        .iter()
        .map(|&t| Quat::from_rotation_y(40f32.to_radians() * t) * Quat::from_rotation_x(0.2 * t))
        .collect();
    let mut clip = random_clip("walk", 1.0, 99);
    clip.targets.insert(
        0,
        TargetChannels {
            target: "root".into(),
            translation: Some(Channel {
                interpolation: Interpolation::Linear,
                times: times.clone(),
                values: translation,
            }),
            rotation: Some(Channel {
                interpolation: Interpolation::Linear,
                times,
                values: rotation,
            }),
            scale: None,
        },
    );
    clip
}

fn clips() -> HashMap<String, ClipData> {
    let mut m = HashMap::new();
    m.insert("m.glb#a".to_string(), random_clip("a", 1.0, 1));
    m.insert("m.glb#b".to_string(), random_clip("b", 1.5, 2));
    m.insert("m.glb#c".to_string(), random_clip("c", 0.8, 3));
    m.insert("m.glb#d".to_string(), random_clip("d", 1.2, 4));
    m.insert("m.glb#walk".to_string(), walk_clip());
    m
}

fn bind(json: &str) -> Result<(GraphPlayer, Vec<Diagnostic>), Diagnostic> {
    let graph: AnimGraph = serde_json::from_str(json).expect("parses");
    let clips = clips();
    let (bound, warnings) = BoundGraph::bind(&graph, &skin(), &mut |path| {
        let clip = clips.get(path).cloned().ok_or_else(|| {
            Diagnostic::error(re_core::Code::new("RE0001"), format!("no clip {path}"))
        })?;
        Ok(Arc::new(Sampler::new(clip)?))
    })?;
    Ok((GraphPlayer::new(Arc::new(bound)), warnings))
}

fn player(json: &str) -> GraphPlayer {
    bind(json).unwrap().0
}

fn graph(params: &str, nodes: &str, layers: &str, masks: &str) -> String {
    format!(
        r#"{{ "format": "real-engine/animgraph", "version": 1,
  "params": [{params}], "nodes": [{nodes}], "layers": [{layers}], "masks": [{masks}] }}"#
    )
}

// --- The f64 reference ----------------------------------------------------

#[derive(Clone, Copy, Debug)]
struct RTrs {
    t: DVec3,
    r: DQuat,
    s: DVec3,
}

fn rest_ref() -> Vec<RTrs> {
    skin()
        .joints
        .iter()
        .map(|j| RTrs {
            t: j.rest.translation.as_dvec3(),
            r: j.rest.rotation.as_dquat(),
            s: j.rest.scale.as_dvec3(),
        })
        .collect()
}

fn span(times: &[f32], t: f64) -> (usize, usize, f64) {
    let last = times.len() - 1;
    if t <= f64::from(times[0]) {
        return (0, 0, 0.0);
    }
    if t >= f64::from(times[last]) {
        return (last, last, 0.0);
    }
    let i = times.iter().rposition(|&k| f64::from(k) <= t).unwrap();
    let (a, b) = (f64::from(times[i]), f64::from(times[i + 1]));
    (i, i + 1, (t - a) / (b - a))
}

fn sample_v(c: &Channel<Vec3>, t: f64) -> DVec3 {
    let (i, j, s) = span(&c.times, t);
    c.values[i].as_dvec3().lerp(c.values[j].as_dvec3(), s)
}

fn sample_q(c: &Channel<Quat>, t: f64) -> DQuat {
    let (i, j, s) = span(&c.times, t);
    c.values[i]
        .as_dquat()
        .slerp(c.values[j].as_dquat(), s)
        .normalize()
}

fn sample_ref(clip: &ClipData, t: f64) -> Vec<RTrs> {
    let skin = skin();
    let mut pose = rest_ref();
    for target in &clip.targets {
        let j = skin.joint(&target.target).unwrap();
        if let Some(c) = &target.translation {
            pose[j].t = sample_v(c, t);
        }
        if let Some(c) = &target.rotation {
            pose[j].r = sample_q(c, t);
        }
        if let Some(c) = &target.scale {
            pose[j].s = sample_v(c, t);
        }
    }
    pose
}

/// The clip `path` at normalised time `norm` (looping).
fn clip_at(path: &str, norm: f64) -> Vec<RTrs> {
    let clip = &clips()[path];
    let d = f64::from(clip.duration);
    sample_ref(clip, norm.rem_euclid(1.0) * d)
}

fn nlerp_ref(a: DQuat, b: DQuat, w: f64) -> DQuat {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    (a * (1.0 - w) + b * w).normalize()
}

fn blend_ref(a: &RTrs, b: &RTrs, w: f64) -> RTrs {
    RTrs {
        t: a.t.lerp(b.t, w),
        r: nlerp_ref(a.r, b.r, w),
        s: a.s.lerp(b.s, w),
    }
}

fn blend_pose(a: &[RTrs], b: &[RTrs], w: &dyn Fn(usize) -> f64) -> Vec<RTrs> {
    a.iter()
        .zip(b)
        .enumerate()
        .map(|(j, (a, b))| blend_ref(a, b, w(j)))
        .collect()
}

/// Accumulates weighted poses the way the evaluator does: each child moves
/// the running result `w / (sum of weights so far)` toward itself.
fn weighted(poses: &[(f64, Vec<RTrs>)]) -> Vec<RTrs> {
    let mut out = poses[0].1.clone();
    let mut total = poses[0].0;
    for (w, p) in &poses[1..] {
        total += w;
        out = blend_pose(&out, p, &|_| w / total);
    }
    out
}

fn add_ref(base: &RTrs, add: &RTrs, reference: &RTrs, w: f64) -> RTrs {
    let delta = reference.r.conjugate() * add.r;
    RTrs {
        t: base.t + (add.t - reference.t) * w,
        r: (base.r * nlerp_ref(DQuat::IDENTITY, delta, w)).normalize(),
        s: base.s * DVec3::ONE.lerp(add.s / reference.s, w),
    }
}

fn weights_1d(thresholds: &[f64], p: f64) -> Vec<f64> {
    let n = thresholds.len();
    let mut w = vec![0.0; n];
    if p <= thresholds[0] {
        w[0] = 1.0;
    } else if p >= thresholds[n - 1] {
        w[n - 1] = 1.0;
    } else {
        let i = (0..n - 1)
            .find(|&i| thresholds[i] <= p && p < thresholds[i + 1])
            .unwrap();
        let s = (p - thresholds[i]) / (thresholds[i + 1] - thresholds[i]);
        w[i] = 1.0 - s;
        w[i + 1] = s;
    }
    w
}

fn weights_2d(points: &[DVec2], p: DVec2) -> Vec<f64> {
    let h: Vec<f64> = points
        .iter()
        .enumerate()
        .map(|(i, &pi)| {
            points
                .iter()
                .enumerate()
                .filter(|&(j, _)| j != i)
                .map(|(_, &pj)| {
                    let d = pj - pi;
                    1.0 - (p - pi).dot(d) / d.length_squared()
                })
                .fold(1.0f64, f64::min)
                .max(0.0)
        })
        .collect();
    let total: f64 = h.iter().sum();
    h.iter().map(|v| v / total).collect()
}

fn yaw_ref(q: DQuat) -> DQuat {
    DQuat::from_xyzw(0.0, q.y, 0.0, q.w).normalize()
}

fn assert_pose(got: &Pose, want: &[RTrs], what: &str) {
    assert_eq!(got.joints.len(), want.len(), "{what}");
    for (j, (g, w)) in got.joints.iter().zip(want).enumerate() {
        let dt = (g.translation.as_dvec3() - w.t).abs().max_element();
        let gr = g.rotation.as_dquat();
        let dr = (gr - w.r).length().min((gr + w.r).length());
        let ds = (g.scale.as_dvec3() - w.s).abs().max_element();
        assert!(
            dt < TOLERANCE && dr < TOLERANCE && ds < TOLERANCE,
            "{what}: joint {j}: {g:?} vs {w:?} (dt {dt:e}, dr {dr:e}, ds {ds:e})"
        );
    }
}

fn steps(p: &mut GraphPlayer, n: usize, pose: &mut Pose) -> Vec<RootMotion> {
    (0..n).map(|_| p.step(DT, pose)).collect()
}

const DT64: f64 = DT as f64;

// --- Blend spaces, additive, layers ---------------------------------------

#[test]
fn a_single_clip_matches_sampling() {
    let mut p = player(&graph(
        "",
        r#"{ "id": "a", "kind": "clip", "clip": "m.glb#a" }"#,
        r#"{ "id": "base", "entry": "s", "states": [{ "id": "s", "node": "a" }] }"#,
        "",
    ));
    let mut pose = Pose::default();
    for n in 1..=150 {
        p.step(DT, &mut pose);
        // Clip `a` is 1 s long: the normalised time is the time, wrapped.
        assert_pose(
            &pose,
            &clip_at("m.glb#a", n as f64 * DT64),
            &format!("step {n}"),
        );
    }
}

#[test]
fn a_1d_blend_space_matches_the_reference() {
    let json = graph(
        r#"{ "id": "speed", "type": "float" }"#,
        r#"{ "id": "a", "kind": "clip", "clip": "m.glb#a" },
           { "id": "b", "kind": "clip", "clip": "m.glb#b" },
           { "id": "c", "kind": "clip", "clip": "m.glb#c", "speed": 2.0 },
           { "id": "move", "kind": "blend1d", "param": "speed", "children": [
             { "node": "a", "threshold": 0.0 }, { "node": "b", "threshold": 1.0 },
             { "node": "c", "threshold": 3.0 }] }"#,
        r#"{ "id": "base", "entry": "s", "states": [{ "id": "s", "node": "move" }] }"#,
        "",
    );
    let durations = [1.0, 1.5, 0.4];
    let paths = ["m.glb#a", "m.glb#b", "m.glb#c"];
    let mut rng = Rng(11);
    let mut p = player(&json);
    let mut pose = Pose::default();
    let mut norm = 0.0;
    for n in 0..120 {
        // Hold the parameter for a few steps, sometimes past the ends.
        let speed = if n % 10 == 0 {
            rng.range(-0.5, 3.5) as f32
        } else {
            match p.get("speed").unwrap() {
                Param::Float(v) => v,
                _ => unreachable!(),
            }
        };
        p.set("speed", Param::Float(speed)).unwrap();
        let w = weights_1d(&[0.0, 1.0, 3.0], f64::from(speed));
        let d: f64 = w.iter().zip(durations).map(|(w, d)| w * d).sum();
        norm += DT64 / d;
        p.step(DT, &mut pose);
        let parts: Vec<(f64, Vec<RTrs>)> = w
            .iter()
            .zip(paths)
            .filter(|(w, _)| **w > 0.0)
            .map(|(w, path)| (*w, clip_at(path, norm)))
            .collect();
        assert_pose(
            &pose,
            &weighted(&parts),
            &format!("step {n}, speed {speed}"),
        );
    }
}

#[test]
fn a_2d_blend_space_matches_the_reference() {
    let points = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0], [-1.0, 0.5]];
    let paths = ["m.glb#a", "m.glb#b", "m.glb#c", "m.glb#d"];
    let children: Vec<String> = points
        .iter()
        .enumerate()
        .map(|(i, p)| format!(r#"{{ "node": "n{i}", "point": [{}, {}] }}"#, p[0], p[1]))
        .collect();
    let nodes: Vec<String> = paths
        .iter()
        .enumerate()
        .map(|(i, path)| format!(r#"{{ "id": "n{i}", "kind": "clip", "clip": "{path}" }}"#))
        .chain([format!(
            r#"{{ "id": "aim", "kind": "blend2d", "x": "x", "y": "y", "sync": false, "children": [{}] }}"#,
            children.join(",")
        )])
        .collect();
    let json = graph(
        r#"{ "id": "x", "type": "float" }, { "id": "y", "type": "float" }"#,
        &nodes.join(","),
        r#"{ "id": "base", "entry": "s", "states": [{ "id": "s", "node": "aim" }] }"#,
        "",
    );
    let pts: Vec<DVec2> = points.iter().map(|p| DVec2::new(p[0], p[1])).collect();
    let durations: Vec<f64> = paths
        .iter()
        .map(|p| f64::from(clips()[*p].duration))
        .collect();
    let mut rng = Rng(5);
    let mut p = player(&json);
    let mut pose = Pose::default();
    for n in 1..=60 {
        let (x, y) = (rng.range(-1.2, 1.2) as f32, rng.range(-0.3, 1.2) as f32);
        p.set("x", Param::Float(x)).unwrap();
        p.set("y", Param::Float(y)).unwrap();
        p.step(DT, &mut pose);
        // Unsynced: each child plays by seconds.
        let time = n as f64 * DT64;
        let w = weights_2d(&pts, DVec2::new(f64::from(x), f64::from(y)));
        let parts: Vec<(f64, Vec<RTrs>)> = w
            .iter()
            .zip(paths.iter().zip(&durations))
            .filter(|(w, _)| **w > 0.0)
            .map(|(w, (path, d))| (*w, clip_at(path, time / d)))
            .collect();
        assert_pose(&pose, &weighted(&parts), &format!("step {n} at ({x}, {y})"));
    }
}

#[test]
fn an_additive_node_adds_motion_relative_to_the_first_frame() {
    let json = graph(
        r#"{ "id": "lean", "type": "float", "default": 0.6 }"#,
        r#"{ "id": "a", "kind": "clip", "clip": "m.glb#a" },
           { "id": "c", "kind": "clip", "clip": "m.glb#c" },
           { "id": "add", "kind": "additive", "base": "a", "additive": "c", "weight_param": "lean" }"#,
        r#"{ "id": "base", "entry": "s", "states": [{ "id": "s", "node": "add" }] }"#,
        "",
    );
    let mut p = player(&json);
    let mut pose = Pose::default();
    for n in 1..=70 {
        p.step(DT, &mut pose);
        // The additive node lasts as long as its base (1 s); both children
        // play at its normalised time.
        let norm = n as f64 * DT64;
        let base = clip_at("m.glb#a", norm);
        let add = clip_at("m.glb#c", norm);
        let reference = clip_at("m.glb#c", 0.0);
        let want: Vec<RTrs> = (0..base.len())
            .map(|j| add_ref(&base[j], &add[j], &reference[j], 0.6))
            .collect();
        assert_pose(&pose, &want, &format!("step {n}"));
    }
}

#[test]
fn masked_and_additive_layers_match_the_reference() {
    let json = graph(
        "",
        r#"{ "id": "a", "kind": "clip", "clip": "m.glb#a" },
           { "id": "b", "kind": "clip", "clip": "m.glb#b" },
           { "id": "d", "kind": "clip", "clip": "m.glb#d" }"#,
        r#"{ "id": "base", "entry": "s", "states": [{ "id": "s", "node": "a" }] },
           { "id": "arm", "weight": 0.7, "mask": "arm", "entry": "s",
             "states": [{ "id": "s", "node": "b" }] },
           { "id": "breathe", "blend": "additive", "weight": 0.5, "entry": "s",
             "reference": "m.glb#c", "states": [{ "id": "s", "node": "d" }] }"#,
        r#"{ "id": "arm", "joints": [{ "joint": "a", "subtree": true },
                                    { "joint": "a2", "weight": 0.25 }, { "joint": "nope" }] }"#,
    );
    let (mut p, warnings) = bind(&json).unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert_eq!(warnings[0].code, codes::GRAPH_MASK_JOINT_MISSING);
    assert!(warnings[0].message.contains("`nope`"));
    // Joints: root, a, b, a1, a2, a21. `a` with its subtree, then `a2` alone
    // overridden to 0.25.
    let mask = [0.0, 1.0, 0.0, 1.0, 0.25, 1.0];
    let mut pose = Pose::default();
    for n in 1..=50 {
        p.step(DT, &mut pose);
        let time = n as f64 * DT64;
        let mut want = blend_pose(&rest_ref(), &clip_at("m.glb#a", time), &|_| 1.0);
        want = blend_pose(&want, &clip_at("m.glb#b", time / 1.5), &|j| 0.7 * mask[j]);
        let add = clip_at("m.glb#d", time / 1.2);
        let reference = sample_ref(&clips()["m.glb#c"], 0.0);
        let want: Vec<RTrs> = (0..want.len())
            .map(|j| add_ref(&want[j], &add[j], &reference[j], 0.5))
            .collect();
        assert_pose(&pose, &want, &format!("step {n}"));
    }
}

// --- State machines --------------------------------------------------------

fn two_states(transition: &str) -> String {
    graph(
        r#"{ "id": "go", "type": "bool" }, { "id": "jump", "type": "trigger" },
           { "id": "speed", "type": "float" }"#,
        r#"{ "id": "a", "kind": "clip", "clip": "m.glb#a" },
           { "id": "b", "kind": "clip", "clip": "m.glb#b" },
           { "id": "c", "kind": "clip", "clip": "m.glb#c" }"#,
        &format!(
            r#"{{ "id": "base", "entry": "idle",
             "states": [{{ "id": "idle", "node": "a" }}, {{ "id": "run", "node": "b" }},
                        {{ "id": "air", "node": "c" }}],
             "transitions": [{transition}] }}"#
        ),
        "",
    )
}

#[test]
fn a_crossfade_blends_the_playing_states() {
    let mut p = player(&two_states(
        r#"{ "id": "t", "from": "idle", "to": "run", "duration": 0.5,
             "conditions": [{ "test": "bool", "param": "go", "value": true }] }"#,
    ));
    let mut pose = Pose::default();
    steps(&mut p, 10, &mut pose);
    p.set("go", Param::Bool(true)).unwrap();
    // The transition fires on step 11, after idle advanced: run starts at 0.
    p.step(DT, &mut pose);
    assert_eq!(p.state(None), Some("run"));
    assert_pose(
        &pose,
        &clip_at("m.glb#a", 11.0 * DT64),
        "the fade starts at the source",
    );
    for k in 1..=40 {
        p.step(DT, &mut pose);
        let idle = clip_at("m.glb#a", (11 + k) as f64 * DT64);
        let run = clip_at("m.glb#b", k as f64 * DT64 / 1.5);
        let w = (k as f64 * DT64 / 0.5).min(1.0);
        let want = if k as f64 * DT64 >= 0.5 - 1e-9 {
            run
        } else {
            blend_pose(&idle, &run, &|_| w)
        };
        assert_pose(&pose, &want, &format!("{k} steps into the fade"));
    }
}

#[test]
fn an_interrupted_crossfade_fades_from_the_frozen_blend() {
    let mut p = player(&two_states(
        r#"{ "id": "t", "from": "idle", "to": "run", "duration": 0.5, "interruptible": true,
             "conditions": [{ "test": "bool", "param": "go", "value": true }] },
           { "id": "up", "from": "any", "to": "air", "duration": 0.25,
             "conditions": [{ "test": "trigger", "param": "jump" }] }"#,
    ));
    let mut pose = Pose::default();
    p.set("go", Param::Bool(true)).unwrap();
    p.step(DT, &mut pose); // idle -> run on the first step
    steps(&mut p, 12, &mut pose);
    p.trigger("jump").unwrap();
    p.step(DT, &mut pose);
    assert_eq!(p.state(None), Some("air"));
    assert_eq!(p.get("jump").unwrap(), Param::Trigger(false), "consumed");
    // Frozen at the step it was interrupted: idle at 14 steps, run at 13,
    // 13 steps into the 0.5 s fade.
    let frozen = blend_pose(
        &clip_at("m.glb#a", 14.0 * DT64),
        &clip_at("m.glb#b", 13.0 * DT64 / 1.5),
        &|_| 13.0 * DT64 / 0.5,
    );
    assert_pose(&pose, &frozen, "the interruption starts at the frozen pose");
    for k in 1..=20 {
        p.step(DT, &mut pose);
        let air = clip_at("m.glb#c", k as f64 * DT64 / 0.8);
        let w = k as f64 * DT64 / 0.25;
        let want = if w >= 1.0 - 1e-9 {
            air
        } else {
            blend_pose(&frozen, &air, &|_| w)
        };
        assert_pose(&pose, &want, &format!("{k} steps after the interruption"));
    }
}

#[test]
fn a_crossfade_that_is_not_interruptible_runs_to_the_end() {
    let mut p = player(&two_states(
        r#"{ "id": "t", "from": "idle", "to": "run", "duration": 0.5,
             "conditions": [{ "test": "bool", "param": "go", "value": true }] },
           { "id": "up", "from": "any", "to": "air", "duration": 0.25,
             "conditions": [{ "test": "trigger", "param": "jump" }] }"#,
    ));
    let mut pose = Pose::default();
    p.set("go", Param::Bool(true)).unwrap();
    p.step(DT, &mut pose);
    p.trigger("jump").unwrap();
    steps(&mut p, 10, &mut pose);
    assert_eq!(p.state(None), Some("run"), "the fade is not interrupted");
    assert_eq!(
        p.get("jump").unwrap(),
        Param::Trigger(true),
        "not consumed yet"
    );
    steps(&mut p, 30, &mut pose);
    assert_eq!(p.state(None), Some("air"), "fires once the fade ends");
}

#[test]
fn exit_times_wait_for_the_normalised_time() {
    // `run` lasts 1.5 s; the transition waits for 0.5 of each pass.
    let mut p = player(&two_states(
        r#"{ "id": "t", "from": "idle", "to": "run" },
           { "id": "back", "from": "run", "to": "idle", "exit_time": 0.5,
             "conditions": [{ "test": "compare", "param": "speed", "op": "<", "value": 1.0 }] }"#,
    ));
    let mut pose = Pose::default();
    p.set("speed", Param::Float(5.0)).unwrap();
    p.step(DT, &mut pose);
    assert_eq!(
        p.state(None),
        Some("run"),
        "a zero-length transition is immediate"
    );
    // Past 0.5 with the condition failing, then the condition holds: it waits
    // for 1.5.
    steps(&mut p, 60, &mut pose);
    p.set("speed", Param::Float(0.0)).unwrap();
    let mut fired = None;
    for k in 61..=200 {
        p.step(DT, &mut pose);
        if p.state(None) == Some("idle") {
            fired = Some(k);
            break;
        }
    }
    // The first step after run's normalised time (one more step of
    // `DT / 1.5` each from the second step on) passes 1.5: about 135.
    let mut norm = 0.0f64;
    let mut want = 1;
    while norm < 1.5 {
        norm += DT64 / 1.5;
        want += 1;
    }
    assert_eq!(fired, Some(want - 1));
    assert!((134..=136).contains(&want), "{want}");
}

#[test]
fn any_transitions_skip_their_target_and_triggers_persist() {
    let mut p = player(&two_states(
        r#"{ "id": "up", "from": "any", "to": "air",
             "conditions": [{ "test": "trigger", "param": "jump" }] },
           { "id": "land", "from": "air", "to": "idle",
             "conditions": [{ "test": "bool", "param": "go", "value": true }] }"#,
    ));
    let mut pose = Pose::default();
    p.trigger("jump").unwrap();
    p.step(DT, &mut pose);
    assert_eq!(p.state(None), Some("air"));
    p.trigger("jump").unwrap();
    steps(&mut p, 5, &mut pose);
    assert_eq!(
        p.state(None),
        Some("air"),
        "`any` does not re-enter its target"
    );
    assert_eq!(
        p.get("jump").unwrap(),
        Param::Trigger(true),
        "kept until used"
    );
    p.set("go", Param::Bool(true)).unwrap();
    p.step(DT, &mut pose);
    assert_eq!(p.state(None), Some("idle"));
    p.set("go", Param::Bool(false)).unwrap();
    p.step(DT, &mut pose);
    assert_eq!(
        p.state(None),
        Some("air"),
        "the kept trigger fires from idle"
    );
}

#[test]
fn the_same_steps_give_the_same_state_bits() {
    let json = two_states(
        r#"{ "id": "t", "from": "idle", "to": "run", "duration": 0.3, "interruptible": true,
             "conditions": [{ "test": "bool", "param": "go", "value": true }] },
           { "id": "up", "from": "any", "to": "air", "duration": 0.2,
             "conditions": [{ "test": "trigger", "param": "jump" }] }"#,
    );
    let run = || {
        let mut p = player(&json);
        let mut pose = Pose::default();
        let mut bits = Vec::new();
        for n in 0..90 {
            if n == 10 {
                p.set("go", Param::Bool(true)).unwrap();
            }
            if n == 20 {
                p.trigger("jump").unwrap();
            }
            p.step(DT, &mut pose);
            p.write_state(&mut bits);
        }
        (bits, pose)
    };
    let (a, pa) = run();
    let (b, pb) = run();
    assert_eq!(a, b);
    assert_eq!(pa, pb);
}

// --- Root motion -----------------------------------------------------------

fn root_motion_graph(looping: bool, speed: f32) -> String {
    graph(
        "",
        &format!(
            r#"{{ "id": "walk", "kind": "clip", "clip": "m.glb#walk", "root_motion": true,
                 "looping": {looping}, "speed": {speed} }}"#
        ),
        r#"{ "id": "base", "entry": "s", "states": [{ "id": "s", "node": "walk" }] }"#,
        "",
    )
}

/// The root's motion from clip time `a` to `b` in its frame at `a`.
fn segment_ref(clip: &ClipData, a: f64, b: f64) -> (DVec3, DQuat) {
    let ra = sample_ref(clip, a)[0];
    let rb = sample_ref(clip, b)[0];
    let ya = yaw_ref(ra.r);
    let mut d = rb.t - ra.t;
    d.y = 0.0;
    let mut t = ya.conjugate() * d;
    t.y = 0.0;
    (t, ya.conjugate() * yaw_ref(rb.r))
}

fn then_ref(a: (DVec3, DQuat), b: (DVec3, DQuat)) -> (DVec3, DQuat) {
    (a.0 + a.1 * b.0, a.1 * b.1)
}

/// Applies step motions to an entity at the origin facing +Z.
fn integrate(motions: &[RootMotion]) -> (DVec3, DQuat) {
    motions
        .iter()
        .fold((DVec3::ZERO, DQuat::IDENTITY), |acc, m| {
            then_ref(acc, (m.translation.as_dvec3(), m.rotation.as_dquat()))
        })
}

#[test]
fn root_motion_moves_the_entity_through_loops() {
    let clip = walk_clip();
    let mut p = player(&root_motion_graph(true, 1.0));
    let mut pose = Pose::default();
    // 2.5 passes of the 1 s clip.
    let motions = steps(&mut p, 150, &mut pose);
    let (t, r) = integrate(&motions);
    let full = segment_ref(&clip, 0.0, 1.0);
    let mut want = then_ref(full, full);
    want = then_ref(want, segment_ref(&clip, 0.0, 0.5));
    // The pose keeps the first frame's yaw, so motion is reported in it.
    let y0 = yaw_ref(sample_ref(&clip, 0.0)[0].r);
    let want = (y0 * want.0, want.1);
    // Relative to the distance travelled (at least 1 m), as for palettes in
    // tests/skin.rs: each step's motion is computed in f32, so 150 steps
    // over 4.3 m drift about 3e-6 of it.
    let scale = want.0.length().max(1.0);
    assert!(
        (t - want.0).abs().max_element() < TOLERANCE * scale,
        "{t} vs {}",
        want.0
    );
    assert!(
        (r - want.1).length().min((r + want.1).length()) < TOLERANCE,
        "{r} vs {}",
        want.1
    );
    // About 5 m forward, turned 100 degrees.
    assert!(t.length() > 3.5, "{t}");

    // The pose: the root holds its first frame's horizontal position and yaw,
    // and keeps the vertical bob and the tilt.
    let now = sample_ref(&clip, 0.5)[0];
    let first = sample_ref(&clip, 0.0)[0];
    let root = pose.joints[0];
    let want_r = y0 * yaw_ref(now.r).conjugate() * now.r;
    assert!((root.rotation.as_dquat() - want_r).length() < TOLERANCE);
    let want_t = DVec3::new(first.t.x, now.t.y, first.t.z);
    assert!((root.translation.as_dvec3() - want_t).abs().max_element() < TOLERANCE);
}

#[test]
fn root_motion_stops_at_the_end_of_a_clip_that_does_not_loop() {
    let clip = walk_clip();
    let mut p = player(&root_motion_graph(false, 2.0));
    let mut pose = Pose::default();
    let motions = steps(&mut p, 120, &mut pose);
    assert!(
        motions[100].is_identity(),
        "held at the end: {:?}",
        motions[100]
    );
    let (t, _) = integrate(&motions);
    let whole = segment_ref(&clip, 0.0, 1.0);
    let y0 = yaw_ref(sample_ref(&clip, 0.0)[0].r);
    assert!((t - y0 * whole.0).abs().max_element() < TOLERANCE);
}

#[test]
fn reversed_root_motion_walks_backwards() {
    let mut p = player(&root_motion_graph(true, -1.0));
    let mut pose = Pose::default();
    let motions = steps(&mut p, 30, &mut pose);
    let (t, _) = integrate(&motions);
    assert!(t.z < -0.5, "{t}");
}

#[test]
fn root_motion_blends_with_the_crossfade() {
    let json = graph(
        r#"{ "id": "go", "type": "bool" }"#,
        r#"{ "id": "walk", "kind": "clip", "clip": "m.glb#walk", "root_motion": true },
           { "id": "a", "kind": "clip", "clip": "m.glb#a" }"#,
        r#"{ "id": "base", "entry": "walk",
             "states": [{ "id": "walk", "node": "walk" }, { "id": "stand", "node": "a" }],
             "transitions": [{ "id": "stop", "from": "walk", "to": "stand", "duration": 0.5,
               "conditions": [{ "test": "bool", "param": "go", "value": true }] }] }"#,
        "",
    );
    let clip = walk_clip();
    let mut p = player(&json);
    let mut pose = Pose::default();
    steps(&mut p, 6, &mut pose);
    p.set("go", Param::Bool(true)).unwrap();
    p.step(DT, &mut pose);
    let y0 = yaw_ref(sample_ref(&clip, 0.0)[0].r);
    for k in 1..=10 {
        let m = p.step(DT, &mut pose);
        let a = (6 + k) as f64 * DT64;
        let full = segment_ref(&clip, a, a + DT64);
        let w = k as f64 * DT64 / 0.5;
        let want = (y0 * full.0).lerp(DVec3::ZERO, w);
        assert!(
            (m.translation.as_dvec3() - want).abs().max_element() < TOLERANCE,
            "{k}: {} vs {want}",
            m.translation
        );
    }
}

// --- Parameters and diagnostics ---------------------------------------------

#[test]
fn unknown_and_mistyped_parameters_are_refused_with_the_list() {
    let mut p = player(&two_states(""));
    let e = p.set("sped", Param::Float(1.0)).unwrap_err();
    assert_eq!(e.code, codes::GRAPH_PARAM_UNKNOWN);
    assert!(
        e.message
            .contains("`go` (bool), `jump` (trigger), `speed` (float)"),
        "{}",
        e.message
    );
    let e = p.set("go", Param::Float(1.0)).unwrap_err();
    assert!(
        e.message.contains("it is a bool, but was given a float"),
        "{}",
        e.message
    );
    assert!(p.trigger("speed").is_err());
    assert!(p.set("speed", Param::Float(f32::NAN)).is_err());
    assert!(p.get("nope").is_err());
}

fn codes_of(json: &str) -> Vec<String> {
    let graph: AnimGraph = serde_json::from_str(json).expect("parses");
    validate(&graph)
        .iter()
        .map(|d| format!("{} {}", d.code, d.message))
        .collect()
}

#[test]
fn validation_names_the_bad_element_by_path() {
    let problems = codes_of(&two_states(
        r#"{ "id": "t", "from": "idle", "to": "jumping" },
           { "id": "t", "from": "nowhere", "to": "run", "duration": -1,
             "conditions": [{ "test": "compare", "param": "go", "op": "<", "value": 1 },
                            { "test": "trigger", "param": "missing" }] }"#,
    ));
    let expected = [
        "RE7607 animation graph: `layers[0].transitions[1].id` `t` is already used in `layers[0].transitions`",
        "RE7607 animation graph: `layers[0].transitions[0].to` names state `jumping`, which the layer lacks",
        "RE7607 animation graph: `layers[0].transitions[1].from` names state `nowhere`, which the layer lacks",
        "RE7607 animation graph: `layers[0].transitions[1].duration` must be 0 or more",
        "RE7607 animation graph: `layers[0].transitions[1].conditions[0].param` names parameter `go`, a bool, where a float is needed",
        "RE7607 animation graph: `layers[0].transitions[1].conditions[1].param` names parameter `missing`, which the graph lacks",
    ];
    assert_eq!(problems, expected);
    assert!(codes_of(&two_states("")).is_empty());
}

#[test]
fn a_blend_tree_cycle_is_reported() {
    let problems = codes_of(&graph(
        r#"{ "id": "w", "type": "float" }"#,
        r#"{ "id": "a", "kind": "clip", "clip": "m.glb#a" },
           { "id": "x", "kind": "additive", "base": "a", "additive": "y" },
           { "id": "y", "kind": "blend1d", "param": "w", "children": [{ "node": "x", "threshold": 0 }] }"#,
        r#"{ "id": "base", "entry": "s", "states": [{ "id": "s", "node": "x" }] }"#,
        "",
    ));
    assert_eq!(
        problems,
        ["RE7611 the blend tree has a cycle: `x` -> `y` -> `x`"]
    );
}

#[test]
fn binding_reports_the_first_problem_and_missing_clips() {
    let e = bind(&graph(
        "",
        r#"{ "id": "a", "kind": "clip", "clip": "m.glb#zzz" }"#,
        r#"{ "id": "base", "entry": "s", "states": [{ "id": "s", "node": "a" }] }"#,
        "",
    ))
    .unwrap_err();
    assert_eq!(e.code, codes::GRAPH_CLIP_MISSING);
    assert!(
        e.message.contains("node `a` plays clip `m.glb#zzz`"),
        "{}",
        e.message
    );

    let e = bind(&graph(
        "",
        r#"{ "id": "a", "kind": "clip", "clip": "m.glb#a" }"#,
        r#"{ "id": "base", "entry": "nope", "states": [{ "id": "any", "node": "b" }] }"#,
        "",
    ))
    .unwrap_err();
    assert_eq!(e.code, codes::GRAPH_INVALID);
    assert!(e.message.contains("`layers[0].entry`"), "{}", e.message);
    assert_eq!(e.related.len(), 2, "{:?}", e.related);
}

#[test]
fn a_graph_without_layers_holds_the_rest_pose() {
    let mut p = player(&graph("", "", "", ""));
    let mut pose = Pose::default();
    assert!(p.step(DT, &mut pose).is_identity());
    assert_pose(&pose, &rest_ref(), "rest");
    assert_eq!(p.state(None), None);
}
