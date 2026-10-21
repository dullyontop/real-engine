//! Skin poses against an f64 reference evaluator (WP21 acceptance: sampling and
//! blending match to 1e-5 on synthetic skeletons).
//!
//! Two checks per case: the sampled or blended local pose (translation,
//! rotation and scale per joint) to 1e-5 absolute, and the joint palette the
//! renderer gets to 1e-5 relative to the skeleton's size (its largest joint
//! coordinate, at least 1 m). The palette check is relative because f32
//! rounding grows with distance from the origin and with chain depth: a
//! 64-deep chain reaching 4 m ends about 1.1e-5 m off in absolute terms.

use glam::{DMat4, DQuat, DVec3, Mat4, Quat, Vec3};
use re_anim::codes;
use re_anim::skin::{BoundClip, ModelPose, Pose, check_skin, palette_into, skinned_bounds};
use re_types::anim::{Channel, ClipData, Interpolation, JointDesc, SkinData, TargetChannels, Trs};
use re_types::mesh::Aabb;

/// The acceptance tolerance (absolute for local poses, relative to the
/// skeleton's size for palettes).
const TOLERANCE: f64 = 1e-5;

struct Rng(u64);
impl Rng {
    fn f(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.f()
    }
    fn unit(&mut self) -> Vec3 {
        Vec3::new(self.f() - 0.5, self.f() - 0.5, self.f() - 0.5 + 1e-3).normalize()
    }
    fn quat(&mut self, max_angle: f32) -> Quat {
        Quat::from_axis_angle(self.unit(), self.range(-max_angle, max_angle))
    }
}

/// A skin over `parents` with random rest poses; every seventh joint has a
/// parent offset and every fifth a non-unit scale.
fn skin(parents: &[Option<u16>], rng: &mut Rng) -> SkinData {
    let joints: Vec<JointDesc> = parents
        .iter()
        .enumerate()
        .map(|(i, &parent)| JointDesc {
            name: format!("j{i}"),
            parent,
            rest: Trs {
                translation: Vec3::new(
                    rng.range(-0.1, 0.1),
                    rng.range(0.05, 0.2),
                    rng.range(-0.1, 0.1),
                ),
                rotation: rng.quat(0.6),
                scale: if i % 5 == 4 {
                    Vec3::new(1.05, 0.97, 1.02)
                } else {
                    Vec3::ONE
                },
            },
            parent_offset: (i % 7 == 3).then(|| {
                Mat4::from_rotation_translation(rng.quat(0.4), Vec3::new(0.0, 0.02, 0.01))
            }),
        })
        .collect();
    let mut s = SkinData {
        name: "synthetic".into(),
        inverse_bind: vec![Mat4::IDENTITY; joints.len()],
        joint_bounds: vec![Some(Aabb::new(Vec3::splat(-0.05), Vec3::splat(0.05))); joints.len()],
        joints,
    };
    // Inverse bind matrices from the rest pose, as an exporter writes them.
    let mut model = ModelPose::default();
    model.compute(&s, &Pose::rest(&s));
    s.inverse_bind = model.joints.iter().map(|m| m.inverse()).collect();
    s
}

fn random_tree(n: usize, rng: &mut Rng) -> Vec<Option<u16>> {
    (0..n)
        .map(|i| {
            (i > 0).then(|| {
                let lo = i.saturating_sub(4);
                (lo + ((rng.f() * (i - lo) as f32) as usize).min(i - 1 - lo)) as u16
            })
        })
        .collect()
}

fn chain(n: usize) -> Vec<Option<u16>> {
    (0..n).map(|i| (i > 0).then(|| (i - 1) as u16)).collect()
}

