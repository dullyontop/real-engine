//! `Audio`: the kira manager, the listener, and the sounds started by commands.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kira::backend::Backend;
use kira::listener::ListenerHandle;
use kira::sound::PlaybackState;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle, StaticSoundSettings};
use kira::track::{SpatialTrackBuilder, SpatialTrackHandle};
use kira::{AudioManager, AudioManagerSettings, Decibels, DefaultBackend, Frame, Tween};
use re_core::{Diagnostic, Location};
use re_types::audio::AudioCmd;
use re_types::ids::EntityId;

use crate::capture::{CaptureBackend, CaptureSettings};
use crate::codes;

/// Changes take effect at once (no fade).
const NOW: Tween = Tween {
    start_time: kira::StartTime::Immediate,
    duration: Duration::ZERO,
    easing: kira::Easing::Linear,
};

/// A playing sound, and its spatial track when it has one. Dropping the track
/// handle removes the track, so it lives as long as the sound.
struct Voice {
    sound: StaticSoundHandle,
    track: Option<SpatialTrackHandle>,
}

impl Voice {
    fn finished(&self) -> bool {
        self.sound.state() == PlaybackState::Stopped
    }
}

/// Everything that depends on the backend type.
struct Mixer<B: Backend> {
    manager: AudioManager<B>,
    listener: ListenerHandle,
    sources: HashMap<EntityId, Voice>,
    one_shots: Vec<Voice>,
    /// Entities whose spatial sounds only pan (`SetSourceAttenuation`).
    unattenuated: HashSet<EntityId>,
}

impl<B: Backend> Mixer<B> {
    fn new(manager: AudioManager<B>) -> Result<Self, Diagnostic> {
        let mut manager = manager;
        let listener = manager
            .add_listener([0.0f32; 3], [0.0f32, 0.0, 0.0, 1.0])
            .map_err(|e| limit(&e.to_string()))?;
        Ok(Mixer {
            manager,
            listener,
            sources: HashMap::new(),
            one_shots: Vec::new(),
            unattenuated: HashSet::new(),
        })
    }

    fn play(
        &mut self,
        data: StaticSoundData,
        position: Option<[f32; 3]>,
        attenuate: bool,
    ) -> Result<Voice, Diagnostic> {
        match position {
            Some(p) => {
                let mut builder = SpatialTrackBuilder::new();
                if !attenuate {
                    builder = builder.attenuation_function(None);
                }
                let mut track = self
                    .manager
                    .add_spatial_sub_track(&self.listener, p, builder)
                    .map_err(|e| limit(&e.to_string()))?;
                let sound = track.play(data).map_err(|e| limit(&e.to_string()))?;
                Ok(Voice {
                    sound,
                    track: Some(track),
                })
            }
            None => {
                let sound = self.manager.play(data).map_err(|e| limit(&e.to_string()))?;
                Ok(Voice { sound, track: None })
            }
        }
    }

    fn prune(&mut self) {
        self.one_shots.retain(|v| !v.finished());
        self.sources.retain(|_, v| !v.finished());
    }
}

fn limit(detail: &str) -> Diagnostic {
    Diagnostic::warning(
        codes::PLAYBACK_LIMIT,
        format!("a sound was not started: {detail}"),
    )
    .with_hint("fewer sounds at once: stop looping sources that are out of earshot")
}

enum Output {
    Device(Mixer<DefaultBackend>),
    Headless(Mixer<CaptureBackend>),
}

/// Runs `$body` with `$m` bound to the mixer, whatever the backend.
macro_rules! with_mixer {
    ($self:expr, $m:ident => $body:expr) => {
        match &mut $self.output {
            Output::Device($m) => $body,
            Output::Headless($m) => $body,
        }
    };
}

/// The engine's audio: a kira manager on the output device, or on the headless
/// [`CaptureBackend`].
pub struct Audio {
    output: Output,
    sample_rate: u32,
    root: Option<PathBuf>,
    clips: HashMap<String, StaticSoundData>,
}

