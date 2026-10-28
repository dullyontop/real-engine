//! `cargo xtask explain`: explain-page coverage (ADR-0004, ARCHITECTURE §7).
//!
//! Scans `crates/*/src/**/*.rs` for string literals that are exactly a code
//! (`"RE"` followed by four digits); comments are ignored. Each code must
//! * have a page at `crates/<crate>/explain/RE####.md`,
//! * lie inside the crate's range (the table of ARCHITECTURE §7, copied below),
//! * be used by one crate only.
//!
//! And each page `crates/<crate>/explain/RE####.md` must be referenced by a string
//! literal ending in `explain/RE####.md` in that crate's sources (the
//! `include_str!` of its `DIAGNOSTICS` entry), so that every code used in source
//! is also in the crate's catalogue and `real explain` knows it.
//!
//! Each page has the shape of ARCHITECTURE §7 ([`page_problems`]): the title
//! line `# RE####: Title`, then the explanation, an `## Example` and a
//! `## How to fix it` section, in that order. A page describes the engine as it
//! is, so it names no milestone ("in M1", "a later milestone").
//!
//! Paths in messages always use `/`, whatever the OS.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The code ranges of ARCHITECTURE §7. A crate that is not listed owns no codes.
/// Each entry is an inclusive `(first, last)` pair of code numbers.
pub fn ranges(krate: &str) -> &'static [(u16, u16)] {
    match krate {
        "re_core" => &[(0, 499)],
        "re_app" => &[(500, 899)],
        "re_cli" => &[(900, 999)],
        "re_reflect" => &[(1000, 1199)],
        "re_scene" => &[(1200, 1999)],
        "re_command" => &[(2000, 2999)],
        // re_script keeps RE9001 ("unsupported API member"), which every
        // layer raises (docs/M2_PLAN.md).
        "re_script" => &[(3000, 3999), (9001, 9001)],
        "re_script_check" => &[(3000, 3999)],
        "re_proc" => &[(4000, 4499)],
        "re_geom" => &[(4500, 4599)],
        "re_physics" => &[(5000, 5999)],
        "re_render" => &[(6000, 6999)],
        "re_assets" => &[(7000, 7199)],
        "re_input" => &[(7200, 7299)],
        "re_audio" => &[(7400, 7499)],
        "re_anim" => &[(7600, 7699)],
        "re_ui" => &[(7700, 7799)],
        "re_agent" => &[(8000, 8099)],
        "re_mcp" => &[(8500, 8599)],
        "re_editor" => &[(8800, 8899)],
        "re_net" => &[(8900, 8999)],
        "re_compat" => &[(9002, 9799)],
        "re_import" => &[(9800, 9899)],
        _ => &[],
    }
}

/// One use of a code in source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Use {
    pub krate: String,
    /// Relative to the workspace root, with `/` separators.
    pub file: String,
    pub line: usize,
}

