//! `CaptureBackend`: a headless kira backend that renders into a buffer.
//!
//! kira's own `MockBackend` keeps its output private, so tests, CI and offline
//! rendering use this one. It renders only when [`CaptureBackend::render`] is
//! called, and it never opens an audio device.

use std::sync::{Arc, Mutex};

use kira::backend::{Backend, Renderer};

/// Settings for [`CaptureBackend`].
#[derive(Debug, Clone, Copy)]
pub struct CaptureSettings {
    /// Frames per second of the rendered audio.
    pub sample_rate: u32,
}

impl Default for CaptureSettings {
    /// 48 kHz.
    fn default() -> Self {
        CaptureSettings {
            sample_rate: 48_000,
        }
    }
}

/// Renders audio on demand, into interleaved stereo `f32`.
pub struct CaptureBackend {
    sample_rate: u32,
    renderer: Option<Arc<Mutex<Renderer>>>,
}

impl Backend for CaptureBackend {
    type Settings = CaptureSettings;
    type Error = std::convert::Infallible;

    fn setup(
        settings: CaptureSettings,
        _internal_buffer_size: usize,
    ) -> Result<(Self, u32), Self::Error> {
        Ok((
            CaptureBackend {
                sample_rate: settings.sample_rate,
                renderer: None,
            },
            settings.sample_rate,
        ))
    }

    fn start(&mut self, renderer: Renderer) -> Result<(), Self::Error> {
        self.renderer = Some(Arc::new(Mutex::new(renderer)));
        Ok(())
    }
}

impl CaptureBackend {
    /// The sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Renders `frames` stereo frames, as a device callback would, and returns
    /// them interleaved (left, right, left, ...).
    pub fn render(&mut self, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0; frames * 2];
        if let Some(r) = &self.renderer {
            // The renderer is only touched from this thread; a poisoned lock means
            // a previous render panicked, and the state is still usable.
            let mut r = r.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            r.on_start_processing();
            r.process(&mut out, 2);
        }
        out
    }
}
