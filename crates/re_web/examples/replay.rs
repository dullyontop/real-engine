//! Plays a project's replays with the browser runtime's code, natively, and prints
//! each run as the JSON line `tools/web-test/browser.mjs` prints for the wasm build,
//! so CI can compare the two. Usage:
//!
//! ```text
//! cargo run --release -p re_web --example replay -- <project dir> <replay path>...
//! ```

use std::path::Path;

use re_web::Session;

fn add_dir(session: &mut Session, root: &Path, dir: &Path) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.expect("directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let rel = path
            .strip_prefix(root)
            .expect("under the project")
            .to_string_lossy()
            .replace('\\', "/");
        if rel.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            add_dir(session, root, &path);
        } else {
            let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{rel}: {e}"));
            session
                .add_file(&rel, &bytes)
                .unwrap_or_else(|e| panic!("{e}"));
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [project, replays @ ..] = args.as_slice() else {
        eprintln!("usage: replay <project dir> <replay path>...");
        std::process::exit(2);
    };
    let root = Path::new(project);
    let mut session = Session::new();
    add_dir(&mut session, root, root);
    for replay in replays {
        match session.run_replay(replay) {
            Ok(run) => {
                let mut run: serde_json::Value = serde_json::from_str(&run).expect("JSON");
                run["replay"] = replay.as_str().into();
                println!("{run}");
            }
            Err(e) => {
                eprintln!("{replay}: {e}");
                std::process::exit(1);
            }
        }
    }
}
