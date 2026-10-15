//! The verify-diff loop (docs/M2_PLAN.md, WP17): which verify results a
//! change altered.
//!
//! [`diff`] compares two verify reports (`{ ok, stages: [{ name, ok,
//! diagnostics, duration_ms, data }], perf }`, ADR-0022) and lists, per stage,
//! whether it passes before and after, the diagnostics that appeared or went
//! away, and the values in its `data` that changed. The PerfReport is compared
//! as a stage named `perf-report`.
//!
//! Timings (`duration_ms` and every key ending in `_ms`) change on every run,
//! so they are left out unless asked for. The host (`re_cli`'s `verify.diff`)
//! runs verify and keeps one baseline report per agent checkpoint.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json};

/// The most value changes listed per stage; the rest are counted.
pub const MAX_VALUES: usize = 100;

/// What changed between two verify reports.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct VerifyDiff {
    /// The baseline report passed.
    pub ok_before: bool,
    /// The new report passes.
    pub ok_after: bool,
    /// False when nothing in the compared results changed.
    pub changed: bool,
    /// One line for people: what changed, stage by stage.
    pub summary: String,
    /// The stages that changed, in report order (unchanged stages are left
    /// out).
    pub stages: Vec<StageDiff>,
}

/// What changed in one stage.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StageDiff {
    /// The stage name (`schema`, `compile`, `boot`, ..., or `perf-report`).
    pub name: String,
    /// `changed`, `added` (only in the new report) or `removed`.
    pub status: String,
    /// Whether the stage passed in the baseline (absent for `added`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok_before: Option<bool>,
    /// Whether it passes now (absent for `removed`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok_after: Option<bool>,
    /// Diagnostics only in the new report.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics_added: Vec<Json>,
    /// Diagnostics only in the baseline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics_removed: Vec<Json>,
    /// Values in the stage's data that changed, by path (`data.frames`,
    /// `data.replays[walk].state_hash`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<ValueChange>,
    /// Value changes not listed (beyond [`MAX_VALUES`]).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub values_not_listed: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// One changed value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ValueChange {
    /// Where it is.
    pub path: String,
    /// Its baseline value (`null` when it was absent).
    pub before: Json,
    /// Its new value (`null` when it is gone).
    pub after: Json,
}

/// Options for [`diff`].
#[derive(Clone, Copy, Debug, Default)]
pub struct DiffOptions {
    /// Compare timings too (`duration_ms` and `*_ms` keys).
    pub include_timings: bool,
}

fn is_timing(key: &str) -> bool {
    key == "duration_ms" || key.ends_with("_ms")
}

/// The identity of a diagnostic: everything but its hint and related notes.
fn diag_key(d: &Json) -> String {
    let pick = |k: &str| d.get(k).cloned().unwrap_or(Json::Null);
    Json::Array(vec![
        pick("code"),
        pick("severity"),
        pick("message"),
        pick("location"),
    ])
    .to_string()
}

/// The key that names an element of a list of records (`name`, `id`, `path`
/// or `file`), if every element has one and they are distinct.
fn element_keys(items: &[Json]) -> Option<Vec<String>> {
    for field in ["name", "id", "path", "file"] {
        let keys: Option<Vec<String>> = items
            .iter()
            .map(|v| v.get(field).and_then(Json::as_str).map(str::to_string))
            .collect();
        if let Some(keys) = keys {
            let mut seen = std::collections::BTreeSet::new();
            if keys.iter().all(|k| seen.insert(k.clone())) {
                return Some(keys);
            }
        }
    }
    None
}

fn diff_values(
    path: &str,
    before: &Json,
    after: &Json,
    opts: DiffOptions,
    out: &mut Vec<ValueChange>,
) {
    if before == after {
        return;
    }
    match (before, after) {
        (Json::Object(a), Json::Object(b)) => {
            let keys: std::collections::BTreeSet<&String> = a.keys().chain(b.keys()).collect();
            for key in keys {
                if !opts.include_timings && is_timing(key) {
                    continue;
                }
                let child = format!("{path}.{key}");
                diff_values(
                    &child,
                    a.get(key).unwrap_or(&Json::Null),
                    b.get(key).unwrap_or(&Json::Null),
                    opts,
                    out,
                );
            }
        }
        (Json::Array(a), Json::Array(b)) => {
            if let (Some(ka), Some(kb)) = (element_keys(a), element_keys(b)) {
                let by_a: BTreeMap<&str, &Json> =
                    ka.iter().map(String::as_str).zip(a.iter()).collect();
                let by_b: BTreeMap<&str, &Json> =
                    kb.iter().map(String::as_str).zip(b.iter()).collect();
                let mut keys: Vec<&str> = ka.iter().map(String::as_str).collect();
                keys.extend(
                    kb.iter()
                        .map(String::as_str)
                        .filter(|k| !by_a.contains_key(k)),
                );
                for key in keys {
                    diff_values(
                        &format!("{path}[{key}]"),
                        by_a.get(key).copied().unwrap_or(&Json::Null),
                        by_b.get(key).copied().unwrap_or(&Json::Null),
                        opts,
                        out,
                    );
                }
            } else if a.len() == b.len() {
                for (i, (x, y)) in a.iter().zip(b).enumerate() {
                    diff_values(&format!("{path}[{i}]"), x, y, opts, out);
                }
            } else {
                out.push(ValueChange {
                    path: path.to_string(),
                    before: before.clone(),
                    after: after.clone(),
                });
            }
        }
        _ => out.push(ValueChange {
            path: path.to_string(),
            before: before.clone(),
            after: after.clone(),
        }),
    }
}

