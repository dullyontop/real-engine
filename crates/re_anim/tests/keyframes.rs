//! Keyframe clips, tracks and the priority mixer.

use std::collections::BTreeMap;
use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};
use std::sync::Arc;

use glam::{Quat, Vec3};
use re_anim::{
    DEFAULT_FADE_TIME, JointKey, Keyframe, KeyframeClip, Marker, Mixer, PoseDirection, PoseEasing,
    PoseStyle, Priority, Track, TrackEvent,
};

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

fn same_rot(a: Quat, b: Quat) -> bool {
    a.angle_between(b) < 1e-3
}

fn key(x: f32, easing: PoseEasing) -> JointKey {
    JointKey {
        translation: Vec3::new(x, 0.0, 0.0),
        easing,
        ..JointKey::default()
    }
}

fn kf(time: f32, name: &str, joints: &[(&str, JointKey)]) -> Keyframe {
    Keyframe {
        time,
        name: name.to_string(),
        joints: joints.iter().map(|(j, k)| ((*j).to_string(), *k)).collect(),
    }
}

fn marker(time: f32, name: &str, value: &str) -> Marker {
    Marker {
        time,
        name: name.to_string(),
        value: value.to_string(),
    }
}

fn kr(name: &str) -> TrackEvent {
    TrackEvent::KeyframeReached(name.to_string())
}

fn mr(name: &str, value: &str) -> TrackEvent {
    TrackEvent::MarkerReached {
        name: name.to_string(),
        value: value.to_string(),
    }
}

/// Named keyframes at 0, 1 and 2, an unnamed one at 0.5, a marker at 1.5.
fn event_clip(looped: bool) -> Arc<KeyframeClip> {
    let k = key(0.0, PoseEasing::LINEAR);
    Arc::new(
        KeyframeClip::new(
            vec![
                kf(0.0, "Start", &[("Arm", k)]),
                kf(0.5, "", &[("Arm", k)]),
                kf(1.0, "Mid", &[("Arm", k)]),
                kf(2.0, "End", &[("Arm", k)]),
            ],
            vec![marker(1.5, "Hit", "42")],
            looped,
            Priority::Action,
        )
        .unwrap(),
    )
}

/// One joint at a constant translation `x` (two keys, 0 and 1 s).
fn hold_clip(joints: &[(&str, f32)], priority: Priority) -> Arc<KeyframeClip> {
    let keys: Vec<(&str, JointKey)> = joints
        .iter()
        .map(|(j, x)| (*j, key(*x, PoseEasing::LINEAR)))
        .collect();
    Arc::new(
        KeyframeClip::new(
            vec![kf(0.0, "", &keys), kf(1.0, "", &keys)],
            vec![],
            true,
            priority,
        )
        .unwrap(),
    )
}

#[test]
fn easing_endpoints_and_midpoints() {
    for style in PoseStyle::ALL {
        for direction in PoseDirection::ALL {
            let e = PoseEasing::new(style, direction);
            assert!(close(e.apply(0.0), 0.0), "{style} {direction} at 0");
            assert!(close(e.apply(1.0), 1.0), "{style} {direction} at 1");
            assert_eq!(PoseStyle::from_name(style.as_str()).unwrap(), style);
            assert_eq!(
                PoseDirection::from_name(direction.as_str()).unwrap(),
                direction
            );
        }
    }
    use PoseDirection::*;
    use PoseStyle::*;
    let e = |s, d, t| PoseEasing::new(s, d).apply(t);
    assert_eq!(e(Linear, In, 0.25), 0.25);
    assert_eq!(e(Constant, In, 0.99), 0.0);
    assert_eq!(e(Constant, Out, 0.5), 0.0);
    assert_eq!(e(Constant, In, 1.0), 1.0);
    // CubicV2: usual directions.
    assert!(close(e(CubicV2, In, 0.5), 0.125));
    assert!(close(e(CubicV2, Out, 0.5), 0.875));
    assert!(close(e(CubicV2, InOut, 0.25), 0.0625));
    assert!(close(e(CubicV2, InOut, 0.5), 0.5));
    assert!(close(e(CubicV2, InOut, 0.75), 0.9375));
    // Legacy Cubic: In and Out swapped.
    assert!(close(e(Cubic, In, 0.5), 0.875));
    assert!(close(e(Cubic, Out, 0.5), 0.125));
    // Legacy bounce `Out` starts slow: 1 - bounce_out(0.5) = 1 - 0.765625.
    assert!(close(e(Bounce, Out, 0.5), 0.234375));
    assert!(close(e(Bounce, In, 0.5), 0.765625));
    // Elastic overshoots somewhere.
    assert!((1..100).any(|i| e(Elastic, In, i as f32 / 100.0) > 1.0));
    // NaN counts as 0.
    assert_eq!(e(Linear, In, f32::NAN), 0.0);
}

