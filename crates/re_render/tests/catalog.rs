//! `re_render`'s diagnostic catalogue is complete and valid. No GPU needed.

use std::collections::BTreeMap;

use re_render::{CODE_RANGE, DIAGNOSTICS};

#[test]
fn catalogue_is_valid() {
    let problems = re_core::validate_catalog(DIAGNOSTICS, CODE_RANGE);
    assert!(problems.is_empty(), "{problems:#?}");
}

/// The catalogue holds exactly the pages in `explain/`, each with the page's text.
#[test]
fn catalogue_matches_the_explain_pages() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("explain");
    let mut pages = BTreeMap::new();
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let code = name.strip_suffix(".md").unwrap_or_else(|| panic!("{name}"));
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
    assert_eq!(catalogue, pages);
}

#[test]
fn backend_names_parse() {
    assert_eq!(
        re_render::parse_backends("Vulkan"),
        Some(wgpu::Backends::VULKAN)
    );
    assert_eq!(re_render::parse_backends(" gl "), Some(wgpu::Backends::GL));
    assert_eq!(re_render::parse_backends("vulcan"), None);
}
