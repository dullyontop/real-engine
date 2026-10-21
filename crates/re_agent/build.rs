//! Embeds the engine's documentation (`docs/**/*.md` at the workspace root) so
//! agents can read it as MCP resources and through `resource.read`
//! (docs/M2_PLAN.md, WP17). The list is generated, so a new document needs no
//! change here.
//!
//! It also embeds every file under `templates/` (ADR-0046), as bytes, for
//! `templates::Library::builtin`.

use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    collect_where(dir, out, &|p| p.extension().is_some_and(|e| e == "md"));
}

fn collect_where(dir: &Path, out: &mut Vec<PathBuf>, keep: &dyn Fn(&Path) -> bool) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_where(&path, out, keep);
        } else if keep(&path) {
            out.push(path);
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let docs = manifest.join("../../docs");
    println!("cargo:rerun-if-changed={}", docs.display());
    let mut files = Vec::new();
    collect(&docs, &mut files);
    files.sort();
    let mut code = String::from(
        "/// Every engine document, as (path under docs/, contents).\npub(crate) const DOCS: &[(&str, &str)] = &[\n",
    );
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let rel = path
            .strip_prefix(&docs)
            .expect("collected under docs/")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        // A file that vanished since it was listed is left out, not an error.
        let Ok(abs) = path.canonicalize() else {
            continue;
        };
        code.push_str(&format!(
            "    ({rel:?}, include_str!({:?})),\n",
            abs.display().to_string()
        ));
    }
    code.push_str("];\n");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo"));
    std::fs::write(out_dir.join("docs.rs"), code).expect("OUT_DIR is writable");

    let templates = manifest.join("../../templates");
    println!("cargo:rerun-if-changed={}", templates.display());
    let mut files = Vec::new();
    collect_where(&templates, &mut files, &|_| true);
    files.sort();
    let mut code = String::from(
        "/// Every file under templates/, as (path, bytes).\npub(super) static FILES: &[(&str, &[u8])] = &[\n",
    );
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
        let rel = path
            .strip_prefix(&templates)
            .expect("collected under templates/")
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        let Ok(abs) = path.canonicalize() else {
            continue;
        };
        code.push_str(&format!(
            "    ({rel:?}, include_bytes!({:?})),\n",
            abs.display().to_string()
        ));
    }
    code.push_str("];\n");
    std::fs::write(out_dir.join("templates.rs"), code).expect("OUT_DIR is writable");
}