impl Audio {
    /// Opens the default output device. When there is none (a server, CI, a
    /// container), logs warning RE7400 and continues on the silent headless
    /// backend, so the game still runs.
    pub fn new_or_headless() -> Audio {
        match AudioManager::<DefaultBackend>::new(AudioManagerSettings::default()) {
            Ok(m) => match Mixer::new(m) {
                Ok(mixer) => Audio::with_output(Output::Device(mixer), 0),
                Err(d) => {
                    tracing::warn!(code = %d.code, "{}; continuing with silent headless audio", d.message);
                    Audio::headless()
                }
            },
            Err(e) => {
                tracing::warn!(
                    code = %codes::NO_OUTPUT_DEVICE,
                    error = %e,
                    "no audio output device; continuing with silent headless audio"
                );
                Audio::headless()
            }
        }
    }

    /// Audio on the headless backend at 48 kHz. Never opens a device.
    pub fn headless() -> Audio {
        Audio::headless_with(CaptureSettings::default())
    }

    /// Audio on the headless backend with these settings. Never opens a device.
    pub fn headless_with(settings: CaptureSettings) -> Audio {
        let manager = AudioManager::<CaptureBackend>::new(AudioManagerSettings {
            backend_settings: settings,
            ..Default::default()
        })
        .unwrap_or_else(|never| match never {});
        // A fresh manager has room for its first listener.
        let mixer = Mixer::new(manager).expect("a new audio manager accepts one listener");
        Audio::with_output(Output::Headless(mixer), settings.sample_rate)
    }

    fn with_output(output: Output, sample_rate: u32) -> Audio {
        Audio {
            output,
            sample_rate,
            root: None,
            clips: HashMap::new(),
        }
    }

    /// True when playing on the headless backend.
    pub fn is_headless(&self) -> bool {
        matches!(self.output, Output::Headless(_))
    }

    /// The headless backend's sample rate, or `None` on a device.
    pub fn headless_sample_rate(&self) -> Option<u32> {
        self.is_headless().then_some(self.sample_rate)
    }

    /// Renders `frames` stereo frames on the headless backend and returns them
    /// interleaved. `None` on a device (it renders by itself).
    pub fn render_headless(&mut self, frames: usize) -> Option<Vec<f32>> {
        match &mut self.output {
            Output::Headless(m) => Some(m.manager.backend_mut().render(frames)),
            Output::Device(_) => None,
        }
    }

    /// Sets the directory clip paths are relative to (the project root). Without
    /// one, only clips registered with [`Audio::insert_clip`] play.
    pub fn set_asset_root(&mut self, root: impl Into<PathBuf>) {
        self.root = Some(root.into());
    }

    /// Registers sound data under a clip name, replacing any earlier clip of that
    /// name (hot reload). Generated sounds need no file.
    pub fn insert_clip(&mut self, name: impl Into<String>, data: StaticSoundData) {
        self.clips.insert(name.into(), data);
    }

    /// Forgets a clip, so the next use reloads it from its file.
    pub fn remove_clip(&mut self, name: &str) {
        self.clips.remove(name);
    }

    /// True when a clip of this name is loaded.
    pub fn has_clip(&self, name: &str) -> bool {
        self.clips.contains_key(name)
    }

    /// Loads a clip from `<asset root>/<name>` (a WAV file) if it is not loaded yet.
    ///
    /// # Errors
    ///
    /// [`codes::CLIP_NOT_FOUND`] when there is no such file (or no asset root),
    /// [`codes::CLIP_DECODE_FAILED`] when it cannot be decoded.
    pub fn load_clip(&mut self, name: &str) -> Result<(), Diagnostic> {
        self.clip(name).map(|_| ())
    }

