//! Records which members scripts reach, for the member-call check
//! (docs/M2_PLAN.md, definition of done item 9: `cargo xtask members`).
//!
//! Off unless `REAL_COMPAT_MEMBER_TRACE` names a directory. Then each process
//! appends `Class.Member` to `<dir>/<pid>.txt` the first time a script
//! reaches that member, naming the declaring class as `coverage.json` does.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// The environment variable that turns tracing on.
pub const TRACE_ENV: &str = "REAL_COMPAT_MEMBER_TRACE";

struct Trace {
    seen: HashSet<String>,
    file: File,
}

static TRACE: OnceLock<Option<Mutex<Trace>>> = OnceLock::new();

/// Notes that a script reached `class.member`.
pub(crate) fn record(class: &str, member: &str) {
    let Some(trace) = TRACE.get_or_init(open) else {
        return;
    };
    let mut trace = trace.lock().unwrap_or_else(|e| e.into_inner());
    let key = format!("{class}.{member}");
    if !trace.seen.contains(&key) {
        // A failed write only loses trace lines; the check then reports the
        // member as not called, which is the safe direction.
        let _ = writeln!(trace.file, "{key}");
        trace.seen.insert(key);
    }
}

fn open() -> Option<Mutex<Trace>> {
    let dir = std::env::var_os(TRACE_ENV)?;
    let dir = Path::new(&dir);
    std::fs::create_dir_all(dir).ok()?;
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(format!("{}.txt", std::process::id())))
        .ok()?;
    Some(Mutex::new(Trace {
        seen: HashSet::new(),
        file,
    }))
}
