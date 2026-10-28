//! Themes (ADR-0058 part 7): named styles in a JSON file (`*.theme.json`,
//! described by `schemas/theme.schema.json`). A style sets any of the visual
//! fields of a node's panel, text, bar and scroll components, named as the
//! components name them; it may extend another style (`base`), and it has a
//! partial style for each state its node can be in (`focused`, `hover`,
//! `pressed`, `disabled`).
//!
//! Themes stack in layers, the engine's default theme ([`ThemeFile::engine`])
//! at the bottom. A style replaces a lower layer's style of the same name.
//! `base` names a style of the style's own layer or a lower one; a style
//! whose base is its own name extends the lower layers' style of that name.
//! [`Theme::new`] resolves every style of the layers with its bases applied.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::OnceLock;

use serde::de::{self, Deserializer, SeqAccess, Visitor};
use serde::ser::{SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};

use crate::codes;
use crate::tree::BarDirection;

/// A colour in a theme: linear RGBA, as scene files write colours. Three
/// numbers are an opaque colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub [f32; 4]);

impl Serialize for Color {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(4))?;
        for c in self.0 {
            seq.serialize_element(&c)?;
        }
        seq.end()
    }
}

impl<'de> Deserialize<'de> for Color {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Color, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Color;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a linear colour: 3 or 4 numbers")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Color, A::Error> {
                let mut c = [1.0f32; 4];
                let mut n = 0;
                while let Some(x) = seq.next_element::<f32>()? {
                    if n == 4 {
                        return Err(de::Error::invalid_length(5, &self));
                    }
                    c[n] = x;
                    n += 1;
                }
                if n < 3 {
                    return Err(de::Error::invalid_length(n, &self));
                }
                Ok(Color(c))
            }
        }
        d.deserialize_seq(V)
    }
}

#[cfg(feature = "schemars")]
impl schemars::JsonSchema for Color {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Color".into()
    }

    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "A linear RGBA colour, as scene files write colours: 3 or 4 numbers (3 is opaque).",
            "type": "array",
            "items": { "type": "number" },
            "minItems": 3,
            "maxItems": 4
        })
    }
}

/// A font family, as `UiText.font` names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum Font {
    /// Source Sans 3, for body text.
    #[default]
    Sans,
    /// Montserrat, for headings.
    Display,
    /// Roboto Mono, for code and numbers.
    Mono,
}

/// Where text sits along one axis of its box, as `UiText.align_x` and
/// `align_y` name it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum Align {
    /// Left or top.
    #[default]
    Start,
    /// In the middle.
    Center,
    /// Right or bottom.
    End,
}

