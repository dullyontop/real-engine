//! `cargo xtask clippy-web`: checks that `tools/ci/clippy-web/clippy.toml`, the clippy
//! settings of CI's wasm32 pass (ADR-0049 §5), is the root `clippy.toml` plus the
//! web-only entries. Clippy reads a single file, so the wasm file repeats the root one.

use std::path::Path;

use anyhow::{Context, Result, bail};
use toml::{Table, Value};

/// The entries only the wasm pass has: methods that panic on wasm32-unknown-unknown.
const WEB_ONLY: [&str; 3] = [
    "std::time::Instant::now",
    "std::time::SystemTime::now",
    "std::process::id",
];

const WEB_FILE: &str = "tools/ci/clippy-web/clippy.toml";

pub fn run(root: &Path) -> Result<()> {
    let read = |rel: &str| -> Result<Option<Table>> {
        let path = root.join(rel);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {rel}"))?;
        Ok(Some(
            text.parse::<Table>()
                .with_context(|| format!("parsing {rel}"))?,
        ))
    };
    let base = read("clippy.toml")?.unwrap_or_default();
    let web = read(WEB_FILE)?.with_context(|| format!("{WEB_FILE} is missing"))?;
    let problems = compare(&base, &web);
    if !problems.is_empty() {
        bail!(
            "{WEB_FILE} must be clippy.toml plus the entries for {}:\n  {}",
            WEB_ONLY.join(", "),
            problems.join("\n  ")
        );
    }
    println!("clippy-web: {WEB_FILE} matches clippy.toml plus the web-only entries");
    Ok(())
}

fn entry_path(entry: &Value) -> Option<&str> {
    match entry {
        Value::String(s) => Some(s),
        Value::Table(t) => t.get("path").and_then(Value::as_str),
        _ => None,
    }
}

/// What is wrong with `web` as `base` plus the web-only entries; empty when it is right.
fn compare(base: &Table, web: &Table) -> Vec<String> {
    let mut problems = Vec::new();
    let mut web = web.clone();
    let methods = web
        .get_mut("disallowed-methods")
        .and_then(Value::as_array_mut);
    match methods {
        Some(methods) => {
            for path in WEB_ONLY {
                let before = methods.len();
                methods.retain(|e| entry_path(e) != Some(path));
                if methods.len() + 1 != before {
                    problems.push(format!(
                        "expected exactly one disallowed-methods entry for {path}"
                    ));
                }
            }
            if methods.is_empty() && !base.contains_key("disallowed-methods") {
                web.remove("disallowed-methods");
            }
        }
        None => problems.push("no disallowed-methods list".to_owned()),
    }
    for (key, value) in base {
        match web.get(key) {
            Some(v) if v == value => {}
            Some(_) => problems.push(format!("`{key}` differs from clippy.toml")),
            None => problems.push(format!("`{key}` from clippy.toml is missing")),
        }
    }
    for key in web.keys() {
        if !base.contains_key(key) {
            problems.push(format!("`{key}` is not in clippy.toml"));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(text: &str) -> Table {
        text.parse().unwrap()
    }

    const WEB_ONLY_ENTRIES: &str = r#"
        { path = "std::time::Instant::now", reason = "r" },
        { path = "std::time::SystemTime::now", reason = "r" },
        { path = "std::process::id", reason = "r" },"#;

    #[test]
    fn root_plus_web_only_passes() {
        let base = table(r#"disallowed-methods = [{ path = "f32::sin", reason = "libm" }]"#);
        let web = table(&format!(
            r#"disallowed-methods = [{{ path = "f32::sin", reason = "libm" }},{WEB_ONLY_ENTRIES}]"#
        ));
        assert_eq!(compare(&base, &web), Vec::<String>::new());
        let web = table(&format!("disallowed-methods = [{WEB_ONLY_ENTRIES}]"));
        assert_eq!(compare(&Table::new(), &web), Vec::<String>::new());
    }

    #[test]
    fn drift_fails() {
        let base = table(r#"disallowed-methods = [{ path = "f32::sin", reason = "libm" }]"#);
        let missing = table(&format!("disallowed-methods = [{WEB_ONLY_ENTRIES}]"));
        assert_eq!(compare(&base, &missing).len(), 1);
        let no_web = table(r#"disallowed-methods = [{ path = "f32::sin", reason = "libm" }]"#);
        assert_eq!(compare(&base, &no_web).len(), 3);
        let extra = table(&format!(
            "msrv = \"1.95\"\ndisallowed-methods = [{WEB_ONLY_ENTRIES}]"
        ));
        assert_eq!(compare(&Table::new(), &extra).len(), 1);
    }
}
