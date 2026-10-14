//! Tweens: a value moving from one end to another over time, with easing.

use std::fmt;
use std::str::FromStr;

use glam::{Quat, Vec2, Vec3, Vec4};
use re_core::Diagnostic;

use crate::codes;

/// An easing curve: maps progress `t` in `[0, 1]` to an eased fraction.
///
/// Every curve gives 0 at `t = 0` and 1 at `t = 1`. `BackOut` and `ElasticOut`
/// overshoot past 1 on the way. Names (for scripts and files) are snake case:
/// `linear`, `quad_in_out`, `back_out` and so on ([`Easing::name`],
/// [`Easing::from_name`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Easing {
    /// Constant speed.
    #[default]
    Linear,
    /// Starts slow (t²).
    QuadIn,
    /// Ends slow.
    QuadOut,
    /// Slow at both ends.
    QuadInOut,
    /// Starts slow (t³).
    CubicIn,
    /// Ends slow.
    CubicOut,
    /// Slow at both ends.
    CubicInOut,
    /// Starts slow, sine-shaped.
    SineIn,
    /// Ends slow, sine-shaped.
    SineOut,
    /// Slow at both ends, sine-shaped.
    SineInOut,
    /// Starts very slow (exponential).
    ExpoIn,
    /// Ends very slow (exponential).
    ExpoOut,
    /// Overshoots the end a little, then settles.
    BackOut,
    /// Overshoots and wobbles around the end like a spring.
    ElasticOut,
    /// Bounces against the end like a dropped ball.
    BounceOut,
}

impl Easing {
    /// Every easing, in declaration order.
    pub const ALL: [Easing; 15] = [
        Easing::Linear,
        Easing::QuadIn,
        Easing::QuadOut,
        Easing::QuadInOut,
        Easing::CubicIn,
        Easing::CubicOut,
        Easing::CubicInOut,
        Easing::SineIn,
        Easing::SineOut,
        Easing::SineInOut,
        Easing::ExpoIn,
        Easing::ExpoOut,
        Easing::BackOut,
        Easing::ElasticOut,
        Easing::BounceOut,
    ];

    /// The snake-case name, for example `quad_in_out`.
    pub const fn name(self) -> &'static str {
        match self {
            Easing::Linear => "linear",
            Easing::QuadIn => "quad_in",
            Easing::QuadOut => "quad_out",
            Easing::QuadInOut => "quad_in_out",
            Easing::CubicIn => "cubic_in",
            Easing::CubicOut => "cubic_out",
            Easing::CubicInOut => "cubic_in_out",
            Easing::SineIn => "sine_in",
            Easing::SineOut => "sine_out",
            Easing::SineInOut => "sine_in_out",
            Easing::ExpoIn => "expo_in",
            Easing::ExpoOut => "expo_out",
            Easing::BackOut => "back_out",
            Easing::ElasticOut => "elastic_out",
            Easing::BounceOut => "bounce_out",
        }
    }

    /// Parses a snake-case name.
    ///
    /// # Errors
    ///
    /// [`codes::UNKNOWN_EASING`], listing the valid names.
    pub fn from_name(name: &str) -> Result<Easing, Diagnostic> {
        Easing::ALL
            .into_iter()
            .find(|e| e.name() == name)
            .ok_or_else(|| {
                let names: Vec<&str> = Easing::ALL.iter().map(|e| e.name()).collect();
                Diagnostic::error(codes::UNKNOWN_EASING, format!("unknown easing `{name}`"))
                    .with_hint(format!("use one of: {}", names.join(", ")))
            })
    }

    /// The eased fraction at progress `t` (clamped to `[0, 1]`).
    pub fn apply(self, t: f32) -> f32 {
        use std::f32::consts::{FRAC_PI_2, PI, TAU};
        let t = if t.is_nan() { 0.0 } else { t.clamp(0.0, 1.0) };
        match self {
            Easing::Linear => t,
            Easing::QuadIn => t * t,
            Easing::QuadOut => 1.0 - (1.0 - t) * (1.0 - t),
            Easing::QuadInOut => {
                if t < 0.5 {
                    2.0 * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
                }
            }
            Easing::CubicIn => t * t * t,
            Easing::CubicOut => 1.0 - (1.0 - t).powi(3),
            Easing::CubicInOut => {
                if t < 0.5 {
                    4.0 * t * t * t
                } else {
                    1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
                }
            }
            Easing::SineIn => 1.0 - (t * FRAC_PI_2).cos(),
            Easing::SineOut => (t * FRAC_PI_2).sin(),
            Easing::SineInOut => -((PI * t).cos() - 1.0) / 2.0,
            Easing::ExpoIn => {
                if t == 0.0 {
                    0.0
                } else {
                    2f32.powf(10.0 * t - 10.0)
                }
            }
            Easing::ExpoOut => {
                if t == 1.0 {
                    1.0
                } else {
                    1.0 - 2f32.powf(-10.0 * t)
                }
            }
            Easing::BackOut => {
                const C1: f32 = 1.70158;
                const C3: f32 = C1 + 1.0;
                1.0 + C3 * (t - 1.0).powi(3) + C1 * (t - 1.0).powi(2)
            }
            Easing::ElasticOut => {
                if t == 0.0 || t == 1.0 {
                    t
                } else {
                    2f32.powf(-10.0 * t) * ((t * 10.0 - 0.75) * (TAU / 3.0)).sin() + 1.0
                }
            }
            Easing::BounceOut => bounce_out(t),
        }
    }
}

