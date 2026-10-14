//! Compares two PNG images with the golden-image tolerance.
//!
//! ```text
//! re-imgdiff <a.png> <b.png> [--max-delta N] [--outliers FRACTION] [--diff out.png]
//! ```
//!
//! Defaults: 2 levels per channel and a 0.001 (0.1%) outlier budget. Prints one
//! summary line and exits with 0 when the images match, 1 when they do not, and 2
//! on a usage or file error. `--diff` writes a picture of the differences
//! (outliers in red, smaller differences in yellow).

use std::path::Path;
use std::process::ExitCode;

use re_render::Image;
use re_render::imgdiff::{self, Tolerance};

const USAGE: &str =
    "usage: re-imgdiff <a.png> <b.png> [--max-delta N] [--outliers FRACTION] [--diff out.png]";

fn main() -> ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(message) => {
            eprint!("{message}");
            if !message.ends_with('\n') {
                eprintln!();
            }
            ExitCode::from(2)
        }
    }
}

fn run(args: Vec<String>) -> Result<bool, String> {
    let mut files = Vec::new();
    let mut tolerance = Tolerance::default();
    let mut diff_path = None;
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| format!("{name} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--max-delta" => {
                tolerance.max_channel_delta = value("--max-delta")?
                    .parse()
                    .map_err(|e| format!("--max-delta: {e}"))?;
            }
            "--outliers" => {
                tolerance.outlier_fraction = value("--outliers")?
                    .parse()
                    .map_err(|e| format!("--outliers: {e}"))?;
            }
            "--diff" => diff_path = Some(value("--diff")?),
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(true);
            }
            _ if arg.starts_with('-') => return Err(format!("unknown option `{arg}`\n{USAGE}")),
            _ => files.push(arg),
        }
    }
    let [a, b] = files.as_slice() else {
        return Err(USAGE.to_string());
    };
    let load = |p: &String| Image::load_png(Path::new(p)).map_err(|d| d.render_human());
    let (ia, ib) = (load(a)?, load(b)?);
    let report = imgdiff::compare(&ia, &ib, tolerance).map_err(|d| d.render_human())?;
    println!("{a} vs {b}: {}", report.summary());
    if let Some(path) = diff_path {
        imgdiff::diff_image(&ia, &ib, tolerance)
            .and_then(|d| d.save_png(Path::new(&path)))
            .map_err(|d| d.render_human())?;
    }
    Ok(report.matches())
}