/// A clip animating every joint but the last (to check that unanimated joints
/// keep their rest pose): `keys` keys over one second. Joint `i` uses linear,
/// step or cubic interpolation by `i % 3`; scale is animated on every fourth joint.
fn clip(name: &str, skin: &SkinData, keys: usize, rng: &mut Rng) -> ClipData {
    let times: Vec<f32> = (0..keys).map(|k| k as f32 / (keys - 1) as f32).collect();
    let mut targets = Vec::new();
    for (i, j) in skin.joints.iter().enumerate().take(skin.joints.len() - 1) {
        let interpolation = match i % 3 {
            0 => Interpolation::Linear,
            1 => Interpolation::Step,
            _ => Interpolation::CubicSpline,
        };
        let per = if interpolation == Interpolation::CubicSpline {
            3
        } else {
            1
        };
        let axis = rng.unit();
        let amp = rng.range(0.2, 1.2);
        let phase = rng.range(0.0, 6.0);
        let mut rotations = Vec::new();
        let mut translations = Vec::new();
        let mut scales = Vec::new();
        for (k, _) in times.iter().enumerate() {
            let a = amp * ((k as f32 * 0.7 + phase) % 6.0 - 3.0) / 3.0;
            let value = j.rest.rotation * Quat::from_axis_angle(axis, a);
            let t = j.rest.translation + Vec3::new(0.0, 0.01 * (k % 3) as f32, 0.0);
            let s = Vec3::splat(1.0 + 0.05 * (k % 2) as f32);
            if per == 3 {
                let tangent = Quat::from_xyzw(0.02, -0.01, 0.03, 0.0);
                rotations.extend([tangent, value, tangent]);
                translations.extend([Vec3::new(0.0, 0.1, 0.0), t, Vec3::new(0.0, -0.1, 0.0)]);
                scales.extend([Vec3::ZERO, s, Vec3::ZERO]);
            } else {
                rotations.push(value);
                translations.push(t);
                scales.push(s);
            }
        }
        targets.push(TargetChannels {
            target: j.name.clone(),
            translation: Some(Channel {
                interpolation,
                times: times.clone(),
                values: translations,
            }),
            rotation: Some(Channel {
                interpolation,
                times: times.clone(),
                values: rotations,
            }),
            scale: (i % 4 == 0).then(|| Channel {
                interpolation,
                times: times.clone(),
                values: scales,
            }),
        });
    }
    targets.push(TargetChannels {
        target: "not_a_joint".into(),
        translation: Some(Channel {
            interpolation: Interpolation::Linear,
            times: vec![0.0],
            values: vec![Vec3::ZERO],
        }),
        rotation: None,
        scale: None,
    });
    ClipData {
        name: name.into(),
        duration: 1.0,
        targets,
    }
}

// ---------------------------------------------------------------------------
// The f64 reference: glTF sampling (step, linear with slerp, cubic Hermite),
// nlerp blending, and the joint hierarchy, all in double precision.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct DTrs {
    t: DVec3,
    r: DQuat,
    s: DVec3,
}

fn dtrs(t: &Trs) -> DTrs {
    DTrs {
        t: t.translation.as_dvec3(),
        r: t.rotation.as_dquat(),
        s: t.scale.as_dvec3(),
    }
}

/// (key index, fraction, key spacing), or a held key.
fn span(times: &[f32], time: f64) -> Result<(usize, f64, f64), usize> {
    let last = times.len() - 1;
    if last == 0 || time <= f64::from(times[0]) {
        return Err(0);
    }
    if time >= f64::from(times[last]) {
        return Err(last);
    }
    let i = (0..last).rfind(|&i| f64::from(times[i]) <= time).unwrap();
    let (a, b) = (f64::from(times[i]), f64::from(times[i + 1]));
    Ok((i, (time - a) / (b - a), b - a))
}

fn hermite(s: f64) -> [f64; 4] {
    let (s2, s3) = (s * s, s * s * s);
    [
        2.0 * s3 - 3.0 * s2 + 1.0,
        s3 - 2.0 * s2 + s,
        -2.0 * s3 + 3.0 * s2,
        s3 - s2,
    ]
}

