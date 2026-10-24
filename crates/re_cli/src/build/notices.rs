//! The third-party notices (ADR-0037): `THIRD_PARTY_NOTICES.txt` lists the
//! crates a game runtime is built from, each with its license and license
//! texts, then the game's own notices.
//!
//! The runtime's part comes from `cargo tree` (the crates cargo builds the
//! runtime from for its target) and `cargo metadata` (where their sources
//! are), both run in the engine's workspace, so it is written where a runtime
//! is built. A prebuilt runtime carries it in its folder, and a build that
//! uses one copies it from there.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use re_app::package::archive::ENGINE_VERSION;
use re_app::package::config::glob_match;
use re_core::files::ProjectFiles;
use re_core::{Diagnostic, Location};
use serde::Serialize;

use crate::codes;

pub use crate::game::NOTICES_FILE;

/// The licenses `deny.toml` allows, in its order. Of a choice of licenses,
/// the one listed first is the one whose terms the notices follow.
pub const ALLOWED: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "ISC",
    "Zlib",
    "CC0-1.0",
    "Unicode-3.0",
];

/// The crates `deny.toml` allows another license (ADR-0019).
pub const EXCEPTIONS: &[(&str, &[&str])] = &[
    ("full_moon", &["MPL-2.0"]),
    ("full_moon_derive", &["MPL-2.0"]),
    ("triple_buffer", &["MPL-2.0"]),
    ("epaint_default_fonts", &["OFL-1.1", "Ubuntu-font-1.0"]),
];

/// Standard license texts, for a crate that ships no license file;
/// `{copyright}` stands for its authors.
const STANDARD: &[(&str, &str)] = &[
    ("MIT", include_str!("licenses/MIT.txt")),
    ("Apache-2.0", include_str!("licenses/Apache-2.0.txt")),
    (
        "Apache-2.0 WITH LLVM-exception",
        include_str!("licenses/Apache-2.0-WITH-LLVM-exception.txt"),
    ),
    ("BSD-2-Clause", include_str!("licenses/BSD-2-Clause.txt")),
    ("BSD-3-Clause", include_str!("licenses/BSD-3-Clause.txt")),
    ("BSL-1.0", include_str!("licenses/BSL-1.0.txt")),
    ("ISC", include_str!("licenses/ISC.txt")),
    ("Zlib", include_str!("licenses/Zlib.txt")),
    ("CC0-1.0", include_str!("licenses/CC0-1.0.txt")),
    ("Unicode-3.0", include_str!("licenses/Unicode-3.0.txt")),
    ("MPL-2.0", include_str!("licenses/MPL-2.0.txt")),
];

/// Name prefixes of license files, compared in lowercase.
const LICENSE_NAMES: &[&str] = &[
    "license",
    "licence",
    "copying",
    "copyright",
    "notice",
    "unlicense",
    "ofl",
    "ufl",
];

/// Extensions of code and data files, which are never license texts.
const NOT_TEXT: &[&str] = &[
    "rs", "c", "h", "cc", "cpp", "hpp", "py", "toml", "json", "yml", "yaml", "sh", "js", "ts",
    "html", "css", "xml", "in", "cmake", "png", "svg", "ttf", "otf",
];

/// Folders never searched for license files.
const SKIPPED_DIRS: &[&str] = &[
    ".git", ".github", "target", "tests", "test", "benches", "examples", "fuzz",
];

/// How many folder levels below a package's root license files are found
/// (`vendor/JoltPhysics/LICENSE`, `fonts/OFL.txt`).
const DEPTH: usize = 3;

/// The heading of the game's own notices; a runtime's part ends before it.
const GAME_HEADING: &str = "THE GAME'S OWN NOTICES";

/// A game runtime whose crates the notices list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Runtime {
    /// The desktop game runtime: `re_cli` with only its `game` feature.
    Desktop,
    /// The web runtime: `re_web`.
    Web,
}

