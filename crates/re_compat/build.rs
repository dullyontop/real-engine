//! Embeds every data file under `classes/`, `enums/` and `libraries/` into the
//! crate (`OUT_DIR/embedded.rs`), so a work package adds a data file without
//! editing Rust. Files are listed in name order.

use std::fmt::Write as _;
use std::path::Path;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = String::new();
    for dir in ["classes", "enums", "libraries"] {
        println!("cargo:rerun-if-changed={dir}");
        let mut files: Vec<String> = std::fs::read_dir(root.join(dir))
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.ends_with(".json"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        let konst = dir.to_uppercase();
        writeln!(out, "/// The `{dir}/*.json` files, by name.").unwrap();
        writeln!(out, "pub const {konst}: &[(&str, &str)] = &[").unwrap();
        for f in files {
            let path = root.join(dir).join(&f);
            println!("cargo:rerun-if-changed={}", path.display());
            writeln!(
                out,
                "    ({f:?}, include_str!({:?})),",
                path.display().to_string()
            )
            .unwrap();
        }
        writeln!(out, "];").unwrap();
    }
    let dest = Path::new(&std::env::var("OUT_DIR").unwrap()).join("embedded.rs");
    std::fs::write(dest, out).unwrap();
}