/// A path relative to `root`, with `/` separators on every OS.
pub fn rel_slash(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The result of a successful scan.
#[derive(Debug, Default)]
pub struct Report {
    /// Code → its uses.
    pub codes: BTreeMap<String, Vec<Use>>,
    /// Explain pages no source uses (reported, not an error), with `/` separators.
    pub unused_pages: Vec<String>,
}

/// Finds string literals that are exactly `RE####`, skipping comments. Returns
/// `(code, 1-based line)` pairs.
pub fn find_codes(source: &str) -> Vec<(String, usize)> {
    string_literals(source)
        .into_iter()
        .filter(|(lit, _)| is_code(lit))
        .collect()
}

/// Finds string literals that name an explain page (`.../explain/RE####.md`,
/// as in `include_str!("../explain/RE0001.md")`), skipping comments. Returns the
/// codes of those pages.
pub fn find_page_refs(source: &str) -> BTreeSet<String> {
    string_literals(source)
        .into_iter()
        .filter_map(|(lit, _)| {
            let name = lit.strip_suffix(".md")?;
            let code = name.rsplit('/').next()?;
            let dir = name.strip_suffix(code)?;
            (is_code(code) && (dir == "explain/" || dir.ends_with("/explain/")))
                .then(|| code.to_string())
        })
        .collect()
}

/// What is wrong with the shape of the explain page of `code`, whose text is
/// `text`: its title line, its sections, and wording that names a milestone.
pub fn page_problems(code: &str, text: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let first = text.lines().next().unwrap_or_default();
    let title = first
        .strip_prefix(&format!("# {code}: "))
        .unwrap_or_default();
    if title.trim().is_empty() {
        problems.push(format!("its first line is not `# {code}: Title`"));
    }
    let headings: Vec<&str> = text.lines().filter(|l| l.starts_with("## ")).collect();
    let at = |name: &str| headings.iter().position(|h| h.trim_end() == name);
    match (at("## Example"), at("## How to fix it")) {
        (Some(example), Some(fix)) if example < fix => {}
        (Some(_), Some(_)) => {
            problems.push("its `## Example` comes after `## How to fix it`".to_string());
        }
        (example, fix) => {
            for (name, found) in [("## Example", example), ("## How to fix it", fix)] {
                if found.is_none() {
                    problems.push(format!("it has no `{name}` section"));
                }
            }
        }
    }
    let words: Vec<&str> = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let milestone = words.iter().find(|w| {
        w.eq_ignore_ascii_case("milestone")
            || w.eq_ignore_ascii_case("milestones")
            || (w.len() > 1 && w.starts_with('M') && w[1..].bytes().all(|b| b.is_ascii_digit()))
    });
    if let Some(word) = milestone {
        problems.push(format!(
            "it names a milestone (`{word}`): say what the engine does now"
        ));
    }
    if !text.ends_with('\n') {
        problems.push("it does not end with a newline".to_string());
    }
    problems
}

fn is_code(literal: &str) -> bool {
    let b = literal.as_bytes();
    b.len() == 6 && b.starts_with(b"RE") && b[2..].iter().all(u8::is_ascii_digit)
}

/// Every string literal's contents (escapes left as written) with its 1-based
/// line, skipping comments, char literals and lifetimes.
fn string_literals(source: &str) -> Vec<(String, usize)> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut line = 1;
    let mut i = 0;
    let line_at = |from: usize, to: usize| bytes[from..to].iter().filter(|&&b| b == b'\n').count();
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                line += 1;
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                let start = i;
                let mut depth = 0usize;
                while i < bytes.len() {
                    if bytes[i] == b'/' && bytes.get(i + 1) == Some(&b'*') {
                        depth += 1;
                        i += 2;
                    } else if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                        depth -= 1;
                        i += 2;
                        if depth == 0 {
                            break;
                        }
                    } else {
                        i += 1;
                    }
                }
                line += line_at(start, i.min(bytes.len()));
            }
            b'r' if is_raw_string_start(bytes, i) => {
                let start = i;
                i += 1;
                let mut hashes = 0;
                while bytes[i] == b'#' {
                    hashes += 1;
                    i += 1;
                }
                i += 1; // the opening quote
                let content_start = i;
                let closing: Vec<u8> = std::iter::once(b'"')
                    .chain(std::iter::repeat_n(b'#', hashes))
                    .collect();
                while i < bytes.len() && !bytes[i..].starts_with(&closing) {
                    i += 1;
                }
                record(&source[content_start..i.min(bytes.len())], line, &mut found);
                i = (i + closing.len()).min(bytes.len());
                line += line_at(start, i);
            }
            b'"' => {
                let start = i;
                i += 1;
                let content_start = i;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                record(&source[content_start..i.min(bytes.len())], line, &mut found);
                i = (i + 1).min(bytes.len());
                line += line_at(start, i);
            }
            b'\'' => {
                // A char literal ('"', '\'', '\n', 'x') or a lifetime ('a).
                if bytes.get(i + 1) == Some(&b'\\') {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\'' {
                        i += 1;
                    }
                    i += 1;
                } else if bytes.get(i + 2) == Some(&b'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            c if c.is_ascii_alphanumeric() || c == b'_' => {
                // Skip identifiers whole, so that a name ending in `r` is not read as
                // a raw string; `br"..."` and `cr"..."` go on to the raw-string arm.
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                let ident = &bytes[start..i];
                if (ident == b"br" || ident == b"cr") && is_raw_string_start(bytes, i - 1) {
                    i -= 1;
                }
            }
            _ => i += 1,
        }
    }
    found
}

fn is_raw_string_start(bytes: &[u8], i: usize) -> bool {
    if bytes.get(i) != Some(&b'r') {
        return false;
    }
    let mut j = i + 1;
    while bytes.get(j) == Some(&b'#') {
        j += 1;
    }
    bytes.get(j) == Some(&b'"')
}