    fn clip(&mut self, name: &str) -> Result<StaticSoundData, Diagnostic> {
        if let Some(data) = self.clips.get(name) {
            return Ok(data.clone());
        }
        let not_found = |why: &str| {
            Diagnostic::error(
                codes::CLIP_NOT_FOUND,
                format!("audio clip `{name}` not found: {why}"),
            )
            .at(Location::file(name))
        };
        let Some(root) = &self.root else {
            return Err(not_found("it is not registered and no asset root is set"));
        };
        let path = root.join(name);
        if !path.is_file() {
            return Err(
                not_found(&format!("no file at {}", slash(&path))).with_hint(
                    "clip names are project-relative paths such as `assets/sfx/pickup.wav`",
                ),
            );
        }
        let decode_failed = |why: String| {
            Diagnostic::error(
                codes::CLIP_DECODE_FAILED,
                format!("cannot decode audio clip `{name}`: {why}"),
            )
            .at(Location::file(name))
            .with_hint("M1 plays WAV files (PCM or float, mono or stereo)")
        };
        let bytes = std::fs::read(&path).map_err(|e| decode_failed(e.to_string()))?;
        let data = crate::wav::decode(&bytes).map_err(|e| decode_failed(e.to_string()))?;
        self.clips.insert(name.to_owned(), data.clone());
        Ok(data)
    }

    /// The length of a clip in seconds, loading it first if needed (see
    /// [`Audio::load_clip`]).
    ///
    /// # Errors
    ///
    /// As [`Audio::load_clip`].
    pub fn clip_seconds(&mut self, name: &str) -> Result<f64, Diagnostic> {
        Ok(self.clip(name)?.duration().as_secs_f64())
    }

    /// The number of sounds started by commands that are still playing.
    pub fn playing(&mut self) -> usize {
        with_mixer!(self, m => {
            m.prune();
            m.sources.len() + m.one_shots.len()
        })
    }

    /// True while the entity's sound plays.
    pub fn is_source_playing(&mut self, entity: EntityId) -> bool {
        with_mixer!(self, m => m.sources.get(&entity).is_some_and(|v| !v.finished()))
    }

    /// Applies one frame's commands, in order.
    ///
    /// A command that fails does not stop the others; its diagnostic is returned.
    /// Finished sounds are cleaned up first.
    pub fn apply(&mut self, cmds: &[AudioCmd]) -> Vec<Diagnostic> {
        with_mixer!(self, m => m.prune());
        let mut problems = Vec::new();
        for cmd in cmds {
            if let Err(d) = self.apply_one(cmd) {
                problems.push(d);
            }
        }
        problems
    }

