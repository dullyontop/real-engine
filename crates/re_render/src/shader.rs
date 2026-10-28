//! The renderer's WGSL files and a small preprocessor for them: `#include
//! "name.wgsl"` lines.
//!
//! An include line is replaced by the named file, itself preprocessed. Each file is
//! included at most once per shader (later includes of the same file are dropped),
//! so shared declarations can be included from several files. Unknown files and
//! include cycles are `RE6030` diagnostics at the include line.
//!
//! For shader hot reload (ADR-0038), [`ShaderSources`] lays files set at run time
//! over the built-in [`SOURCES`], and [`validate`] checks a shader with naga before
//! the device sees it ([`ShaderSources::check`] checks every shader the set files
//! change). The preprocessor remembers which file and line each line of
//! its output came from, so a compile error (`RE6031`) points at the line its
//! author wrote, not at the joined source.

use std::collections::BTreeMap;

use re_core::{Diagnostic, Location};

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
    ("probe.wgsl", include_str!("shaders/probe.wgsl")),
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
    ("instances.wgsl", include_str!("shaders/instances.wgsl")),
    ("forward_set.wgsl", include_str!("shaders/forward_set.wgsl")),
    ("shadow_set.wgsl", include_str!("shaders/shadow_set.wgsl")),
    ("mask_set.wgsl", include_str!("shaders/mask_set.wgsl")),
];

/// The files of [`SOURCES`] the renderer makes shader modules from; the others
/// only hold declarations for these to include.
pub const ROOTS: &[&str] = &[
    "forward.wgsl",
    "forward_skinned.wgsl",
    "forward_set.wgsl",
    "lines.wgsl",
    "effects.wgsl",
    "sky.wgsl",
    "quad.wgsl",
    "highlight.wgsl",
    "shadow.wgsl",
    "shadow_skinned.wgsl",
    "shadow_set.wgsl",
    "mask.wgsl",
    "mask_skinned.wgsl",
    "mask_set.wgsl",
    "post.wgsl",
    "ui_blit.wgsl",
    "probe.wgsl",
];

/// True when `name` is one of the engine's own shader files ([`SOURCES`]), which
/// a file set with [`ShaderSources::set`] replaces rather than adds to.
pub fn is_engine_file(name: &str) -> bool {
    SOURCES.iter().any(|(n, _)| *n == name)
}

/// Expands the includes of the file `name` from `sources`.
pub fn preprocess(name: &str, sources: &[(&str, &str)]) -> Result<String, Diagnostic> {
    preprocess_mapped(name, sources).map(|p| p.source)
}

/// A preprocessed shader, with the file and line each of its lines came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preprocessed {
    /// The joined source.
    pub source: String,
    files: Vec<String>,
    /// For each line of `source`: an index into `files`, and the 1-based line there.
    lines: Vec<(usize, u32)>,
}

impl Preprocessed {
    /// The file and 1-based line that `line` (1-based) of [`Preprocessed::source`]
    /// came from.
    pub fn origin(&self, line: u32) -> Option<(&str, u32)> {
        let &(file, at) = self
            .lines
            .get(usize::try_from(line).ok()?.checked_sub(1)?)?;
        Some((&self.files[file], at))
    }
}

/// [`preprocess`], keeping where each line came from.
pub fn preprocess_mapped(name: &str, sources: &[(&str, &str)]) -> Result<Preprocessed, Diagnostic> {
    let mut out = Preprocessed {
        source: String::new(),
        files: Vec::new(),
        lines: Vec::new(),
    };
    let mut stack = Vec::new();
    expand(name, sources, &mut stack, &mut out)?;
    Ok(out)
}