#[test]
fn unknown_names_are_reported() {
    let err = PoseStyle::from_name("cubic").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7603");
    assert!(err.hint.unwrap().contains("CubicV2"));
    let err = PoseDirection::from_name("Both").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7603");
    let err = Priority::from_name("action").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7605");
    assert!(err.hint.unwrap().contains("Action4"));
}

#[test]
fn priorities_are_ordered_low_to_high() {
    for w in Priority::ALL.windows(2) {
        assert!(w[0] < w[1]);
    }
    assert_eq!(Priority::from_name("Movement").unwrap(), Priority::Movement);
    assert_eq!(Priority::Action2.as_str(), "Action2");
    assert_eq!(Priority::default(), Priority::Core);
}

#[test]
fn sampling_linear_between_keys() {
    let k0 = JointKey {
        translation: Vec3::ZERO,
        rotation: Quat::IDENTITY,
        weight: 1.0,
        easing: PoseEasing::LINEAR,
    };
    let k1 = JointKey {
        translation: Vec3::new(4.0, 0.0, 0.0),
        rotation: Quat::from_rotation_y(FRAC_PI_2),
        weight: 0.5,
        easing: PoseEasing::LINEAR,
    };
    let clip = KeyframeClip::new(
        vec![kf(1.0, "", &[("Arm", k0)]), kf(3.0, "", &[("Arm", k1)])],
        vec![],
        false,
        Priority::Core,
    )
    .unwrap();
    assert_eq!(clip.length(), 3.0);
    let mut out = BTreeMap::new();
    clip.sample(1.5, &mut out);
    let (t, r, w) = out["Arm"];
    assert!(close(t.x, 1.0));
    assert!(same_rot(r, Quat::from_rotation_y(FRAC_PI_2 / 4.0)));
    assert!(close(w, 0.875));
    clip.sample(2.0, &mut out);
    assert!(same_rot(out["Arm"].1, Quat::from_rotation_y(FRAC_PI_4)));
    // Before the first key holds it; after the last holds the last.
    clip.sample(0.0, &mut out);
    assert_eq!(out["Arm"].0, Vec3::ZERO);
    clip.sample(10.0, &mut out);
    assert_eq!(out["Arm"].0.x, 4.0);
}

#[test]
fn sampling_constant_holds_then_jumps() {
    let constant = PoseEasing::new(PoseStyle::Constant, PoseDirection::In);
    let clip = KeyframeClip::new(
        vec![
            kf(0.0, "", &[("Arm", key(0.0, constant))]),
            kf(2.0, "", &[("Arm", key(4.0, constant))]),
        ],
        vec![],
        false,
        Priority::Core,
    )
    .unwrap();
    let mut out = BTreeMap::new();
    clip.sample(1.99, &mut out);
    assert_eq!(out["Arm"].0.x, 0.0);
    clip.sample(2.0, &mut out);
    assert_eq!(out["Arm"].0.x, 4.0);
}

#[test]
fn joints_missing_from_a_keyframe_interpolate_across_it() {
    let l = PoseEasing::LINEAR;
    let clip = KeyframeClip::new(
        vec![
            kf(0.0, "", &[("Arm", key(0.0, l)), ("Leg", key(0.0, l))]),
            kf(1.0, "", &[("Leg", key(9.0, l))]),
            kf(2.0, "", &[("Arm", key(2.0, l))]),
        ],
        vec![],
        false,
        Priority::Core,
    )
    .unwrap();
    let mut out = BTreeMap::new();
    clip.sample(1.0, &mut out);
    assert!(close(out["Arm"].0.x, 1.0));
    assert_eq!(out["Leg"].0.x, 9.0);
    clip.sample(1.5, &mut out);
    assert_eq!(out["Leg"].0.x, 9.0, "after its last key Leg holds");
}

