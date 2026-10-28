//! The bundled font families (ADR-0035): three open-licence families as
//! files under `assets/fonts/`, given to an egui context as one font family
//! per family and weight, each falling back to egui's own fonts for glyphs
//! they lack.

use std::sync::Arc;

/// A bundled font family.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Family {
    /// A geometric sans for headings and display text.
    Montserrat,
    /// A humanist sans for body text.
    #[default]
    SourceSans3,
    /// A monospace for code and numbers.
    RobotoMono,
}

impl Family {
    pub const ALL: [Family; 3] = [Family::Montserrat, Family::SourceSans3, Family::RobotoMono];

    /// Its short name, as egui's family names and the font table use it.
    pub fn slug(self) -> &'static str {
        match self {
            Family::Montserrat => "montserrat",
            Family::SourceSans3 => "source-sans-3",
            Family::RobotoMono => "roboto-mono",
        }
    }

    /// The family of a short name.
    pub fn from_slug(slug: &str) -> Option<Family> {
        Family::ALL.into_iter().find(|f| f.slug() == slug)
    }

    /// The weights it has a file for, lightest first.
    pub fn weights(self) -> &'static [u16] {
        match self {
            Family::Montserrat => &[400, 500, 700, 900],
            Family::SourceSans3 | Family::RobotoMono => &[400, 700],
        }
    }

    /// The weight drawn for `want`: the nearest it has, the heavier one on a
    /// tie (600 draws as 700).
    pub fn nearest_weight(self, want: u16) -> u16 {
        self.weights()
            .iter()
            .copied()
            .min_by_key(|&w| (w.abs_diff(want), u16::MAX - w))
            .unwrap_or(400)
    }
}

/// A family, weight and slant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Face {
    pub family: Family,
    /// CSS-style: 400 regular, 700 bold.
    pub weight: u16,
    pub italic: bool,
}

impl Default for Face {
    fn default() -> Self {
        Face {
            family: Family::default(),
            weight: 400,
            italic: false,
        }
    }
}

/// The bundled font files, by file name.
const FILES: &[(&str, &[u8])] = &[
    (
        "montserrat-latin-400.ttf",
        include_bytes!("../assets/fonts/montserrat-latin-400.ttf"),
    ),
    (
        "montserrat-latin-500.ttf",
        include_bytes!("../assets/fonts/montserrat-latin-500.ttf"),
    ),
    (
        "montserrat-latin-700.ttf",
        include_bytes!("../assets/fonts/montserrat-latin-700.ttf"),
    ),
    (
        "montserrat-latin-900.ttf",
        include_bytes!("../assets/fonts/montserrat-latin-900.ttf"),
    ),
    (
        "source-sans-3-latin-400.ttf",
        include_bytes!("../assets/fonts/source-sans-3-latin-400.ttf"),
    ),
    (
        "source-sans-3-latin-700.ttf",
        include_bytes!("../assets/fonts/source-sans-3-latin-700.ttf"),
    ),
    (
        "roboto-mono-latin-400.ttf",
        include_bytes!("../assets/fonts/roboto-mono-latin-400.ttf"),
    ),
    (
        "roboto-mono-latin-700.ttf",
        include_bytes!("../assets/fonts/roboto-mono-latin-700.ttf"),
    ),
];

/// The egui font family that draws `face`, in bold when `bold` (rich
/// text's `<b>`).
pub fn egui_family(face: Face, bold: bool) -> egui::FontFamily {
    let want = if bold { 700 } else { face.weight };
    let w = face.family.nearest_weight(want);
    egui::FontFamily::Name(Arc::from(format!("re.{}.{w}", face.family.slug())))
}

/// Adds the bundled families to `defs`: one egui family per family and
/// weight, each falling back to `defs`' proportional fonts for missing
/// glyphs. A host with fonts of its own adds them to its definitions with
/// this.
pub fn add_to(defs: &mut egui::FontDefinitions) {
    let fallback = defs
        .families
        .get(&egui::FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    // Families by name, as the font table had them, so the definitions are
    // the same whichever host adds them.
    let mut families = Family::ALL;
    families.sort_by_key(|f| f.slug());
    for family in families {
        for w in family.weights() {
            let file = format!("{}-latin-{w}.ttf", family.slug());
            let bytes = FILES
                .iter()
                .find(|(n, _)| *n == file)
                .map(|(_, b)| *b)
                .expect("every weight's file is bundled");
            let name = format!("re.{}.{w}", family.slug());
            defs.font_data
                .insert(name.clone(), Arc::new(egui::FontData::from_static(bytes)));
            let mut chain = vec![name.clone()];
            chain.extend(fallback.iter().cloned());
            defs.families
                .insert(egui::FontFamily::Name(Arc::from(name)), chain);
        }
    }
}

/// Gives `ctx` egui's default fonts plus the bundled families. This
/// replaces the context's fonts; a host with its own uses [`add_to`] on its
/// definitions instead. The fonts are usable from the next pass on.
pub fn install(ctx: &egui::Context) {
    let mut defs = egui::FontDefinitions::default();
    add_to(&mut defs);
    ctx.set_fonts(defs);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(f: egui::FontFamily) -> String {
        match f {
            egui::FontFamily::Name(n) => n.to_string(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn faces_resolve_to_the_nearest_weight() {
        let face = |family, weight| Face {
            family,
            weight,
            italic: false,
        };
        assert_eq!(
            name(egui_family(face(Family::Montserrat, 700), false)),
            "re.montserrat.700"
        );
        assert_eq!(
            name(egui_family(face(Family::Montserrat, 900), false)),
            "re.montserrat.900"
        );
        assert_eq!(
            name(egui_family(face(Family::SourceSans3, 600), false)),
            "re.source-sans-3.700"
        );
        assert_eq!(
            name(egui_family(face(Family::RobotoMono, 300), false)),
            "re.roboto-mono.400"
        );
        assert_eq!(
            name(egui_family(face(Family::Montserrat, 400), true)),
            "re.montserrat.700"
        );
        for family in Family::ALL {
            assert_eq!(Family::from_slug(family.slug()), Some(family));
            for w in family.weights() {
                let file = format!("{}-latin-{w}.ttf", family.slug());
                assert!(FILES.iter().any(|(n, _)| *n == file), "{file} is bundled");
            }
        }
    }
}
