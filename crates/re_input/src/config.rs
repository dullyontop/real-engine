//! `InputConfig`: the `[input]` section of `real.toml` (ARCHITECTURE §8).
//!
//! ```toml
//! [input.buttons]               # action = list of bindings
//! jump = ["key:Space", "pad:South"]
//!
//! [input.axes]
//! move = [{ composite = ["key:KeyW", "key:KeyS", "key:KeyA", "key:KeyD"] }, { stick = "left", dead_zone = 0.15 }]
//! look = [{ mouse_delta = 0.002 }, { stick = "right", dead_zone = 0.15 }]
//! ```

use std::collections::BTreeMap;

use re_core::{Diagnostic, Location};
use serde::{Deserialize, Serialize};

use crate::codes;
use crate::map::{ActionMap, AxisSource};
use crate::names::{parse_button, parse_stick, suggest};

/// The `[input]` section of `real.toml`: actions and their bindings, as text.
///
/// Parse it with [`InputConfig::from_real_toml`] (or as part of a larger serde
/// struct), then turn it into an [`ActionMap`] with
/// [`InputConfig::to_action_map`], which checks every name.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputConfig {
    /// Button actions: action name to bindings (`key:Space`, `mouse:Left`, `pad:South`).
    #[serde(default)]
    pub buttons: BTreeMap<String, Vec<String>>,
    /// Axis actions: action name to sources, in priority order.
    #[serde(default)]
    pub axes: BTreeMap<String, Vec<AxisSourceConfig>>,
}

/// One axis source as written in `real.toml`. Exactly one of `composite`,
/// `stick` and `mouse_delta` is set; `dead_zone` goes with `stick`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AxisSourceConfig {
    /// Four bindings: up, down, left, right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub composite: Option<[String; 4]>,
    /// `left` or `right`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stick: Option<String>,
    /// The stick's dead-zone radius in `[0, 1)`; 0.15 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dead_zone: Option<f32>,
    /// Mouse motion scale (units per device count).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouse_delta: Option<f32>,
}

/// The dead zone used when a stick source gives none.
pub const DEFAULT_DEAD_ZONE: f32 = 0.15;

#[derive(Deserialize)]
struct RealToml {
    #[serde(default)]
    input: InputConfig,
}

impl InputConfig {
    /// Reads the `[input]` section from the text of a `real.toml`. Other sections
    /// are ignored; a missing `[input]` gives an empty config. `file` is used in
    /// diagnostics.
    ///
    /// # Errors
    ///
    /// [`codes::CONFIG_INVALID`] for TOML syntax errors and for unknown or
    /// mistyped keys inside `[input]`, located at the offending line.
    pub fn from_real_toml(text: &str, file: &str) -> Result<InputConfig, Diagnostic> {
        toml::from_str::<RealToml>(text)
            .map(|t| t.input)
            .map_err(|e| {
                let mut d = Diagnostic::error(
                    codes::CONFIG_INVALID,
                    format!("invalid `[input]` configuration: {}", e.message()),
                );
                d.location = Some(match e.span() {
                    Some(span) => {
                        let (line, column) = line_col(text, span.start);
                        Location::file_at(file, line, column)
                    }
                    None => Location::file(file),
                });
                d
            })
    }

    /// Builds the action map, checking every binding.
    ///
    /// # Errors
    ///
    /// Every problem found, not just the first: [`codes::UNKNOWN_BINDING`] for a
    /// name that is not a key, mouse button or pad button (with the closest valid
    /// name as a hint), [`codes::AXIS_SOURCE_INVALID`] for a malformed axis source
    /// and [`codes::ACTION_KIND_CONFLICT`] for an action that is both a button and
    /// an axis.
    pub fn to_action_map(&self) -> Result<ActionMap<String>, Vec<Diagnostic>> {
        let mut map = ActionMap::new();
        let mut problems = Vec::new();
        let at = || Location::file("real.toml");

        for (action, bindings) in &self.buttons {
            if self.axes.contains_key(action) {
                problems.push(
                    Diagnostic::error(
                        codes::ACTION_KIND_CONFLICT,
                        format!("action `{action}` is defined in both `[input.buttons]` and `[input.axes]`"),
                    )
                    .at(at())
                    .with_hint("rename one of them; an action is either a button or an axis"),
                );
                continue;
            }
            for text in bindings {
                match binding(text, &format!("[input.buttons] {action}")) {
                    Ok(b) => {
                        map.bind_button(action.clone(), b);
                    }
                    Err(d) => problems.push(d.at(at())),
                }
            }
        }

        for (action, sources) in &self.axes {
            for (i, src) in sources.iter().enumerate() {
                let context = format!("[input.axes] {action}, source {}", i + 1);
                match axis_source(src, &context) {
                    Ok(s) => {
                        map.bind_axis(action.clone(), s);
                    }
                    Err(ds) => problems.extend(ds.into_iter().map(|d| d.at(at()))),
                }
            }
        }

        if problems.is_empty() {
            Ok(map)
        } else {
            Err(problems)
        }
    }
}