#[test]
fn the_earlier_keys_easing_shapes_the_segment() {
    let v2_in = PoseEasing::new(PoseStyle::CubicV2, PoseDirection::In);
    let clip = KeyframeClip::new(
        vec![
            kf(0.0, "", &[("Arm", key(0.0, v2_in))]),
            kf(1.0, "", &[("Arm", key(8.0, PoseEasing::LINEAR))]),
        ],
        vec![],
        false,
        Priority::Core,
    )
    .unwrap();
    assert!(close(clip.sample_joint("Arm", 0.5).unwrap().0.x, 1.0));
}

#[test]
fn validation_errors() {
    let k = JointKey::default();
    let bad = |kfs: Vec<Keyframe>, markers: Vec<Marker>| {
        let err = KeyframeClip::new(kfs, markers, false, Priority::Core).unwrap_err();
        assert_eq!(err.code.to_string(), "RE7604");
    };
    bad(vec![kf(-1.0, "a", &[("Arm", k)])], vec![]);
    bad(vec![kf(f32::NAN, "a", &[("Arm", k)])], vec![]);
    bad(vec![kf(f32::INFINITY, "a", &[("Arm", k)])], vec![]);
    bad(vec![], vec![marker(-0.5, "m", "")]);
    let zero_rot = JointKey {
        rotation: Quat::from_xyzw(0.0, 0.0, 0.0, 0.0),
        ..k
    };
    bad(vec![kf(0.0, "", &[("Arm", zero_rot)])], vec![]);
    let nan_weight = JointKey {
        weight: f32::NAN,
        ..k
    };
    bad(vec![kf(0.0, "", &[("Arm", nan_weight)])], vec![]);
    let nan_pos = JointKey {
        translation: Vec3::NAN,
        ..k
    };
    bad(vec![kf(0.0, "", &[("Arm", nan_pos)])], vec![]);

    // Unsorted input is sorted; rotations are normalised.
    let scaled = JointKey {
        rotation: Quat::from_xyzw(0.0, 0.0, 0.0, 2.0),
        ..k
    };
    let clip = KeyframeClip::new(
        vec![
            kf(2.0, "b", &[("Arm", scaled)]),
            kf(1.0, "a", &[("Arm", k)]),
        ],
        vec![],
        false,
        Priority::Core,
    )
    .unwrap();
    assert_eq!(clip.keyframes()[0].name, "a");
    assert_eq!(clip.length(), 2.0);
    assert!(close(
        clip.keyframes()[1].joints["Arm"].rotation.length(),
        1.0
    ));
    // An empty clip is valid and has length 0.
    let empty = KeyframeClip::new(vec![], vec![], false, Priority::Core).unwrap();
    assert_eq!(empty.length(), 0.0);
}

#[test]
fn events_fire_as_times_are_crossed_then_ended_then_stopped() {
    let mut track = Track::new(event_clip(false));
    assert!(!track.looped());
    assert_eq!(track.priority(), Priority::Action);
    track.play(0.0, 1.0, 1.0);
    assert!(track.is_playing());
    assert_eq!(track.advance(0.25), vec![kr("Start")]);
    assert_eq!(track.advance(1.0), vec![kr("Mid")]);
    assert_eq!(
        track.advance(1.0),
        vec![mr("Hit", "42"), kr("End"), TrackEvent::Ended]
    );
    assert_eq!(track.time_position(), 2.0);
    assert!(!track.is_playing());
    assert_eq!(track.weight_target(), 0.0);
    // Holds the last pose and fades out over the default 0.1 s.
    assert_eq!(track.advance(DEFAULT_FADE_TIME / 2.0), vec![]);
    assert!(close(track.weight_current(), 0.5));
    assert_eq!(track.time_position(), 2.0);
    assert_eq!(
        track.advance(DEFAULT_FADE_TIME / 2.0),
        vec![TrackEvent::Stopped]
    );
    assert!(track.is_finished());
    assert_eq!(track.advance(1.0), vec![]);
}

#[test]
fn fixed_steps_fade_out_in_six_frames() {
    let mut track = Track::new(event_clip(false));
    track.play(0.0, 1.0, 1.0);
    track.set_time_position(2.0);
    let mut steps = 0;
    let mut events = Vec::new();
    while !track.is_finished() && steps < 100 {
        events.extend(track.advance(1.0 / 60.0));
        steps += 1;
    }
    assert_eq!(
        events,
        vec![kr("End"), TrackEvent::Ended, TrackEvent::Stopped]
    );
    assert_eq!(steps, 7, "one step to end, six to fade");
}

