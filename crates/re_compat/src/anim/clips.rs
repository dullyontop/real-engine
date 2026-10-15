//! Where an animation's keyframes come from.
//!
//! An `Animation.AnimationId` is resolved, in order, through:
//! 1. the project's substitute table (`[compat.animations]` in `real.toml`,
//!    passed in with [`super::set_substitutes`]), keyed by the whole id or its
//!    number, whose value is `builtin:<clip>` or the dotted path of a
//!    `KeyframeSequence` in the place (`game.ReplicatedStorage.Anims.Wave`);
//! 2. the built-in table, which maps the numbers of the standard character
//!    animations to the engine's own clips (`assets/rigs/clips/`);
//! 3. otherwise an empty clip, with an `RE9201` warning.
//!
//! The built-in clips are written for this repository: simple R6 and R15
//! idle, walk, run, jump, fall, climb and sit cycles.

use std::collections::BTreeMap;
use std::sync::Arc;

use re_anim::{
    JointKey, Keyframe, KeyframeClip, Marker, PoseDirection, PoseEasing, PoseStyle, Priority,
};
use re_reflect::Value;
use re_scene::{Entity, World};
use serde::Deserialize;

use super::value_cframe;
use crate::class::ClassRegistry;
use crate::datatypes::CFrame;
use crate::instance::tree;

/// The built-in clips, by name.
const BUILTIN: &[(&str, &str)] = &[
    (
        "r6_idle",
        include_str!("../../assets/rigs/clips/r6_idle.json"),
    ),
    (
        "r6_walk",
        include_str!("../../assets/rigs/clips/r6_walk.json"),
    ),
    (
        "r6_run",
        include_str!("../../assets/rigs/clips/r6_run.json"),
    ),
    (
        "r6_jump",
        include_str!("../../assets/rigs/clips/r6_jump.json"),
    ),
    (
        "r6_fall",
        include_str!("../../assets/rigs/clips/r6_fall.json"),
    ),
    (
        "r6_climb",
        include_str!("../../assets/rigs/clips/r6_climb.json"),
    ),
    (
        "r6_sit",
        include_str!("../../assets/rigs/clips/r6_sit.json"),
    ),
    (
        "r15_idle",
        include_str!("../../assets/rigs/clips/r15_idle.json"),
    ),
    (
        "r15_walk",
        include_str!("../../assets/rigs/clips/r15_walk.json"),
    ),
    (
        "r15_run",
        include_str!("../../assets/rigs/clips/r15_run.json"),
    ),
    (
        "r15_jump",
        include_str!("../../assets/rigs/clips/r15_jump.json"),
    ),
    (
        "r15_fall",
        include_str!("../../assets/rigs/clips/r15_fall.json"),
    ),
    (
        "r15_climb",
        include_str!("../../assets/rigs/clips/r15_climb.json"),
    ),
    (
        "r15_sit",
        include_str!("../../assets/rigs/clips/r15_sit.json"),
    ),
];

