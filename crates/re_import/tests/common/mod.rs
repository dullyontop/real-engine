//! Shared helpers for the re_import tests.

#![allow(dead_code, reason = "test helpers, not all of them used by every test")]

use std::path::{Path, PathBuf};

use re_compat::data::ClassFile;
use re_compat::{ClassRegistry, ClassRegistryBuilder};

/// Stand-in classes the data files do not declare yet (and a test class with
/// properties); the importer reads whatever the class layer defines.
pub const TEST_CLASSES: &str = include_str!("../fixtures/classes.json");

pub fn registry() -> ClassRegistry {
    let mut b = ClassRegistryBuilder::builtin().unwrap().classes(
        "tests/fixtures/classes.json",
        ClassFile::parse("classes.json", TEST_CLASSES).unwrap(),
    );
    for (class, prop, a) in re_compat::runtime::accessors() {
        b = b.accessor(class, prop, a);
    }
    b.build().unwrap()
}

pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Every file under `dir`, relative and sorted, with its text.
pub fn tree_text(dir: &Path) -> Vec<(String, String)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read_to_string(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}
