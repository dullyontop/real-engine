//! A small WGSL preprocessor: `#include "name.wgsl"` lines.
//!
//! An include line is replaced by the named file, itself preprocessed. Each file is
//! included at most once per shader (later includes of the same file are dropped),
//! so shared declarations can be included from several files. Unknown files and
//! include cycles are `RE6030` diagnostics.

use re_core::Diagnostic;

use crate::codes;

/// The renderer's WGSL sources, by include name.
pub const SOURCES: &[(&str, &str)] = &[
    ("common.wgsl", include_str!("shaders/common.wgsl")),
    ("forward.wgsl", include_str!("shaders/forward.wgsl")),
    ("shadow.wgsl", include_str!("shaders/shadow.wgsl")),
    ("lines.wgsl", include_str!("shaders/lines.wgsl")),
    ("ui_blit.wgsl", include_str!("shaders/ui_blit.wgsl")),
    ("fx.wgsl", include_str!("shaders/fx.wgsl")),
    ("effects.wgsl", include_str!("shaders/effects.wgsl")),
    ("sky.wgsl", include_str!("shaders/sky.wgsl")),
    ("mask.wgsl", include_str!("shaders/mask.wgsl")),
    ("highlight.wgsl", include_str!("shaders/highlight.wgsl")),
    ("lighting.wgsl", include_str!("shaders/lighting.wgsl")),
    ("quad.wgsl", include_str!("shaders/quad.wgsl")),
    ("tonemap.wgsl", include_str!("shaders/tonemap.wgsl")),
    ("post.wgsl", include_str!("shaders/post.wgsl")),
    ("skin.wgsl", include_str!("shaders/skin.wgsl")),
    (
        "forward_skinned.wgsl",
        include_str!("shaders/forward_skinned.wgsl"),
    ),
    (
        "shadow_skinned.wgsl",
        include_str!("shaders/shadow_skinned.wgsl"),
    ),
    (
        "mask_skinned.wgsl",
        include_str!("shaders/mask_skinned.wgsl"),
    ),
];

/// Expands the includes of the file `name` from `sources`.
pub fn preprocess(name: &str, sources: &[(&str, &str)]) -> Result<String, Diagnostic> {
    let mut out = String::new();
    let mut stack = Vec::new();
    let mut done = Vec::new();
    expand(name, sources, &mut stack, &mut done, &mut out)?;
    Ok(out)
}

fn expand<'a>(
    name: &'a str,
    sources: &[(&'a str, &'a str)],
    stack: &mut Vec<&'a str>,
    done: &mut Vec<&'a str>,
    out: &mut String,
) -> Result<(), Diagnostic> {
    if stack.contains(&name) {
        let chain = stack.join(" -> ");
        return Err(Diagnostic::error(
            codes::SHADER_INCLUDE,
            format!("`#include` cycle: {chain} -> {name}"),
        ));
    }
    if done.contains(&name) {
        return Ok(());
    }
    let Some(&(name, source)) = sources.iter().find(|(n, _)| *n == name) else {
        let from = stack
            .last()
            .map_or(String::new(), |f| format!(" (from `{f}`)"));
        return Err(Diagnostic::error(
            codes::SHADER_INCLUDE,
            format!("unknown shader file `{name}`{from}"),
        ));
    };
    stack.push(name);
    for (number, line) in source.lines().enumerate() {
        match include_target(line) {
            Some(Ok(target)) => expand(target, sources, stack, done, out)?,
            Some(Err(())) => {
                return Err(Diagnostic::error(
                    codes::SHADER_INCLUDE,
                    format!(
                        "malformed `#include` in `{name}` line {}: expected `#include \"file.wgsl\"`",
                        number + 1
                    ),
                ));
            }
            None => {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    stack.pop();
    done.push(name);
    Ok(())
}

/// `None` for an ordinary line, the quoted name for an include line, and an error
/// for a malformed include line.
fn include_target(line: &str) -> Option<Result<&str, ()>> {
    let rest = line.trim().strip_prefix("#include")?;
    let rest = rest.trim();
    Some(
        rest.strip_prefix('"')
            .and_then(|r| r.strip_suffix('"'))
            .filter(|n| !n.is_empty() && !n.contains('"'))
            .ok_or(()),
    )
}

/// Creates a shader module from one of the renderer's [`SOURCES`].
///
/// # Panics
///
/// Panics if the built-in sources fail to preprocess, which a unit test rules out.
pub(crate) fn module(device: &wgpu::Device, name: &str) -> wgpu::ShaderModule {
    let source = preprocess(name, SOURCES)
        .unwrap_or_else(|d| panic!("built-in shader `{name}` is broken: {d}"));
    device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(name),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_expand_once_and_in_place() {
        let sources = [
            (
                "a.wgsl",
                "#include \"c.wgsl\"\n#include \"b.wgsl\"\nfn a() {}",
            ),
            ("b.wgsl", "  #include \"c.wgsl\"\nfn b() {}"),
            ("c.wgsl", "fn c() {}"),
        ];
        let out = preprocess("a.wgsl", &sources).unwrap();
        assert_eq!(out, "fn c() {}\nfn b() {}\nfn a() {}\n");
    }

    #[test]
    fn cycles_and_unknown_files_are_diagnostics() {
        let sources = [
            ("a.wgsl", "#include \"b.wgsl\""),
            ("b.wgsl", "#include \"a.wgsl\""),
            ("d.wgsl", "#include \"missing.wgsl\""),
            ("e.wgsl", "#include missing.wgsl"),
        ];
        let cycle = preprocess("a.wgsl", &sources).unwrap_err();
        assert_eq!(cycle.code, codes::SHADER_INCLUDE);
        assert!(
            cycle.message.contains("a.wgsl -> b.wgsl -> a.wgsl"),
            "{cycle}"
        );
        let missing = preprocess("d.wgsl", &sources).unwrap_err();
        assert!(
            missing.message.contains("`missing.wgsl` (from `d.wgsl`)"),
            "{missing}"
        );
        let malformed = preprocess("e.wgsl", &sources).unwrap_err();
        assert!(malformed.message.contains("line 1"), "{malformed}");
    }

    #[test]
    fn built_in_shaders_preprocess_and_validate() {
        for (name, _) in SOURCES {
            let source = preprocess(name, SOURCES).unwrap();
            if matches!(
                *name,
                "common.wgsl" | "fx.wgsl" | "skin.wgsl" | "tonemap.wgsl" | "lighting.wgsl"
            ) {
                continue; // declarations only (fx.wgsl and skin.wgsl need the including file's `frame` or `palette`)
            }
            let module = naga_parse(&source).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(module > 0, "{name} has no entry points");
        }
    }

    /// Parses and validates WGSL with the naga that wgpu re-exports; returns the
    /// number of entry points.
    fn naga_parse(source: &str) -> Result<usize, String> {
        let module =
            wgpu::naga::front::wgsl::parse_str(source).map_err(|e| e.emit_to_string(source))?;
        wgpu::naga::valid::Validator::new(
            wgpu::naga::valid::ValidationFlags::all(),
            wgpu::naga::valid::Capabilities::empty(),
        )
        .validate(&module)
        .map_err(|e| format!("{e:?}"))?;
        Ok(module.entry_points.len())
    }
}
