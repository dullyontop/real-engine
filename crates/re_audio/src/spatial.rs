//! Spatial sounds: how they fall with distance, and doppler.
//!
//! kira's spatial tracks fall with distance on one fixed curve (the level in
//! decibels falling evenly), chosen when the track is built. A spatial sound
//! here plays on a spatial track that only pans, with an [`Attenuation`] effect
//! before the panning that applies the sound's [`Spatial`] curve, so the curve
//! can change while the sound plays.
//!
//! Doppler multiplies a sound's rate by a factor from the source's and the
//! listener's velocities along the line between them. Positions reach the mixer
//! once a frame, and often move only on fixed steps, so [`Motion`] estimates a
//! velocity over the last fifth of a second rather than from one frame's move.

use std::collections::VecDeque;

use kira::Frame;
use kira::command::{CommandReader, CommandWriter, command_writer_and_reader};
use kira::effect::{Effect, EffectBuilder};
use kira::info::Info;
use re_types::audio::{Rolloff, Spatial};

/// The speed of sound in air, in metres per second.
pub(crate) const SPEED_OF_SOUND: f32 = 343.0;

/// How far back [`Motion`] looks, in seconds: long enough to smooth positions
/// that move only on fixed steps between shorter frames, short enough to
/// follow a turn.
const WINDOW: f64 = 0.2;

/// A distance curve as the audio thread applies it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Curve {
    rolloff: Rolloff,
    min: f32,
    max: f32,
}

impl Curve {
    /// The curve of `spatial`, or none (`Off`) when the sound is not
    /// attenuated (`SetSourceAttenuation`).
    pub(crate) fn new(spatial: &Spatial, attenuate: bool) -> Curve {
        Curve {
            rolloff: if attenuate {
                spatial.rolloff
            } else {
                Rolloff::Off
            },
            min: spatial.min_distance,
            max: spatial.max_distance,
        }
    }

    /// The amplitude at `distance` metres from the listener, 0 to 1.
    pub(crate) fn gain(&self, distance: f32) -> f32 {
        if self.rolloff == Rolloff::Off || distance <= self.min {
            return 1.0;
        }
        let d = distance.min(self.max);
        let x = (d - self.min) / (self.max - self.min);
        match self.rolloff {
            Rolloff::Off => 1.0,
            Rolloff::Inverse => self.min / d,
            Rolloff::Linear => 1.0 - x,
            // -60 dB at the end is silence, as kira's own curve has it.
            Rolloff::Natural if x >= 1.0 => 0.0,
            Rolloff::Natural => libm::powf(10.0, -3.0 * x),
        }
    }
}

/// Builds an [`Attenuation`]; its handle sets the curve while the sound plays.
pub(crate) struct AttenuationBuilder(pub(crate) Curve);

impl EffectBuilder for AttenuationBuilder {
    type Handle = CommandWriter<Curve>;

    fn build(self) -> (Box<dyn Effect>, Self::Handle) {
        let (writer, reader) = command_writer_and_reader();
        let effect = Attenuation {
            curve: self.0,
            reader,
            gain: None,
        };
        (Box::new(effect), writer)
    }
}

/// Scales a spatial track by its curve at the distance to the listener.
struct Attenuation {
    curve: Curve,
    reader: CommandReader<Curve>,
    /// The gain at the end of the last chunk: a change ramps across one chunk,
    /// so a moving sound never clicks.
    gain: Option<f32>,
}

impl Effect for Attenuation {
    fn on_start_processing(&mut self) {
        if let Some(curve) = self.reader.read() {
            self.curve = curve;
        }
    }

    fn process(&mut self, input: &mut [Frame], _dt: f64, info: &Info) {
        // Without a listener, kira silences the track after its effects.
        let Some(distance) = info.listener_distance() else {
            return;
        };
        let to = self.curve.gain(distance);
        let from = self.gain.replace(to).unwrap_or(to);
        let n = input.len() as f32;
        for (i, frame) in input.iter_mut().enumerate() {
            *frame *= from + (to - from) * (i + 1) as f32 / n;
        }
    }
}

/// Recent positions of a source or of the listener, for a velocity.
#[derive(Clone, Debug, Default)]
pub(crate) struct Motion {
    /// Seconds of the audio clock and positions, oldest first: the newest
    /// sample at least [`WINDOW`] old, and every later one.
    samples: VecDeque<(f64, [f32; 3])>,
}

impl Motion {
    /// Records the position at `t` seconds; a second record at the same time
    /// replaces the first. A move faster than sound (over at least a frame of
    /// 30 Hz) is a teleport, which starts the estimate again.
    pub(crate) fn record(&mut self, t: f64, p: [f32; 3]) {
        if self.samples.back().is_some_and(|&(last, _)| t <= last) {
            self.samples.pop_back();
        }
        if let Some(&(last, q)) = self.samples.back()
            && distance(p, q) > SPEED_OF_SOUND * (t - last).max(1.0 / 30.0) as f32
        {
            self.samples.clear();
        }
        self.samples.push_back((t, p));
        while self.samples.len() > 1 && t - self.samples[1].0 >= WINDOW {
            self.samples.pop_front();
        }
    }

    /// The average velocity over the recorded window, in metres per second.
    pub(crate) fn velocity(&self) -> [f32; 3] {
        match (self.samples.front(), self.samples.back()) {
            (Some(&(t0, p0)), Some(&(t1, p1))) if t1 > t0 => {
                let dt = (t1 - t0) as f32;
                [
                    (p1[0] - p0[0]) / dt,
                    (p1[1] - p0[1]) / dt,
                    (p1[2] - p0[2]) / dt,
                ]
            }
            _ => [0.0; 3],
        }
    }
}

