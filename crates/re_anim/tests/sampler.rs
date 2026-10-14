//! The clip sampler at keyframes and midpoints, for each interpolation mode.

use glam::{Quat, Vec3};
use re_anim::{Playhead, Sampler};
use re_types::anim::{Channel, ClipData, Interpolation, TargetChannels};

fn clip(interpolation: Interpolation) -> ClipData {
    ClipData {
        name: "move".into(),
        duration: 2.0,
        targets: vec![TargetChannels {
            target: "Box".into(),
            translation: Some(Channel {
                interpolation,
                times: vec![0.0, 1.0, 2.0],
                values: vec![
                    Vec3::ZERO,
                    Vec3::new(2.0, 0.0, 0.0),
                    Vec3::new(2.0, 4.0, 0.0),
                ],
            }),
            rotation: Some(Channel {
                interpolation,
                times: vec![0.0, 2.0],
                values: vec![
                    Quat::IDENTITY,
                    Quat::from_rotation_y(std::f32::consts::FRAC_PI_2),
                ],
            }),
            scale: None,
        }],
    }
}

fn close(a: Vec3, b: Vec3) -> bool {
    (a - b).length() < 1e-5
}

#[test]
fn linear_at_keyframes_and_midpoints() {
    let s = Sampler::new(clip(Interpolation::Linear)).unwrap();
    let t = |time: f32| s.sample_target(0, time).unwrap().translation.unwrap();
    // Keyframes.
    assert!(close(t(0.0), Vec3::ZERO));
    assert!(close(t(1.0), Vec3::new(2.0, 0.0, 0.0)));
    assert!(close(t(2.0), Vec3::new(2.0, 4.0, 0.0)));
    // Midpoints.
    assert!(close(t(0.5), Vec3::new(1.0, 0.0, 0.0)));
    assert!(close(t(1.5), Vec3::new(2.0, 2.0, 0.0)));
    // Outside the keys: held.
    assert!(close(t(-1.0), Vec3::ZERO));
    assert!(close(t(9.0), Vec3::new(2.0, 4.0, 0.0)));

    // Rotation is slerped: half way through a 90 degree turn is 45 degrees.
    let r = s.sample_target(0, 1.0).unwrap().rotation.unwrap();
    let expect = Quat::from_rotation_y(std::f32::consts::FRAC_PI_4);
    assert!(r.angle_between(expect) < 1e-5, "{r:?}");
    assert!((r.length() - 1.0).abs() < 1e-6);
    assert_eq!(s.sample_target(0, 1.0).unwrap().scale, None);
}

#[test]
fn step_holds_the_previous_key() {
    let s = Sampler::new(clip(Interpolation::Step)).unwrap();
    let t = |time: f32| s.sample_target(0, time).unwrap().translation.unwrap();
    assert!(close(t(0.0), Vec3::ZERO));
    assert!(close(t(0.99), Vec3::ZERO));
    assert!(close(t(1.0), Vec3::new(2.0, 0.0, 0.0)));
    assert!(close(t(1.5), Vec3::new(2.0, 0.0, 0.0)));
    assert!(close(t(2.0), Vec3::new(2.0, 4.0, 0.0)));
    let r = s.sample_target(0, 1.9).unwrap().rotation.unwrap();
    assert!(r.angle_between(Quat::IDENTITY) < 1e-6);
}

#[test]
fn cubic_spline_with_zero_tangents_hits_keys_and_eases() {
    let mut c = clip(Interpolation::Linear);
    let tr = c.targets[0].translation.as_mut().unwrap();
    tr.interpolation = Interpolation::CubicSpline;
    tr.values = tr
        .values
        .iter()
        .flat_map(|&v| [Vec3::ZERO, v, Vec3::ZERO])
        .collect();
    c.targets[0].rotation = None;
    let s = Sampler::new(c).unwrap();
    let t = |time: f32| s.sample_target(0, time).unwrap().translation.unwrap();
    assert!(close(t(0.0), Vec3::ZERO));
    assert!(close(t(1.0), Vec3::new(2.0, 0.0, 0.0)));
    // Zero tangents: the Hermite midpoint is the plain average (smoothstep at 0.5).
    assert!(close(t(0.5), Vec3::new(1.0, 0.0, 0.0)));
    // ...but a quarter of the way it is behind linear (ease in).
    assert!(t(0.25).x < 0.5);
}