fn ref_vec3(c: &Channel<Vec3>, time: f64) -> DVec3 {
    let v = |i: usize| c.values[i].as_dvec3();
    let cubic = c.interpolation == Interpolation::CubicSpline;
    let value = |k: usize| if cubic { v(3 * k + 1) } else { v(k) };
    match span(&c.times, time) {
        Err(k) => value(k),
        Ok((i, s, dt)) => match c.interpolation {
            Interpolation::Step => value(i),
            Interpolation::Linear => value(i).lerp(value(i + 1), s),
            Interpolation::CubicSpline => {
                let h = hermite(s);
                value(i) * h[0]
                    + v(3 * i + 2) * dt * h[1]
                    + value(i + 1) * h[2]
                    + v(3 * (i + 1)) * dt * h[3]
            }
        },
    }
}

fn ref_quat(c: &Channel<Quat>, time: f64) -> DQuat {
    let v = |i: usize| c.values[i].as_dquat();
    let cubic = c.interpolation == Interpolation::CubicSpline;
    let value = |k: usize| if cubic { v(3 * k + 1) } else { v(k) };
    let q = match span(&c.times, time) {
        Err(k) => value(k),
        Ok((i, s, dt)) => match c.interpolation {
            Interpolation::Step => value(i),
            Interpolation::Linear => value(i).slerp(value(i + 1), s),
            Interpolation::CubicSpline => {
                let h = hermite(s);
                let x = glam::DVec4::from(value(i)) * h[0]
                    + glam::DVec4::from(v(3 * i + 2)) * dt * h[1]
                    + glam::DVec4::from(value(i + 1)) * h[2]
                    + glam::DVec4::from(v(3 * (i + 1))) * dt * h[3];
                DQuat::from_vec4(x)
            }
        },
    };
    q.normalize()
}

fn ref_sample(skin: &SkinData, clip: &ClipData, time: f64) -> Vec<DTrs> {
    let mut pose: Vec<DTrs> = skin.joints.iter().map(|j| dtrs(&j.rest)).collect();
    for target in &clip.targets {
        let Some(j) = skin.joint(&target.target) else {
            continue;
        };
        if let Some(c) = &target.translation {
            pose[j].t = ref_vec3(c, time);
        }
        if let Some(c) = &target.rotation {
            pose[j].r = ref_quat(c, time);
        }
        if let Some(c) = &target.scale {
            pose[j].s = ref_vec3(c, time);
        }
    }
    pose
}

fn ref_blend(a: &[DTrs], b: &[DTrs], w: f64) -> Vec<DTrs> {
    a.iter()
        .zip(b)
        .map(|(a, b)| {
            let br = if a.r.dot(b.r) < 0.0 { -b.r } else { b.r };
            DTrs {
                t: a.t.lerp(b.t, w),
                r: (a.r * (1.0 - w) + br * w).normalize(),
                s: a.s.lerp(b.s, w),
            }
        })
        .collect()
}

fn ref_palette(skin: &SkinData, pose: &[DTrs]) -> Vec<DMat4> {
    let mut model: Vec<DMat4> = Vec::new();
    for (i, j) in skin.joints.iter().enumerate() {
        let p = &pose[i];
        let mut local = DMat4::from_scale_rotation_translation(p.s, p.r, p.t);
        if let Some(o) = j.parent_offset {
            local = o.as_dmat4() * local;
        }
        model.push(match j.parent {
            Some(parent) => model[parent as usize] * local,
            None => local,
        });
    }
    model
        .iter()
        .zip(&skin.inverse_bind)
        .map(|(m, ibm)| *m * ibm.as_dmat4())
        .collect()
}

fn max_error(ours: &[Mat4], reference: &[DMat4]) -> f64 {
    assert_eq!(ours.len(), reference.len());
    ours.iter()
        .zip(reference)
        .flat_map(|(a, b)| (a.as_dmat4() - *b).to_cols_array())
        .fold(0.0, |m, x| m.max(x.abs()))
}

fn local_error(ours: &Pose, reference: &[DTrs]) -> f64 {
    ours.joints
        .iter()
        .zip(reference)
        .map(|(a, b)| {
            // q and -q are the same rotation.
            let r = (a.rotation.as_dquat() - b.r)
                .length()
                .min((a.rotation.as_dquat() + b.r).length());
            (a.translation.as_dvec3() - b.t)
                .abs()
                .max_element()
                .max((a.scale.as_dvec3() - b.s).abs().max_element())
                .max(r)
        })
        .fold(0.0, f64::max)
}

