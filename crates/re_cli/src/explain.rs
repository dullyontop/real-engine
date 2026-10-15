//! `real explain RE####` and the `explain` command (ADR-0004): the explain pages
//! of every crate `real` links.

use re_app::App;
use re_command::{CommandRegistry, Tier};
use re_core::{Code, DiagInfo, Diagnostic};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::codes;

/// Every diagnostic catalogue linked into this build, in code order.
pub fn catalog() -> Vec<&'static DiagInfo> {
    let lists: &[&[DiagInfo]] = &[
        re_core::DIAGNOSTICS,
        re_reflect::DIAGNOSTICS,
        re_scene::DIAGNOSTICS,
        re_command::DIAGNOSTICS,
        re_app::DIAGNOSTICS,
        crate::DIAGNOSTICS,
        re_input::DIAGNOSTICS,
        re_physics::DIAGNOSTICS,
        re_render::DIAGNOSTICS,
        re_assets::DIAGNOSTICS,
        re_audio::DIAGNOSTICS,
        re_anim::DIAGNOSTICS,
        re_script::DIAGNOSTICS,
        #[cfg(feature = "tooling")]
        re_script_check::DIAGNOSTICS,
        #[cfg(feature = "tooling")]
        re_editor::DIAGNOSTICS,
    ];
    let mut all: Vec<&'static DiagInfo> = lists.iter().flat_map(|l| l.iter()).collect();
    all.sort_by_key(|d| d.code);
    all
}

/// The catalogue entry for `text` (`RE1001`, case-insensitive).
///
/// # Errors
///
/// `RE0900` when the text is not a code or the code is unknown, with the
/// closest known code as a suggestion.
pub fn lookup(text: &str) -> Result<&'static DiagInfo, Diagnostic> {
    let all = catalog();
    let wanted = Code::parse(&text.trim().to_ascii_uppercase());
    if let Some(code) = wanted
        && let Some(info) = all.iter().find(|d| d.code == code)
    {
        return Ok(info);
    }
    let names: Vec<String> = all.iter().map(|d| d.code.to_string()).collect();
    let suggestion = re_reflect::suggest(text, names.iter().map(String::as_str));
    let message = match suggestion {
        Some(s) => format!("unknown diagnostic code {text} (did you mean {s}?)"),
        None => format!("unknown diagnostic code {text}"),
    };
    Err(Diagnostic::error(codes::UNKNOWN_CODE, message)
        .with_hint("run `real explain` with no code to list every code"))
}

/// Arguments of the `explain` command.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExplainArgs {
    /// The diagnostic code, such as `RE1001`.
    pub code: String,
}

/// Registers the `explain` command (read tier) on an app's registry.
pub fn register(registry: &mut CommandRegistry<App>) {
    registry.register_typed::<ExplainArgs, _>(
        "explain",
        "Explain a diagnostic code (RE####): what it means, an example and how to fix it.",
        Tier::Read,
        true,
        |_app, args, _call| {
            let info = lookup(&args.code)?;
            Ok(json!({ "code": info.code, "title": info.title, "page": info.page }))
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_starts_with_its_heading_and_codes_are_unique() {
        let all = catalog();
        for w in all.windows(2) {
            assert_ne!(w[0].code, w[1].code, "duplicate code {}", w[0].code);
        }
        for d in &all {
            assert!(
                d.page.starts_with(&format!("# {}", d.code)),
                "{} page heading",
                d.code
            );
        }
    }

    #[test]
    fn own_catalogue_is_valid() {
        let problems = re_core::validate_catalog(crate::DIAGNOSTICS, 900..=999);
        assert!(problems.is_empty(), "{problems:?}");
        let problems = re_core::validate_catalog(re_app::DIAGNOSTICS, re_app::CODE_RANGE);
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn lookup_suggests_the_closest_code() {
        let known = re_app::codes::PROJECT_NOT_FOUND;
        let lower = known.to_string().to_lowercase();
        assert_eq!(lookup(&lower).unwrap().code, known);
        // A code in the runtime range that has no page.
        let e = lookup(&format!("RE{}", 599)).unwrap_err();
        assert_eq!(e.code, codes::UNKNOWN_CODE);
        assert!(e.message.contains("did you mean"));
    }
}
