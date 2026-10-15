//! Font families to the bundled open-licence substitutes (ADR-0035).
//!
//! `assets/fonts/substitutes.json` maps each family a script can name (the
//! stem of `rbxasset://fonts/families/<Stem>.json`) and each legacy
//! `Enum.Font` item to one of three substitutes; [`install`] gives an egui
//! context one font family per substitute and weight.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde::Deserialize;

const SUBSTITUTES: &str = include_str!("../../assets/fonts/substitutes.json");

/// The bundled font files, by file name.
const FILES: &[(&str, &[u8])] = &[
    (
        "montserrat-latin-400.ttf",
        include_bytes!("../../assets/fonts/montserrat-latin-400.ttf"),
    ),
    (
        "montserrat-latin-500.ttf",
        include_bytes!("../../assets/fonts/montserrat-latin-500.ttf"),
    ),
    (
        "montserrat-latin-700.ttf",
        include_bytes!("../../assets/fonts/montserrat-latin-700.ttf"),
    ),
    (
        "montserrat-latin-900.ttf",
        include_bytes!("../../assets/fonts/montserrat-latin-900.ttf"),
    ),
    (
        "source-sans-3-latin-400.ttf",
        include_bytes!("../../assets/fonts/source-sans-3-latin-400.ttf"),
    ),
    (
        "source-sans-3-latin-700.ttf",
        include_bytes!("../../assets/fonts/source-sans-3-latin-700.ttf"),
    ),
    (
        "roboto-mono-latin-400.ttf",
        include_bytes!("../../assets/fonts/roboto-mono-latin-400.ttf"),
    ),
    (
        "roboto-mono-latin-700.ttf",
        include_bytes!("../../assets/fonts/roboto-mono-latin-700.ttf"),
    ),
];

#[derive(Deserialize)]
struct Table {
    substitutes: BTreeMap<String, BTreeMap<String, String>>,
    default: String,
    families: BTreeMap<String, String>,
    #[serde(rename = "enum")]
    enum_faces: BTreeMap<String, (String, String, String)>,
}

fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| serde_json::from_str(SUBSTITUTES).expect("substitutes.json is valid"))
}

/// The CSS-style number of an `Enum.FontWeight` item.
pub(crate) fn weight_number(weight: &str) -> u32 {
    match weight {
        "Thin" => 100,
        "ExtraLight" => 200,
        "Light" => 300,
        "Medium" => 500,
        "SemiBold" => 600,
        "Bold" => 700,
        "ExtraBold" => 800,
        "Heavy" => 900,
        _ => 400,
    }
}

/// The family stem of a font family asset id.
pub(crate) fn family_stem(family: &str) -> &str {
    let f = family.rsplit('/').next().unwrap_or(family);
    f.strip_suffix(".json").unwrap_or(f)
}

/// The family asset id of a stem.
pub(crate) fn family_id(stem: &str) -> String {
    format!("rbxasset://fonts/families/{stem}.json")
}

/// The face (family stem, weight item, style item) of a legacy font name,
/// `None` for an unknown one.
pub(crate) fn enum_face(item: &str) -> Option<(&'static str, &'static str, &'static str)> {
    table()
        .enum_faces
        .get(item)
        .map(|(f, w, s)| (f.as_str(), w.as_str(), s.as_str()))
}

/// The legacy font name of a face, `"Unknown"` when none matches.
pub(crate) fn enum_of(stem: &str, weight: &str, style: &str) -> &'static str {
    table()
        .enum_faces
        .iter()
        .find(|(_, (f, w, s))| f == stem && w == weight && s == style)
        .map_or("Unknown", |(k, _)| k.as_str())
}

/// The egui font family that draws `family` (asset id or stem) at `weight`
/// (an `Enum.FontWeight` item, or "Bold" for bold rich text).
pub(crate) fn egui_family(family: &str, weight: &str) -> egui::FontFamily {
    let t = table();
    let sub = t
        .families
        .get(family_stem(family))
        .unwrap_or(&t.default)
        .as_str();
    let want = weight_number(weight);
    let weights = &t.substitutes[sub];
    // The nearest weight, the heavier one on a tie (600 draws as 700).
    let best = weights
        .keys()
        .filter_map(|k| k.parse::<u32>().ok())
        .min_by_key(|&w| (w.abs_diff(want), u32::MAX - w))
        .unwrap_or(400);
    egui::FontFamily::Name(Arc::from(format!("re.{sub}.{best}")))
}

/// Adds the substitutes to `defs`: one family per substitute and weight,
/// each falling back to `defs`' proportional fonts for missing glyphs. A
/// host with fonts of its own adds them to its definitions with this.
pub fn add_to(defs: &mut egui::FontDefinitions) {
    let t = table();
    let fallback = defs
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for (sub, weights) in &t.substitutes {
        for (w, file) in weights {
            let bytes = FILES
                .iter()
                .find(|(n, _)| n == file)
                .map(|(_, b)| *b)
                .expect("every substitute file is bundled");
            let name = format!("re.{sub}.{w}");
            defs.font_data
                .insert(name.clone(), Arc::new(egui::FontData::from_static(bytes)));
            let mut chain = vec![name.clone()];
            chain.extend(fallback.iter().cloned());
            defs.families
                .insert(egui::FontFamily::Name(Arc::from(name)), chain);
        }
    }
}

/// Gives `ctx` egui's default fonts plus the substitutes. This replaces the
/// context's fonts; a host with its own uses [`add_to`] on its definitions
/// instead. The fonts are usable from the next pass on.
pub fn install(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    add_to(&mut defs);
    ctx.set_fonts(defs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_and_weights_resolve() {
        let name = |f: egui::FontFamily| match f {
            egui::FontFamily::Name(n) => n.to_string(),
            other => format!("{other:?}"),
        };
        assert_eq!(
            name(egui_family(
                "rbxasset://fonts/families/GothamSSm.json",
                "Bold"
            )),
            "re.montserrat.700"
        );
        assert_eq!(name(egui_family("GothamSSm", "Heavy")), "re.montserrat.900");
        assert_eq!(
            name(egui_family("SourceSansPro", "SemiBold")),
            "re.source-sans-3.700"
        );
        assert_eq!(
            name(egui_family("Inconsolata", "Regular")),
            "re.roboto-mono.400"
        );
        assert_eq!(
            name(egui_family("NoSuchFamily", "Light")),
            "re.source-sans-3.400"
        );
        assert_eq!(
            enum_face("GothamBold"),
            Some(("GothamSSm", "Bold", "Normal"))
        );
        assert_eq!(enum_of("GothamSSm", "Bold", "Normal"), "GothamBold");
        assert_eq!(enum_of("GothamSSm", "Thin", "Normal"), "Unknown");
        for (_, file) in table().substitutes.values().flatten() {
            assert!(FILES.iter().any(|(n, _)| n == file), "{file} is bundled");
        }
    }
}
