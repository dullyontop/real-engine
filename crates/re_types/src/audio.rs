//! Audio commands: what `re_app` asks `re_audio` to do each frame.
//!
//! Entity-driven sounds (the scene's `AudioSource` component) are keyed by
//! [`EntityId`]; one-shot sounds requested by scripts are fire-and-forget. Clip
//! names are project asset paths such as `assets/sfx/pickup.wav`.
//!
//! The mixer is a tree of named buses with effects, plus send tracks
//! ([`AudioGraphDesc`], the `[audio]` section of `real.toml`, ADR-0041). Sounds
//! play on a bus; commands change bus volumes and effect parameters, or apply a
//! mixer snapshot, but never the graph's structure.

use std::collections::BTreeMap;

use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};

use crate::ids::EntityId;

/// One audio command.
///
/// Serialized with a `cmd` tag, for example `{"cmd": "stop_source", "entity": 7}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum AudioCmd {
    /// Play a clip once, not tied to an entity.
    PlayOneShot {
        /// The clip's asset path.
        clip: String,
        /// Where it plays in the world; `None` for non-spatial (UI) sounds.
        position: Option<Vec3>,
        /// Volume, linear, 1 is unchanged.
        volume: f32,
    },
    /// Start (or restart) an entity's sound.
    StartSource {
        /// The entity.
        entity: EntityId,
        /// The clip's asset path.
        clip: String,
        /// Volume, linear.
        volume: f32,
        /// Loop until stopped.
        looping: bool,
        /// Attenuate and pan by distance to the listener.
        spatial: bool,
        /// World position.
        position: Vec3,
    },
    /// Stop an entity's sound.
    StopSource {
        /// The entity.
        entity: EntityId,
    },
    /// Change an entity's volume.
    SetSourceVolume {
        /// The entity.
        entity: EntityId,
        /// Volume, linear.
        volume: f32,
    },
    /// Move an entity's sound.
    SetSourcePosition {
        /// The entity.
        entity: EntityId,
        /// World position.
        position: Vec3,
    },
    /// Move the listener (usually the active camera).
    SetListener {
        /// World position.
        position: Vec3,
        /// World rotation (the listener faces −Z).
        rotation: Quat,
    },
    /// Change an entity's playback rate (1 is normal speed; it shifts the
    /// pitch with the speed).
    SetSourceRate {
        /// The entity.
        entity: EntityId,
        /// The rate, positive.
        rate: f32,
    },
    /// Jump to a time in an entity's sound.
    SeekSource {
        /// The entity.
        entity: EntityId,
        /// Seconds from the start of the clip.
        seconds: f64,
    },
    /// Turn distance attenuation of an entity's spatial sound on or off (on by
    /// default). Sent before `StartSource`, it applies to the entity's sounds
    /// until its next `StopSource` (or `StopAll`). Off, the sound still pans
    /// by direction and its volume is only what `volume` says, so the sender
    /// can apply its own distance model.
    SetSourceAttenuation {
        /// The entity.
        entity: EntityId,
        /// True to attenuate by distance.
        enabled: bool,
    },
    /// Set the master volume.
    SetMasterVolume {
        /// Volume, linear.
        volume: f32,
    },
    /// Stop every sound and return the mixer to the graph's own volumes and
    /// effect parameters (for a restart).
    StopAll,
    /// Choose the bus an entity's sound plays on. Sent before `StartSource`, it
    /// applies to the entity's sounds until its next `StopSource` (or `StopAll`).
    /// Without it, sounds play on the graph's `default_bus`.
    SetSourceBus {
        /// The entity.
        entity: EntityId,
        /// A bus name from the graph.
        bus: String,
    },
    /// Play a clip once on a bus, not tied to an entity.
    PlayOneShotOnBus {
        /// The clip's asset path.
        clip: String,
        /// Where it plays in the world; `None` for non-spatial (UI) sounds.
        position: Option<Vec3>,
        /// Volume, linear, 1 is unchanged.
        volume: f32,
        /// A bus name from the graph.
        bus: String,
    },
    /// Tween a bus's or a send's volume.
    SetBusVolume {
        /// A bus or send name from the graph.
        bus: String,
        /// The volume in decibels (0 is unchanged).
        volume_db: f32,
        /// The tween's length in seconds; 0 is at once.
        seconds: f32,
    },
    /// Tween one parameter of one effect on a bus or send.
    SetEffectParam {
        /// A bus or send name from the graph.
        bus: String,
        /// The effect's position in the bus's `effects` list, from 0.
        effect: u32,
        /// A field name of the effect's description, such as `cutoff_hz`.
        param: String,
        /// The new value.
        value: EffectParam,
        /// The tween's length in seconds; 0 is at once. Choices (a filter's
        /// `mode`) change at once.
        seconds: f32,
    },
    /// Tween to a mixer snapshot from the graph; `default` returns to the
    /// graph's own values.
    ApplySnapshot {
        /// A snapshot name from the graph, or `default`.
        name: String,
        /// The tween's length in seconds; 0 is at once.
        seconds: f32,
    },
}

