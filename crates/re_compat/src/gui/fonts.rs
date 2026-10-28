//! Font families to the bundled open-licence families (ADR-0035), which the
//! UI core carries (`re_ui::fonts`).
//!
//! `assets/fonts/substitutes.json` maps each family a script can name (the
//! stem of `rbxasset://fonts/families/<Stem>.json`) and each legacy
//! `Enum.Font` item to one of the three bundled families.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

const SUBSTITUTES: &str = include_str!("../../assets/fonts/substitutes.json");

#[derive(Deserialize)]
struct Table {
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
pub(crate) fn weight_number(weight: &str) -> u16 {
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

/// The bundled face that draws `family` (asset id or stem) at `weight` (an
/// `Enum.FontWeight` item).
pub(crate) fn face(family: &str, weight: &str, italic: bool) -> re_ui::Face {
    let t = table();
    let sub = t.families.get(family_stem(family)).unwrap_or(&t.default);
    re_ui::Face {
        family: re_ui::Family::from_slug(sub).unwrap_or_default(),
        weight: weight_number(weight),
        italic,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(f: re_ui::Face) -> String {
        match re_ui::fonts::egui_family(f, false) {
            egui::FontFamily::Name(n) => n.to_string(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn families_and_weights_resolve() {
        assert_eq!(
            name(face(
                "rbxasset://fonts/families/GothamSSm.json",
                "Bold",
                false
            )),
            "re.montserrat.700"
        );
        assert_eq!(name(face("GothamSSm", "Heavy", false)), "re.montserrat.900");
        assert_eq!(
            name(face("SourceSansPro", "SemiBold", false)),
            "re.source-sans-3.700"
        );
        assert_eq!(
            name(face("Inconsolata", "Regular", false)),
            "re.roboto-mono.400"
        );
        assert_eq!(
            name(face("NoSuchFamily", "Light", false)),
            "re.source-sans-3.400"
        );
        assert_eq!(
            enum_face("GothamBold"),
            Some(("GothamSSm", "Bold", "Normal"))
        );
        assert_eq!(enum_of("GothamSSm", "Bold", "Normal"), "GothamBold");
        assert_eq!(enum_of("GothamSSm", "Thin", "Normal"), "Unknown");
        let t = table();
        for sub in t.families.values().chain([&t.default]) {
            assert!(re_ui::Family::from_slug(sub).is_some(), "{sub} is bundled");
        }
    }
}
