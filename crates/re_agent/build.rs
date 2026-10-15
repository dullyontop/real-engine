//! Embeds the engine's documentation (`docs/**/*.md` at the workspace root) so
//! agents can read it as MCP resources and through `resource.read`
//! (docs/M2_PLAN.md, WP17). The list is generated, so a new document needs no
//! change here.

use std::path::{Path, PathBuf};

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, out);
        } else if path.extension().is_some_and(|e| e == "md") {
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
        let abs = path.canonicalize().expect("a collected file exists");
        code.push_str(&format!(
            "    ({rel:?}, include_str!({:?})),\n",
            abs.display().to_string()
        ));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("set by cargo")).join("docs.rs");
    std::fs::write(out, code).expect("OUT_DIR is writable");
}