    fn apply_one(&mut self, cmd: &AudioCmd) -> Result<(), Diagnostic> {
        match cmd {
            AudioCmd::PlayOneShot {
                clip,
                position,
                volume,
            } => {
                let data = self
                    .clip(clip)?
                    .volume(linear_to_decibels(check_volume(*volume)?));
                let position = position.map(|p| p.to_array());
                with_mixer!(self, m => {
                    let voice = m.play(data, position, true)?;
                    m.one_shots.push(voice);
                });
            }
            AudioCmd::StartSource {
                entity,
                clip,
                volume,
                looping,
                spatial,
                position,
            } => {
                let mut data = self
                    .clip(clip)?
                    .volume(linear_to_decibels(check_volume(*volume)?));
                if *looping {
                    data = data.loop_region(..);
                }
                let position = spatial.then(|| position.to_array());
                with_mixer!(self, m => {
                    if let Some(mut old) = m.sources.remove(entity) {
                        old.sound.stop(NOW);
                    }
                    let attenuate = !m.unattenuated.contains(entity);
                    let voice = m.play(data, position, attenuate)?;
                    m.sources.insert(*entity, voice);
                });
            }
            AudioCmd::StopSource { entity } => with_mixer!(self, m => {
                if let Some(mut v) = m.sources.remove(entity) {
                    v.sound.stop(NOW);
                }
            }),
            AudioCmd::SetSourceVolume { entity, volume } => {
                let db = linear_to_decibels(check_volume(*volume)?);
                with_mixer!(self, m => {
                    if let Some(v) = m.sources.get_mut(entity) {
                        v.sound.set_volume(db, NOW);
                    }
                });
            }
            AudioCmd::SetSourcePosition { entity, position } => with_mixer!(self, m => {
                if let Some(track) = m.sources.get_mut(entity).and_then(|v| v.track.as_mut()) {
                    track.set_position(position.to_array(), NOW);
                }
            }),
            AudioCmd::SetSourceRate { entity, rate } => {
                if !(rate.is_finite() && *rate > 0.0) {
                    return Err(Diagnostic::error(
                        codes::INVALID_RATE,
                        format!("playback rate {rate} is not a finite number > 0"),
                    ));
                }
                with_mixer!(self, m => {
                    if let Some(v) = m.sources.get_mut(entity) {
                        v.sound.set_playback_rate(f64::from(*rate), NOW);
                    }
                });
            }
            AudioCmd::SeekSource { entity, seconds } => with_mixer!(self, m => {
                if let Some(v) = m.sources.get_mut(entity) {
                    v.sound.seek_to(if seconds.is_finite() { seconds.max(0.0) } else { 0.0 });
                }
            }),
            AudioCmd::SetSourceAttenuation { entity, enabled } => with_mixer!(self, m => {
                if *enabled {
                    m.unattenuated.remove(entity);
                } else {
                    m.unattenuated.insert(*entity);
                }
            }),
            AudioCmd::SetListener { position, rotation } => with_mixer!(self, m => {
                m.listener.set_position(position.to_array(), NOW);
                m.listener.set_orientation(rotation.normalize().to_array(), NOW);
            }),
            AudioCmd::SetMasterVolume { volume } => {
                let db = linear_to_decibels(check_volume(*volume)?);
                with_mixer!(self, m => m.manager.main_track().set_volume(db, NOW));
            }
            AudioCmd::StopAll => with_mixer!(self, m => {
                for (_, mut v) in m.sources.drain() {
                    v.sound.stop(NOW);
                }
                for mut v in m.one_shots.drain(..) {
                    v.sound.stop(NOW);
                }
            }),
        }
        Ok(())
    }
}

fn check_volume(volume: f32) -> Result<f32, Diagnostic> {
    if volume.is_finite() && volume >= 0.0 {
        Ok(volume)
    } else {
        Err(Diagnostic::error(
            codes::INVALID_VOLUME,
            format!("volume {volume} is not a finite number >= 0 (1 is unchanged, 0 is silent)"),
        ))
    }
}

/// A linear volume (1 is unchanged, 0 is silent) as kira decibels.
pub fn linear_to_decibels(volume: f32) -> Decibels {
    if volume <= 0.0 || !volume.is_finite() {
        Decibels::SILENCE
    } else {
        Decibels((20.0 * volume.log10()).max(Decibels::SILENCE.0))
    }
}

fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// A generated sine tone as kira sound data: procedural audio needs no files.
pub fn sine(freq: f32, seconds: f32, sample_rate: u32, amplitude: f32) -> StaticSoundData {
    let n = (seconds.max(0.0) * sample_rate as f32) as usize;
    let frames: Arc<[Frame]> = (0..n)
        .map(|i| {
            let phase = std::f32::consts::TAU * freq * i as f32 / sample_rate as f32;
            Frame::from_mono(amplitude * phase.sin())
        })
        .collect();
    StaticSoundData {
        sample_rate,
        frames,
        settings: StaticSoundSettings::default(),
        slice: None,
    }
}

/// Root mean square of one channel (0 left, 1 right) of interleaved stereo.
pub fn rms(interleaved: &[f32], channel: usize) -> f32 {
    let (sum, n) = interleaved
        .iter()
        .skip(channel)
        .step_by(2)
        .fold((0.0f64, 0usize), |(s, n), &x| {
            (s + f64::from(x) * f64::from(x), n + 1)
        });
    if n == 0 {
        0.0
    } else {
        (sum / n as f64).sqrt() as f32
    }
}