fn record(literal: &str, line: usize, found: &mut Vec<(String, usize)>) {
    found.push((literal.to_string(), line));
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .collect::<std::io::Result<_>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Scans the workspace at `root`. Returns the report, or every problem found.
pub fn scan(root: &Path) -> Result<std::result::Result<Report, Vec<String>>> {
    let crates_dir = root.join("crates");
    let mut report = Report::default();
    let mut crate_dirs: Vec<PathBuf> = Vec::new();
    if crates_dir.is_dir() {
        for entry in std::fs::read_dir(&crates_dir)? {
            let path = entry?.path();
            if path.is_dir() {
                crate_dirs.push(path);
            }
        }
    }
    crate_dirs.sort();

    // Crate name → the explain pages its sources reference.
    let mut page_refs: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for dir in &crate_dirs {
        let krate = dir.file_name().unwrap().to_string_lossy().to_string();
        let mut files = Vec::new();
        rust_files(&dir.join("src"), &mut files)?;
        for file in files {
            let text = std::fs::read_to_string(&file)
                .with_context(|| format!("reading {}", file.display()))?;
            for (code, line) in find_codes(&text) {
                report.codes.entry(code).or_default().push(Use {
                    krate: krate.clone(),
                    file: rel_slash(root, &file),
                    line,
                });
            }
            page_refs
                .entry(krate.clone())
                .or_default()
                .extend(find_page_refs(&text));
        }
    }

    let mut problems = Vec::new();
    for (code, uses) in &report.codes {
        let number: u16 = code[2..].parse().expect("four digits");
        let crates: BTreeSet<&str> = uses.iter().map(|u| u.krate.as_str()).collect();
        if crates.len() > 1 {
            let places: Vec<String> = uses
                .iter()
                .map(|u| format!("{}:{}", u.file, u.line))
                .collect();
            problems.push(format!(
                "{code} is used by more than one crate ({}): {}",
                crates.into_iter().collect::<Vec<_>>().join(", "),
                places.join(", ")
            ));
            continue;
        }
        let first = &uses[0];
        let krate = first.krate.as_str();
        let owned = ranges(krate);
        if !owned.iter().any(|&(lo, hi)| (lo..=hi).contains(&number)) {
            let owned: Vec<String> = owned
                .iter()
                .map(|(lo, hi)| format!("RE{lo:04}-RE{hi:04}"))
                .collect();
            problems.push(format!(
                "{code} ({}:{}) is outside {krate}'s range ({})",
                first.file,
                first.line,
                if owned.is_empty() {
                    "it owns no codes".to_string()
                } else {
                    owned.join(", ")
                }
            ));
        }
        let page = root
            .join("crates")
            .join(krate)
            .join("explain")
            .join(format!("{code}.md"));
        if !page.is_file() {
            problems.push(format!(
                "{code} ({}:{}) has no explain page: create crates/{krate}/explain/{code}.md",
                first.file, first.line
            ));
        }
    }

    for dir in &crate_dirs {
        let krate = dir.file_name().unwrap().to_string_lossy().to_string();
        let explain = dir.join("explain");
        if !explain.is_dir() {
            continue;
        }
        let mut pages: Vec<PathBuf> = std::fs::read_dir(&explain)?
            .map(|e| e.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        pages.sort();
        for page in pages {
            let stem = page
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let well_named = page.extension().is_some_and(|e| e == "md")
                && stem.len() == 6
                && stem.starts_with("RE")
                && stem[2..].bytes().all(|b| b.is_ascii_digit());
            let rel = rel_slash(root, &page);
            if !well_named {
                problems.push(format!("{rel} is not named RE####.md"));
                continue;
            }
            let text = std::fs::read_to_string(&page)
                .with_context(|| format!("reading {}", page.display()))?;
            for problem in page_problems(&stem, &text) {
                problems.push(format!("{rel}: {problem}"));
            }
            if !page_refs
                .get(&krate)
                .is_some_and(|refs| refs.contains(&stem))
            {
                problems.push(format!(
                    "{rel} is not in {krate}'s catalogue: add a DIAGNOSTICS entry with \
                     include_str!(\"../explain/{stem}.md\")"
                ));
            }
            if !report.codes.contains_key(&stem) {
                report.unused_pages.push(rel);
            }
        }
    }

    Ok(if problems.is_empty() {
        Ok(report)
    } else {
        Err(problems)
    })
}

pub fn run(root: &Path) -> Result<()> {
    match scan(root)? {
        Ok(report) => {
            for page in &report.unused_pages {
                println!("explain: note: {page} is not used by any source file");
            }
            println!(
                "explain: ok ({} codes, each with a page in its crate's catalogue, in range, used by one crate; every page well formed)",
                report.codes.len()
            );
            Ok(())
        }
        Err(problems) => {
            for p in &problems {
                eprintln!("  {p}");
            }
            bail!("explain: {} problem(s)", problems.len())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A catalogue source with RE0001's page.
    const CATALOGUE_0001: &str = "const P: &str = include_str!(\"../explain/RE0001.md\");\n";

    /// A well-formed explain page for `code`.
    fn page(code: &str) -> String {
        format!(
            "# {code}: Something went wrong\n\nWhat it means.\n\n## Example\n\n```text\n\
             error[{code}]: it went wrong\n```\n\n## How to fix it\n\nFix it.\n"
        )
    }

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn fails_on_a_missing_page() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "crates/re_core/src/codes.rs",
            "pub const A: Code = Code::new(\"RE0001\");\npub const B: Code = Code::new(\"RE0002\");\n",
        );
        write(dir.path(), "crates/re_core/src/lib.rs", CATALOGUE_0001);
        write(
            dir.path(),
            "crates/re_core/explain/RE0001.md",
            &page("RE0001"),
        );
        let problems = scan(dir.path()).unwrap().expect_err("RE0002 has no page");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("RE0002") && problems[0].contains("no explain page"));
        assert!(
            problems[0].contains("crates/re_core/src/codes.rs:2"),
            "{}",
            problems[0]
        );
        assert!(run(dir.path()).is_err());
    }

    #[test]
    fn passes_when_every_code_has_a_page() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "crates/re_core/src/codes.rs",
            "const A: &str = \"RE0001\";\n",
        );
        write(
            dir.path(),
            "crates/re_core/src/lib.rs",
            &format!("{CATALOGUE_0001}const P: &str = include_str!(\"../explain/RE0009.md\");\n"),
        );
        write(
            dir.path(),
            "crates/re_core/explain/RE0001.md",
            &page("RE0001"),
        );
        write(
            dir.path(),
            "crates/re_core/explain/RE0009.md",
            &page("RE0009"),
        );
        let report = scan(dir.path()).unwrap().unwrap();
        assert_eq!(report.codes.len(), 1);
        assert_eq!(report.unused_pages, ["crates/re_core/explain/RE0009.md"]);
        assert!(run(dir.path()).is_ok());
    }

    #[test]
    fn fails_on_a_page_missing_from_the_catalogue() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "crates/re_core/src/codes.rs",
            "const A: &str = \"RE0001\";\nconst B: &str = \"RE0002\";\n",
        );
        // RE0002 only appears in a comment, which does not count.
        write(
            dir.path(),
            "crates/re_core/src/lib.rs",
            &format!("{CATALOGUE_0001}// include_str!(\"../explain/RE0002.md\")\n"),
        );
        write(
            dir.path(),
            "crates/re_core/explain/RE0001.md",
            &page("RE0001"),
        );
        write(
            dir.path(),
            "crates/re_core/explain/RE0002.md",
            &page("RE0002"),
        );
        let problems = scan(dir.path()).unwrap().unwrap_err();
        assert_eq!(
            problems,
            [
                "crates/re_core/explain/RE0002.md is not in re_core's catalogue: add a \
                 DIAGNOSTICS entry with include_str!(\"../explain/RE0002.md\")"
            ]
        );
    }

    #[test]
    fn page_refs_are_found() {
        let src = r#"
const A: &str = include_str!("../explain/RE0001.md");
const B: &str = include_str!("explain/RE0002.md");
const C: &str = "notexplain/RE0003.md";
const D: &str = "../explain/RE0004.txt";
// include_str!("../explain/RE0005.md")
"#;
        let refs: Vec<String> = find_page_refs(src).into_iter().collect();
        assert_eq!(refs, ["RE0001", "RE0002"]);
    }

    #[test]
    fn pages_have_a_title_an_example_and_a_fix_and_name_no_milestone() {
        assert!(page_problems("RE0001", &page("RE0001")).is_empty());
        let problems = |text: &str| page_problems("RE0001", text);
        assert_eq!(
            problems("# RE0001\n\n## Example\n\n## How to fix it\n"),
            ["its first line is not `# RE0001: Title`"]
        );
        assert_eq!(
            problems("# RE0001: A\n\n## How to fix it\n"),
            ["it has no `## Example` section"]
        );
        assert_eq!(
            problems("# RE0001: A\n\n## How to fix it\n\n## Example\n"),
            ["its `## Example` comes after `## How to fix it`"]
        );
        let stale = page("RE0001").replace("What it means.", "M1 plays WAV files.");
        assert_eq!(
            problems(&stale),
            ["it names a milestone (`M1`): say what the engine does now"]
        );
        let later = page("RE0001").replace("Fix it.", "It comes in a later milestone.");
        assert_eq!(problems(&later).len(), 1, "{:?}", problems(&later));
        // A word that only starts with M and a digit is not a milestone.
        let hex = page("RE0001").replace("What it means.", "Mode M2x and RE0001.");
        assert!(problems(&hex).is_empty(), "{:?}", problems(&hex));
        assert_eq!(
            problems(page("RE0001").trim_end()),
            ["it does not end with a newline"]
        );
    }

    #[test]
    fn a_malformed_page_fails_the_scan() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "crates/re_core/src/codes.rs",
            "const A: &str = \"RE0001\";\n",
        );
        write(dir.path(), "crates/re_core/src/lib.rs", CATALOGUE_0001);
        write(
            dir.path(),
            "crates/re_core/explain/RE0001.md",
            "# RE0001: A\n\nIn M1 this is a warning.\n\n## How to fix it\n\nFix it.\n",
        );
        let problems = scan(dir.path()).unwrap().unwrap_err();
        assert_eq!(
            problems,
            [
                "crates/re_core/explain/RE0001.md: it has no `## Example` section",
                "crates/re_core/explain/RE0001.md: it names a milestone (`M1`): say what the \
                 engine does now",
            ]
        );
    }

    #[test]
    fn relative_paths_use_slashes() {
        let root = Path::new("ws");
        let file = root.join("crates").join("re_core").join("src").join("a.rs");
        assert_eq!(rel_slash(root, &file), "crates/re_core/src/a.rs");
    }

    #[test]
    fn fails_outside_the_range_and_across_crates() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "crates/re_core/src/lib.rs",
            "const A: &str = \"RE6001\";\n",
        );
        write(
            dir.path(),
            "crates/re_core/explain/RE6001.md",
            &page("RE6001"),
        );
        write(
            dir.path(),
            "crates/re_core/src/diag.rs",
            "const P: &str = include_str!(\"../explain/RE6001.md\");\n",
        );
        write(
            dir.path(),
            "crates/re_types/src/lib.rs",
            "const A: &str = \"RE0003\";\n",
        );
        write(
            dir.path(),
            "crates/re_types/explain/RE0003.md",
            &page("RE0003"),
        );
        write(
            dir.path(),
            "crates/re_scene/src/a.rs",
            "const A: &str = \"RE1300\";\n",
        );
        write(
            dir.path(),
            "crates/re_command/src/b.rs",
            "const A: &str = \"RE1300\";\n",
        );
        let problems = scan(dir.path()).unwrap().unwrap_err();
        let text = problems.join("\n");
        assert!(
            text.contains("RE6001 (crates/re_core/src/lib.rs:1) is outside re_core's range"),
            "{text}"
        );
        assert!(
            text.contains("RE0003") && text.contains("it owns no codes"),
            "{text}"
        );
        assert!(
            text.contains("RE1300 is used by more than one crate (re_command, re_scene)"),
            "{text}"
        );
    }

    #[test]
    fn comments_and_non_literals_are_ignored() {
        let src = r####"
// Code::new("RE0001") in a line comment
/* "RE0002" in a block /* nested "RE0003" */ comment */
/// Doc: `HostError { code: "RE0501" }`
let a = 'x'; let q = '"'; let e = '\''; fn f<'a>(x: &'a str) {}
let s = "not RE0004 because of the prefix";
let t = "RE00045";
let raw = r#"RE0005"#;
let esc = "say \"RE0006\"";
let ok = "RE0007";
"####;
        let codes: Vec<String> = find_codes(src).into_iter().map(|(c, _)| c).collect();
        assert_eq!(codes, ["RE0005", "RE0007"]);
        let lines: Vec<usize> = find_codes(src).into_iter().map(|(_, l)| l).collect();
        assert_eq!(lines, [8, 10]);
    }
}
