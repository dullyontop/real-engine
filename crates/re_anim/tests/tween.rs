//! Tweens and easing curves.

use glam::{Quat, Vec3};
use re_anim::{Easing, Tween};

#[test]
fn every_easing_starts_at_zero_and_ends_at_one() {
    for e in Easing::ALL {
        assert!(e.apply(0.0).abs() < 1e-6, "{e} at 0 = {}", e.apply(0.0));
        assert!(
            (e.apply(1.0) - 1.0).abs() < 1e-6,
            "{e} at 1 = {}",
            e.apply(1.0)
        );
        assert_eq!(Easing::from_name(e.name()).unwrap(), e);
    }
    assert_eq!(Easing::Linear.apply(0.25), 0.25);
    assert_eq!(Easing::QuadIn.apply(0.5), 0.25);
    assert_eq!(Easing::QuadOut.apply(0.5), 0.75);
    assert_eq!(Easing::QuadInOut.apply(0.5), 0.5);
    assert!(Easing::BackOut.apply(0.6) > 1.0, "back_out overshoots");
}

#[test]
fn unknown_easing_lists_the_names() {
    let err = Easing::from_name("easeOutQuad").unwrap_err();
    assert_eq!(err.code.to_string(), "RE7601");
    assert!(err.hint.unwrap().contains("quad_out"));
}

#[test]
fn tween_advances_and_finishes_exactly() {
    let mut t = Tween::new(Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0), 2.0, Easing::Linear).unwrap();
    assert_eq!(t.value(), Vec3::ZERO);
    assert_eq!(t.advance(0.5), Vec3::new(2.5, 0.0, 0.0));
    assert_eq!(t.progress(), 0.25);
    assert!(!t.is_finished());
    t.advance(10.0);
    assert!(t.is_finished());
    assert_eq!(t.value(), Vec3::new(10.0, 0.0, 0.0));
    assert_eq!(t.elapsed(), 2.0);
    t.advance(-1.0);
    assert_eq!(t.elapsed(), 2.0, "negative dt is ignored");
    t.restart();
    assert_eq!(t.value(), Vec3::ZERO);
}

#[test]
fn zero_duration_is_finished_at_once() {
    let t = Tween::new(1.0_f32, 5.0, 0.0, Easing::CubicIn).unwrap();
    assert!(t.is_finished());
    assert_eq!(t.value(), 5.0);
}

#[test]
fn invalid_duration_is_a_diagnostic() {
    for d in [-1.0, f32::NAN, f32::INFINITY] {
        let err = Tween::new(0.0_f32, 1.0, d, Easing::Linear).unwrap_err();
        assert_eq!(err.code.to_string(), "RE7602");
    }
}

#[test]
fn quaternion_tween_slerps() {
    let to = Quat::from_rotation_z(std::f32::consts::PI / 2.0);
    let t = Tween::new(Quat::IDENTITY, to, 1.0, Easing::Linear).unwrap();
    let mid = t.value_at(0.5);
    assert!(mid.angle_between(Quat::from_rotation_z(std::f32::consts::PI / 4.0)) < 1e-5);
}

#[test]
fn reversed_returns_from_the_current_value() {
    let mut t = Tween::new(0.0_f32, 10.0, 1.0, Easing::Linear).unwrap();
    t.advance(0.4);
    let back = t.reversed();
    assert_eq!(back.from(), 4.0);
    assert_eq!(back.to(), 0.0);
    assert!((back.duration() - 0.4).abs() < 1e-6);
}