/// The mixer graph: the `[audio]` section of `real.toml` (ADR-0041).
///
/// Buses form a tree under the main track; each bus may also feed send tracks,
/// which sum into the main track. The default value, used when a project has no
/// `[audio]` section, has the buses `music`, `sfx` and `ui` under the main track
/// and no effects. A missing `buses` key keeps those three.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioGraphDesc {
    /// The bus that sounds without a bus play on.
    pub default_bus: String,
    /// The buses, by name.
    pub buses: BTreeMap<String, BusDesc>,
    /// The send tracks, by name (at most 15).
    pub sends: BTreeMap<String, SendDesc>,
    /// Mixer snapshots, by name. `default` is reserved for the graph's own values.
    pub snapshots: BTreeMap<String, SnapshotDesc>,
}

impl Default for AudioGraphDesc {
    fn default() -> Self {
        AudioGraphDesc {
            default_bus: "sfx".into(),
            buses: ["music", "sfx", "ui"]
                .into_iter()
                .map(|name| (name.to_string(), BusDesc::default()))
                .collect(),
            sends: BTreeMap::new(),
            snapshots: BTreeMap::new(),
        }
    }
}

impl AudioGraphDesc {
    /// True for the default graph (what a project without `[audio]` gets).
    pub fn is_default(&self) -> bool {
        *self == AudioGraphDesc::default()
    }
}

/// One bus: a mixer track that sounds play on.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BusDesc {
    /// The parent bus; empty for the main track.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub parent: String,
    /// The bus's volume in decibels (0 is unchanged).
    pub volume_db: f32,
    /// The sends this bus feeds, after its effects and volume: send name to
    /// level in decibels.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub sends: BTreeMap<String, f32>,
    /// The bus's effects, run in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectDesc>,
}

/// One send track: it takes what buses send to it and outputs to the main track.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SendDesc {
    /// The send's volume in decibels (0 is unchanged).
    pub volume_db: f32,
    /// The send's effects, run in order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<EffectDesc>,
}

/// A mixer snapshot: bus volumes and effect parameters to tween to. Fields it
/// does not name keep their current values.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SnapshotDesc {
    /// Bus or send name to volume in decibels.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub buses: BTreeMap<String, f32>,
    /// Effect parameters to set.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<SnapshotEffect>,
}

/// The parameters a snapshot sets on one effect, for example
/// `{ bus = "sfx", effect = 0, cutoff_hz = 600.0 }`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SnapshotEffect {
    /// A bus or send name.
    pub bus: String,
    /// The effect's position in the bus's `effects` list, from 0.
    pub effect: u32,
    /// Field name of the effect's description to value.
    #[serde(flatten)]
    pub params: BTreeMap<String, EffectParam>,
}

/// A value for one effect parameter: a number, or a choice such as a filter's
/// `mode` (`"high_pass"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EffectParam {
    /// A number in the field's unit.
    Number(f32),
    /// A choice, by its name in the file format.
    Choice(String),
}

/// One effect on a bus or send, serialized with a `kind` tag, for example
/// `{ kind = "filter", mode = "high_pass", cutoff_hz = 120.0 }`. Missing fields
/// take kira's defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectDesc {
    /// Freeverb.
    Reverb(Reverb),
    /// A 12 dB per octave state-variable filter; steeper filters are several.
    Filter(Filter),
    /// An echo.
    Delay(Delay),
    /// A compressor (no sidechain, knee or lookahead).
    Compressor(Compressor),
    /// One band of equalisation.
    Eq(EqBand),
    /// Clipping distortion.
    Distortion(Distortion),
}

impl EffectDesc {
    /// The `kind` tag: `reverb`, `filter`, `delay`, `compressor`, `eq` or
    /// `distortion`.
    pub fn kind(&self) -> &'static str {
        match self {
            EffectDesc::Reverb(_) => "reverb",
            EffectDesc::Filter(_) => "filter",
            EffectDesc::Delay(_) => "delay",
            EffectDesc::Compressor(_) => "compressor",
            EffectDesc::Eq(_) => "eq",
            EffectDesc::Distortion(_) => "distortion",
        }
    }
}