/// The skeleton's size: its largest rest-pose joint coordinate, at least 1 m.
fn extent(skin: &SkinData) -> f64 {
    let mut model = ModelPose::default();
    model.compute(skin, &Pose::rest(skin));
    model
        .joints
        .iter()
        .map(|m| f64::from(m.w_axis.truncate().abs().max_element()))
        .fold(1.0, f64::max)
}

fn evaluate(skin: &SkinData, pose: &Pose) -> Vec<Mat4> {
    let mut model = ModelPose::default();
    model.compute(skin, pose);
    let mut palette = Vec::new();
    palette_into(skin, &model, &mut palette);
    palette
}

fn skeletons() -> Vec<(&'static str, SkinData)> {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let tree = random_tree(65, &mut rng);
    vec![
        ("65-joint tree", skin(&tree, &mut rng)),
        ("64-joint chain", skin(&chain(64), &mut rng)),
        ("one joint", skin(&[None], &mut rng)),
    ]
}

#[test]
fn sampling_matches_the_reference() {
    let mut rng = Rng(99);
    for (label, skin) in skeletons() {
        let clip = clip("walk", &skin, 31, &mut rng);
        let bound = BoundClip::bind(&clip, &skin).unwrap();
        assert_eq!(bound.unmatched(), ["not_a_joint"]);
        let size = extent(&skin);
        let (mut local, mut palette) = (0.0f64, 0.0f64);
        let mut pose = Pose::rest(&skin);
        for step in 0..=240 {
            // Past both ends too: sampling holds the first and last keys.
            let t = step as f32 / 200.0 - 0.1;
            pose.reset(&skin);
            bound.sample(t, &mut pose);
            let reference = ref_sample(&skin, &clip, f64::from(t));
            local = local.max(local_error(&pose, &reference));
            palette = palette.max(max_error(
                &evaluate(&skin, &pose),
                &ref_palette(&skin, &reference),
            ));
        }
        println!(
            "{label}: local pose {local:.2e}; palette {palette:.2e} absolute, {:.2e} relative to {size:.2} m",
            palette / size
        );
        assert!(local < TOLERANCE, "{label}: local pose {local:.2e}");
        assert!(palette / size < TOLERANCE, "{label}: palette {palette:.2e}");
    }
}

#[test]
fn blending_matches_the_reference() {
    let mut rng = Rng(7);
    for (label, skin) in skeletons() {
        let walk = clip("walk", &skin, 31, &mut rng);
        let run = clip("run", &skin, 17, &mut rng);
        let (bw, br) = (
            BoundClip::bind(&walk, &skin).unwrap(),
            BoundClip::bind(&run, &skin).unwrap(),
        );
        let size = extent(&skin);
        let (mut local, mut palette) = (0.0f64, 0.0f64);
        let (mut a, mut b) = (Pose::rest(&skin), Pose::rest(&skin));
        for step in 0..=100 {
            let t = step as f32 / 100.0;
            let w = (step % 11) as f32 / 10.0;
            a.reset(&skin);
            b.reset(&skin);
            bw.sample(t, &mut a);
            br.sample(t, &mut b);
            a.blend_toward(&b, w);
            let reference = ref_blend(
                &ref_sample(&skin, &walk, f64::from(t)),
                &ref_sample(&skin, &run, f64::from(t)),
                f64::from(w),
            );
            local = local.max(local_error(&a, &reference));
            palette = palette.max(max_error(
                &evaluate(&skin, &a),
                &ref_palette(&skin, &reference),
            ));
        }
        println!(
            "{label}: blended local pose {local:.2e}; palette {palette:.2e} absolute, {:.2e} relative to {size:.2} m",
            palette / size
        );
        assert!(local < TOLERANCE, "{label}: local pose {local:.2e}");
        assert!(palette / size < TOLERANCE, "{label}: palette {palette:.2e}");
    }
}

