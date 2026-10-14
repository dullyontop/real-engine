//! `re_core`'s own diagnostic catalogue is complete and valid.

use std::collections::BTreeMap;

use re_core::{CODE_RANGE, Code, DIAGNOSTICS, DiagInfo, validate_catalog};

#[test]
fn catalogue_is_valid() {
    let problems = validate_catalog(DIAGNOSTICS, CODE_RANGE);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// The catalogue holds exactly the pages in `explain/`, each with the page's text.
/// Together with `cargo xtask explain` (every code used in `src/` has a page),
/// this means every code used in source is in `DIAGNOSTICS`.
#[test]
fn catalogue_matches_the_explain_pages() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("explain");
    let mut pages = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let code = name.strip_suffix(".md").unwrap_or_else(|| panic!("{name}"));
        // Normalise line endings, in case a checkout converted them.
        let text = std::fs::read_to_string(&path)
            .unwrap()
            .replace("\r\n", "\n");
        pages.insert(code.to_string(), text);
    }
    let catalogue: BTreeMap<String, String> = DIAGNOSTICS
        .iter()
        .map(|d| (d.code.to_string(), d.page.replace("\r\n", "\n")))
        .collect();
    assert_eq!(catalogue.len(), DIAGNOSTICS.len(), "duplicate codes");
    assert_eq!(
        catalogue.keys().collect::<Vec<_>>(),
        pages.keys().collect::<Vec<_>>()
    );
    assert_eq!(catalogue, pages);
}

#[test]
fn validation_finds_each_kind_of_problem() {
    const A: Code = Code::new("RE0001");
    const FAR: Code = Code::new("RE7001");
    let bad = [
        DiagInfo {
            code: A,
            title: "ok",
            page: "# RE0001: ok\n",
        },
        DiagInfo {
            code: A,
            title: "dup",
            page: "# RE0001: dup\n",
        },
        DiagInfo {
            code: FAR,
            title: " ",
            page: "no heading",
        },
    ];
    let problems = validate_catalog(&bad, 0..=499);
    assert_eq!(problems.len(), 4, "{problems:#?}");
}
