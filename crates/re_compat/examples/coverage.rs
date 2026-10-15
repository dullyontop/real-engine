//! Writes `crates/re_compat/coverage.json` from the class, enum and library
//! files, or with `--check` fails when the committed file differs.
//! `cargo xtask coverage [--check]` runs this.
//!
//! ```text
//! cargo run -p re_compat --example coverage -- [--check] [PATH]
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use re_compat::{ClassRegistryBuilder, CoverageFile, DataSet};

fn main() -> ExitCode {
    let mut check = false;
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("coverage.json");
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--check" => check = true,
            _ => path = PathBuf::from(arg),
        }
    }
    let table = DataSet::builtin().and_then(|data| {
        // The registry checks what the table alone cannot: superclasses,
        // cycles and own property types.
        ClassRegistryBuilder::from_data(data.clone()).build_data_only()?;
        CoverageFile::from_data(&data)
    });
    let text = match table {
        Ok(t) => t.to_text(),
        Err(e) => {
            eprintln!("{}", e.to_diagnostic().render_human());
            return ExitCode::FAILURE;
        }
    };
    if check {
        // A Windows checkout may turn the line ends into CRLF.
        let committed = std::fs::read_to_string(&path)
            .unwrap_or_default()
            .replace("\r\n", "\n");
        if committed != text {
            eprintln!(
                "{} is out of date; run `cargo xtask coverage`",
                path.display()
            );
            return ExitCode::FAILURE;
        }
        println!("coverage: {} is up to date", path.display());
    } else {
        if let Err(e) = std::fs::write(&path, &text) {
            eprintln!("cannot write {}: {e}", path.display());
            return ExitCode::FAILURE;
        }
        println!("coverage: wrote {}", path.display());
    }
    ExitCode::SUCCESS
}
