//! A compat game played in the browser keeps its saved data in the files the
//! boot script stores in IndexedDB, and finds it there at the next start.

use std::path::Path;
use std::sync::Mutex;

use re_compat::ClassRegistry;
use re_web::Session;

static PERSISTED: Mutex<Vec<(String, Option<Vec<u8>>)>> = Mutex::new(Vec::new());

fn persist(key: &str, bytes: Option<&[u8]>) {
    PERSISTED
        .lock()
        .unwrap()
        .push((key.to_owned(), bytes.map(<[u8]>::to_vec)));
}

const SERVER: &str = r#"
local store = game:GetService("DataStoreService"):GetDataStore("Scores")
local best = store:GetAsync("best")
print("best", best)
store:SetAsync("best", (best or 0) + 1)
print("saved")
"#;

/// A compat project whose only script is `SERVER`, handed over file by file.
fn session(saved: &[(String, Vec<u8>)]) -> Session {
    let dir = tempfile::tempdir().unwrap();
    let classes = ClassRegistry::builtin().unwrap();
    re_import::new_project(dir.path(), Some("compat"), &classes).unwrap();
    let input = dir.path().join("loader.luau");
    std::fs::write(
        &input,
        format!(
            "local s = Instance.new(\"Script\")\ns.Name = \"Main\"\ns.Source = [==[{SERVER}]==]\n\
             s.Parent = game:GetService(\"ServerScriptService\")\n"
        ),
    )
    .unwrap();
    let report = re_import::import(&input, dir.path(), &classes, &Default::default()).unwrap();
    assert!(report.ok(), "{report:?}");
    std::fs::remove_file(input).unwrap();
    let mut session = Session::new();
    add_dir(&mut session, dir.path(), dir.path());
    for (key, bytes) in saved {
        session.add_saved(key, bytes);
    }
    session
}

fn add_dir(session: &mut Session, root: &Path, dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_str()
            .unwrap()
            .replace('\\', "/");
        if rel.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            add_dir(session, root, &path);
        } else {
            session
                .add_file(&rel, &std::fs::read(&path).unwrap())
                .unwrap();
        }
    }
}

fn play(session: &Session) -> Vec<String> {
    let mut app = session.open_for_play(persist).unwrap();
    let report = app.run_headless(Some(30), None).unwrap();
    assert!(report.ok(), "{:?}", report.diagnostics);
    report.output
}

#[test]
fn saved_data_reaches_indexeddb_and_comes_back() {
    assert_eq!(
        play(&session(&[])),
        ["[server] best\tnil", "[server] saved"]
    );
    let saved: Vec<(String, Vec<u8>)> = PERSISTED
        .lock()
        .unwrap()
        .iter()
        .filter_map(|(k, b)| Some((k.clone(), b.clone()?)))
        .collect();
    assert!(!saved.is_empty());
    assert!(
        saved.iter().all(|(k, _)| k.starts_with("data/")),
        "{saved:?}"
    );

    assert_eq!(
        play(&session(&saved)),
        ["[server] best\t1", "[server] saved"]
    );
}