/// A partial set of one component's fields: each is `None` or the value a
/// style gives it.
macro_rules! patch {
    (
        $(#[$m:meta])*
        $name:ident for $component:literal {
            $( $(#[$fm:meta])* $field:ident: $ty:ty ),* $(,)?
        }
    ) => {
        $(#[$m])*
        #[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
        #[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
        #[serde(default, deny_unknown_fields)]
        pub struct $name {
            $(
                $(#[$fm])*
                #[serde(skip_serializing_if = "Option::is_none")]
                pub $field: Option<$ty>,
            )*
        }

        impl $name {
            /// The component whose fields these are.
            pub const COMPONENT: &'static str = $component;
            /// The fields' names, in the order the component declares them.
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($field)),*];

            /// `self` with the fields `top` sets taken from `top`.
            pub fn over(&self, top: &$name) -> $name {
                $name { $($field: top.$field.or(self.$field)),* }
            }

            /// The names of the fields this sets.
            pub fn set(&self) -> Vec<&'static str> {
                let mut out = Vec::new();
                $(if self.$field.is_some() {
                    out.push(stringify!($field));
                })*
                out
            }

            /// Whether it sets no field.
            pub fn is_empty(&self) -> bool {
                true $(&& self.$field.is_none())*
            }
        }
    };
}

patch! {
    /// A style's `UiPanel` fields.
    PanelPatch for "UiPanel" {
        /// The fill (alpha 0: none).
        color: Color,
        /// The corner radius in points.
        corner: f32,
        /// The outline's width in points (0: none).
        outline: f32,
        outline_color: Color,
        /// The colour the fill blends to across the box (alpha 0: none).
        gradient: Color,
        /// The direction of the blend in degrees, 0 left to right, clockwise.
        gradient_angle_deg: f32,
    }
}

patch! {
    /// A style's `UiText` fields: all but the text and whether it is rich.
    TextPatch for "UiText" {
        font: Font,
        /// CSS-style: 400 regular, 700 bold.
        weight: i32,
        /// The size in points.
        size: f32,
        color: Color,
        align_x: Align,
        align_y: Align,
        /// Wraps at the box's width.
        wrap: bool,
        /// Takes the largest size that fits the box.
        fit: bool,
        /// Ends text that does not fit with an ellipsis.
        truncate: bool,
        /// Line spacing, 1 for the font's own.
        line_height: f32,
        /// A one-point outline (alpha 0: none).
        outline_color: Color,
    }
}

patch! {
    /// A style's `UiBar` fields: all but the value.
    BarPatch for "UiBar" {
        direction: BarDirection,
        color: Color,
    }
}

patch! {
    /// A style's `UiScroll` fields: the scroll bar's.
    ScrollPatch for "UiScroll" {
        /// The scroll bar's width in points.
        bar: f32,
        bar_color: Color,
    }
}

/// What a style or one of its states sets: some fields of each visual
/// component.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct Patch {
    #[serde(skip_serializing_if = "PanelPatch::is_empty")]
    pub panel: PanelPatch,
    #[serde(skip_serializing_if = "TextPatch::is_empty")]
    pub text: TextPatch,
    #[serde(skip_serializing_if = "BarPatch::is_empty")]
    pub bar: BarPatch,
    #[serde(skip_serializing_if = "ScrollPatch::is_empty")]
    pub scroll: ScrollPatch,
}

impl Patch {
    /// `self` with the fields `top` sets taken from `top`.
    pub fn over(&self, top: &Patch) -> Patch {
        Patch {
            panel: self.panel.over(&top.panel),
            text: self.text.over(&top.text),
            bar: self.bar.over(&top.bar),
            scroll: self.scroll.over(&top.scroll),
        }
    }

    /// The fields this sets, as `(component, field)`.
    pub fn set(&self) -> Vec<(&'static str, &'static str)> {
        let tag = |c: &'static str, fields: Vec<&'static str>| {
            fields.into_iter().map(move |f| (c, f)).collect::<Vec<_>>()
        };
        [
            tag(PanelPatch::COMPONENT, self.panel.set()),
            tag(TextPatch::COMPONENT, self.text.set()),
            tag(BarPatch::COMPONENT, self.bar.set()),
            tag(ScrollPatch::COMPONENT, self.scroll.set()),
        ]
        .concat()
    }

    /// Whether it sets no field.
    pub fn is_empty(&self) -> bool {
        self.panel.is_empty()
            && self.text.is_empty()
            && self.bar.is_empty()
            && self.scroll.is_empty()
    }
}

/// A style as a theme file writes it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct StyleFile {
    /// The style this one extends: a style of this theme or of the themes
    /// under it. This style's own name extends the style of that name in the
    /// themes under it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "PanelPatch::is_empty")]
    pub panel: PanelPatch,
    #[serde(skip_serializing_if = "TextPatch::is_empty")]
    pub text: TextPatch,
    #[serde(skip_serializing_if = "BarPatch::is_empty")]
    pub bar: BarPatch,
    #[serde(skip_serializing_if = "ScrollPatch::is_empty")]
    pub scroll: ScrollPatch,
    /// Over the style while its node has keyboard or gamepad focus.
    #[serde(skip_serializing_if = "Patch::is_empty")]
    pub focused: Patch,
    /// Over the style while the pointer is over its node.
    #[serde(skip_serializing_if = "Patch::is_empty")]
    pub hover: Patch,
    /// Over the style while its node is pressed.
    #[serde(skip_serializing_if = "Patch::is_empty")]
    pub pressed: Patch,
    /// Over the style while its node is a disabled button.
    #[serde(skip_serializing_if = "Patch::is_empty")]
    pub disabled: Patch,
}

