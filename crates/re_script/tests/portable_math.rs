//! Luau's transcendental maths come from the `libm` crate on every platform
//! (ADR-0036): constant and variable arguments, the fastcall path and the table
//! path all give libm's bits.

mod common;

use common::Rig;

fn number(rig: &mut Rig, source: &str) -> f64 {
    rig.exec(source).unwrap().as_f64().unwrap()
}

#[test]
fn math_functions_give_libm_bits() {
    let mut rig = Rig::new();
    // glibc's sin rounds this input one bit differently from libm, so the
    // first case fails on Linux if a call reaches the C library.
    let x = 1.000_017_361_111_111_2_f64;
    let cases: [(&str, f64); 13] = [
        ("math.sin(x)", libm::sin(x)),
        ("math.cos(x)", libm::cos(x)),
        ("math.tan(x)", libm::tan(x)),
        ("math.asin(x / 2)", libm::asin(x / 2.0)),
        ("math.acos(x / 2)", libm::acos(x / 2.0)),
        ("math.atan(x)", libm::atan(x)),
        ("math.atan2(x, 0.3)", libm::atan2(x, 0.3)),
        ("math.sinh(x)", libm::sinh(x)),
        ("math.exp(x)", libm::exp(x)),
        ("math.log(x)", libm::log(x)),
        ("math.log(x, 7)", libm::log(x) / libm::log(7.0)),
        ("math.log10(x)", libm::log10(x)),
        ("math.pow(x, 1.7)", libm::pow(x, 1.7)),
    ];
    for (expr, want) in cases {
        let got = number(&mut rig, &format!("local x = {x:?}\nreturn {expr}"));
        assert_eq!(got.to_bits(), want.to_bits(), "{expr}: {got} != {want}");
    }
    // Constant arguments are not folded by the compiler with the C library.
    let folded = number(&mut rig, "return math.sin(1.0000173611111112)");
    assert_eq!(folded.to_bits(), libm::sin(x).to_bits());
    assert_eq!(number(&mut rig, "return math.log(8, 2)"), 3.0);
}

#[test]
fn vector_angle_is_signed_by_its_axis() {
    let mut rig = Rig::new();
    let a = number(
        &mut rig,
        "return vector.angle(vector.create(1, 0, 0), vector.create(0, 1, 0))",
    );
    assert_eq!(a.to_bits(), libm::atan2(1.0, 0.0).to_bits());
    let signed = number(
        &mut rig,
        "return vector.angle(vector.create(1, 0, 0), vector.create(0, 1, 0), vector.create(0, 0, -1))",
    );
    assert_eq!(signed, -a);
}
