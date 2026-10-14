//! The diagnostic model: codes, rendering and the JSON form.

use re_core::{Code, Diagnostic, Location, Severity, render_human_all, render_json_lines};

const SCRIPT_ERROR: Code = Code::new("RE3004");
const NOTE: Code = Code::new("RE3005");

#[test]
fn codes_parse_and_print() {
    assert_eq!(SCRIPT_ERROR.number(), 3004);
    assert_eq!(SCRIPT_ERROR.to_string(), "RE3004");
    assert_eq!(Code::parse("RE0007"), Code::from_number(7));
    assert_eq!(Code::from_number(10_000), None);
    for bad in [
        "", "RE", "RE123", "RE12345", "re1234", "RX1234", "RE12a4", "RE-123",
    ] {
        assert_eq!(Code::parse(bad), None, "{bad:?}");
    }
}

#[test]
#[should_panic(expected = "four digits")]
fn malformed_code_literal_panics() {
    let text: &'static str = Box::leak(String::from("RE12").into_boxed_str());
    let _ = Code::new(text);
}

fn sample() -> Diagnostic {
    Diagnostic::error(SCRIPT_ERROR, "attempt to call a nil value (field 'Jump')")
        .at(Location::file_at("scripts/game/player.luau", 12, 5))
        .with_hint("did you mean `jump`?")
        .with_related(
            Diagnostic::note(NOTE, "`jump` is defined here").at(Location::file_at(
                "scripts/shared/input.luau",
                3,
                1,
            )),
        )
}

#[test]
fn human_output_looks_like_rustc() {
    let text = sample().render_human();
    let expected = "\
error[RE3004]: attempt to call a nil value (field 'Jump')
  --> scripts/game/player.luau:12:5
  = hint: did you mean `jump`?
  note[RE3005]: `jump` is defined here
    --> scripts/shared/input.luau:3:1
";
    assert_eq!(text, expected);
}

#[test]
fn locations_render() {
    assert_eq!(Location::file("real.toml").to_string(), "real.toml");
    assert_eq!(
        Location::File {
            file: "a.luau".into(),
            line: Some(4),
            column: None
        }
        .to_string(),
        "a.luau:4"
    );
    assert_eq!(
        Location::entity("e7kq2m9x4tb").to_string(),
        "entity `e7kq2m9x4tb`"
    );
    assert_eq!(
        Location::command("entity.set").to_string(),
        "command `entity.set`"
    );
}

#[test]
fn json_is_one_line_and_round_trips() {
    let d = sample();
    let line = d.render_json();
    assert!(!line.contains('\n'));
    assert!(
        line.starts_with(r#"{"code":"RE3004","severity":"error","message":"#),
        "{line}"
    );
    assert!(line.contains(
        r#""location":{"kind":"file","file":"scripts/game/player.luau","line":12,"column":5}"#
    ));
    let back: Diagnostic = serde_json::from_str(&line).unwrap();
    assert_eq!(back, d);
}

#[test]
fn empty_parts_are_left_out_of_json() {
    let d = Diagnostic::warning(SCRIPT_ERROR, "slow").at(Location::entity("crate_3"));
    assert_eq!(
        d.render_json(),
        r#"{"code":"RE3004","severity":"warning","message":"slow","location":{"kind":"entity","entity":"crate_3"}}"#
    );
    let cmd: Diagnostic = serde_json::from_str(
        r#"{"code":"RE2001","severity":"note","message":"m","location":{"kind":"command","command":"entity.set"}}"#,
    )
    .unwrap();
    assert_eq!(cmd.severity, Severity::Note);
    assert_eq!(cmd.location, Some(Location::command("entity.set")));
}

#[test]
fn bad_json_codes_and_unknown_keys_are_rejected() {
    assert!(
        serde_json::from_str::<Diagnostic>(r#"{"code":"E1","severity":"error","message":"m"}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<Diagnostic>(
            r#"{"code":"RE0001","severity":"error","message":"m","extra":1}"#
        )
        .is_err()
    );
}

#[test]
fn several_diagnostics_render_in_order() {
    let a = Diagnostic::error(SCRIPT_ERROR, "first");
    let b = Diagnostic::warning(NOTE, "second");
    assert_eq!(
        render_human_all(&[a.clone(), b.clone()]),
        "error[RE3004]: first\nwarning[RE3005]: second\n"
    );
    let lines = render_json_lines(&[a, b]);
    assert_eq!(lines.lines().count(), 2);
    assert!(lines.ends_with('\n'));
}

#[test]
fn display_and_error_trait() {
    let d = Diagnostic::error(SCRIPT_ERROR, "boom").at(Location::command("undo"));
    assert_eq!(d.to_string(), "error[RE3004]: boom (at command `undo`)");
    let e: Box<dyn std::error::Error> = Box::new(d.clone());
    assert_eq!(e.to_string(), d.to_string());
    assert!(d.is_error());
}