/// [`EffectDesc::Reverb`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Reverb {
    /// How long the tail lasts, 0 to 1.
    pub feedback: f32,
    /// How much high frequencies are absorbed, 0 to 1.
    pub damping: f32,
    /// Stereo width of the tail, 0 (mono) to 1.
    pub stereo_width: f32,
    /// Wet share of the output, 0 (dry) to 1 (wet only).
    pub mix: f32,
}

impl Default for Reverb {
    fn default() -> Self {
        Reverb {
            feedback: 0.9,
            damping: 0.1,
            stereo_width: 1.0,
            mix: 0.5,
        }
    }
}

/// [`EffectDesc::Filter`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Filter {
    /// Which frequencies pass.
    pub mode: FilterMode,
    /// The cutoff frequency in Hz, 10 to 20,000.
    pub cutoff_hz: f32,
    /// Resonance at the cutoff, 0 to 1.
    pub resonance: f32,
    /// Wet share of the output, 0 to 1.
    pub mix: f32,
}

impl Default for Filter {
    fn default() -> Self {
        Filter {
            mode: FilterMode::LowPass,
            cutoff_hz: 1000.0,
            resonance: 0.0,
            mix: 1.0,
        }
    }
}

/// Which frequencies a [`Filter`] lets through.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    /// Below the cutoff.
    #[default]
    LowPass,
    /// Above the cutoff.
    HighPass,
    /// Around the cutoff.
    BandPass,
    /// All but around the cutoff.
    Notch,
}

/// [`EffectDesc::Delay`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Delay {
    /// The delay time in seconds, 0.001 to 10; fixed once the bus is built.
    pub seconds: f32,
    /// The level of each repeat relative to the previous one, in decibels, 0 or less.
    pub feedback_db: f32,
    /// Wet share of the output, 0 to 1.
    pub mix: f32,
}

impl Default for Delay {
    fn default() -> Self {
        Delay {
            seconds: 0.5,
            feedback_db: -6.0,
            mix: 0.5,
        }
    }
}

/// [`EffectDesc::Compressor`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Compressor {
    /// The level above which the volume is reduced, in decibels.
    pub threshold_db: f32,
    /// How much it is reduced: 1 is not at all, 4 means 4 dB over the threshold
    /// comes out as 1 dB. At least 1.
    pub ratio: f32,
    /// How fast the reduction starts, in milliseconds.
    pub attack_ms: f32,
    /// How fast it ends, in milliseconds.
    pub release_ms: f32,
    /// Gain after the reduction, in decibels.
    pub makeup_db: f32,
    /// Wet share of the output, 0 to 1.
    pub mix: f32,
}

impl Default for Compressor {
    fn default() -> Self {
        Compressor {
            threshold_db: 0.0,
            ratio: 1.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 0.0,
            mix: 1.0,
        }
    }
}

/// [`EffectDesc::Eq`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EqBand {
    /// The band's shape (`kind` is the effect's tag, so this is `shape`).
    pub shape: EqShape,
    /// The band's centre or corner frequency in Hz, 10 to 20,000.
    pub hz: f32,
    /// The gain in decibels; negative cuts.
    pub gain_db: f32,
    /// The band's width: higher is narrower. Above 0.
    pub q: f32,
}

impl Default for EqBand {
    fn default() -> Self {
        EqBand {
            shape: EqShape::Bell,
            hz: 1000.0,
            gain_db: 0.0,
            q: 1.0,
        }
    }
}

/// The shape of an [`EqBand`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqShape {
    /// Around the frequency.
    #[default]
    Bell,
    /// At and below the frequency.
    LowShelf,
    /// At and above the frequency.
    HighShelf,
}

/// [`EffectDesc::Distortion`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Distortion {
    /// How the signal is clipped (`kind` is the effect's tag, so this is `mode`).
    pub mode: DistortionMode,
    /// Gain before clipping, in decibels.
    pub drive_db: f32,
    /// Wet share of the output, 0 to 1.
    pub mix: f32,
}

impl Default for Distortion {
    fn default() -> Self {
        Distortion {
            mode: DistortionMode::HardClip,
            drive_db: 0.0,
            mix: 1.0,
        }
    }
}

/// How a [`Distortion`] clips.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistortionMode {
    /// Clamp to -1 to 1.
    #[default]
    HardClip,
    /// Bend smoothly towards -1 and 1.
    SoftClip,
}

/// A bus's or send's measured output over a window (see `re_audio`'s
/// `Audio::bus_levels`), after its effects and volume.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BusLevel {
    /// The bus or send name.
    pub bus: String,
    /// The largest sample magnitude of either channel, in decibels (-60 is silence).
    pub peak_db: f32,
    /// The root mean square of both channels, in decibels (-60 is silence).
    pub rms_db: f32,
}
