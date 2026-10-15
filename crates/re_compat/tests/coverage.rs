//! The coverage table: the committed `coverage.json` matches the data files,
//! and the gap-report keys derive from it.

use re_compat::data::{CoverageFile, DataSet};
use re_compat::{COVERAGE_JSON, Status};

#[test]
fn committed_coverage_matches_the_data_files() {
    let fresh = CoverageFile::from_data(&DataSet::builtin().unwrap()).unwrap();
    // A Windows checkout may turn the committed file's line ends into CRLF.
    assert_eq!(
        COVERAGE_JSON.replace("\r\n", "\n"),
        fresh.to_text(),
        "coverage.json is out of date; run `cargo xtask coverage`"
    );
    let parsed = CoverageFile::parse(COVERAGE_JSON).unwrap();
    assert_eq!(parsed, fresh);
    let rows: usize = parsed.totals.values().sum();
    let counted = parsed.classes.len()
        + parsed
            .classes
            .iter()
            .map(|c| c.members.len())
            .sum::<usize>()
        + parsed.enums.len()
        + parsed.libraries.len()
        + parsed
            .libraries
            .iter()
            .map(|l| l.members.len() + l.value_members.len())
            .sum::<usize>();
    assert_eq!(rows, counted);
}

#[test]
fn keys_expand_services_and_instance_globals() {
    let t = CoverageFile::parse(
        r#"{
          "format": "real-engine/compat-coverage", "version": 1, "totals": {},
          "classes": [
            {"name": "Instance", "status": "partial", "members": [
              {"name": "Name", "kind": "property", "status": "implemented"},
              {"name": "Destroy", "kind": "method", "status": "missing"}]},
            {"name": "Players", "superclass": "Instance", "service": true, "status": "partial",
             "members": [{"name": "Destroy", "kind": "method", "status": "stub-error"}]},
            {"name": "DataModel", "superclass": "Instance", "status": "partial", "members": []}],
          "enums": [{"name": "KeyCode", "status": "missing", "items": ["A"]}],
          "libraries": [
            {"name": "game", "class": "DataModel", "status": "implemented", "members": []},
            {"name": "task", "status": "partial", "note": "n", "members": [
              {"name": "wait", "kind": "member", "status": "implemented"}]}]
        }"#,
    )
    .unwrap();
    let k = t.keys();
    assert_eq!(k["Players.Name"], Status::Implemented);
    // The subclass's own row wins over the inherited one.
    assert_eq!(k["Players.Destroy"], Status::StubError);
    assert_eq!(k["game"], Status::Implemented);
    assert_eq!(k["game.Name"], Status::Implemented);
    assert_eq!(k["game.Destroy"], Status::Missing);
    assert_eq!(k["task"], Status::Partial);
    assert_eq!(k["task.wait"], Status::Implemented);
    assert_eq!(k["Enum"], Status::Missing);
    assert_eq!(k["Enum.KeyCode"], Status::Missing);
    assert!(
        !k.contains_key("Instance.Name"),
        "classes that are not services key by receiver"
    );
}

#[test]
fn data_schemas_are_generated_for_every_format() {
    let names: Vec<&str> = re_compat::data::schemas().iter().map(|(n, _)| *n).collect();
    assert_eq!(
        names,
        [
            "compat-classes.schema.json",
            "compat-coverage.schema.json",
            "compat-enums.schema.json",
            "compat-libraries.schema.json"
        ]
    );
}