/// The rate factor for a source at `source` moving at `source_velocity`, heard
/// by a listener at `listener` moving at `listener_velocity`. `scale` is
/// `Spatial::doppler`: 0 is none, 1 physical, and more exaggerates. Each speed
/// along the line counts up to half the speed of sound, so the factor stays
/// between a third and three.
pub(crate) fn doppler(
    scale: f32,
    source: [f32; 3],
    source_velocity: [f32; 3],
    listener: [f32; 3],
    listener_velocity: [f32; 3],
) -> f64 {
    let gap = distance(source, listener);
    if scale <= 0.0 || gap < 1e-3 {
        return 1.0;
    }
    // The unit vector from the source to the listener.
    let dir = [
        (listener[0] - source[0]) / gap,
        (listener[1] - source[1]) / gap,
        (listener[2] - source[2]) / gap,
    ];
    let limit = 0.5 * SPEED_OF_SOUND;
    let toward = (scale * dot(source_velocity, dir)).clamp(-limit, limit);
    let away = (scale * dot(listener_velocity, dir)).clamp(-limit, limit);
    f64::from((SPEED_OF_SOUND - away) / (SPEED_OF_SOUND - toward))
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
    let d = [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    libm::sqrtf(dot(d, d))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(rolloff: Rolloff, min: f32, max: f32) -> Curve {
        let spatial = Spatial {
            rolloff,
            min_distance: min,
            max_distance: max,
            doppler: 0.0,
        };
        Curve::new(&spatial, true)
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn curves_fall_as_documented() {
        let natural = curve(Rolloff::Natural, 1.0, 101.0);
        assert_eq!(natural.gain(0.5), 1.0);
        assert_eq!(natural.gain(1.0), 1.0);
        // Halfway is -30 dB.
        assert!(
            close(natural.gain(51.0), 0.031_622_78),
            "{}",
            natural.gain(51.0)
        );
        assert_eq!(natural.gain(101.0), 0.0);
        assert_eq!(natural.gain(500.0), 0.0);

        let linear = curve(Rolloff::Linear, 2.0, 12.0);
        assert!(close(linear.gain(7.0), 0.5));
        assert!(close(linear.gain(9.5), 0.25));
        assert_eq!(linear.gain(12.0), 0.0);

        // -6 dB per doubling beyond min, and the level at max beyond it.
        let inverse = curve(Rolloff::Inverse, 2.0, 16.0);
        assert!(close(inverse.gain(4.0), 0.5));
        assert!(close(inverse.gain(8.0), 0.25));
        assert!(close(inverse.gain(16.0), 0.125));
        assert!(close(inverse.gain(1000.0), 0.125));

        assert_eq!(curve(Rolloff::Off, 1.0, 2.0).gain(1000.0), 1.0);
        let spatial = Spatial::default();
        assert_eq!(Curve::new(&spatial, false).gain(1000.0), 1.0);
    }

    #[test]
    fn motion_averages_stepped_positions_and_forgets_teleports() {
        let mut m = Motion::default();
        assert_eq!(m.velocity(), [0.0; 3]);
        // A 10 m/s mover stepped at 60 Hz, sampled by 240 Hz frames: three
        // frames in four see no move.
        for frame in 0..240 {
            let t = f64::from(frame) / 240.0;
            let step = (frame / 4) as f32;
            m.record(t, [step * 10.0 / 60.0, 0.0, 0.0]);
        }
        let v = m.velocity();
        assert!((v[0] - 10.0).abs() < 1.0 && v[1] == 0.0, "{v:?}");

        // Standing still for longer than the window reads as still.
        let p = m.samples.back().unwrap().1;
        for frame in 1..=60 {
            m.record(1.0 + f64::from(frame) / 240.0, p);
        }
        assert_eq!(m.velocity(), [0.0; 3]);

        // A 90 m jump in one frame is a teleport, not motion.
        let t = m.samples.back().unwrap().0;
        m.record(t + 1.0 / 60.0, [100.0, 0.0, 0.0]);
        m.record(t + 2.0 / 60.0, [100.0, 0.0, 0.0]);
        assert_eq!(m.velocity(), [0.0; 3]);
    }

    #[test]
    fn doppler_raises_an_approaching_source_and_lowers_a_receding_one() {
        let (s, l) = ([0.0, 0.0, -100.0], [0.0; 3]);
        let still = [0.0; 3];
        // A source closing at 34.3 m/s: 343 / (343 - 34.3).
        let near = doppler(1.0, s, [0.0, 0.0, 34.3], l, still);
        assert!((near - 1.0 / 0.9).abs() < 1e-4, "{near}");
        let far = doppler(1.0, s, [0.0, 0.0, -34.3], l, still);
        assert!((far - 1.0 / 1.1).abs() < 1e-4, "{far}");
        // The listener moving away: (343 - 34.3) / 343.
        let away = doppler(1.0, s, still, l, [0.0, 0.0, 34.3]);
        assert!((away - 0.9).abs() < 1e-4, "{away}");
        // Motion across the line changes nothing; neither does scale 0.
        assert_eq!(doppler(1.0, s, [30.0, 0.0, 0.0], l, still), 1.0);
        assert_eq!(doppler(0.0, s, [0.0, 0.0, 34.3], l, still), 1.0);
        // Exaggerated speeds stop at half the speed of sound each way.
        assert!((doppler(100.0, s, [0.0, 0.0, 34.3], l, still) - 2.0).abs() < 1e-6);
        assert!((doppler(100.0, s, [0.0, 0.0, 34.3], l, [0.0, 0.0, 34.3]) - 1.0).abs() < 1e-6);
    }
}
