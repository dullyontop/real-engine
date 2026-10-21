//! `Meter`: a pass-through kira effect that measures a track's output.
//!
//! kira has no metering API, so every bus and send ends with this effect
//! (ADR-0041). The audio thread accumulates the peak and the sum of squares of
//! what passes through; [`MeterSlot::take`] reads them on the main thread and
//! starts a new window. The slot is a seqlock over atomics: the audio thread
//! never waits, and the reader retries the rare read that overlaps a write.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

use kira::Frame;
use kira::effect::Effect;
use kira::info::Info;

/// What one window measured.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Window {
    /// The largest sample magnitude of either channel.
    pub peak: f32,
    /// The root mean square over both channels.
    pub rms: f32,
}

/// Shared between a [`Meter`] (the writer, on the audio thread) and the graph
/// (the reader).
#[derive(Debug, Default)]
pub(crate) struct MeterSlot {
    /// Odd while the writer is writing.
    seq: AtomicU64,
    /// Bumped by the reader to start a new window.
    window: AtomicU64,
    /// The window the published values belong to.
    published: AtomicU64,
    sum_sq: AtomicU64,
    samples: AtomicU64,
    peak: AtomicU32,
}

impl MeterSlot {
    /// The window since the last call (or since the meter was built), and starts
    /// a new one. Silence when nothing was rendered.
    pub(crate) fn take(&self) -> Window {
        let (sum_sq, samples, peak, published) = loop {
            let before = self.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let sum_sq = f64::from_bits(self.sum_sq.load(Ordering::Relaxed));
            let samples = self.samples.load(Ordering::Relaxed);
            let peak = f32::from_bits(self.peak.load(Ordering::Relaxed));
            let published = self.published.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) == before {
                break (sum_sq, samples, peak, published);
            }
        };
        let window = self.window.fetch_add(1, Ordering::AcqRel);
        if published != window {
            // Nothing was rendered in this window: the values are an older one's.
            return Window::default();
        }
        Window {
            peak,
            rms: if samples == 0 {
                0.0
            } else {
                (sum_sq / samples as f64).sqrt() as f32
            },
        }
    }
}

/// The effect appended to every bus and send.
pub(crate) struct Meter {
    slot: Arc<MeterSlot>,
    window: u64,
    sum_sq: f64,
    samples: u64,
    peak: f32,
}

impl Meter {
    /// A meter writing into `slot`.
    pub(crate) fn new(slot: Arc<MeterSlot>) -> Meter {
        Meter {
            slot,
            window: 0,
            sum_sq: 0.0,
            samples: 0,
            peak: 0.0,
        }
    }
}

impl Effect for Meter {
    fn process(&mut self, input: &mut [Frame], _dt: f64, _info: &Info) {
        let window = self.slot.window.load(Ordering::Acquire);
        if window != self.window {
            self.window = window;
            self.sum_sq = 0.0;
            self.samples = 0;
            self.peak = 0.0;
        }
        for f in input.iter() {
            self.sum_sq += f64::from(f.left) * f64::from(f.left);
            self.sum_sq += f64::from(f.right) * f64::from(f.right);
            self.peak = self.peak.max(f.left.abs()).max(f.right.abs());
        }
        self.samples += 2 * input.len() as u64;
        let slot = &self.slot;
        slot.seq.fetch_add(1, Ordering::AcqRel);
        slot.sum_sq.store(self.sum_sq.to_bits(), Ordering::Relaxed);
        slot.samples.store(self.samples, Ordering::Relaxed);
        slot.peak.store(self.peak.to_bits(), Ordering::Relaxed);
        slot.published.store(self.window, Ordering::Relaxed);
        slot.seq.fetch_add(1, Ordering::Release);
    }
}