fn expand<'a>(
    name: &'a str,
    sources: &[(&'a str, &'a str)],
    stack: &mut Vec<&'a str>,
    out: &mut Preprocessed,
) -> Result<(), Diagnostic> {
    if stack.contains(&name) {
        let chain = stack.join(" -> ");
        return Err(Diagnostic::error(
            codes::SHADER_INCLUDE,
            format!("`#include` cycle: {chain} -> {name}"),
        ));
    }
    if out.files.iter().any(|f| f == name) {
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
    // Listed when it starts, so a later include of it is dropped; cycles are
    // caught by the stack first.
    let file = out.files.len();
    out.files.push(name.to_owned());
    stack.push(name);
    for (number, line) in source.lines().enumerate() {
        match include_target(line) {
            Some(Ok(target)) => expand(target, sources, stack, out).map_err(|d| {
                // An unknown file or a cycle is reported at the line that includes it.
                if d.location.is_some() {
                    d
                } else {
                    d.at(Location::file_at(name, number as u32 + 1, 1))
                }
            })?,
            Some(Err(())) => {
                return Err(Diagnostic::error(
                    codes::SHADER_INCLUDE,
                    format!(
                        "malformed `#include` in `{name}` line {}: expected `#include \"file.wgsl\"`",
                        number + 1
                    ),
                )
                .at(Location::file_at(name, number as u32 + 1, 1)));
            }
            None => {
                out.source.push_str(line);
                out.source.push('\n');
                out.lines.push((file, number as u32 + 1));
            }
        }
    }
    stack.pop();
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

/// The files shaders are built from: the built-in [`SOURCES`], with files set at
/// run time in their place or beside them (ADR-0038).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShaderSources {
    set: BTreeMap<String, String>,
}

impl ShaderSources {
    /// Sets the file `name`: it replaces the built-in file of that name, or is
    /// added as a new file to include. `None` goes back to the built-in file, or
    /// drops an added one.
    pub fn set(&mut self, name: &str, source: Option<String>) {
        match source {
            Some(source) => {
                self.set.insert(name.to_owned(), source);
            }
            None => {
                self.set.remove(name);
            }
        }
    }

    /// Every file by name: the set ones, then the built-in ones they leave.
    pub fn files(&self) -> Vec<(&str, &str)> {
        let set = self.set.iter().map(|(n, s)| (n.as_str(), s.as_str()));
        let built_in = SOURCES.iter().filter(|(n, _)| !self.set.contains_key(*n));
        set.chain(built_in.copied()).collect()
    }

    /// The files set with [`ShaderSources::set`], by name, with their sources.
    pub fn set_files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.set.iter().map(|(n, s)| (n.as_str(), s.as_str()))
    }

    /// The file `name` with its includes expanded.
    pub fn preprocess(&self, name: &str) -> Result<Preprocessed, Diagnostic> {
        preprocess_mapped(name, &self.files())
    }

    /// Checks every one of the [`ROOTS`] that the set files change, as
    /// [`Renderer::reload_shaders`](crate::Renderer::reload_shaders) does before
    /// it builds anything: `RE6030` for an include problem and `RE6031` for a
    /// compile error, each once. It needs no GPU, so `real verify` runs it.
    pub fn check(&self) -> Vec<Diagnostic> {
        let mut problems: Vec<Diagnostic> = Vec::new();
        for root in ROOTS {
            let checked = self.preprocess(root).and_then(|pre| {
                if pre.files.iter().any(|f| self.set.contains_key(f)) {
                    validate(root, &pre)
                } else {
                    Ok(())
                }
            });
            if let Err(d) = checked
                && !problems.contains(&d)
            {
                problems.push(d);
            }
        }
        problems
    }
}

/// Parses and validates the shader `pre`, preprocessed from the file `name`, with
/// naga before it reaches the device: `RE6031` at the file and line of the first
/// error. The web build has no naga and leaves the checks to the device.
#[cfg(not(target_arch = "wasm32"))]
pub fn validate(name: &str, pre: &Preprocessed) -> Result<(), Diagnostic> {
    use wgpu::naga;
    let module = naga::front::wgsl::parse_str(&pre.source).map_err(|e| {
        let message = match e.labels().next() {
            Some((_, label)) if !label.is_empty() && !e.message().contains(label) => {
                format!("{} ({label})", e.message())
            }
            _ => e.message().to_owned(),
        };
        let at = e
            .location(&pre.source)
            .map(|l| (l.line_number, l.line_position));
        compile_error(name, pre, &message, at)
    })?;
    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(&module)
    .map_err(|e| {
        // The smallest span is the one the author looks for: the expression,
        // not the whole function around it.
        let at = e
            .spans()
            .map(|(span, _)| span.location(&pre.source))
            .min_by_key(|l| l.length)
            .map(|l| (l.line_number, l.line_position));
        compile_error(name, pre, &error_chain(&e), at)
    })?;
    Ok(())
}