impl Runtime {
    /// The package and features `cargo tree` lists, as the runtime's build
    /// selects them.
    fn packages(self) -> &'static [&'static str] {
        match self {
            Runtime::Desktop => &[
                "-p",
                "re_cli",
                "--no-default-features",
                "--features",
                "game",
            ],
            Runtime::Web => &["-p", "re_web"],
        }
    }

    fn label(self) -> &'static str {
        match self {
            Runtime::Desktop => "game runtime",
            Runtime::Web => "web runtime",
        }
    }
}

/// What the notices hold, for the build report.
#[derive(Clone, Debug, Default, Serialize)]
pub struct NoticesReport {
    /// The third-party crates listed; none when the runtime's part was copied
    /// from a prebuilt runtime.
    pub crates: Option<usize>,
    /// The distinct license texts; none when copied.
    pub texts: Option<usize>,
    /// Crates that ship no license file at their root, listed with the
    /// standard text of their license and their authors as the copyright line.
    pub standard_texts: Vec<String>,
    /// The game's own notice files (`[package] notices`).
    pub game_files: Vec<String>,
}

/// A package of the runtime's crate graph.
#[derive(Clone, Debug)]
struct Package {
    name: String,
    version: String,
    license: Option<String>,
    license_file: Option<String>,
    authors: Vec<String>,
    root: PathBuf,
    /// A crate of the engine's workspace.
    ours: bool,
}

fn failed(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::NOTICES_FAILED, message)
}

/// A section heading: the title between two rules.
fn heading(title: &str) -> String {
    let rule = "=".repeat(80);
    format!("{rule}\n{title}\n{rule}\n")
}

/// The line above and below a license text's label.
fn text_rule() -> String {
    "-".repeat(80)
}

/// The runtime's part of the notices for `triple`, listed with cargo in the
/// engine workspace `ws`.
///
/// # Errors
///
/// `RE0954` when cargo fails, or when a crate's license is not allowed or no
/// text of it can be found.
pub fn runtime_notices(
    ws: &Path,
    runtime: Runtime,
    triple: &str,
) -> Result<(String, NoticesReport), Diagnostic> {
    let packages = packages(ws, runtime, triple)?;
    let intro = format!(
        "The Real Engine {ENGINE_VERSION} {} for {triple} is built from the third-party software listed below.",
        runtime.label()
    );
    gather(&intro, &packages)
}