#[test]
fn looping_wraps_with_events_on_both_sides() {
    let mut track = Track::new(event_clip(true));
    assert!(track.looped());
    track.play(0.0, 1.0, 1.0);
    assert_eq!(
        track.advance(1.75),
        vec![kr("Start"), kr("Mid"), mr("Hit", "42")]
    );
    assert_eq!(
        track.advance(0.5),
        vec![kr("End"), TrackEvent::DidLoop, kr("Start")]
    );
    assert!(close(track.time_position(), 0.25));
    assert!(track.is_playing());
    // Per-track override.
    track.set_looped(false);
    assert_eq!(
        track.advance(2.0),
        vec![kr("Mid"), mr("Hit", "42"), kr("End"), TrackEvent::Ended]
    );
}

#[test]
fn many_loops_in_one_step_terminate() {
    let mut track = Track::new(event_clip(true));
    track.play(0.0, 1.0, 1.0);
    let events = track.advance(1.0e6);
    assert!(events.iter().filter(|e| **e == TrackEvent::DidLoop).count() >= 64);
    assert!((0.0..=2.0).contains(&track.time_position()));
}

#[test]
fn zero_speed_holds_and_negative_speed_runs_backward() {
    let mut track = Track::new(event_clip(false));
    track.play(0.0, 1.0, 0.0);
    assert_eq!(track.advance(1.0), vec![]);
    assert_eq!(track.time_position(), 0.0);
    // The start event is still pending once the track moves.
    track.adjust_speed(1.0);
    assert_eq!(track.advance(0.1), vec![kr("Start")]);

    let mut back = Track::new(event_clip(false));
    back.set_time_position(2.0);
    back.play(0.0, 1.0, -1.0);
    assert_eq!(back.speed(), -1.0);
    assert_eq!(back.advance(0.25), vec![kr("End")]);
    assert!(close(back.time_position(), 1.75));
    assert_eq!(back.advance(1.0), vec![mr("Hit", "42"), kr("Mid")]);
    assert_eq!(back.advance(1.0), vec![kr("Start"), TrackEvent::Ended]);
    assert_eq!(back.time_position(), 0.0);

    let mut looped_back = Track::new(event_clip(true));
    looped_back.set_time_position(0.5);
    looped_back.play(0.0, 1.0, -2.0);
    assert_eq!(
        looped_back.advance(0.5),
        vec![kr("Start"), TrackEvent::DidLoop, kr("End"), mr("Hit", "42")]
    );
    assert!(close(looped_back.time_position(), 1.5));
}

#[test]
fn replaying_an_ended_track_restarts_it() {
    let mut track = Track::new(event_clip(false));
    track.play(0.0, 1.0, 1.0);
    track.advance(5.0);
    track.play(0.0, 1.0, 1.0);
    assert_eq!(track.time_position(), 0.0);
    assert!(track.is_playing());
    assert_eq!(track.advance(0.1), vec![kr("Start")]);
}

#[test]
fn weights_fade_linearly() {
    let mut track = Track::new(event_clip(true));
    assert_eq!(track.weight_current(), 0.0);
    track.play(0.2, 1.0, 1.0);
    assert_eq!(track.weight_target(), 1.0);
    track.advance(0.1);
    assert!(close(track.weight_current(), 0.5));
    track.advance(0.1);
    assert!(close(track.weight_current(), 1.0));
    track.adjust_weight(0.5, 0.2);
    track.advance(0.1);
    assert!(close(track.weight_current(), 0.75));
    track.advance(0.5);
    assert!(close(track.weight_current(), 0.5));
    track.stop(0.5);
    assert!(!track.is_playing());
    track.advance(0.25);
    assert!(close(track.weight_current(), 0.25));
    assert_eq!(track.advance(0.25), vec![TrackEvent::Stopped]);
    // Instant stop.
    let mut t2 = Track::new(event_clip(true));
    t2.play(0.0, 1.0, 1.0);
    t2.stop(0.0);
    assert_eq!(t2.weight_current(), 0.0);
    assert_eq!(t2.advance(0.0), vec![TrackEvent::Stopped]);
}

