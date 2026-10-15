//! `re_geom`'s diagnostic catalogue is complete and valid.

use std::collections::BTreeMap;

use re_geom::{CODE_RANGE, DIAGNOSTICS};

#[test]
fn catalogue_is_valid() {
    let problems = re_core::validate_catalog(DIAGNOSTICS, CODE_RANGE);
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn catalogue_matches_the_explain_pages() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("explain");
    let mut pages = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let code = name.strip_suffix(".md").unwrap().to_string();
        pages.insert(
            code,
            std::fs::read_to_string(&path)
                .unwrap()
                .replace("\r\n", "\n"),
        );
    }
    let catalogue: BTreeMap<String, String> = DIAGNOSTICS
        .iter()
        .map(|d| (d.code.to_string(), d.page.replace("\r\n", "\n")))
        .collect();
    assert_eq!(catalogue.len(), DIAGNOSTICS.len(), "duplicate codes");
    assert_eq!(catalogue, pages);
}