fn binding(text: &str, context: &str) -> Result<crate::Button, Diagnostic> {
    parse_button(text).ok_or_else(|| {
        let d = Diagnostic::error(codes::UNKNOWN_BINDING, format!("unknown input binding `{text}` in {context}"));
        match suggest(text) {
            Some(c) => d.with_hint(format!("did you mean `{c}`? Bindings are `key:<winit KeyCode>`, `mouse:<button>` or `pad:<gilrs Button>`")),
            None => d.with_hint("bindings are `key:<winit KeyCode>` (key:KeyW), `mouse:<button>` (mouse:Left) or `pad:<gilrs Button>` (pad:South)"),
        }
    })
}

fn axis_source(src: &AxisSourceConfig, context: &str) -> Result<AxisSource, Vec<Diagnostic>> {
    let invalid =
        |msg: String| Diagnostic::error(codes::AXIS_SOURCE_INVALID, format!("{msg} in {context}"));
    let kinds = [
        src.composite.is_some(),
        src.stick.is_some(),
        src.mouse_delta.is_some(),
    ];
    if kinds.iter().filter(|&&k| k).count() != 1 {
        return Err(vec![invalid("an axis source needs exactly one of `composite`, `stick` and `mouse_delta`".into())
            .with_hint("for example { composite = [\"key:KeyW\", \"key:KeyS\", \"key:KeyA\", \"key:KeyD\"] }")]);
    }
    if src.dead_zone.is_some() && src.stick.is_none() {
        return Err(vec![invalid(
            "`dead_zone` only applies to a `stick` source".into(),
        )]);
    }
    if let Some([up, down, left, right]) = &src.composite {
        let parts: Vec<_> = [up, down, left, right]
            .into_iter()
            .map(|t| binding(t, context))
            .collect();
        let errors: Vec<Diagnostic> = parts
            .iter()
            .filter_map(|r| r.as_ref().err().cloned())
            .collect();
        if !errors.is_empty() {
            return Err(errors);
        }
        let [up, down, left, right] =
            [0, 1, 2, 3].map(|i| *parts[i].as_ref().expect("checked above"));
        return Ok(AxisSource::Composite {
            up,
            down,
            left,
            right,
        });
    }
    if let Some(name) = &src.stick {
        let stick = parse_stick(name).ok_or_else(|| {
            vec![invalid(format!("unknown stick `{name}`")).with_hint("use `left` or `right`")]
        })?;
        let dead_zone = src.dead_zone.unwrap_or(DEFAULT_DEAD_ZONE);
        if !(0.0..1.0).contains(&dead_zone) {
            return Err(vec![invalid(format!(
                "dead_zone {dead_zone} is outside [0, 1)"
            ))]);
        }
        return Ok(AxisSource::Stick { stick, dead_zone });
    }
    let scale = src.mouse_delta.expect("one kind is set");
    if !scale.is_finite() {
        return Err(vec![invalid(format!(
            "mouse_delta scale {scale} is not a finite number"
        ))]);
    }
    Ok(AxisSource::MouseDelta { scale })
}

/// The 1-based line and column (in characters) of byte offset `at`.
fn line_col(text: &str, at: usize) -> (u32, u32) {
    let before = &text[..at.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
    (line as u32, column as u32)
}