impl StyleFile {
    /// The fields the style sets in every state.
    pub fn look(&self) -> Patch {
        Patch {
            panel: self.panel,
            text: self.text,
            bar: self.bar,
            scroll: self.scroll,
        }
    }
}

/// A theme file (`*.theme.json`, ADR-0058): named styles for native UI
/// nodes. A style sets any of the visual fields of `UiPanel`, `UiText`,
/// `UiBar` and `UiScroll`, may extend another style (`base`), and has a
/// partial style for each state (`focused`, `hover`, `pressed`, `disabled`).
///
/// ```json
/// { "format": "real-engine/theme", "version": 1,
///   "styles": {
///     "button": { "base": "button",
///                 "panel": { "color": [0.02, 0.02, 0.03, 0.95], "corner": 8.0 },
///                 "hover": { "panel": { "color": [0.05, 0.05, 0.07, 0.95] } } },
///     "button.danger": { "base": "button", "panel": { "color": [0.6, 0.02, 0.02] } } } }
/// ```
///
/// A project's theme (`[ui] theme` in `real.toml`) lies over the engine's,
/// and a root's `UiRoot.theme` takes the project's place for that root: a
/// style replaces the engine's style of its name, unless its `base` is its
/// own name, which extends that style instead. Colours are linear, as in
/// scene files.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ThemeFile {
    /// An optional JSON Schema reference, ignored.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Always [`ThemeFile::FORMAT`].
    pub format: String,
    /// The file format version, [`ThemeFile::VERSION`].
    pub version: u32,
    /// The styles by name, such as `button` or `button.primary`.
    #[serde(default)]
    pub styles: BTreeMap<String, StyleFile>,
}

impl ThemeFile {
    /// The value of the `format` key.
    pub const FORMAT: &'static str = "real-engine/theme";
    /// The current file format version.
    pub const VERSION: u32 = 1;

    /// A theme with no styles.
    pub fn new() -> ThemeFile {
        ThemeFile {
            schema: None,
            format: Self::FORMAT.to_string(),
            version: Self::VERSION,
            styles: BTreeMap::new(),
        }
    }

    /// Reads a theme file's text.
    ///
    /// # Errors
    ///
    /// `RE7700` when it is not JSON, does not match the schema, or has
    /// another format or version.
    pub fn parse(text: &str) -> Result<ThemeFile, ThemeError> {
        let file: ThemeFile = serde_json::from_str(text)
            .map_err(|e| ThemeError::invalid(format!("the theme does not parse: {e}")))?;
        if file.format != Self::FORMAT {
            return Err(ThemeError::invalid(format!(
                "the theme's format is `{}`, not `{}`",
                file.format,
                Self::FORMAT
            )));
        }
        if file.version != Self::VERSION {
            return Err(ThemeError::invalid(format!(
                "the theme's version is {}; this engine reads version {}",
                file.version,
                Self::VERSION
            )));
        }
        Ok(file)
    }

    /// The engine's default theme (`assets/theme/default.theme.json`): dark,
    /// with Montserrat headings and Source Sans 3 text.
    pub fn engine() -> &'static ThemeFile {
        static ENGINE: OnceLock<ThemeFile> = OnceLock::new();
        ENGINE.get_or_init(|| {
            ThemeFile::parse(include_str!("../assets/theme/default.theme.json"))
                .expect("the engine's theme parses (a re_ui test checks it)")
        })
    }
}

impl Default for ThemeFile {
    fn default() -> Self {
        ThemeFile::new()
    }
}

