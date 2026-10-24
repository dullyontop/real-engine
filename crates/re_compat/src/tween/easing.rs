//! Easing curves: every `Enum.EasingStyle` in every `Enum.EasingDirection`.
//!
//! Each style is defined by its "in" curve `f` on `[0, 1]`. The directions
//! derive from it: `Out(t) = 1 - f(1 - t)` and `InOut(t)` is `f(2t) / 2` for
//! the first half and `1 - f(2 - 2t) / 2` for the second. `Back` overshoots
//! with the usual constant 1.70158, and `Elastic` has amplitude 1 and a
//! period of 0.3.

use std::f64::consts::PI;

/// `Enum.EasingStyle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EasingStyle {
    /// No easing.
    Linear,
    /// A quarter sine wave.
    Sine,
    /// Pulls back before moving.
    Back,
    /// `t²`.
    Quad,
    /// `t⁴`.
    Quart,
    /// `t⁵`.
    Quint,
    /// Bounces at the end of an "out" curve.
    Bounce,
    /// A decaying spring.
    Elastic,
    /// `2^(10t - 10)`.
    Exponential,
    /// A quarter circle.
    Circular,
    /// `t³`.
    Cubic,
}

/// `Enum.EasingDirection`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EasingDirection {
    /// The curve as defined: slow start.
    In,
    /// Mirrored: slow end.
    Out,
    /// In for the first half, out for the second.
    InOut,
}

impl EasingStyle {
    /// Every style, in enum order.
    pub const ALL: [EasingStyle; 11] = [
        EasingStyle::Linear,
        EasingStyle::Sine,
        EasingStyle::Back,
        EasingStyle::Quad,
        EasingStyle::Quart,
        EasingStyle::Quint,
        EasingStyle::Bounce,
        EasingStyle::Elastic,
        EasingStyle::Exponential,
        EasingStyle::Circular,
        EasingStyle::Cubic,
    ];

    /// The style named by an `Enum.EasingStyle` item.
    pub fn from_name(name: &str) -> Option<EasingStyle> {
        Some(match name {
            "Linear" => EasingStyle::Linear,
            "Sine" => EasingStyle::Sine,
            "Back" => EasingStyle::Back,
            "Quad" => EasingStyle::Quad,
            "Quart" => EasingStyle::Quart,
            "Quint" => EasingStyle::Quint,
            "Bounce" => EasingStyle::Bounce,
            "Elastic" => EasingStyle::Elastic,
            "Exponential" => EasingStyle::Exponential,
            "Circular" => EasingStyle::Circular,
            "Cubic" => EasingStyle::Cubic,
            _ => return None,
        })
    }

    /// The "in" curve.
    fn ease_in(self, t: f64) -> f64 {
        match self {
            EasingStyle::Linear => t,
            EasingStyle::Sine => 1.0 - libm::cos(t * PI / 2.0),
            EasingStyle::Back => {
                const C1: f64 = 1.70158;
                (C1 + 1.0) * t * t * t - C1 * t * t
            }
            EasingStyle::Quad => t * t,
            EasingStyle::Cubic => t * t * t,
            EasingStyle::Quart => t * t * t * t,
            EasingStyle::Quint => t * t * t * t * t,
            EasingStyle::Bounce => 1.0 - bounce_out(1.0 - t),
            EasingStyle::Elastic => {
                if t <= 0.0 || t >= 1.0 {
                    t.clamp(0.0, 1.0)
                } else {
                    let p = 0.3;
                    -(libm::pow(2f64, 10.0 * t - 10.0))
                        * libm::sin((t - 1.0 - p / 4.0) * 2.0 * PI / p)
                }
            }
            EasingStyle::Exponential => {
                if t <= 0.0 {
                    0.0
                } else {
                    libm::pow(2f64, 10.0 * t - 10.0)
                }
            }
            EasingStyle::Circular => 1.0 - (1.0 - t * t).max(0.0).sqrt(),
        }
    }
}

impl EasingDirection {
    /// The direction named by an `Enum.EasingDirection` item.
    pub fn from_name(name: &str) -> Option<EasingDirection> {
        Some(match name {
            "In" => EasingDirection::In,
            "Out" => EasingDirection::Out,
            "InOut" => EasingDirection::InOut,
            _ => return None,
        })
    }
}

/// The standard bounce: four parabolic arcs.
fn bounce_out(t: f64) -> f64 {
    const N: f64 = 7.5625;
    const D: f64 = 2.75;
    if t < 1.0 / D {
        N * t * t
    } else if t < 2.0 / D {
        let t = t - 1.5 / D;
        N * t * t + 0.75
    } else if t < 2.5 / D {
        let t = t - 2.25 / D;
        N * t * t + 0.9375
    } else {
        let t = t - 2.625 / D;
        N * t * t + 0.984375
    }
}

/// The eased value of `alpha` (clamped to `[0, 1]`): 0 at 0 and 1 at 1, and
/// outside `[0, 1]` in between for `Back` and `Elastic`.
pub fn ease(style: EasingStyle, direction: EasingDirection, alpha: f64) -> f64 {
    let t = if alpha.is_nan() {
        0.0
    } else {
        alpha.clamp(0.0, 1.0)
    };
    match direction {
        EasingDirection::In => style.ease_in(t),
        EasingDirection::Out => 1.0 - style.ease_in(1.0 - t),
        EasingDirection::InOut => {
            if t < 0.5 {
                style.ease_in(2.0 * t) / 2.0
            } else {
                1.0 - style.ease_in(2.0 - 2.0 * t) / 2.0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_curve_starts_at_0_and_ends_at_1() {
        for s in EasingStyle::ALL {
            for d in [
                EasingDirection::In,
                EasingDirection::Out,
                EasingDirection::InOut,
            ] {
                assert!(ease(s, d, 0.0).abs() < 1e-9, "{s:?} {d:?} at 0");
                assert!((ease(s, d, 1.0) - 1.0).abs() < 1e-9, "{s:?} {d:?} at 1");
            }
        }
    }
}