fn bounce_out(t: f32) -> f32 {
    const N1: f32 = 7.5625;
    const D1: f32 = 2.75;
    if t < 1.0 / D1 {
        N1 * t * t
    } else if t < 2.0 / D1 {
        let t = t - 1.5 / D1;
        N1 * t * t + 0.75
    } else if t < 2.5 / D1 {
        let t = t - 2.25 / D1;
        N1 * t * t + 0.9375
    } else {
        let t = t - 2.625 / D1;
        N1 * t * t + 0.984375
    }
}

impl fmt::Display for Easing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Easing {
    type Err = Diagnostic;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Easing::from_name(s)
    }
}

/// A value a [`Tween`] can move: anything with an interpolation.
pub trait Tweenable: Copy {
    /// The value a fraction `s` of the way from `a` to `b`. `s` may lie outside
    /// `[0, 1]` for overshooting easings.
    fn interpolate(a: Self, b: Self, s: f32) -> Self;
}

impl Tweenable for f32 {
    fn interpolate(a: Self, b: Self, s: f32) -> Self {
        a + (b - a) * s
    }
}

impl Tweenable for Vec2 {
    fn interpolate(a: Self, b: Self, s: f32) -> Self {
        a.lerp(b, s)
    }
}

impl Tweenable for Vec3 {
    fn interpolate(a: Self, b: Self, s: f32) -> Self {
        a.lerp(b, s)
    }
}

impl Tweenable for Vec4 {
    fn interpolate(a: Self, b: Self, s: f32) -> Self {
        a.lerp(b, s)
    }
}

impl Tweenable for Quat {
    /// Spherical interpolation along the shorter arc.
    fn interpolate(a: Self, b: Self, s: f32) -> Self {
        a.slerp(b, s)
    }
}

/// A value moving from `from` to `to` over `duration` seconds.
///
/// ```
/// use re_anim::{Easing, Tween};
///
/// let mut door = Tween::new(0.0_f32, 90.0, 0.5, Easing::QuadOut).unwrap();
/// door.advance(0.25);
/// assert!(door.value() > 45.0); // quad_out is past half way at half time
/// door.advance(1.0);
/// assert!(door.is_finished());
/// assert_eq!(door.value(), 90.0);
/// ```
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tween<T: Tweenable> {
    from: T,
    to: T,
    duration: f32,
    easing: Easing,
    elapsed: f32,
}

impl<T: Tweenable> Tween<T> {
    /// A tween at its start.
    ///
    /// # Errors
    ///
    /// [`codes::TWEEN_INVALID`] when `duration` is negative or not finite. A zero
    /// duration is valid: the tween is finished at once, at `to`.
    pub fn new(from: T, to: T, duration: f32, easing: Easing) -> Result<Tween<T>, Diagnostic> {
        if !duration.is_finite() || duration < 0.0 {
            return Err(Diagnostic::error(
                codes::TWEEN_INVALID,
                format!("tween duration must be a finite number of seconds >= 0, got {duration}"),
            ));
        }
        Ok(Tween {
            from,
            to,
            duration,
            easing,
            elapsed: 0.0,
        })
    }

    /// The start value.
    pub fn from(&self) -> T {
        self.from
    }

    /// The end value.
    pub fn to(&self) -> T {
        self.to
    }

    /// The length in seconds.
    pub fn duration(&self) -> f32 {
        self.duration
    }

    /// The easing curve.
    pub fn easing(&self) -> Easing {
        self.easing
    }

    /// Seconds since the start, at most `duration`.
    pub fn elapsed(&self) -> f32 {
        self.elapsed
    }

    /// Progress in `[0, 1]` before easing.
    pub fn progress(&self) -> f32 {
        if self.duration == 0.0 {
            1.0
        } else {
            self.elapsed / self.duration
        }
    }

    /// The value `t` seconds after the start (clamped to the tween).
    pub fn value_at(&self, t: f32) -> T {
        let progress = if self.duration == 0.0 {
            1.0
        } else {
            (t / self.duration).clamp(0.0, 1.0)
        };
        let s = self.easing.apply(progress);
        if progress >= 1.0 {
            // Exactly the end value, whatever rounding the curve does.
            return self.to;
        }
        T::interpolate(self.from, self.to, s)
    }

    /// The current value.
    pub fn value(&self) -> T {
        self.value_at(self.elapsed)
    }

    /// Moves time forward by `dt` seconds (negative or non-finite `dt` is
    /// ignored) and returns the new value.
    pub fn advance(&mut self, dt: f32) -> T {
        if dt.is_finite() && dt > 0.0 {
            self.elapsed = (self.elapsed + dt).min(self.duration);
        }
        self.value()
    }

    /// True once `duration` seconds have passed.
    pub fn is_finished(&self) -> bool {
        self.elapsed >= self.duration
    }

    /// Back to the start.
    pub fn restart(&mut self) {
        self.elapsed = 0.0;
    }

    /// The same tween played backwards from its current value: the new tween goes
    /// to `from`, taking as long as this one has run so far.
    #[must_use]
    pub fn reversed(&self) -> Tween<T> {
        Tween {
            from: self.value(),
            to: self.from,
            duration: self.elapsed,
            easing: self.easing,
            elapsed: 0.0,
        }
    }
}
