//! `Audio`: the kira manager, the listener, the mixer graph, and the sounds
//! started by commands.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use kira::backend::Backend;
use kira::command::CommandWriter;
use kira::listener::ListenerHandle;
use kira::sound::PlaybackState;
use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle, StaticSoundSettings};
use kira::track::{SpatialTrackBuilder, SpatialTrackHandle};
use kira::{
    AudioManager, AudioManagerSettings, Capacities, Decibels, DefaultBackend, Frame, Tween,
};
use re_core::files::{FileError, ProjectFiles};
use re_core::{Diagnostic, Location};
use re_types::audio::{AudioCmd, AudioGraphDesc, BusLevel, Rolloff, SoundOp, Spatial};
use re_types::ids::EntityId;

use crate::capture::{CaptureBackend, CaptureSettings};
use crate::codes;
use crate::graph::{BUS_CAPACITY, Graph, tween};
use crate::spatial::{AttenuationBuilder, Curve, Motion, doppler};

/// Changes take effect at once (no fade).
const NOW: Tween = Tween {
    start_time: kira::StartTime::Immediate,
    duration: Duration::ZERO,
    easing: kira::Easing::Linear,
};

/// The fastest a source plays (four octaves up); a faster rate plays at this
/// one. kira steps through every frame of the clip that a sound passes, so its
/// cost grows with the rate, and a huge rate never finishes a buffer.
pub const MAX_RATE: f32 = 16.0;

/// The longest fade a sound control takes, in seconds: an hour. A longer one
/// is refused (RE7411).
pub const MAX_FADE: f32 = 3600.0;

/// The longest glide of a doppler change: a frame's step, up to this.
const MAX_GLIDE: Duration = Duration::from_millis(100);

/// A playing sound.
struct Voice {
    sound: StaticSoundHandle,
    /// A spatial sound's track and how it falls with distance; `None` for a
    /// sound that does not pan.
    spatial: Option<Placed>,
    /// The rate before doppler, and its fade.
    rate: Ramp,
    /// The rate doppler last sent: `None` while the rate is sent as it is,
    /// fades included.
    followed: Option<f64>,
    /// A stop was asked for: kira reports `Stopping` only once the audio
    /// thread reads it, and a fade-out under way outlives its entity.
    stopping: bool,
}

/// A spatial sound's track, curve and motion. Dropping the track handle removes
/// the track, so it lives as long as the sound. Rebuilding the graph removes
/// the bus tracks, and with them the sounds on them.
struct Placed {
    track: SpatialTrackHandle,
    curve: CommandWriter<Curve>,
    position: [f32; 3],
    motion: Motion,
    /// How much the pitch follows motion (`Spatial::doppler`).
    doppler: f32,
}

impl Voice {
    fn finished(&self) -> bool {
        self.sound.state() == PlaybackState::Stopped
    }

    fn has_doppler(&self) -> bool {
        self.spatial.as_ref().is_some_and(|p| p.doppler > 0.0)
    }

    /// Applies a checked op; `now` is the audio clock.
    fn control(&mut self, op: &SoundOp, now: f64) {
        match *op {
            // The audio sync starts sources from their components; a one-shot
            // plays once.
            SoundOp::Start => {}
            SoundOp::Stop { fade } => {
                self.sound.stop(tween(fade));
                self.stopping = true;
            }
            SoundOp::Pause { fade } => self.sound.pause(tween(fade)),
            SoundOp::Resume { fade } => self.sound.resume(tween(fade)),
            SoundOp::Seek { seconds } => self.sound.seek_to(seconds),
            SoundOp::Volume { volume, fade } => self
                .sound
                .set_volume(linear_to_decibels(volume), tween(fade)),
            SoundOp::Rate { rate, fade } => self.set_rate(rate, fade, now),
        }
    }