/// See the native [`validate`]: without naga, there is nothing to check here.
#[cfg(target_arch = "wasm32")]
pub fn validate(_name: &str, _pre: &Preprocessed) -> Result<(), Diagnostic> {
    Ok(())
}

/// An error and the errors that caused it, joined by colons.
pub(crate) fn error_chain(e: &dyn std::error::Error) -> String {
    let mut out = e.to_string();
    let mut cause = e.source();
    while let Some(c) = cause {
        out.push_str(": ");
        out.push_str(&c.to_string());
        cause = c.source();
    }
    out
}

/// `RE6031` for `message`, at the file and line that `at` (a line and column
/// of the preprocessed shader) came from.
#[cfg(not(target_arch = "wasm32"))]
fn compile_error(
    name: &str,
    pre: &Preprocessed,
    message: &str,
    at: Option<(u32, u32)>,
) -> Diagnostic {
    let origin = at.and_then(|(line, column)| pre.origin(line).map(|(f, l)| (f, l, column)));
    let Some((file, line, column)) = origin else {
        return Diagnostic::error(
            codes::SHADER_COMPILE,
            format!("`{name}` does not compile: {message}"),
        )
        .at(Location::file(name));
    };
    // The same for every shader that includes `file`, so a report lists it once.
    Diagnostic::error(
        codes::SHADER_COMPILE,
        format!("`{file}` line {line}: {message}"),
    )
    .at(Location::file_at(file, line, column))
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
        // At the include line that closes the cycle.
        assert_eq!(cycle.location, Some(Location::file_at("b.wgsl", 1, 1)));
        let missing = preprocess("d.wgsl", &sources).unwrap_err();
        assert!(
            missing.message.contains("`missing.wgsl` (from `d.wgsl`)"),
            "{missing}"
        );
        assert_eq!(missing.location, Some(Location::file_at("d.wgsl", 1, 1)));
        let malformed = preprocess("e.wgsl", &sources).unwrap_err();
        assert!(malformed.message.contains("line 1"), "{malformed}");
    }

    #[test]
    fn every_line_maps_back_to_its_file() {
        let sources = [
            ("a.wgsl", "// a1\n#include \"b.wgsl\"\nfn a() {}\n"),
            ("b.wgsl", "// b1\nfn b() {}\n"),
        ];
        let pre = preprocess_mapped("a.wgsl", &sources).unwrap();
        assert_eq!(pre.source, "// a1\n// b1\nfn b() {}\nfn a() {}\n");
        let origins: Vec<_> = (0..=5).map(|line| pre.origin(line)).collect();
        assert_eq!(
            origins,
            [
                None,
                Some(("a.wgsl", 1)),
                Some(("b.wgsl", 1)),
                Some(("b.wgsl", 2)),
                Some(("a.wgsl", 3)),
                None
            ]
        );
    }

    /// A file with a parse error on line 2 and a type error on line 5, and a
    /// shader that includes it.
    fn broken() -> ShaderSources {
        let mut sources = ShaderSources::default();
        sources.set(
            "lib.wgsl",
            Some("fn f() -> f32 {\n    let x = 1.0 +;\n    return x;\n}\n".to_owned()),
        );
        sources.set(
            "main.wgsl",
            Some(
                "// The fragment.\n#include \"lib.wgsl\"\n@fragment\nfn fs() -> @location(0) vec4<f32> {\n    return vec4<f32>(f());\n}\n"
                    .to_owned(),
            ),
        );
        sources
    }

    #[test]
    fn compile_errors_point_at_the_file_and_line_that_has_them() {
        let sources = broken();
        let pre = sources.preprocess("main.wgsl").unwrap();
        let parse = validate("main.wgsl", &pre).unwrap_err();
        assert_eq!(parse.code, codes::SHADER_COMPILE);
        assert_eq!(
            parse.location,
            Some(Location::file_at("lib.wgsl", 2, 18)),
            "{parse}"
        );
        assert!(parse.message.starts_with("`lib.wgsl` line 2: "), "{parse}");

        let mut sources = sources;
        sources.set(
            "lib.wgsl",
            Some("// Returns one.\n\n\nfn f() -> f32 {\n    return 1u;\n}\n".to_owned()),
        );
        let pre = sources.preprocess("main.wgsl").unwrap();
        let typed = validate("main.wgsl", &pre).unwrap_err();
        assert_eq!(typed.code, codes::SHADER_COMPILE);
        let Some(Location::File { file, line, .. }) = &typed.location else {
            panic!("no file location: {typed:?}");
        };
        assert_eq!((file.as_str(), *line), ("lib.wgsl", Some(5)), "{typed}");
    }

    #[test]
    fn a_broken_file_is_the_same_error_from_every_shader_that_includes_it() {
        let mut sources = broken();
        sources.set(
            "other.wgsl",
            Some("#include \"lib.wgsl\"\n@fragment\nfn fs() -> @location(0) vec4<f32> {\n    return vec4<f32>(f());\n}\n".to_owned()),
        );
        let errors: Vec<Diagnostic> = ["main.wgsl", "other.wgsl"]
            .iter()
            .map(|name| validate(name, &sources.preprocess(name).unwrap()).unwrap_err())
            .collect();
        assert_eq!(errors[0], errors[1]);
    }

    #[test]
    fn set_files_replace_built_in_ones_and_none_restores_them() {
        let mut sources = ShaderSources::default();
        let built_in = sources.preprocess("sky.wgsl").unwrap();
        assert_eq!(built_in.source, preprocess("sky.wgsl", SOURCES).unwrap());
        sources.set(
            "common.wgsl",
            Some(format!("{}\n// edited\n", source_of("common.wgsl"))),
        );
        let edited = sources.preprocess("sky.wgsl").unwrap();
        assert!(edited.source.contains("// edited"));
        assert!(validate("sky.wgsl", &edited).is_ok());
        sources.set("common.wgsl", None);
        assert_eq!(sources.preprocess("sky.wgsl").unwrap(), built_in);
        assert!(is_engine_file("common.wgsl") && !is_engine_file("lib.wgsl"));
    }

    #[test]
    fn a_check_reports_each_problem_of_the_set_files_once() {
        let mut sources = ShaderSources::default();
        assert_eq!(sources.check(), []);
        // Nothing includes a new file, so nothing is checked against it.
        sources.set("unused.wgsl", Some("not wgsl".to_owned()));
        assert_eq!(sources.check(), []);
        // Every root includes `common.wgsl`; its error is reported once.
        sources.set(
            "common.wgsl",
            Some(format!(
                "{}\nfn broken() -> f32 {{\n    return 1u;\n}}\n",
                source_of("common.wgsl")
            )),
        );
        let problems = sources.check();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].code, codes::SHADER_COMPILE);
        let Some(Location::File { file, .. }) = &problems[0].location else {
            panic!("no file location: {problems:?}");
        };
        assert_eq!(file, "common.wgsl");
        // An include of a missing file, at the line that includes it.
        sources.set("common.wgsl", None);
        sources.set("sky.wgsl", Some("#include \"missing.wgsl\"\n".to_owned()));
        let problems = sources.check();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert_eq!(problems[0].code, codes::SHADER_INCLUDE);
        assert_eq!(
            problems[0].location,
            Some(Location::file_at("sky.wgsl", 1, 1))
        );
        let set: Vec<&str> = sources.set_files().map(|(name, _)| name).collect();
        assert_eq!(set, ["sky.wgsl", "unused.wgsl"]);
    }

    fn source_of(name: &str) -> &'static str {
        SOURCES.iter().find(|(n, _)| *n == name).unwrap().1
    }

    #[test]
    fn built_in_shaders_preprocess_and_validate() {
        for (name, _) in SOURCES {
            let source = preprocess(name, SOURCES).unwrap();
            if !ROOTS.contains(name) {
                continue; // declarations only (fx.wgsl, skin.wgsl and instances.wgsl need the including file's `frame`, `palette` or `set_part`)
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
