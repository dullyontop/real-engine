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

/// Whether the installed luau-lsp is the pinned one, whose output the tier 3
/// snapshot records. CI pins it, so there it must be.
fn checker_is_pinned() -> bool {
    let version =
        std::process::Command::new(types::find_checker(&TypesOptions::default()).unwrap())
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
    let pinned = version == types::LUAU_LSP_VERSION;
    assert!(
        pinned || std::env::var_os("CI").is_none(),
        "CI must run luau-lsp {}, found {version:?}",
        types::LUAU_LSP_VERSION
    );
    pinned
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

    // Tier 2: the Roblox-style surface that is not built yet shows up as
    // gaps; what the data model and runtime cover (`game`, `workspace`,
    // `Instance.new`, `RunService`) and the native API do not.
    let gap = &report.scan.gap.missing;
    assert!(
        gap.contains_key("Players.PlayerAdded"),
        "Players.PlayerAdded missing from {gap:?}"
    );
    for name in ["game", "Instance", "workspace", "RunService.Heartbeat"] {
        assert!(!gap.contains_key(name), "{name} is covered: {gap:?}");
    }
    assert!(!gap.contains_key("world.query") && !gap.contains_key("vector.create"));
    assert_eq!(report.scan.services["Players"], 1);
    assert_eq!(report.scan.dynamic_requires, 1);
    assert_eq!(
        report.scan.parse_failed.len(),
        1,
        "the syntax error does not parse either"
    );

    // Tiers 1 and 2 are snapshotted whether or not a checker is installed.
    let mut snapshot = report.clone();
    snapshot.types = None;
    insta::assert_json_snapshot!("fixtures_report", snapshot);

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

    if !checker_is_pinned() {
        println!(
            "skipped the tier 3 snapshot: luau-lsp is not {}",
            types::LUAU_LSP_VERSION
        );
        return;
    }
    let mut t = t.clone();
    t.checker = t.checker.as_ref().map(|_| "luau-lsp".to_string());
    insta::assert_json_snapshot!("fixtures_types", t);
}

#[test]
fn missing_and_overlapping_directories() {
    let dirs = [
        PathBuf::from("tests/fixtures"),
        PathBuf::from("tests/fixtures/lib"),
        PathBuf::from("tests/no_such_dir"),
    ];
    let report = corpus::run(&dirs, &CorpusOptions::default());
    // lib/ is inside fixtures/, so its file is counted once.
    assert_eq!(report.files, 9);
    assert!(!report.compile_passed());
    assert!(
        report
            .compile
            .failed
            .iter()
            .any(|d| d.message.contains("tests/no_such_dir")),
        "{:#?}",
        report.compile.failed
    );
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