#[test]
fn higher_priority_overrides_on_its_joints_only() {
    let mut mixer = Mixer::new();
    let mut idle = Track::new(hold_clip(&[("Arm", 1.0), ("Leg", 1.0)], Priority::Idle));
    idle.play(0.0, 1.0, 1.0);
    let mut action = Track::new(hold_clip(&[("Arm", 5.0)], Priority::Core));
    action.set_priority(Priority::Action);
    action.play(0.2, 1.0, 1.0);
    mixer.add(idle);
    let action_id = mixer.add(action);

    let mut out = BTreeMap::new();
    mixer.evaluate(&mut out);
    assert_eq!(out["Arm"].0.x, 1.0, "action has weight 0 so far");
    mixer.advance(0.1);
    mixer.evaluate(&mut out);
    assert!(close(out["Arm"].0.x, 3.0), "half-faded action blends");
    assert!(close(out["Leg"].0.x, 1.0));
    mixer.advance(0.1);
    mixer.evaluate(&mut out);
    assert!(close(out["Arm"].0.x, 5.0), "full action overrides idle");
    assert!(close(out["Leg"].0.x, 1.0), "idle still poses the leg");

    mixer.get_mut(action_id).unwrap().stop(0.0);
    let events = mixer.advance(0.0);
    assert_eq!(events, vec![(action_id, TrackEvent::Stopped)]);
    assert!(mixer.get(action_id).is_none());
    assert_eq!(mixer.len(), 1);
    mixer.evaluate(&mut out);
    assert!(close(out["Arm"].0.x, 1.0));
}

#[test]
fn same_priority_tracks_blend_by_weight() {
    let rot_clip = |angle: f32, x: f32| {
        let k = JointKey {
            translation: Vec3::new(x, 0.0, 0.0),
            rotation: Quat::from_rotation_y(angle),
            ..JointKey::default()
        };
        Arc::new(
            KeyframeClip::new(
                vec![kf(0.0, "", &[("Arm", k)])],
                vec![],
                true,
                Priority::Idle,
            )
            .unwrap(),
        )
    };
    let mut mixer = Mixer::new();
    let mut a = Track::new(rot_clip(0.0, 2.0));
    a.play(0.0, 1.0, 1.0);
    let mut b = Track::new(rot_clip(FRAC_PI_2, 4.0));
    b.play(0.0, 1.0, 1.0);
    mixer.add(a);
    let b_id = mixer.add(b);
    let mut out = BTreeMap::new();
    mixer.evaluate(&mut out);
    assert!(close(out["Arm"].0.x, 3.0));
    assert!(same_rot(out["Arm"].1, Quat::from_rotation_y(FRAC_PI_4)));

    // A lone half-weight track blends halfway from the rest pose.
    mixer.remove(b_id);
    mixer
        .get_mut(re_anim::TrackId(1))
        .unwrap()
        .adjust_weight(0.5, 0.0);
    mixer.evaluate(&mut out);
    assert!(close(out["Arm"].0.x, 1.0));
}

#[test]
fn key_weights_scale_the_track_weight() {
    let k = JointKey {
        translation: Vec3::new(4.0, 0.0, 0.0),
        weight: 0.25,
        ..JointKey::default()
    };
    let clip = Arc::new(
        KeyframeClip::new(
            vec![kf(0.0, "", &[("Arm", k)])],
            vec![],
            true,
            Priority::Idle,
        )
        .unwrap(),
    );
    let mut mixer = Mixer::new();
    let mut t = Track::new(clip);
    t.play(0.0, 1.0, 1.0);
    mixer.add(t);
    let mut out = BTreeMap::new();
    mixer.evaluate(&mut out);
    assert!(close(out["Arm"].0.x, 1.0));
}

#[test]
fn playing_lists_tracks_in_start_order() {
    let mut mixer = Mixer::new();
    let a = mixer.add(Track::new(event_clip(true)));
    let b = mixer.add(Track::new(event_clip(true)));
    let c = mixer.add(Track::new(event_clip(true)));
    assert_eq!(mixer.playing().count(), 0);
    mixer.get_mut(b).unwrap().play_default();
    mixer.get_mut(a).unwrap().play_default();
    assert_eq!(mixer.playing().collect::<Vec<_>>(), vec![b, a]);
    mixer.get_mut(b).unwrap().stop(1.0);
    mixer.get_mut(c).unwrap().play_default();
    assert_eq!(mixer.playing().collect::<Vec<_>>(), vec![a, c]);
}