    /// Fades the rate before doppler to `rate` (a checked rate, at most
    /// [`MAX_RATE`]) over `fade` seconds.
    fn set_rate(&mut self, rate: f32, fade: f32, now: f64) {
        let rate = f64::from(rate.min(MAX_RATE));
        self.rate = Ramp {
            from: self.rate.at(now),
            to: rate,
            start: now,
            seconds: f64::from(fade),
        };
        // With doppler, `follow` sends the product every frame.
        if !self.has_doppler() {
            self.sound.set_playback_rate(rate, tween(fade));
        }
    }

    fn set_position(&mut self, position: [f32; 3]) {
        if let Some(placed) = &mut self.spatial {
            placed.track.set_position(position, NOW);
            placed.position = position;
        }
    }

    fn set_spatial(&mut self, spatial: &Spatial, attenuate: bool) {
        if let Some(placed) = &mut self.spatial {
            placed.curve.write(Curve::new(spatial, attenuate));
            placed.doppler = spatial.doppler;
        }
    }

    /// Doppler, once a frame: records where the sound is at `now`, and sends
    /// its rate times the factor from its motion and the listener's, gliding
    /// over `glide`.
    fn follow(&mut self, now: f64, ear: &Ear, glide: Tween) {
        let Some(placed) = &mut self.spatial else {
            return;
        };
        if placed.doppler <= 0.0 {
            placed.motion = Motion::default();
            if self.followed.take().is_some() {
                // Doppler was turned off: the rate alone, finishing its fade.
                let left = self.rate.remaining(now) as f32;
                self.sound.set_playback_rate(self.rate.to, tween(left));
            }
            return;
        }
        placed.motion.record(now, placed.position);
        let factor = doppler(
            placed.doppler,
            placed.position,
            placed.motion.velocity(),
            ear.position,
            ear.motion.velocity(),
        );
        let rate = (self.rate.at(now) * factor).min(f64::from(MAX_RATE));
        if self.followed != Some(rate) {
            self.followed = Some(rate);
            self.sound.set_playback_rate(rate, glide);
        }
    }
}

/// A value fading linearly from `from` to `to` over `seconds` from `start`, on
/// the audio clock.
#[derive(Clone, Copy, Debug)]
struct Ramp {
    from: f64,
    to: f64,
    start: f64,
    seconds: f64,
}

impl Ramp {
    fn fixed(value: f64) -> Ramp {
        Ramp {
            from: value,
            to: value,
            start: 0.0,
            seconds: 0.0,
        }
    }

    fn at(&self, t: f64) -> f64 {
        let elapsed = t - self.start;
        if elapsed >= self.seconds {
            self.to
        } else {
            self.from + (self.to - self.from) * (elapsed / self.seconds).max(0.0)
        }
    }

    fn remaining(&self, t: f64) -> f64 {
        (self.start + self.seconds - t).max(0.0)
    }
}

/// Where the listener is, and how it moves (for doppler).
#[derive(Default)]
struct Ear {
    position: [f32; 3],
    motion: Motion,
}

/// A one-shot, with the id `SoundControl` names it by; also a source's sound
/// that outlives its entity while it fades out.
struct OneShot {
    id: Option<u64>,
    voice: Voice,
}