/// A theme that cannot be used, with the code a host reports it with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThemeError {
    /// `RE7700`.
    pub code: &'static str,
    /// Which layer the problem is in (its index in the layers given to
    /// [`Theme::new`]), when it is in one.
    pub layer: Option<usize>,
    pub message: String,
}

impl ThemeError {
    fn invalid(message: String) -> ThemeError {
        ThemeError {
            code: codes::THEME_INVALID,
            layer: None,
            message,
        }
    }
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ThemeError {}

/// The states of a node that change its look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub focused: bool,
    pub hover: bool,
    pub pressed: bool,
    pub disabled: bool,
}

/// A style with its bases applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    /// What it sets in every state.
    pub look: Patch,
    pub focused: Patch,
    pub hover: Patch,
    pub pressed: Patch,
    pub disabled: Patch,
}

impl Style {
    /// This style extended by `file`: the fields `file` sets win, in the look
    /// and in each state.
    fn extend(&self, file: &StyleFile) -> Style {
        Style {
            look: self.look.over(&file.look()),
            focused: self.focused.over(&file.focused),
            hover: self.hover.over(&file.hover),
            pressed: self.pressed.over(&file.pressed),
            disabled: self.disabled.over(&file.disabled),
        }
    }

    /// What the style sets for a node in `state`: its look, then the states
    /// the node is in, in the order focused, hover, pressed, disabled (the
    /// last wins).
    pub fn at(&self, state: State) -> Patch {
        let mut p = self.look;
        for (on, v) in [
            (state.focused, &self.focused),
            (state.hover, &self.hover),
            (state.pressed, &self.pressed),
            (state.disabled, &self.disabled),
        ] {
            if on {
                p = p.over(v);
            }
        }
        p
    }
}

/// Theme layers resolved: every style they define, by name.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Theme {
    styles: BTreeMap<String, Style>,
}

impl Theme {
    /// Resolves `layers`, the bottom one first (usually
    /// [`ThemeFile::engine`]).
    ///
    /// # Errors
    ///
    /// `RE7700` for each style whose base no layer it can reach defines, or
    /// whose bases come back to it (once, however many styles extend it).
    pub fn new(layers: &[&ThemeFile]) -> Result<Theme, Vec<ThemeError>> {
        let names: BTreeSet<&str> = layers
            .iter()
            .flat_map(|l| l.styles.keys().map(String::as_str))
            .collect();
        let mut styles = BTreeMap::new();
        let mut errors = Vec::new();
        for name in names {
            let (layer, file) = find(layers, layers.len(), name).expect("a layer defines it");
            match resolve(layers, layer, name, file, &mut Vec::new()) {
                Ok(style) => {
                    styles.insert(name.to_string(), style);
                }
                // A broken base breaks every style over it: report it once.
                Err(e) if !errors.contains(&e) => errors.push(e),
                Err(_) => {}
            }
        }
        if errors.is_empty() {
            Ok(Theme { styles })
        } else {
            Err(errors)
        }
    }

    /// The engine's default theme alone.
    pub fn engine() -> &'static Theme {
        static ENGINE: OnceLock<Theme> = OnceLock::new();
        ENGINE.get_or_init(|| {
            Theme::new(&[ThemeFile::engine()]).expect("the engine's theme resolves")
        })
    }

    /// The style `name`.
    pub fn style(&self, name: &str) -> Option<&Style> {
        self.styles.get(name)
    }

    /// The styles' names, in order.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.styles.keys().map(String::as_str)
    }
}

/// The highest of the first `below` layers that defines `name`, and its
/// style.
fn find<'a>(layers: &[&'a ThemeFile], below: usize, name: &str) -> Option<(usize, &'a StyleFile)> {
    (0..below)
        .rev()
        .find_map(|i| layers[i].styles.get(name).map(|s| (i, s)))
}

