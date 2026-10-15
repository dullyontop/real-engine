//! The corpus validation suite (ADR-0024).
//!
//! * `fixtures`: always runs, over the synthetic files in `tests/fixtures/`.
//! * `external_corpus`: runs over the directories in `REAL_SCENE_SCRIPTS` (`:`
//!   separated on Unix, `;` on Windows) and skips when it is unset. Nothing from
//!   those directories is committed; the report goes to a temporary file.

use std::path::PathBuf;

use re_script_check::codes;
use re_script_check::corpus::{self, CorpusOptions};
use re_script_check::types::{self, TypesOptions};

fn checker_present() -> bool {
    let found = types::find_checker(&TypesOptions::default()).is_some();
    assert!(
        found || std::env::var_os("CI").is_none(),
        "luau-lsp must be installed on CI"
    );
    found
}

#[test]
fn fixtures() {
    let dirs = [PathBuf::from("tests/fixtures")];
    let with_types = checker_present();
    let report = corpus::run(
        &dirs,
        &CorpusOptions {
            types: with_types,
            ..CorpusOptions::default()
        },
    );
    println!("{}", report.to_json());

    // Tier 1: everything compiles except the deliberate syntax error.
    assert_eq!(report.files, 9);
    assert_eq!(report.compile.ok, 8);
    assert_eq!(report.compile.failed.len(), 1);
    assert!(matches!(
        &report.compile.failed[0].location,
        Some(re_core::Location::File { file, .. }) if file == "tests/fixtures/syntax_error.luau"
    ));

    // Tier 2: the Roblox-style surface shows up as gaps; the native API does not.
    let gap = &report.scan.gap.missing;
    for name in [
        "game",
        "Instance",
        "workspace",
        "Players.PlayerAdded",
        "RunService.Heartbeat",
    ] {
        assert!(gap.contains_key(name), "{name} missing from {gap:?}");
    }
    assert!(!gap.contains_key("world.query") && !gap.contains_key("vector.create"));
    assert_eq!(report.scan.services["Players"], 1);
    assert_eq!(report.scan.dynamic_requires, 1);
    assert_eq!(
        report.scan.parse_failed.len(),
        1,
        "the syntax error does not parse either"
    );

    // Tier 3: the deliberate type error is reported against engine.d.luau.
    let Some(t) = &report.types else {
        println!("skipped tier 3: luau-lsp not found");
        return;
    };
    let type_errors: Vec<_> = t
        .diagnostics
        .iter()
        .filter(|d| {
            d.code == codes::TYPE_ERROR
                && matches!(&d.location, Some(re_core::Location::File { file, line: Some(3), .. }) if file == "tests/fixtures/type_error.luau")
        })
        .collect();
    assert_eq!(type_errors.len(), 1, "{:#?}", t.diagnostics);

    let mut snapshot = report.clone();
    if let Some(t) = &mut snapshot.types {
        t.checker = t.checker.as_ref().map(|_| "luau-lsp".to_string());
    }
    insta::assert_json_snapshot!("fixtures_report", snapshot);
}

#[test]
fn external_corpus() {
    let dirs = corpus::dirs_from_env();
    if dirs.is_empty() {
        println!("skipped: REAL_SCENE_SCRIPTS not set");
        return;
    }
    let report = corpus::run(&dirs, &CorpusOptions::default());
    let out = std::env::temp_dir().join("re_script_check-corpus.json");
    std::fs::write(&out, report.to_json()).expect("write the report");
    println!(
        "corpus: {} files, {} compiled, {} failed, {} gap names; report: {}",
        report.files,
        report.compile.ok,
        report.compile.failed.len(),
        report.scan.gap.missing.len(),
        out.display()
    );
    for d in report.compile.failed.iter().take(10) {
        println!("  {}: {}", d.code, d.message);
    }
    assert!(report.files > 0, "no .lua or .luau files under {dirs:?}");
    assert!(
        report.compile_passed(),
        "tier 1 is the gate: every file must compile"
    );
}