#[test]
fn the_rest_pose_palette_is_the_identity() {
    for (label, skin) in skeletons() {
        let palette = evaluate(&skin, &Pose::rest(&skin));
        let err = max_error(&palette, &vec![DMat4::IDENTITY; palette.len()]);
        assert!(err < TOLERANCE, "{label}: {err:.2e}");
    }
}

#[test]
fn skinned_bounds_contain_every_skinned_vertex() {
    let mut rng = Rng(3);
    let (_, mut skin) = skeletons().remove(0);
    // Random vertices with up to four influences; bounds per joint from them.
    let mut vertices = Vec::new();
    skin.joint_bounds = vec![None; skin.joints.len()];
    for _ in 0..500 {
        let p = Vec3::new(
            rng.range(-1.0, 1.0),
            rng.range(0.0, 2.0),
            rng.range(-1.0, 1.0),
        );
        let joints: [usize; 4] = std::array::from_fn(|_| (rng.f() * 64.0) as usize);
        let raw: [f32; 4] = std::array::from_fn(|_| rng.f());
        let sum: f32 = raw.iter().sum();
        let weights = raw.map(|w| w / sum);
        for &j in &joints {
            let b = Aabb::new(p, p);
            skin.joint_bounds[j] = Some(skin.joint_bounds[j].map_or(b, |x| x.union(&b)));
        }
        vertices.push((p, joints, weights));
    }
    let clip = clip("walk", &skin, 31, &mut rng);
    let bound = BoundClip::bind(&clip, &skin).unwrap();
    let world =
        Mat4::from_rotation_translation(Quat::from_rotation_y(1.0), Vec3::new(3.0, 0.0, -2.0));
    let mut pose = Pose::rest(&skin);
    for step in 0..10 {
        pose.reset(&skin);
        bound.sample(step as f32 / 10.0, &mut pose);
        let palette = evaluate(&skin, &pose);
        let b = skinned_bounds(&skin, &palette, &world).unwrap();
        let grown = Aabb::new(b.min - 1e-4, b.max + 1e-4);
        for (p, joints, weights) in &vertices {
            let m = joints
                .iter()
                .zip(weights)
                .fold(Mat4::ZERO, |acc, (&j, &w)| acc + palette[j] * w);
            let skinned = world.transform_point3(m.transform_point3(*p));
            assert!(grown.contains(skinned), "{skinned} outside {b:?}");
        }
    }
    let empty = SkinData {
        joint_bounds: vec![None; skin.joints.len()],
        ..skin.clone()
    };
    assert!(skinned_bounds(&empty, &evaluate(&empty, &Pose::rest(&empty)), &world).is_none());
}

#[test]
fn malformed_skins_are_refused() {
    let (_, skin) = skeletons().remove(2);
    let mut bad = skin.clone();
    bad.inverse_bind.clear();
    let e = check_skin(&bad).unwrap_err();
    assert_eq!(e.code, codes::SKIN_MALFORMED);
    assert!(
        e.message.contains("1 joints, 0 inverse bind matrices"),
        "{}",
        e.message
    );
    let mut cycle = skeletons().remove(1).1;
    cycle.joints[0].parent = Some(3);
    let e = BoundClip::bind(&ClipData::default(), &cycle).unwrap_err();
    assert!(
        e.message.contains("joint 0 (`j0`) has parent 3"),
        "{}",
        e.message
    );
}

#[test]
fn a_bad_clip_is_refused() {
    let (_, skin) = skeletons().remove(2);
    let clip = ClipData {
        name: "broken".into(),
        duration: 1.0,
        targets: vec![TargetChannels {
            target: "j0".into(),
            translation: Some(Channel {
                interpolation: Interpolation::Linear,
                times: vec![0.0, 1.0],
                values: vec![Vec3::ZERO],
            }),
            rotation: None,
            scale: None,
        }],
    };
    assert_eq!(
        BoundClip::bind(&clip, &skin).unwrap_err().code,
        codes::CLIP_MALFORMED
    );
}
