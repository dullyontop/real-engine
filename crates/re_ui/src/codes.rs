//! re_ui's diagnostic codes, `RE7700`–`RE7799` (ARCHITECTURE §7). re_ui
//! reaches no engine crate, so a code here is its text, and the host reports
//! it with its own diagnostic type; `DIAGNOSTICS` is the catalogue `real
//! explain` shows.

/// A theme file does not parse, fails its schema, or has a style whose base
/// is missing or comes back to it.
pub const THEME_INVALID: &str = "RE7700";
/// A node names a style that its theme lacks.
pub const STYLE_MISSING: &str = "RE7701";
/// A component field that differs from its default while the node's style
/// sets it, so the value does not show.
pub const STYLE_HIDES_FIELD: &str = "RE7702";

/// The catalogue: each code with its title and explain page, in code order.
pub const DIAGNOSTICS: &[(&str, &str, &str)] = &[
    (
        THEME_INVALID,
        "Theme file invalid",
        include_str!("../explain/RE7700.md"),
    ),
    (
        STYLE_MISSING,
        "UI style not in the theme",
        include_str!("../explain/RE7701.md"),
    ),
    (
        STYLE_HIDES_FIELD,
        "UI field hidden by its style",
        include_str!("../explain/RE7702.md"),
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_is_in_range_and_in_order() {
        let mut last = 7699;
        for (code, title, page) in DIAGNOSTICS {
            let n: u16 = code
                .strip_prefix("RE")
                .and_then(|d| d.parse().ok())
                .expect("RE and four digits");
            assert!(n > last && n <= 7799, "{code}");
            last = n;
            assert!(!title.is_empty() && !title.ends_with('.'), "{code}");
            assert!(
                page.starts_with(&format!("# {code}: {title}\n")),
                "{code}'s page starts with its heading"
            );
        }
    }
}