#[test]
fn sample_into_and_names() {
    let s = Sampler::new(clip(Interpolation::Linear)).unwrap();
    let mut out = Vec::new();
    s.sample_into(0.5, &mut out);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].index, 0);
    assert_eq!(s.target_index("Box"), Some(0));
    assert_eq!(s.target_index("Lid"), None);
    assert_eq!(s.wrap_time(2.5, true), 0.5);
    assert_eq!(s.wrap_time(2.5, false), 2.0);
    assert_eq!(s.wrap_time(-0.5, true), 1.5);
}

#[test]
fn malformed_clip_is_a_diagnostic() {
    let mut c = clip(Interpolation::Linear);
    c.targets[0].translation.as_mut().unwrap().values.pop();
    let err = Sampler::new(c).unwrap_err();
    assert_eq!(err.code.to_string(), "RE7600");
    assert!(
        err.message.contains("translation channel of `Box`"),
        "{}",
        err.message
    );

    let mut c = clip(Interpolation::Linear);
    c.targets[0].translation.as_mut().unwrap().times = vec![0.0, 1.0, 1.0];
    assert!(Sampler::new(c).is_err());
}

#[test]
fn playhead_loops_and_clamps() {
    let mut p = Playhead::default();
    p.advance(2.5, 2.0);
    assert!((p.clip_time(2.0) - 0.5).abs() < 1e-6);

    let mut once = Playhead {
        looping: false,
        ..Playhead::default()
    };
    once.advance(3.0, 2.0);
    assert_eq!(once.clip_time(2.0), 2.0);
    assert!(once.is_finished(2.0));

    let mut paused = Playhead {
        playing: false,
        ..Playhead::default()
    };
    paused.advance(1.0, 2.0);
    assert_eq!(paused.time, 0.0);

    let mut backwards = Playhead {
        speed: -1.0,
        ..Playhead::default()
    };
    backwards.advance(0.5, 2.0);
    assert!((backwards.clip_time(2.0) - 1.5).abs() < 1e-6);
}

#[test]
fn non_finite_times_never_panic() {
    for interpolation in [
        Interpolation::Step,
        Interpolation::Linear,
        Interpolation::CubicSpline,
    ] {
        let mut clip = clip(interpolation);
        if interpolation == Interpolation::CubicSpline {
            // Three values per key: in-tangent, value, out-tangent.
            for t in &mut clip.targets {
                let c = t.translation.as_mut().unwrap();
                c.values = c
                    .values
                    .iter()
                    .flat_map(|&v| [Vec3::ZERO, v, Vec3::ZERO])
                    .collect();
                let r = t.rotation.as_mut().unwrap();
                r.values = r
                    .values
                    .iter()
                    .flat_map(|&q| {
                        [
                            Quat::from_xyzw(0.0, 0.0, 0.0, 0.0),
                            q,
                            Quat::from_xyzw(0.0, 0.0, 0.0, 0.0),
                        ]
                    })
                    .collect();
            }
        }
        let sampler = Sampler::new(clip).unwrap();
        // NaN holds the first key; the infinities clamp to the ends.
        let nan = sampler.sample_target(0, f32::NAN).unwrap();
        assert!(
            close(nan.translation.unwrap(), Vec3::ZERO),
            "{interpolation:?}"
        );
        assert!(nan.rotation.unwrap().is_finite(), "{interpolation:?}");
        let low = sampler.sample_target(0, f32::NEG_INFINITY).unwrap();
        assert!(
            close(low.translation.unwrap(), Vec3::ZERO),
            "{interpolation:?}"
        );
        let high = sampler.sample_target(0, f32::INFINITY).unwrap();
        assert!(
            close(high.translation.unwrap(), Vec3::new(2.0, 4.0, 0.0)),
            "{interpolation:?}"
        );
        assert_eq!(sampler.sample(f32::NAN).len(), 1);
    }
}
