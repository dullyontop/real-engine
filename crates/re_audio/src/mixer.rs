//! `Audio`: the kira manager, the listener, the mixer graph, and the sounds
//! started by commands.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kira::backend::Backend;
use kira::listener::ListenerHandle;
use kira::sound::PlaybackState;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle, StaticSoundSettings};
use kira::track::{SpatialTrackBuilder, SpatialTrackHandle};
use kira::{
    AudioManager, AudioManagerSettings, Capacities, Decibels, DefaultBackend, Frame, Tween,
};
use re_core::{Diagnostic, Location};
use re_types::audio::{AudioCmd, AudioGraphDesc, BusLevel};
use re_types::ids::EntityId;

use crate::capture::{CaptureBackend, CaptureSettings};
use crate::codes;
use crate::graph::{BUS_CAPACITY, Graph};

/// Changes take effect at once (no fade).
const NOW: Tween = Tween {
    start_time: kira::StartTime::Immediate,
    duration: Duration::ZERO,
    easing: kira::Easing::Linear,
};

/// A playing sound, and its spatial track when it has one. Dropping the track
/// handle removes the track, so it lives as long as the sound. Rebuilding the
/// graph removes the bus tracks, and with them the sounds on them.
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
    /// Entities' buses (`SetSourceBus`).
    buses: HashMap<EntityId, String>,
    graph: Graph,
}

impl<B: Backend> Mixer<B> {
    fn new(manager: AudioManager<B>) -> Result<Self, Diagnostic> {
        let mut manager = manager;
        let listener = manager
            .add_listener([0.0f32; 3], [0.0f32, 0.0, 0.0, 1.0])
            .map_err(|e| limit(&e.to_string()))?;
        let (graph, problems) = Graph::build(&mut manager, &AudioGraphDesc::default());
        debug_assert!(problems.is_empty(), "{problems:?}");
        Ok(Mixer {
            manager,
            listener,
            sources: HashMap::new(),
            one_shots: Vec::new(),
            unattenuated: HashSet::new(),
            buses: HashMap::new(),
            graph,
        })
    }

    /// Plays `data` on `bus` (the default bus when empty or unknown; the
    /// warning for an unknown name is pushed to `problems` once).
    fn play(
        &mut self,
        data: StaticSoundData,
        position: Option<[f32; 3]>,
        attenuate: bool,
        bus: &str,
        problems: &mut Vec<Diagnostic>,
    ) -> Result<Voice, Diagnostic> {
        let listener = self.listener.id();
        let (track, warning) = self.graph.bus_track(bus);
        problems.extend(warning);
        match position {
            Some(p) => {
                let mut builder = SpatialTrackBuilder::new();
                if !attenuate {
                    builder = builder.attenuation_function(None);
                }
                let mut track = track
                    .add_spatial_sub_track(listener, p, builder)
                    .map_err(|e| limit(&e.to_string()))?;
                let sound = track.play(data).map_err(|e| limit(&e.to_string()))?;
                Ok(Voice {
                    sound,
                    track: Some(track),
                })
            }
            None => {
                let sound = track.play(data).map_err(|e| limit(&e.to_string()))?;
                Ok(Voice { sound, track: None })
            }
        }
    }