/// The asset numbers of the standard character animations, with the
/// built-in clip that stands in for each.
const STANDARD_IDS: &[(u64, &str)] = &[
    // R6
    (180435571, "r6_idle"),
    (180435792, "r6_idle"),
    (180426354, "r6_walk"),
    (125750702, "r6_jump"),
    (180436148, "r6_fall"),
    (180436334, "r6_climb"),
    (178130996, "r6_sit"),
    (182393478, "r6_idle"),
    // R15
    (507766666, "r15_idle"),
    (507766951, "r15_idle"),
    (507777826, "r15_walk"),
    (507767714, "r15_run"),
    (507765000, "r15_jump"),
    (507767968, "r15_fall"),
    (507765644, "r15_climb"),
    (2506281703, "r15_sit"),
    (507768375, "r15_idle"),
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClipFile {
    format: String,
    version: u32,
    name: String,
    #[allow(dead_code)]
    doc: String,
    looped: bool,
    priority: String,
    keyframes: Vec<KeyframeDoc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyframeDoc {
    time: f32,
    #[serde(default)]
    name: String,
    poses: BTreeMap<String, PoseDoc>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PoseDoc {
    /// `CFrame.Angles(x, y, z)`, radians.
    #[serde(default)]
    angles: [f32; 3],
    #[serde(default)]
    position: [f32; 3],
}

/// Parses a built-in clip file.
fn parse_builtin(name: &str, text: &str) -> Result<KeyframeClip, String> {
    let f: ClipFile = serde_json::from_str(text).map_err(|e| format!("clip {name}: {e}"))?;
    if f.format != "real-engine/compat-clip" || f.version != 1 || f.name != name {
        return Err(format!("clip {name}: wrong format, version or name"));
    }
    let priority = Priority::from_name(&f.priority).map_err(|d| d.message)?;
    let keyframes = f
        .keyframes
        .into_iter()
        .map(|k| Keyframe {
            time: k.time,
            name: k.name,
            joints: k
                .poses
                .into_iter()
                .map(|(joint, p)| {
                    let c = CFrame::from_euler_xyz(p.angles[0], p.angles[1], p.angles[2]);
                    let [x, y, z, w] = c.quaternion();
                    (
                        joint,
                        JointKey {
                            translation: glam::Vec3::from(p.position),
                            rotation: glam::Quat::from_xyzw(x, y, z, w),
                            weight: 1.0,
                            easing: PoseEasing::LINEAR,
                        },
                    )
                })
                .collect(),
        })
        .collect();
    KeyframeClip::new(keyframes, Vec::new(), f.looped, priority).map_err(|d| d.message)
}

/// The built-in clip `name`.
pub fn builtin(name: &str) -> Option<Arc<KeyframeClip>> {
    let (_, text) = BUILTIN.iter().find(|(n, _)| *n == name)?;
    // The files are tested to parse (`builtin_clips_parse`).
    parse_builtin(name, text).ok().map(Arc::new)
}

/// The names of the built-in clips.
pub fn builtin_names() -> impl Iterator<Item = &'static str> {
    BUILTIN.iter().map(|(n, _)| *n)
}

/// The asset number in an id (`rbxassetid://123`,
/// `http://www.roblox.com/asset/?id=123`, or `123`).
pub fn asset_number(id: &str) -> Option<u64> {
    let tail = match id.rfind("id=") {
        Some(i) => &id[i + 3..],
        None => id.rsplit("://").next().unwrap_or(id),
    };
    let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Where an id resolves to, before any instance is read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A built-in clip.
    Builtin(String),
    /// A `KeyframeSequence` at this dotted path from `game`.
    Path(String),
    /// Nothing: an empty clip.
    Unknown,
}

/// Resolves an animation id through `substitutes` and the built-in table.
pub fn resolve(id: &str, substitutes: &BTreeMap<String, String>) -> Source {
    let number = asset_number(id);
    let sub = substitutes
        .get(id)
        .or_else(|| number.and_then(|n| substitutes.get(&n.to_string())));
    if let Some(v) = sub {
        return match v.strip_prefix("builtin:") {
            Some(name) => Source::Builtin(name.to_string()),
            None => Source::Path(v.clone()),
        };
    }
    if let Some(name) = id.strip_prefix("builtin:") {
        return Source::Builtin(name.to_string());
    }
    match number.and_then(|n| STANDARD_IDS.iter().find(|(i, _)| *i == n)) {
        Some((_, name)) => Source::Builtin(name.to_string()),
        None => Source::Unknown,
    }
}

/// The instance at a dotted path from `game` (`game.ReplicatedStorage.X`,
/// with or without the leading `game.`).
pub fn find_path(w: &World, game: Entity, path: &str) -> Option<Entity> {
    let rest = path.strip_prefix("game.").unwrap_or(path);
    let mut e = game;
    for name in rest.split('.').filter(|s| !s.is_empty()) {
        e = tree::find_child(w, e, name, false)?;
    }
    Some(e)
}

fn prop(w: &World, classes: &ClassRegistry, e: Entity, name: &str) -> Option<Value> {
    classes.get_property(w, e, name).ok()
}

fn number(v: Option<Value>, default: f32) -> f32 {
    match v {
        Some(Value::Float(x)) => x as f32,
        Some(Value::Int(i)) => i as f32,
        _ => default,
    }
}

fn enum_name(v: Option<Value>, default: &str) -> String {
    match v {
        Some(Value::Enum(s)) | Some(Value::Str(s)) => s,
        _ => default.to_string(),
    }
}

/// Reads the poses under `parent` (nested by part) into `joints`.
fn read_poses(
    w: &World,
    classes: &ClassRegistry,
    parent: Entity,
    joints: &mut BTreeMap<String, JointKey>,
) {
    for p in tree::children(w, parent) {
        if tree::class(w, p) != Some("Pose") {
            continue;
        }
        let cf = prop(w, classes, p, "CFrame")
            .and_then(|v| value_cframe(&v))
            .unwrap_or_default();
        let [x, y, z, qw] = cf.quaternion();
        let style = PoseStyle::from_name(&enum_name(prop(w, classes, p, "EasingStyle"), "Linear"))
            .unwrap_or(PoseStyle::Linear);
        let direction =
            PoseDirection::from_name(&enum_name(prop(w, classes, p, "EasingDirection"), "In"))
                .unwrap_or(PoseDirection::In);
        joints.insert(
            tree::name(w, p),
            JointKey {
                translation: glam::Vec3::from(cf.p),
                rotation: glam::Quat::from_xyzw(x, y, z, qw),
                weight: number(prop(w, classes, p, "Weight"), 1.0),
                easing: PoseEasing::new(style, direction),
            },
        );
        read_poses(w, classes, p, joints);
    }
}

/// Builds a clip from a `KeyframeSequence` instance.
pub fn from_sequence(
    w: &World,
    classes: &ClassRegistry,
    seq: Entity,
) -> Result<KeyframeClip, String> {
    let looped = !matches!(prop(w, classes, seq, "Loop"), Some(Value::Bool(false)));
    let priority = Priority::from_name(&enum_name(prop(w, classes, seq, "Priority"), "Action"))
        .unwrap_or(Priority::Action);
    let mut keyframes = Vec::new();
    let mut markers = Vec::new();
    for k in tree::children(w, seq) {
        if tree::class(w, k) != Some("Keyframe") {
            continue;
        }
        let time = number(prop(w, classes, k, "Time"), 0.0);
        let mut joints = BTreeMap::new();
        read_poses(w, classes, k, &mut joints);
        for m in tree::children(w, k) {
            if tree::class(w, m) == Some("KeyframeMarker") {
                let value = match prop(w, classes, m, "Value") {
                    Some(Value::Str(s)) => s,
                    _ => String::new(),
                };
                markers.push(Marker {
                    time,
                    name: tree::name(w, m),
                    value,
                });
            }
        }
        keyframes.push(Keyframe {
            time,
            name: tree::name(w, k),
            joints,
        });
    }
    KeyframeClip::new(keyframes, markers, looped, priority).map_err(|d| d.message)
}

/// An empty clip: a track over it plays for no time and moves nothing.
pub fn empty() -> Arc<KeyframeClip> {
    Arc::new(
        KeyframeClip::new(Vec::new(), Vec::new(), false, Priority::Core)
            .expect("an empty clip is valid"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_clips_parse() {
        for (name, text) in BUILTIN {
            let c = parse_builtin(name, text).unwrap();
            assert!(c.length() > 0.0, "{name}");
        }
        for (_, name) in STANDARD_IDS {
            assert!(builtin(name).is_some(), "{name}");
        }
    }

    #[test]
    fn ids_resolve_through_substitutes_then_the_standard_table() {
        let mut subs = BTreeMap::new();
        subs.insert("42".to_string(), "builtin:r6_run".to_string());
        subs.insert(
            "rbxassetid://7".to_string(),
            "game.ReplicatedStorage.Wave".to_string(),
        );
        assert_eq!(asset_number("rbxassetid://42"), Some(42));
        assert_eq!(
            asset_number("http://www.roblox.com/asset/?id=180426354"),
            Some(180426354)
        );
        assert_eq!(
            resolve("rbxassetid://42", &subs),
            Source::Builtin("r6_run".into())
        );
        assert_eq!(
            resolve("rbxassetid://7", &subs),
            Source::Path("game.ReplicatedStorage.Wave".into())
        );
        assert_eq!(
            resolve("http://www.roblox.com/asset/?id=180426354", &subs),
            Source::Builtin("r6_walk".into())
        );
        assert_eq!(resolve("rbxassetid://1", &subs), Source::Unknown);
        assert_eq!(resolve("", &subs), Source::Unknown);
    }
}