/// Everything that depends on the backend type.
struct Mixer<B: Backend> {
    manager: AudioManager<B>,
    listener: ListenerHandle,
    ear: Ear,
    sources: HashMap<EntityId, Voice>,
    one_shots: Vec<OneShot>,
    /// Entities whose spatial sounds only pan (`SetSourceAttenuation`).
    unattenuated: HashSet<EntityId>,
    /// Entities' buses (`SetSourceBus`).
    buses: HashMap<EntityId, String>,
    /// Entities' distance curves and doppler (`SetSourceSpatial`).
    spatial: HashMap<EntityId, Spatial>,
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
            ear: Ear::default(),
            sources: HashMap::new(),
            one_shots: Vec::new(),
            unattenuated: HashSet::new(),
            buses: HashMap::new(),
            spatial: HashMap::new(),
            graph,
        })
    }

    /// Plays `data` on `bus` (the default bus when empty or unknown; the
    /// warning for an unknown name is pushed to `problems` once), at `position`
    /// with `spatial`'s curve, or without one when `attenuate` is false.
    fn play(
        &mut self,
        data: StaticSoundData,
        position: Option<[f32; 3]>,
        spatial: &Spatial,
        attenuate: bool,
        bus: &str,
        problems: &mut Vec<Diagnostic>,
    ) -> Result<Voice, Diagnostic> {
        let listener = self.listener.id();
        let (track, warning) = self.graph.bus_track(bus);
        problems.extend(warning);
        let (sound, spatial) = match position {
            Some(p) => {
                // The track only pans; the effect before the panning applies
                // the curve, which can change while the sound plays.
                let mut builder = SpatialTrackBuilder::new().attenuation_function(None);
                let curve = builder.add_effect(AttenuationBuilder(Curve::new(spatial, attenuate)));
                let mut track = track
                    .add_spatial_sub_track(listener, p, builder)
                    .map_err(|e| limit(&e.to_string()))?;
                let sound = track.play(data).map_err(|e| limit(&e.to_string()))?;
                let placed = Placed {
                    track,
                    curve,
                    position: p,
                    motion: Motion::default(),
                    doppler: spatial.doppler,
                };
                (sound, Some(placed))
            }
            None => (track.play(data).map_err(|e| limit(&e.to_string()))?, None),
        };
        Ok(Voice {
            sound,
            spatial,
            rate: Ramp::fixed(1.0),
            followed: None,
            stopping: false,
        })
    }

    /// Ends a source's sound: at once, unless a fade-out is under way, which
    /// then finishes without its entity.
    fn retire(&mut self, mut voice: Voice) {
        if voice.stopping {
            self.one_shots.push(OneShot { id: None, voice });
        } else {
            voice.sound.stop(NOW);
        }
    }

    /// Doppler, once a frame: where the listener is now, then each source's rate.
    fn follow(&mut self, now: f64, glide: Tween) {
        self.ear.motion.record(now, self.ear.position);
        for voice in self.sources.values_mut() {
            voice.follow(now, &self.ear, glide);
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
        self.one_shots.retain(|s| !s.voice.finished());
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
    files: Option<ProjectFiles>,
    clips: HashMap<String, StaticSoundData>,
    /// The time [`Audio::advance`] has been given, the clock that rate fades
    /// and doppler run on; on the headless backend, also the time rendered and
    /// the frames that came to: counting whole frames of the total keeps steps
    /// of any length exact.
    advanced: Duration,
    advanced_frames: u64,
    /// The last step [`Audio::advance`] was given: doppler glides over it.
    last_step: Duration,
    /// The volume `SetMasterVolume` set, and the player's volumes
    /// ([`Audio::set_user_volume`]), linear.
    master: f32,
    user_master: f32,
    user_buses: BTreeMap<String, f32>,
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
            files: None,
            clips: HashMap::new(),
            advanced: Duration::ZERO,
            advanced_frames: 0,
            last_step: Duration::ZERO,
            master: 1.0,
            user_master: 1.0,
            user_buses: BTreeMap::new(),
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
    ///
    /// The player's volumes ([`Audio::set_user_volume`]) carry over to the new
    /// graph's buses and sends of the same names.
    pub fn set_graph(&mut self, desc: &AudioGraphDesc) -> Vec<Diagnostic> {
        let problems = with_mixer!(self, m => m.set_graph(desc));
        for (bus, volume) in &self.user_buses {
            // A name the new graph lacks waits for a graph that has it.
            let db = linear_to_decibels(*volume);
            with_mixer!(self, m => m.graph.set_user_volume(bus, db).ok());
        }
        problems
    }

    /// Sets the player's volume (the settings menu's sliders, ADR-0059 §8) for
    /// a bus or send, or for everything with `None`: linear, 0 to 1. It scales
    /// the track after its effects, fader and meter, so [`Audio::bus_levels`]
    /// never shows it and no snapshot changes it. It keeps through
    /// [`Audio::set_graph`] and `StopAll`, and for everything it adds to
    /// `SetMasterVolume` in decibels.
    ///
    /// # Errors
    ///
    /// [`codes::INVALID_VOLUME`] for a volume outside 0 to 1, and
    /// [`codes::UNKNOWN_BUS`] for a name the graph does not have; neither
    /// changes anything.
    pub fn set_user_volume(&mut self, bus: Option<&str>, volume: f32) -> Result<(), Diagnostic> {
        if !(volume.is_finite() && (0.0..=1.0).contains(&volume)) {
            return Err(Diagnostic::error(
                codes::INVALID_VOLUME,
                format!("user volume {volume} is not a number from 0 to 1"),
            ));
        }
        match bus {
            None => {
                self.user_master = volume;
                self.set_main_volume();
            }
            Some(bus) => {
                let db = linear_to_decibels(volume);
                with_mixer!(self, m => m.graph.set_user_volume(bus, db))?;
                self.user_buses.insert(bus.to_string(), volume);
            }
        }
        Ok(())
    }

    /// The main track's volume: `SetMasterVolume`'s and the player's, added in
    /// decibels.
    fn set_main_volume(&mut self) {
        let db = linear_to_decibels(self.master).0 + linear_to_decibels(self.user_master).0;
        with_mixer!(self, m => m.manager.main_track().set_volume(Decibels(db), NOW));
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

    /// Moves the audio clock on by `dt`, once a frame: rate fades and doppler
    /// follow it. On the headless backend it also renders `dt` of audio and
    /// drops it, so sounds play on and end, their voices are freed and the
    /// meters read what plays, as on a device, which renders by itself.
    pub fn advance(&mut self, dt: Duration) {
        self.advanced = self.advanced.saturating_add(dt);
        self.last_step = dt;
        if !self.is_headless() {
            return;
        }
        let due = self.advanced.as_nanos() * u128::from(self.sample_rate) / 1_000_000_000;
        let due = u64::try_from(due).unwrap_or(u64::MAX);
        let mut frames = due.saturating_sub(self.advanced_frames);
        self.advanced_frames = due;
        // A long `dt` renders a quarter of a second at a time, so the buffer
        // stays small.
        let piece = u64::from(self.sample_rate / 4).max(1);
        while frames > 0 {
            let n = frames.min(piece);
            self.render_headless(n as usize);
            frames -= n;
        }
    }

    /// Sets the directory clip paths are relative to (the project root). Without
    /// one, only clips registered with [`Audio::insert_clip`] play.
    pub fn set_asset_root(&mut self, root: impl Into<PathBuf>) {
        self.set_files(ProjectFiles::project(root));
    }

    /// Reads clips through the project's files (ADR-0037); clip names are
    /// project paths.
    pub fn set_files(&mut self, files: ProjectFiles) {
        self.files = Some(files);
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
        let Some(files) = &self.files else {
            return Err(not_found("it is not registered and no asset root is set"));
        };
        let hint = "clip names are project-relative paths such as `assets/sfx/pickup.wav`";
        let file = match files.read_source(name) {
            Ok(bytes) => bytes,
            Err(e) if e.is_not_found() => return Err(not_found("no such file").with_hint(hint)),
            Err(e @ FileError::InvalidPath { .. }) => {
                return Err(not_found(&e.to_string()).with_hint(hint));
            }
            Err(e) => {
                return Err(Diagnostic::error(
                    codes::CLIP_DECODE_FAILED,
                    format!("cannot read audio clip `{name}`: {e}"),
                )
                .at(Location::file(name)));
            }
        };
        let decode_failed = |why: String| {
            Diagnostic::error(
                codes::CLIP_DECODE_FAILED,
                format!("cannot decode audio clip `{name}`: {why}"),
            )
            .at(Location::file(name))
            .with_hint("M1 plays WAV files (PCM or float, mono or stereo)")
        };
        let data = crate::wav::decode(&file).map_err(|e| decode_failed(e.to_string()))?;
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

    /// True while the entity's sound plays; paused, it still counts.
    pub fn is_source_playing(&mut self, entity: EntityId) -> bool {
        with_mixer!(self, m => m.sources.get(&entity).is_some_and(|v| !v.finished()))
    }

    /// Applies one frame's commands, in order, then doppler.
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
        let (now, glide) = (
            self.now(),
            tween(self.last_step.min(MAX_GLIDE).as_secs_f32()),
        );
        with_mixer!(self, m => m.follow(now, glide));
        problems
    }

    /// The audio clock, in seconds.
    fn now(&self) -> f64 {
        self.advanced.as_secs_f64()
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
                id,
            } => self.one_shot(
                clip,
                position.map(|p| p.to_array()),
                *volume,
                "",
                *id,
                problems,
            )?,
            AudioCmd::PlayOneShotOnBus {
                clip,
                position,
                volume,
                bus,
                id,
            } => self.one_shot(
                clip,
                position.map(|p| p.to_array()),
                *volume,
                bus,
                *id,
                problems,
            )?,
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
                    if let Some(old) = m.sources.remove(entity) {
                        m.retire(old);
                    }
                    let attenuate = !m.unattenuated.contains(entity);
                    let bus = m.buses.get(entity).cloned().unwrap_or_default();
                    let spatial = m.spatial.get(entity).copied().unwrap_or_default();
                    let voice = m.play(data, position, &spatial, attenuate, &bus, problems)?;
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
                m.spatial.remove(entity);
                if let Some(v) = m.sources.remove(entity) {
                    m.retire(v);
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
                if let Some(v) = m.sources.get_mut(entity) {
                    v.set_position(position.to_array());
                }
            }),
            AudioCmd::SetSourceRate { entity, rate } => {
                check_rate(*rate)?;
                let now = self.now();
                with_mixer!(self, m => {
                    if let Some(v) = m.sources.get_mut(entity) {
                        v.set_rate(*rate, 0.0, now);
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
                m.ear.position = position.to_array();
            }),
            AudioCmd::SetMasterVolume { volume } => {
                self.master = check_volume(*volume)?;
                self.set_main_volume();
            }
            AudioCmd::StopAll => with_mixer!(self, m => {
                m.unattenuated.clear();
                m.buses.clear();
                m.spatial.clear();
                // A restart also starts from the graph's own volumes and effects.
                quiet(m.graph.apply_snapshot(crate::DEFAULT_SNAPSHOT, 0.0))?;
                for (_, mut v) in m.sources.drain() {
                    v.sound.stop(NOW);
                }
                for mut s in m.one_shots.drain(..) {
                    s.voice.sound.stop(NOW);
                }
            }),
            AudioCmd::SoundControl { id, op } => {
                check_op(op)?;
                let now = self.now();
                with_mixer!(self, m => {
                    for s in m.one_shots.iter_mut().filter(|s| s.id == Some(*id)) {
                        s.voice.control(op, now);
                    }
                });
            }
            AudioCmd::SourceControl { entity, op } => {
                check_op(op)?;
                let now = self.now();
                with_mixer!(self, m => {
                    if let Some(v) = m.sources.get_mut(entity) {
                        v.control(op, now);
                    }
                });
            }
            AudioCmd::SetSourceSpatial { entity, spatial } => {
                check_spatial(spatial)?;
                with_mixer!(self, m => {
                    m.spatial.insert(*entity, *spatial);
                    let attenuate = !m.unattenuated.contains(entity);
                    if let Some(v) = m.sources.get_mut(entity) {
                        v.set_spatial(spatial, attenuate);
                    }
                });
            }
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
        id: Option<u64>,
        problems: &mut Vec<Diagnostic>,
    ) -> Result<(), Diagnostic> {
        let data = self
            .clip(clip)?
            .volume(linear_to_decibels(check_volume(volume)?));
        with_mixer!(self, m => {
            let voice = m.play(data, position, &Spatial::default(), true, bus, problems)?;
            m.one_shots.push(OneShot { id, voice });
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

fn check_rate(rate: f32) -> Result<f32, Diagnostic> {
    if rate.is_finite() && rate > 0.0 {
        Ok(rate)
    } else {
        Err(Diagnostic::error(
            codes::INVALID_RATE,
            format!("playback rate {rate} is not a finite number > 0"),
        ))
    }
}

/// Checks an op's values: a fade must be from 0 to [`MAX_FADE`] seconds and a
/// seek a finite number of seconds, 0 or more (RE7411); a volume and a rate as
/// their own commands say.
fn check_op(op: &SoundOp) -> Result<(), Diagnostic> {
    let fade = |fade: f32| {
        if (0.0..=MAX_FADE).contains(&fade) {
            Ok(())
        } else {
            Err(Diagnostic::error(
                codes::INVALID_SOUND_CONTROL,
                format!("fade {fade} s is not a number of seconds from 0 to {MAX_FADE}"),
            ))
        }
    };
    match *op {
        SoundOp::Start => Ok(()),
        SoundOp::Stop { fade: f } | SoundOp::Pause { fade: f } | SoundOp::Resume { fade: f } => {
            fade(f)
        }
        SoundOp::Seek { seconds } if seconds.is_finite() && seconds >= 0.0 => Ok(()),
        SoundOp::Seek { seconds } => Err(Diagnostic::error(
            codes::INVALID_SOUND_CONTROL,
            format!("seek to {seconds} s is not a finite time >= 0"),
        )),
        SoundOp::Volume { volume, fade: f } => check_volume(volume).and_then(|_| fade(f)),
        SoundOp::Rate { rate, fade: f } => check_rate(rate).and_then(|_| fade(f)),
    }
}

/// Checks spatial settings (RE7412): distances finite, `min_distance` 0 or
/// more and `max_distance` above it (and `min_distance` above 0 for `inverse`,
/// whose level halves with each doubling of it); `doppler` finite, 0 or more.
fn check_spatial(s: &Spatial) -> Result<(), Diagnostic> {
    let why = if !(s.min_distance.is_finite() && s.min_distance >= 0.0) {
        format!(
            "min_distance {} is not a finite number >= 0",
            s.min_distance
        )
    } else if !(s.max_distance.is_finite() && s.max_distance > s.min_distance) {
        format!(
            "max_distance {} is not a finite number above min_distance {}",
            s.max_distance, s.min_distance
        )
    } else if s.rolloff == Rolloff::Inverse && s.min_distance == 0.0 {
        "the inverse rolloff needs a min_distance above 0".to_string()
    } else if !(s.doppler.is_finite() && s.doppler >= 0.0) {
        format!("doppler {} is not a finite number >= 0", s.doppler)
    } else {
        return Ok(());
    };
    Err(Diagnostic::error(
        codes::INVALID_SPATIAL,
        format!("spatial sound settings refused: {why}"),
    ))
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
        Decibels((20.0 * libm::log10f(volume)).max(Decibels::SILENCE.0))
    }
}

/// A generated sine tone as kira sound data: procedural audio needs no files.
pub fn sine(freq: f32, seconds: f32, sample_rate: u32, amplitude: f32) -> StaticSoundData {
    let n = (seconds.max(0.0) * sample_rate as f32) as usize;
    let frames: Arc<[Frame]> = (0..n)
        .map(|i| {
            let phase = std::f32::consts::TAU * freq * i as f32 / sample_rate as f32;
            Frame::from_mono(amplitude * libm::sinf(phase))
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