    /// Replaces the graph. The sounds on the old buses stop.
    fn set_graph(&mut self, desc: &AudioGraphDesc) -> Vec<Diagnostic> {
        let stopped = self.sources.len() + self.one_shots.len();
        self.sources.clear();
        self.one_shots.clear();
        let (graph, problems) = Graph::build(&mut self.manager, desc);
        // The old graph's tracks go when its handles drop, after the new graph
        // is built, so the main track never has fewer buses than it needs.
        self.graph = graph;
        if stopped > 0 {
            tracing::info!(
                stopped,
                "the audio graph was rebuilt; sounds on the replaced buses stopped"
            );
        }
        problems
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
        match AudioManager::<DefaultBackend>::new(manager_settings(Default::default())) {
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
        let manager = AudioManager::<CaptureBackend>::new(manager_settings(settings))
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

    /// Replaces the mixer graph (the `[audio]` section of `real.toml`,
    /// ADR-0041), stopping the sounds on the buses it replaces. Every audio
    /// starts with the default graph.
    ///
    /// A graph with structural problems (an unknown parent or send, a cycle, too
    /// many sends; RE7407) is replaced by the default graph, so a typo never
    /// silences a game. An invalid effect is dropped and the rest of its bus
    /// still builds (RE7406). The returned diagnostics say what was dropped.
    pub fn set_graph(&mut self, desc: &AudioGraphDesc) -> Vec<Diagnostic> {
        with_mixer!(self, m => m.set_graph(desc))
    }

    /// The graph in use: the one last set, or the default graph when it had
    /// structural problems.
    pub fn graph(&self) -> &AudioGraphDesc {
        match &self.output {
            Output::Device(m) => m.graph.desc(),
            Output::Headless(m) => m.graph.desc(),
        }
    }

    /// Every bus's and send's level since the previous call (or since the graph
    /// was built), after its effects and volume, sorted by name. A window with
    /// nothing rendered reads as silence (-60 dB).
    pub fn bus_levels(&mut self) -> Vec<BusLevel> {
        with_mixer!(self, m => m.graph.levels())
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
            if let Err(d) = self.apply_one(cmd, &mut problems) {
                problems.push(d);
            }
        }
        problems
    }

    fn apply_one(
        &mut self,
        cmd: &AudioCmd,
        problems: &mut Vec<Diagnostic>,
    ) -> Result<(), Diagnostic> {
        match cmd {
            AudioCmd::PlayOneShot {
                clip,
                position,
                volume,
            } => self.one_shot(clip, position.map(|p| p.to_array()), *volume, "", problems)?,
            AudioCmd::PlayOneShotOnBus {
                clip,
                position,
                volume,
                bus,
            } => self.one_shot(clip, position.map(|p| p.to_array()), *volume, bus, problems)?,
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
                    let bus = m.buses.get(entity).cloned().unwrap_or_default();
                    let voice = m.play(data, position, attenuate, &bus, problems)?;
                    m.sources.insert(*entity, voice);
                });
            }
            AudioCmd::SetSourceBus { entity, bus } => with_mixer!(self, m => {
                m.buses.insert(*entity, bus.clone());
            }),
            AudioCmd::SetBusVolume {
                bus,
                volume_db,
                seconds,
            } => {
                if !volume_db.is_finite() {
                    return Err(Diagnostic::error(
                        codes::INVALID_VOLUME,
                        format!("bus volume {volume_db} dB is not finite"),
                    ));
                }
                let seconds = check_seconds(*seconds, codes::INVALID_VOLUME)?;
                with_mixer!(self, m => quiet(m.graph.set_volume(bus, *volume_db, seconds))?);
            }
            AudioCmd::SetEffectParam {
                bus,
                effect,
                param,
                value,
                seconds,
            } => {
                let seconds = check_seconds(*seconds, codes::INVALID_EFFECT)?;
                with_mixer!(self, m => quiet(m.graph.set_param(bus, *effect, param, value, seconds))?);
            }
            AudioCmd::ApplySnapshot { name, seconds } => {
                let seconds = check_seconds(*seconds, codes::INVALID_EFFECT)?;
                with_mixer!(self, m => quiet(m.graph.apply_snapshot(name, seconds))?);
            }
            AudioCmd::StopSource { entity } => with_mixer!(self, m => {
                m.unattenuated.remove(entity);
                m.buses.remove(entity);
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
                m.unattenuated.clear();
                m.buses.clear();
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

impl Audio {
    fn one_shot(
        &mut self,
        clip: &str,
        position: Option<[f32; 3]>,
        volume: f32,
        bus: &str,
        problems: &mut Vec<Diagnostic>,
    ) -> Result<(), Diagnostic> {
        let data = self
            .clip(clip)?
            .volume(linear_to_decibels(check_volume(volume)?));
        with_mixer!(self, m => {
            let voice = m.play(data, position, true, bus, problems)?;
            m.one_shots.push(voice);
        });
        Ok(())
    }
}

/// Settings for a manager: room for buses and the spatial voices under them.
fn manager_settings<B: Backend>(backend_settings: B::Settings) -> AudioManagerSettings<B>
where
    B::Settings: Default,
{
    AudioManagerSettings {
        capacities: Capacities {
            sub_track_capacity: BUS_CAPACITY,
            ..Default::default()
        },
        backend_settings,
        ..Default::default()
    }
}

/// A graph result whose error is a diagnostic only the first time.
fn quiet(r: Result<(), Option<Diagnostic>>) -> Result<(), Diagnostic> {
    match r {
        Ok(()) | Err(None) => Ok(()),
        Err(Some(d)) => Err(d),
    }
}

fn check_seconds(seconds: f32, code: re_core::Code) -> Result<f32, Diagnostic> {
    if seconds.is_finite() && seconds >= 0.0 {
        Ok(seconds)
    } else {
        Err(Diagnostic::error(
            code,
            format!("tween length {seconds} s is not a finite number >= 0"),
        ))
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
