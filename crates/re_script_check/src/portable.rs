//! Portability warnings (ADR-0036, D2 determinism).
//!
//! Luau's `math` functions run on `libm` in every engine VM
//! (`re_script::portable_math`), so they give the same bits on every
//! platform. The `^` operator does not: it calls the platform's C `pow`, and
//! the compiler folds a constant `a ^ b` with the `pow` of the machine that
//! compiled it. Only the exponents 2, 0.5 and 3 are exact everywhere, because
//! Luau compiles them to a multiplication or a square root. [`pow_warnings`]
//! warns about every other `^` and `^=` (`RE3083`), with the portable spelling
//! `math.pow(x, y)` as the fix.

use full_moon::LuaVersion;
use full_moon::ast::{self, BinOp, CompoundOp, Expression};
use full_moon::tokenizer::TokenReference;
use full_moon::visitors::Visitor;
use re_core::{Diagnostic, Location};

use crate::codes;

/// Exponents Luau compiles without calling `pow`.
const EXACT: [f64; 3] = [2.0, 0.5, 3.0];

/// The stack the parse runs on (full_moon recurses through nesting).
const STACK: usize = 512 * 1024 * 1024;

/// One `RE3083` warning for each `^` or `^=` in `source` whose exponent is
/// not the literal 2, 0.5 or 3. A file that does not parse gives none (the
/// scan reports it).
pub fn pow_warnings(source: &str, name: &str) -> Vec<Diagnostic> {
    let source = source.to_string();
    let file = name.to_string();
    std::thread::Builder::new()
        .name("re_script_check pow".into())
        .stack_size(STACK)
        .spawn(move || on_this_thread(&source, &file))
        .ok()
        .and_then(|h| h.join().ok())
        .unwrap_or_default()
}

fn on_this_thread(source: &str, name: &str) -> Vec<Diagnostic> {
    let result = full_moon::parse_fallible(source, LuaVersion::luau());
    if !result.errors().is_empty() {
        return Vec::new();
    }
    let mut walker = Walker {
        file: name.to_string(),
        out: Vec::new(),
    };
    walker.visit_ast(result.ast());
    walker.out
}

struct Walker {
    file: String,
    out: Vec<Diagnostic>,
}

/// Whether `e` is one of the [`EXACT`] number literals.
fn exact(e: &Expression) -> bool {
    match e {
        Expression::Number(t) => {
            let text = t.token().to_string().replace('_', "");
            text.parse::<f64>().is_ok_and(|v| EXACT.contains(&v))
        }
        Expression::Parentheses { expression, .. } => exact(expression),
        _ => false,
    }
}

impl Walker {
    fn warn(&mut self, at: &TokenReference, what: &str) {
        let start = at.token().start_position();
        self.out.push(
            Diagnostic::warning(
                codes::PLATFORM_POW,
                format!(
                    "`{what}` with this exponent calls the platform's `pow`, which differs between Linux, Windows, macOS and the web"
                ),
            )
            .at(Location::File {
                file: self.file.clone(),
                line: Some(start.line() as u32),
                column: Some(start.character() as u32),
            })
            .with_hint("write `math.pow(x, y)`, which gives the same result everywhere (only the exponents 2, 0.5 and 3 are exact with `^`)"),
        );
    }
}

impl Visitor for Walker {
    fn visit_expression(&mut self, e: &Expression) {
        if let Expression::BinaryOperator {
            binop: BinOp::Caret(t),
            rhs,
            ..
        } = e
            && !exact(rhs)
        {
            self.warn(t, "^");
        }
    }

    fn visit_compound_assignment(&mut self, c: &ast::CompoundAssignment) {
        if let CompoundOp::CaretEqual(t) = c.compound_operator()
            && !exact(c.rhs())
        {
            self.warn(t, "^=");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(source: &str) -> Vec<u32> {
        pow_warnings(source, "scripts/a.luau")
            .iter()
            .map(|d| match &d.location {
                Some(Location::File { line, .. }) => line.unwrap_or(0),
                _ => 0,
            })
            .collect()
    }

    #[test]
    fn only_inexact_exponents_warn() {
        let source = "\
local a = x ^ 2
local b = x ^ 0.5
local c = x ^ (3)
local d = x ^ 1.5
local e = x ^ y
local f = 2 ^ 10
a ^= 2
a ^= n
local g = math.pow(x, 1.5)
";
        assert_eq!(lines(source), vec![4, 5, 6, 8]);
        let d = &pow_warnings("local d = x ^ 1.5\n", "s.luau")[0];
        assert_eq!(d.code, codes::PLATFORM_POW);
        assert!(d.hint.as_deref().unwrap().contains("math.pow"));
    }

    #[test]
    fn a_file_that_does_not_parse_gives_no_warnings() {
        assert!(lines("local = ^ ^").is_empty());
    }
}