/// `report`'s stages by name, in order, with the PerfReport as a stage named
/// `perf-report` (its data is the report).
fn stages(report: &Json) -> Vec<(String, Json)> {
    let mut out: Vec<(String, Json)> = report
        .get("stages")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let name = s.get("name").and_then(Json::as_str)?;
            Some((name.to_string(), s.clone()))
        })
        .collect();
    if let Some(perf) = report.get("perf").filter(|p| !p.is_null()) {
        let mut stage = Map::new();
        stage.insert("name".into(), Json::from("perf-report"));
        stage.insert("ok".into(), Json::Bool(true));
        stage.insert("data".into(), perf.clone());
        out.push(("perf-report".to_string(), Json::Object(stage)));
    }
    out
}

fn stage_diff(
    name: &str,
    before: Option<&Json>,
    after: Option<&Json>,
    opts: DiffOptions,
) -> StageDiff {
    let ok = |s: Option<&Json>| s.map(|s| s.get("ok").and_then(Json::as_bool).unwrap_or(false));
    let diags = |s: Option<&Json>| -> Vec<Json> {
        s.and_then(|s| s.get("diagnostics"))
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let (db, da) = (diags(before), diags(after));
    let keys_b: Vec<String> = db.iter().map(diag_key).collect();
    let keys_a: Vec<String> = da.iter().map(diag_key).collect();
    let diagnostics_added = da
        .iter()
        .zip(&keys_a)
        .filter(|(_, k)| !keys_b.contains(k))
        .map(|(d, _)| d.clone())
        .collect();
    let diagnostics_removed = db
        .iter()
        .zip(&keys_b)
        .filter(|(_, k)| !keys_a.contains(k))
        .map(|(d, _)| d.clone())
        .collect();
    let mut values = Vec::new();
    let data = |s: Option<&Json>| s.and_then(|s| s.get("data")).cloned().unwrap_or(Json::Null);
    diff_values("data", &data(before), &data(after), opts, &mut values);
    if opts.include_timings {
        let t = |s: Option<&Json>| {
            s.and_then(|s| s.get("duration_ms"))
                .cloned()
                .unwrap_or(Json::Null)
        };
        diff_values("duration_ms", &t(before), &t(after), opts, &mut values);
    }
    let values_not_listed = values.len().saturating_sub(MAX_VALUES);
    values.truncate(MAX_VALUES);
    StageDiff {
        name: name.to_string(),
        status: match (before, after) {
            (None, _) => "added",
            (_, None) => "removed",
            _ => "changed",
        }
        .to_string(),
        ok_before: ok(before),
        ok_after: ok(after),
        diagnostics_added,
        diagnostics_removed,
        values,
        values_not_listed,
    }
}

fn stage_changed(d: &StageDiff) -> bool {
    d.status != "changed"
        || d.ok_before != d.ok_after
        || !d.diagnostics_added.is_empty()
        || !d.diagnostics_removed.is_empty()
        || !d.values.is_empty()
}

/// What changed from the `before` report to the `after` report.
pub fn diff(before: &Json, after: &Json, opts: DiffOptions) -> VerifyDiff {
    let sb = stages(before);
    let sa = stages(after);
    let mut names: Vec<&str> = sa.iter().map(|(n, _)| n.as_str()).collect();
    for (n, _) in &sb {
        if !names.contains(&n.as_str()) {
            names.push(n);
        }
    }
    let find = |list: &[(String, Json)], name: &str| -> Option<Json> {
        list.iter().find(|(n, _)| n == name).map(|(_, s)| s.clone())
    };
    let stages: Vec<StageDiff> = names
        .into_iter()
        .map(|name| {
            stage_diff(
                name,
                find(&sb, name).as_ref(),
                find(&sa, name).as_ref(),
                opts,
            )
        })
        .filter(stage_changed)
        .collect();
    let ok_before = before.get("ok").and_then(Json::as_bool).unwrap_or(false);
    let ok_after = after.get("ok").and_then(Json::as_bool).unwrap_or(false);
    let summary = if stages.is_empty() {
        "no verify result changed".to_string()
    } else {
        let parts: Vec<String> = stages
            .iter()
            .map(|s| {
                let mut bits = Vec::new();
                match (s.ok_before, s.ok_after) {
                    (Some(true), Some(false)) => bits.push("now fails".to_string()),
                    (Some(false), Some(true)) => bits.push("now passes".to_string()),
                    _ => {}
                }
                if s.status != "changed" {
                    bits.push(s.status.clone());
                }
                if !s.diagnostics_added.is_empty() {
                    bits.push(format!("{} new diagnostics", s.diagnostics_added.len()));
                }
                if !s.diagnostics_removed.is_empty() {
                    bits.push(format!("{} diagnostics gone", s.diagnostics_removed.len()));
                }
                let n = s.values.len() + s.values_not_listed;
                if n > 0 {
                    bits.push(format!("{n} values changed"));
                }
                format!("{} ({})", s.name, bits.join(", "))
            })
            .collect();
        format!(
            "verify {} → {}; changed: {}",
            if ok_before { "ok" } else { "failing" },
            if ok_after { "ok" } else { "failing" },
            parts.join("; ")
        )
    };
    VerifyDiff {
        ok_before,
        ok_after,
        changed: !stages.is_empty() || ok_before != ok_after,
        summary,
        stages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(boot_ok: bool, frames: u64, hash: &str, ms: f64) -> Json {
        let mut boot_diags = vec![];
        if !boot_ok {
            boot_diags.push(json!({ "code": "RE8008", "severity": "error", "message": "boom" }));
        }
        json!({
            "ok": boot_ok,
            "stages": [
                { "name": "schema", "ok": true, "diagnostics": [], "duration_ms": ms, "data": { "files": 3 } },
                { "name": "boot", "ok": boot_ok, "diagnostics": boot_diags, "duration_ms": ms,
                  "data": { "frames": frames, "replays": [ { "name": "walk", "state_hash": hash } ] } }
            ],
            "perf": { "frame_ms": ms, "entities": 10 }
        })
    }

    #[test]
    fn identical_reports_differ_only_in_timings() {
        let d = diff(
            &report(true, 120, "a", 1.0),
            &report(true, 120, "a", 9.0),
            DiffOptions::default(),
        );
        assert!(!d.changed, "{d:?}");
        assert_eq!(d.summary, "no verify result changed");
        let d = diff(
            &report(true, 120, "a", 1.0),
            &report(true, 120, "a", 9.0),
            DiffOptions {
                include_timings: true,
            },
        );
        assert!(d.changed);
        assert!(
            d.stages
                .iter()
                .any(|s| s.values.iter().any(|v| v.path == "duration_ms"))
        );
        assert!(d.stages.iter().any(|s| s.name == "perf-report"));
    }

    #[test]
    fn a_failing_boot_is_reported_with_its_values() {
        let d = diff(
            &report(true, 120, "a", 1.0),
            &report(false, 60, "b", 1.0),
            DiffOptions::default(),
        );
        assert!(d.changed);
        assert!(d.ok_before && !d.ok_after);
        assert_eq!(d.stages.len(), 1, "{d:#?}");
        let boot = &d.stages[0];
        assert_eq!(boot.name, "boot");
        assert_eq!((boot.ok_before, boot.ok_after), (Some(true), Some(false)));
        assert_eq!(boot.diagnostics_added.len(), 1);
        let paths: Vec<&str> = boot.values.iter().map(|v| v.path.as_str()).collect();
        assert_eq!(paths, ["data.frames", "data.replays[walk].state_hash"]);
        assert!(
            d.summary
                .contains("boot (now fails, 1 new diagnostics, 2 values changed)"),
            "{}",
            d.summary
        );
    }

    #[test]
    fn added_and_removed_stages() {
        let before =
            json!({ "ok": true, "stages": [ { "name": "old", "ok": true, "diagnostics": [] } ] });
        let after =
            json!({ "ok": true, "stages": [ { "name": "boot", "ok": true, "diagnostics": [] } ] });
        let d = diff(&before, &after, DiffOptions::default());
        let status: Vec<(&str, &str)> = d
            .stages
            .iter()
            .map(|s| (s.name.as_str(), s.status.as_str()))
            .collect();
        assert_eq!(status, [("boot", "added"), ("old", "removed")]);
    }
}