/// Runs cargo in `ws` and returns what it printed.
fn run_cargo(ws: &Path, args: &[&str]) -> Result<Vec<u8>, Diagnostic> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(ws)
        .args(args)
        .output()
        .map_err(|e| failed(format!("cannot run cargo: {e}")))?;
    if !output.status.success() {
        return Err(failed(format!(
            "cargo {} could not list the runtime's crates: {}",
            args[0],
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

/// The packages cargo builds the runtime from for `triple`, sorted by name and
/// version.
fn packages(ws: &Path, runtime: Runtime, triple: &str) -> Result<Vec<Package>, Diagnostic> {
    let mut args = vec!["tree", "--locked", "--quiet"];
    args.extend(runtime.packages());
    // Every crate cargo builds for the runtime but its dev-dependencies: build
    // scripts compile C and C++ sources into it (`luau0-src` holds Luau's, a
    // build-dependency of `mlua-sys`), and proc macros write code into it.
    args.extend([
        "-e",
        "normal,build",
        "--target",
        triple,
        "--prefix",
        "none",
        "--format",
        "{p}",
    ]);
    let tree = run_cargo(ws, &args)?;
    let mut listed = BTreeSet::new();
    for line in String::from_utf8_lossy(&tree).lines() {
        let mut words = line.split_whitespace();
        if let (Some(name), Some(version)) = (words.next(), words.next()) {
            let version = version.strip_prefix('v').unwrap_or(version);
            listed.insert((name.to_owned(), version.to_owned()));
        }
    }
    let metadata = run_cargo(
        ws,
        &[
            "metadata",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            triple,
        ],
    )?;
    let metadata: serde_json::Value = serde_json::from_slice(&metadata)
        .map_err(|e| failed(format!("cargo metadata printed no metadata: {e}")))?;
    let ours: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect();
    let text = |p: &serde_json::Value, key: &str| p[key].as_str().map(str::to_owned);
    let mut found: BTreeMap<(String, String), Package> = BTreeMap::new();
    for p in metadata["packages"].as_array().into_iter().flatten() {
        let (Some(name), Some(version)) = (text(p, "name"), text(p, "version")) else {
            continue;
        };
        let key = (name, version);
        if !listed.contains(&key) || found.contains_key(&key) {
            continue;
        }
        let manifest = PathBuf::from(text(p, "manifest_path").unwrap_or_default());
        let package = Package {
            name: key.0.clone(),
            version: key.1.clone(),
            license: text(p, "license"),
            license_file: text(p, "license_file"),
            authors: p["authors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(str::to_owned))
                .collect(),
            root: manifest.parent().map(Path::to_path_buf).unwrap_or_default(),
            ours: p["id"].as_str().is_some_and(|id| ours.contains(id)),
        };
        found.insert(key, package);
    }
    if let Some((name, version)) = listed.iter().find(|k| !found.contains_key(*k)) {
        return Err(failed(format!(
            "cargo metadata does not list {name} {version}, which cargo tree does"
        )));
    }
    Ok(found.into_values().collect())
}

/// The license texts so far, each printed once and referred to by number.
#[derive(Default)]
struct Texts {
    texts: Vec<String>,
    numbers: HashMap<String, usize>,
}

impl Texts {
    /// The number of `text` (from 1), adding it when it is new.
    fn add(&mut self, text: String) -> usize {
        if let Some(n) = self.numbers.get(&text) {
            return *n;
        }
        self.texts.push(text.clone());
        self.numbers.insert(text, self.texts.len());
        self.texts.len()
    }
}

/// A listed crate: its heading lines, and its texts with their labels.
struct Listed {
    lines: Vec<String>,
    texts: Vec<(usize, String)>,
}

/// The runtime's part of the notices for `packages`.
fn gather(intro: &str, packages: &[Package]) -> Result<(String, NoticesReport), Diagnostic> {
    let mut texts = Texts::default();
    let mut crates = Vec::new();
    let mut bundled = Vec::new();
    let mut report = NoticesReport::default();
    for p in packages {
        let mut files = license_files(&p.root);
        if let Some(file) = &p.license_file {
            let file = file.replace('\\', "/");
            if !files.contains(&file) {
                files.push(file);
            }
        }
        let mut found = Vec::new();
        for file in &files {
            if let Some(text) = read_text(&p.root.join(file)) {
                found.push((texts.add(text), file.clone()));
            }
        }
        if p.ours {
            // The engine's own crates: only the files they bundle (the compat
            // fonts) carry licenses of their own.
            if !found.is_empty() {
                bundled.push(Listed {
                    lines: vec![p.name.clone()],
                    texts: found,
                });
            }
            continue;
        }
        let id = format!("{} {}", p.name, p.version);
        let Some(expression) = &p.license else {
            return Err(failed(format!("{id} names no license in its manifest"))
                .with_hint("deny.toml needs a clarification for it, and the notices with it"));
        };
        let expr = parse_expr(expression).map_err(|why| {
            failed(format!(
                "{id}: cannot read its license `{expression}`: {why}"
            ))
        })?;
        let Some((_, chosen)) = expr.choose(&|license| rank(&p.name, license)) else {
            return Err(failed(format!(
                "{id} is licensed `{expression}`, which deny.toml does not allow"
            ))
            .with_hint("ADR-0019: replace the crate, or add an exception for it to deny.toml and to the notices' list"));
        };
        if !files.iter().any(|f| !f.contains('/')) {
            // No license file at the root: the standard text of each license
            // the crate is used under, with its authors as the copyright line.
            report.standard_texts.push(id.clone());
            let copyright = if p.authors.is_empty() {
                format!("the {} authors", p.name)
            } else {
                p.authors.join(", ")
            };
            let deeper = !found.is_empty();
            for license in &chosen {
                match STANDARD.iter().find(|(l, _)| l == license) {
                    Some((_, standard)) => {
                        let text = normalized(&standard.replace("{copyright}", &copyright));
                        found.push((
                            texts.add(text),
                            format!(
                                "the standard {license} text (the crate ships no license file)"
                            ),
                        ));
                    }
                    // Its deeper files carry it, as egui's fonts carry theirs.
                    None if deeper => {}
                    None => {
                        return Err(failed(format!(
                            "{id} ships no license file, and there is no standard {license} text"
                        )));
                    }
                }
            }
        }
        crates.push(Listed {
            lines: vec![id, format!("    License: {expression}")],
            texts: found,
        });
    }

    let mut out = String::from("THIRD-PARTY NOTICES\n\n");
    out.push_str(&wrap(&format!(
        "{intro} Each entry gives a crate's version and its license (an SPDX expression, where \"/\" means OR), then its license texts by number; the texts follow the list, and a text several crates share is printed once."
    )));
    out.push('\n');
    section(&mut out, "CRATES", &crates);
    section(&mut out, "FILES THE ENGINE BUNDLES", &bundled);
    out.push('\n');
    out.push_str(&heading("LICENSE TEXTS"));
    for (i, text) in texts.texts.iter().enumerate() {
        let rule = text_rule();
        out.push_str(&format!("\n{rule}\n[{}]\n{rule}\n{text}\n", i + 1));
    }
    report.crates = Some(crates.len());
    report.texts = Some(texts.texts.len());
    Ok((out, report))
}

/// A section of listed crates, if there are any.
fn section(out: &mut String, title: &str, listed: &[Listed]) {
    if listed.is_empty() {
        return;
    }
    out.push('\n');
    out.push_str(&heading(title));
    for l in listed {
        out.push('\n');
        for line in &l.lines {
            out.push_str(line);
            out.push('\n');
        }
        for (n, label) in &l.texts {
            out.push_str(&format!("    [{n}] {label}\n"));
        }
    }
}

/// `text` wrapped at 80 columns.
fn wrap(text: &str) -> String {
    let mut out = String::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.len() + 1 + word.len() > 80 {
            out.push_str(&line);
            out.push('\n');
            line.clear();
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The position of `license` in [`ALLOWED`], then in the crate's
/// [`EXCEPTIONS`]; None when the crate may not use it.
fn rank(name: &str, license: &str) -> Option<usize> {
    let license = license.trim_end_matches('+');
    ALLOWED.iter().position(|a| *a == license).or_else(|| {
        EXCEPTIONS
            .iter()
            .find(|(c, _)| *c == name)
            .and_then(|(_, allowed)| allowed.iter().position(|a| *a == license))
            .map(|i| ALLOWED.len() + i)
    })
}

/// True for a file name that names a license text.
fn is_license_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    LICENSE_NAMES.iter().any(|p| lower.starts_with(p))
        && !lower
            .rsplit_once('.')
            .is_some_and(|(_, ext)| NOT_TEXT.contains(&ext))
}

/// The license files under `root`, as paths relative to it with `/`: those at
/// the root first, then those up to [`DEPTH`] levels below. Every file in a
/// `LICENSES` folder counts, as REUSE lays them out.
fn license_files(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, rel: &str, depth: usize, all: bool, found: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() {
                name.clone()
            } else {
                format!("{rel}/{name}")
            };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                let lower = name.to_ascii_lowercase();
                if depth < DEPTH && !SKIPPED_DIRS.contains(&lower.as_str()) {
                    let all = all || lower == "licenses" || lower == "licences";
                    walk(&entry.path(), &path, depth + 1, all, found);
                }
            } else if (all || is_license_file(&name)) && entry.path().is_file() {
                found.push(path);
            }
        }
    }
    let mut found = Vec::new();
    walk(root, "", 0, false, &mut found);
    found.sort_by(|a, b| (a.matches('/').count(), a).cmp(&(b.matches('/').count(), b)));
    found
}

/// A license text as the notices print it: `\n` line ends, no trailing spaces,
/// no blank lines around it.
fn normalized(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    lines.join("\n").trim_matches('\n').to_owned()
}

/// The text of the license file at `path`; None when it is empty, binary or
/// unreadable. A file that is not UTF-8 is read as Latin-1.
fn read_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => e.into_bytes().iter().map(|&b| char::from(b)).collect(),
    };
    let text = normalized(&text);
    (!text.is_empty()).then_some(text)
}

/// A license expression.
#[derive(Debug, PartialEq)]
enum Expr {
    /// One license, or one with an exception (`Apache-2.0 WITH LLVM-exception`).
    License(String),
    /// Every one applies.
    And(Vec<Expr>),
    /// Any one may be chosen.
    Or(Vec<Expr>),
}

impl Expr {
    /// The licenses to follow: of each choice, the alternative whose licenses
    /// rank best by `rank`, with its worst rank. None when no alternative is
    /// allowed.
    fn choose(&self, rank: &dyn Fn(&str) -> Option<usize>) -> Option<(usize, Vec<String>)> {
        match self {
            Expr::License(license) => rank(license).map(|r| (r, vec![license.clone()])),
            Expr::And(items) => {
                let mut worst = 0;
                let mut all = Vec::new();
                for item in items {
                    let (r, licenses) = item.choose(rank)?;
                    worst = worst.max(r);
                    all.extend(licenses);
                }
                Some((worst, all))
            }
            Expr::Or(items) => items
                .iter()
                .filter_map(|item| item.choose(rank))
                .min_by_key(|(r, _)| *r),
        }
    }
}

/// Parses an SPDX license expression; `/` is OR, as old manifests write it.
fn parse_expr(text: &str) -> Result<Expr, String> {
    let spaced = text
        .replace('(', " ( ")
        .replace(')', " ) ")
        .replace('/', " OR ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut pos = 0;
    let expr = parse_or(&tokens, &mut pos)?;
    match tokens.get(pos) {
        None => Ok(expr),
        Some(t) => Err(format!("unexpected `{t}`")),
    }
}

fn is_keyword(token: &str, keyword: &str) -> bool {
    token.eq_ignore_ascii_case(keyword)
}

fn is_license_id(token: &str) -> bool {
    !token.is_empty()
        && !["AND", "OR", "WITH"].iter().any(|k| is_keyword(token, k))
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+' | ':'))
}

fn parse_or(tokens: &[&str], pos: &mut usize) -> Result<Expr, String> {
    let mut items = vec![parse_and(tokens, pos)?];
    while tokens.get(*pos).is_some_and(|t| is_keyword(t, "OR")) {
        *pos += 1;
        items.push(parse_and(tokens, pos)?);
    }
    Ok(if items.len() == 1 {
        items.remove(0)
    } else {
        Expr::Or(items)
    })
}

fn parse_and(tokens: &[&str], pos: &mut usize) -> Result<Expr, String> {
    let mut items = vec![parse_term(tokens, pos)?];
    while tokens.get(*pos).is_some_and(|t| is_keyword(t, "AND")) {
        *pos += 1;
        items.push(parse_term(tokens, pos)?);
    }
    Ok(if items.len() == 1 {
        items.remove(0)
    } else {
        Expr::And(items)
    })
}

fn parse_term(tokens: &[&str], pos: &mut usize) -> Result<Expr, String> {
    match tokens.get(*pos) {
        Some(&"(") => {
            *pos += 1;
            let expr = parse_or(tokens, pos)?;
            if tokens.get(*pos) != Some(&")") {
                return Err("a `(` is not closed".into());
            }
            *pos += 1;
            Ok(expr)
        }
        Some(id) if is_license_id(id) => {
            *pos += 1;
            if tokens.get(*pos).is_some_and(|t| is_keyword(t, "WITH")) {
                let Some(exception) = tokens.get(*pos + 1).filter(|t| is_license_id(t)) else {
                    return Err("WITH names no exception".into());
                };
                *pos += 2;
                return Ok(Expr::License(format!("{id} WITH {exception}")));
            }
            Ok(Expr::License((*id).to_owned()))
        }
        Some(t) => Err(format!("unexpected `{t}`")),
        None => Err("it ends too soon".into()),
    }
}

/// The runtime's part of a notices file: all of it before the game's own
/// notices, so an earlier game's file can stand for its runtime's.
pub fn runtime_part(text: &str) -> String {
    let marker = format!("\n\n{}", heading(GAME_HEADING));
    let part = text.find(&marker).map_or(text, |i| &text[..=i]);
    let mut part = part.trim_end_matches('\n').to_owned();
    part.push('\n');
    part
}

/// The game's own notices: each project file `[package] notices` matches,
/// printed whole under its path. Empty when there are no patterns.
///
/// # Errors
///
/// `RE0954` when a pattern matches no file, or a file cannot be read.
pub fn game_part(
    files: &ProjectFiles,
    patterns: &[String],
) -> Result<(String, Vec<String>), Diagnostic> {
    if patterns.is_empty() {
        return Ok((String::new(), Vec::new()));
    }
    let all: Vec<String> = files
        .list("", "")
        .into_iter()
        .map(|p| p.as_str().to_owned())
        .filter(|p| !p.starts_with(".real/"))
        .collect();
    let mut chosen = BTreeSet::new();
    for pattern in patterns {
        let matched: Vec<&String> = all.iter().filter(|p| glob_match(pattern, p)).collect();
        if matched.is_empty() {
            return Err(failed(format!(
                "[package] notices pattern `{pattern}` matches no file"
            ))
            .at(Location::file("real.toml")));
        }
        chosen.extend(matched.into_iter().cloned());
    }
    let mut out = format!("\n{}", heading(GAME_HEADING));
    for path in &chosen {
        let bytes = files.read_source(path).map_err(|e| {
            failed(format!("cannot read the notice file `{path}`: {e}"))
                .at(Location::file(path.clone()))
        })?;
        let rule = text_rule();
        out.push_str(&format!(
            "\n{rule}\n{path}\n{rule}\n{}\n",
            normalized(&String::from_utf8_lossy(&bytes))
        ));
    }
    Ok((out, chosen.into_iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chosen(name: &str, expression: &str) -> Option<Vec<String>> {
        parse_expr(expression)
            .unwrap()
            .choose(&|l| rank(name, l))
            .map(|(_, licenses)| licenses)
    }

    #[test]
    fn the_lists_match_deny_toml() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deny.toml");
        let deny: toml::Table = std::fs::read_to_string(path).unwrap().parse().unwrap();
        let licenses = deny["licenses"].as_table().unwrap();
        let allow: Vec<&str> = licenses["allow"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(allow, ALLOWED);
        let exceptions: Vec<(&str, Vec<&str>)> = licenses["exceptions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                let allowed = e["allow"].as_array().unwrap();
                (
                    e["crate"].as_str().unwrap(),
                    allowed.iter().map(|v| v.as_str().unwrap()).collect(),
                )
            })
            .collect();
        let ours: Vec<(&str, Vec<&str>)> =
            EXCEPTIONS.iter().map(|(c, l)| (*c, l.to_vec())).collect();
        assert_eq!(exceptions, ours);
    }

    #[test]
    fn every_allowed_license_has_a_standard_text() {
        for license in ALLOWED {
            assert!(
                STANDARD.iter().any(|(l, _)| l == license),
                "no standard text for {license}"
            );
        }
        for (license, text) in STANDARD {
            assert!(text.len() > 500, "{license}");
        }
    }

    #[test]
    fn of_a_choice_the_first_allowed_license_is_followed() {
        assert_eq!(chosen("a", "MIT OR Apache-2.0").unwrap(), ["MIT"]);
        assert_eq!(chosen("a", "Apache-2.0 / MIT").unwrap(), ["MIT"]);
        assert_eq!(chosen("a", "Apache-2.0/MIT").unwrap(), ["MIT"]);
        assert_eq!(
            chosen("a", "(MIT OR Apache-2.0) AND Unicode-3.0").unwrap(),
            ["MIT", "Unicode-3.0"]
        );
        assert_eq!(
            chosen("a", "Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT").unwrap(),
            ["MIT"]
        );
        assert_eq!(
            chosen("a", "Apache-2.0 WITH LLVM-exception").unwrap(),
            ["Apache-2.0 WITH LLVM-exception"]
        );
        assert_eq!(
            chosen("a", "Apache-2.0 OR GPL-2.0-only").unwrap(),
            ["Apache-2.0"]
        );
        assert_eq!(chosen("a", "0BSD OR MIT OR Apache-2.0").unwrap(), ["MIT"]);
        assert_eq!(chosen("a", "GPL-3.0-only"), None);
        assert_eq!(chosen("a", "MIT AND GPL-3.0-only"), None);
        assert_eq!(chosen("triple_buffer", "MPL-2.0").unwrap(), ["MPL-2.0"]);
        assert_eq!(chosen("kira", "MPL-2.0"), None);
        assert_eq!(
            chosen(
                "epaint_default_fonts",
                "(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0"
            )
            .unwrap(),
            ["MIT", "OFL-1.1", "Ubuntu-font-1.0"]
        );
    }

    #[test]
    fn malformed_expressions_are_refused() {
        for bad in [
            "",
            "MIT OR",
            "(MIT",
            "MIT)",
            "MIT WITH",
            "AND MIT",
            "MIT Apache-2.0",
        ] {
            assert!(parse_expr(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn license_files_are_found_at_the_root_and_below() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for file in [
            "LICENSE-MIT",
            "COPYRIGHT",
            "Licence.md",
            "src/license.rs",
            "tests/LICENSE",
            "fonts/OFL.txt",
            "a/b/c/LICENSE",
            "a/b/c/d/LICENSE",
            "LICENSES/Apache-2.0.txt",
            "README.md",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "text").unwrap();
        }
        assert_eq!(
            license_files(root),
            [
                "COPYRIGHT",
                "LICENSE-MIT",
                "Licence.md",
                "LICENSES/Apache-2.0.txt",
                "fonts/OFL.txt",
                "a/b/c/LICENSE",
            ]
        );
    }

    fn package(dir: &Path, name: &str, license: &str, files: &[(&str, &str)]) -> Package {
        let root = dir.join(name);
        std::fs::create_dir_all(&root).unwrap();
        for (file, text) in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        Package {
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            license: Some(license.to_owned()),
            license_file: None,
            authors: vec!["Ada <ada@example.com>".to_owned()],
            root,
            ours: false,
        }
    }

    #[test]
    fn shared_texts_print_once_and_missing_ones_take_the_standard_text() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let mut ours = package(
            d,
            "re_fonts",
            "MIT OR Apache-2.0",
            &[("assets/OFL-A.txt", "font license\r\n")],
        );
        ours.ours = true;
        let packages = vec![
            package(d, "alpha", "MIT", &[("LICENSE", "same text\n")]),
            package(
                d,
                "beta",
                "MIT/Apache-2.0",
                &[("LICENSE-MIT", "same text  \n\n")],
            ),
            package(d, "gamma", "MIT OR Apache-2.0", &[]),
            package(
                d,
                "epaint_default_fonts",
                "(MIT OR Apache-2.0) AND OFL-1.1",
                &[("fonts/OFL.txt", "ofl")],
            ),
            ours,
        ];
        let (text, report) = gather("Intro.", &packages).unwrap();
        assert_eq!(report.crates, Some(4));
        // gamma's and the fonts' standard MIT texts are the same text.
        assert_eq!(report.texts, Some(4), "{text}");
        assert_eq!(
            report.standard_texts,
            ["gamma 1.0.0", "epaint_default_fonts 1.0.0"]
        );
        assert_eq!(text.matches("\nsame text\n").count(), 1, "{text}");
        assert!(
            text.contains("alpha 1.0.0\n    License: MIT\n    [1] LICENSE\n"),
            "{text}"
        );
        assert!(
            text.contains("beta 1.0.0\n    License: MIT/Apache-2.0\n    [1] LICENSE-MIT\n"),
            "{text}"
        );
        assert!(
            text.contains("Copyright (c) Ada <ada@example.com>\n\nPermission is hereby granted"),
            "{text}"
        );
        assert!(
            text.contains("re_fonts\n    [4] assets/OFL-A.txt\n"),
            "{text}"
        );
        assert!(!text.contains('\r'));
        // The same packages give the same file.
        assert_eq!(gather("Intro.", &packages).unwrap().0, text);

        let no_text = package(d, "epsilon", "MIT AND OFL-1.1", &[]);
        let refused = package(d, "zeta", "GPL-3.0-only", &[("LICENSE", "gpl")]);
        for p in [no_text, refused] {
            let err = gather("Intro.", &[p]).unwrap_err();
            assert_eq!(err.code, codes::NOTICES_FAILED);
        }
    }

    #[test]
    fn the_runtime_part_ends_before_the_game_s_notices() {
        let runtime = "THIRD-PARTY NOTICES\n\ncrates\n";
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("real.toml"), "[project]\nname = \"x\"\n").unwrap();
        std::fs::create_dir_all(dir.path().join("LICENSES")).unwrap();
        std::fs::write(dir.path().join("LICENSES/music.txt"), "music by someone\n").unwrap();
        let files = re_app::package::dev_files(dir.path());
        let (game, listed) = game_part(&files, &["LICENSES/*.txt".to_owned()]).unwrap();
        assert_eq!(listed, ["LICENSES/music.txt"]);
        let whole = format!("{runtime}{game}");
        assert!(whole.contains("THE GAME'S OWN NOTICES"), "{whole}");
        assert_eq!(runtime_part(&whole), runtime);
        assert_eq!(runtime_part(runtime), runtime);
        let err = game_part(&files, &["NOTICES/*".to_owned()]).unwrap_err();
        assert_eq!(err.code, codes::NOTICES_FAILED);
        assert_eq!(game_part(&files, &[]).unwrap().0, "");
    }

    /// The real runtime's notices list its crates, the engine's fonts, and
    /// the standard text for a crate that ships none. Luau's sources come in
    /// through a build-dependency, and serde's derives through a proc macro.
    #[test]
    fn the_desktop_runtime_s_notices_list_its_crates() {
        let ws = super::super::engine_workspace().unwrap();
        let (text, report) =
            runtime_notices(&ws, Runtime::Desktop, &crate::game::target_triple()).unwrap();
        for crate_line in [
            "\nluau0-src ",
            "\nmlua 0.12.2\n",
            "\noxijolt-sys ",
            "\nserde_derive ",
            "\nwgpu ",
            "\nzstd-sys ",
        ] {
            assert!(text.contains(crate_line), "{crate_line}");
        }
        assert!(
            !text.contains("\nre_app"),
            "the engine's crates are not third-party"
        );
        assert!(text.contains("assets/fonts/OFL-Montserrat.txt"));
        assert!(text.contains("JoltPhysics/LICENSE"));
        assert!(report.standard_texts.iter().any(|c| c.starts_with("egui ")));
        assert!(report.crates.unwrap() > 200);
    }
}