/// Style `name` of layer `layer` (`file`) with its bases applied. `chain`
/// holds the styles being resolved that lead here.
fn resolve(
    layers: &[&ThemeFile],
    layer: usize,
    name: &str,
    file: &StyleFile,
    chain: &mut Vec<(usize, String)>,
) -> Result<Style, ThemeError> {
    let fail = |message: String| ThemeError {
        code: codes::THEME_INVALID,
        layer: Some(layer),
        message,
    };
    if chain.iter().any(|(l, n)| *l == layer && n == name) {
        let path: Vec<&str> = chain.iter().map(|(_, n)| n.as_str()).collect();
        return Err(fail(format!(
            "style `{name}` extends itself: {} -> {name}",
            path.join(" -> ")
        )));
    }
    let base = match &file.base {
        None => Style::default(),
        Some(b) => {
            // A style's own name reaches only the layers under it.
            let below = if b == name { layer } else { layer + 1 };
            let Some((at, base)) = find(layers, below, b) else {
                return Err(fail(if b == name {
                    format!(
                        "style `{name}` extends `{name}` of the themes under it, which have none"
                    )
                } else {
                    format!(
                        "style `{name}` extends `{b}`, which neither its theme nor one under it has"
                    )
                }));
            };
            chain.push((layer, name.to_string()));
            let style = resolve(layers, at, b, base, chain);
            chain.pop();
            style?
        }
    };
    Ok(base.extend(file))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(json: &str) -> ThemeFile {
        ThemeFile::parse(&format!(
            r#"{{ "format": "real-engine/theme", "version": 1, "styles": {json} }}"#
        ))
        .expect("parses")
    }

    #[test]
    fn the_engine_theme_has_its_styles() {
        let names: Vec<&str> = Theme::engine().names().collect();
        assert_eq!(
            names,
            [
                "bar",
                "body",
                "button",
                "button.primary",
                "heading",
                "input",
                "list",
                "panel",
                "row",
                "title"
            ]
        );
        let primary = Theme::engine().style("button.primary").expect("defined");
        // From `button`, under its own colour.
        assert_eq!(primary.look.text.align_x, Some(Align::Center));
        assert_ne!(
            primary.look.panel.color,
            Theme::engine()
                .style("button")
                .and_then(|b| b.look.panel.color)
        );
    }

    /// The example in [`ThemeFile`]'s documentation (and so in the schema).
    #[test]
    fn the_documented_example_extends_the_engine_theme() {
        let file = ThemeFile::parse(
            r#"{ "format": "real-engine/theme", "version": 1,
              "styles": {
                "button": { "base": "button",
                            "panel": { "color": [0.02, 0.02, 0.03, 0.95], "corner": 8.0 },
                            "hover": { "panel": { "color": [0.05, 0.05, 0.07, 0.95] } } },
                "button.danger": { "base": "button", "panel": { "color": [0.6, 0.02, 0.02] } } } }"#,
        )
        .expect("parses");
        let theme = Theme::new(&[ThemeFile::engine(), &file]).expect("resolves");
        let engine = Theme::engine().style("button").expect("defined");
        let button = theme.style("button").expect("defined");
        assert_eq!(button.look.panel.corner, Some(8.0));
        // The engine's button text, kept by extending it.
        assert_eq!(button.look.text, engine.look.text);
        let danger = theme.style("button.danger").expect("defined");
        assert_eq!(danger.look.panel.corner, Some(8.0));
        assert_eq!(danger.look.text, engine.look.text);
    }

    #[test]
    fn files_parse_strictly() {
        let err = |json: &str| ThemeFile::parse(json).expect_err("rejected");
        let e = err(
            r#"{ "format": "real-engine/theme", "version": 1, "styles": { "a": { "panel": { "colour": [1, 1, 1] } } } }"#,
        );
        assert_eq!(e.code, "RE7700");
        assert!(
            e.message.contains("unknown field `colour`"),
            "{}",
            e.message
        );
        let e = err(
            r#"{ "format": "real-engine/theme", "version": 1, "styles": { "a": { "text": { "font": "Serif" } } } }"#,
        );
        assert!(
            e.message.contains("unknown variant `Serif`"),
            "{}",
            e.message
        );
        let e = err(
            r#"{ "format": "real-engine/theme", "version": 1, "styles": { "a": { "panel": { "color": [1, 1] } } } }"#,
        );
        assert!(e.message.contains("3 or 4 numbers"), "{}", e.message);
        let e = err(r#"{ "format": "real-engine/scene", "version": 1 }"#);
        assert!(e.message.contains("format"), "{}", e.message);
        let e = err(r#"{ "format": "real-engine/theme", "version": 2 }"#);
        assert!(e.message.contains("version 1"), "{}", e.message);
        // Three numbers are opaque; the file writes back as it read.
        let t = theme(
            r#"{ "a": { "panel": { "color": [0.5, 0.25, 0] }, "hover": { "text": { "size": 20 } } } }"#,
        );
        let a = &t.styles["a"];
        assert_eq!(a.panel.color, Some(Color([0.5, 0.25, 0.0, 1.0])));
        let again = ThemeFile::parse(&serde_json::to_string(&t).expect("writes")).expect("reads");
        assert_eq!(again, t);
    }

    #[test]
    fn bases_layers_and_states_resolve() {
        let low = theme(
            r#"{
                "button": { "panel": { "color": [0, 0, 1], "corner": 8 }, "hover": { "panel": { "color": [0, 1, 0] } } },
                "label": { "text": { "size": 14 } }
            }"#,
        );
        let high = theme(
            r#"{
                "button": { "base": "button", "panel": { "corner": 4 }, "pressed": { "panel": { "color": [1, 0, 0] } } },
                "label": { "text": { "weight": 700 } },
                "big": { "base": "label", "text": { "size": 30 } }
            }"#,
        );
        let t = Theme::new(&[&low, &high]).expect("resolves");
        let button = t.style("button").expect("defined");
        // Its own name extends the lower layer's style.
        assert_eq!(button.look.panel.color, Some(Color([0.0, 0.0, 1.0, 1.0])));
        assert_eq!(button.look.panel.corner, Some(4.0));
        // States apply in order, the last winning.
        let at = |s: State| button.at(s).panel.color.map(|c| c.0);
        assert_eq!(at(State::default()), Some([0.0, 0.0, 1.0, 1.0]));
        let hover = State {
            hover: true,
            ..State::default()
        };
        assert_eq!(at(hover), Some([0.0, 1.0, 0.0, 1.0]));
        let pressed = State {
            pressed: true,
            ..hover
        };
        assert_eq!(at(pressed), Some([1.0, 0.0, 0.0, 1.0]));
        // A style replaces a lower layer's of the same name, and a base is
        // looked up from its own layer down.
        let label = t.style("label").expect("defined");
        assert_eq!(label.look.text.size, None);
        let big = t.style("big").expect("defined");
        assert_eq!(big.look.text.weight, Some(700));
        assert_eq!(big.look.text.size, Some(30.0));
        assert_eq!(big.look.set(), [("UiText", "weight"), ("UiText", "size")]);
    }

    #[test]
    fn missing_and_circular_bases_are_errors() {
        let t = theme(
            r#"{
                "a": { "base": "b" },
                "b": { "base": "a" },
                "c": { "base": "nowhere" },
                "d": { "base": "d" },
                "e": { "base": "c" }
            }"#,
        );
        let errors = Theme::new(&[&t]).expect_err("rejected");
        let messages: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "style `a` extends itself: a -> b -> a",
                "style `b` extends itself: b -> a -> b",
                "style `c` extends `nowhere`, which neither its theme nor one under it has",
                "style `d` extends `d` of the themes under it, which have none",
            ]
        );
        assert!(
            errors
                .iter()
                .all(|e| e.code == "RE7700" && e.layer == Some(0))
        );
    }
}
