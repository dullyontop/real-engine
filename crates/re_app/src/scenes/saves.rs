//! Save slots (ADR-0059 §7): the store behind the `save` library.
//!
//! A slot is the game's data under a name, kept as `saves/<slot>.json` in the
//! run's data folder when the run keeps data ([`App::persist_data`]) and
//! `[saves] mode` is `persist`, and in memory otherwise, starting from the
//! project's `data/seed/saves/`, so verify runs, replays and recording runs
//! never read or write a player's saves. A write is kept in memory at once
//! and reaches the folder at EndFrame, once per slot. A file that is not a
//! save is set aside as `<slot>.json.bad`, as compat sets aside bad data
//! files, and its slot reads as empty.
//!
//! [`App::persist_data`]: crate::App::persist_data

use std::collections::{BTreeMap, BTreeSet};

use re_core::files::{ProjectFiles, Writable, WritableDir};
use re_core::{Diagnostic, Location};
use re_script::{SaveInfo, SaveSlot};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::codes;

/// A save file's `format`.
pub const SAVE_FORMAT: &str = "real-engine/save";

/// The save file version this build writes, and the newest it reads.
pub const SAVE_FILE_VERSION: u32 = 1;

/// The data folder's folder of save files.
pub const SAVES_DIR: &str = "saves";

/// The project folder a repeatable run's saves start from.
pub const SEED_DIR: &str = "data/seed/saves";

/// `[saves]` in `real.toml`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SavesConfig {
    /// The game's data version this build writes, and migrates older slots
    /// to (`save.version()`): 1 or more.
    #[serde(default = "first_version")]
    pub version: u32,
    /// Whether a run that keeps data keeps its saves.
    #[serde(default)]
    pub mode: SaveMode,
}

fn first_version() -> u32 {
    1
}

impl Default for SavesConfig {
    fn default() -> Self {
        SavesConfig {
            version: first_version(),
            mode: SaveMode::default(),
        }
    }
}

impl SavesConfig {
    /// True for `[saves]` as written when it is left out.
    pub fn is_default(&self) -> bool {
        *self == SavesConfig::default()
    }

    /// Why the section is wrong, as a sentence (`RE0576` at the config).
    pub fn problem(&self) -> Option<String> {
        (self.version == 0).then(|| "[saves] version is 0; data versions start at 1".to_string())
    }
}

/// `[saves] mode`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum SaveMode {
    /// Saves are kept in the data folder when the run keeps data (`real
    /// run`, the editor, a packaged game).
    #[default]
    Persist,
    /// Saves last as long as the run, even when it keeps data.
    Session,
}

/// A save file: `saves/<slot>.json` in the data folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(title = "Real Engine save")]
pub struct SaveFile {
    /// An optional JSON Schema reference, for a seed written by hand.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Always `real-engine/save`.
    #[schemars(extend("const" = "real-engine/save"))]
    pub format: String,
    /// The file's version: 1.
    #[schemars(extend("const" = 1))]
    pub version: u32,
    /// The game's data version: `[saves] version` of the build that wrote it.
    #[schemars(range(min = 1))]
    pub data_version: u32,
    /// The run's `os.time()` when the slot was written: seconds since 1970,
    /// UTC.
    pub saved_at: i64,
    /// The game's data: the table `save.write` was given, as JSON (an array
    /// for a table with the keys 1 to n, else an object).
    #[schemars(extend("type" = ["object", "array"]))]
    pub data: Json,
}

/// `bytes` as a save slot, or why they are not a save file (the end of a
/// sentence that starts with the file's name).
///
/// # Errors
///
/// Not JSON, another `format`, a newer file version, a missing or wrong
/// field, a `data_version` of 0, or data that is not a table.
pub fn parse(bytes: &[u8]) -> Result<SaveSlot, String> {
    let json: Json = serde_json::from_slice(bytes).map_err(|e| format!("is not JSON ({e})"))?;
    match json.get("format").and_then(Json::as_str) {
        Some(SAVE_FORMAT) => {}
        Some(other) => return Err(format!("has format {other:?}, not {SAVE_FORMAT:?}")),
        None => {
            return Err(format!(
                "is not a save file (no \"format\": {SAVE_FORMAT:?})"
            ));
        }
    }
    if let Some(v) = json.get("version").and_then(Json::as_u64)
        && v > u64::from(SAVE_FILE_VERSION)
    {
        return Err(format!(
            "is save file version {v}, newer than this build reads ({SAVE_FILE_VERSION})"
        ));
    }
    let file: SaveFile =
        serde_json::from_value(json).map_err(|e| format!("is not a valid save file ({e})"))?;
    if file.version != SAVE_FILE_VERSION {
        return Err(format!(
            "is save file version {}; this build reads version {SAVE_FILE_VERSION}",
            file.version
        ));
    }
    if file.data_version == 0 {
        return Err("has data_version 0; data versions start at 1".to_string());
    }
    if !(file.data.is_object() || file.data.is_array()) {
        return Err("holds data that is not a table".to_string());
    }
    Ok(SaveSlot {
        data_version: file.data_version,
        saved_at: file.saved_at,
        data: file.data,
    })
}

/// A save file's fields, borrowed, to write one without copying the data.
#[derive(Serialize)]
struct FileOut<'a> {
    format: &'static str,
    version: u32,
    data_version: u32,
    saved_at: i64,
    data: &'a Json,
}

/// `slot` as a save file: pretty JSON with a final newline, so it reads
/// plainly in a text editor.
pub fn to_file(slot: &SaveSlot) -> Vec<u8> {
    let file = FileOut {
        format: SAVE_FORMAT,
        version: SAVE_FILE_VERSION,
        data_version: slot.data_version,
        saved_at: slot.saved_at,
        data: &slot.data,
    };
    let mut bytes = serde_json::to_vec_pretty(&file).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// The slot a file name stands for: `<slot>.json` with a slot name.
fn slot_of(name: &str) -> Option<&str> {
    name.strip_suffix(".json")
        .filter(|s| re_script::is_slot_name(s))
}

/// Where the store keeps its slots.
#[derive(Clone, Debug)]
enum Keep {
    /// The data folder's `saves/`, which every write reaches at EndFrame.
    Folder(WritableDir),
    /// Memory: nothing outlasts the run.
    Memory,
}

/// The file `name` of `dir`, for messages: its path on disk, or where it is.
fn place(dir: &WritableDir, name: &str) -> String {
    match dir.path() {
        Some(p) => p.join(name).display().to_string(),
        None => format!("{SAVES_DIR}/{name} in the data folder"),
    }
}

/// The save slots of a run (ADR-0059 §7). The slots are read at their first
/// use; it lives outside the World, and a restart keeps it, as it keeps the
/// files of the data folder.
#[derive(Debug)]
pub struct Saves {
    version: u32,
    keep: Keep,
    files: ProjectFiles,
    /// The slots, once read.
    slots: Option<BTreeMap<String, SaveSlot>>,
    /// The slots written or deleted since the last [`Saves::end_frame`].
    dirty: BTreeSet<String>,
    /// Problems found reading the slots, for the next [`Saves::end_frame`].
    problems: Vec<Diagnostic>,
}

impl Saves {
    /// The saves of a run of the project `files` with `[saves]` `config`: in
    /// the data folder when `persist` (the run keeps data) and the mode is
    /// `persist`, else in memory, starting from `data/seed/saves/`.
    pub fn new(files: &ProjectFiles, config: &SavesConfig, persist: bool) -> Saves {
        let keep = if persist && config.mode == SaveMode::Persist {
            Keep::Folder(files.writable(Writable::Data).sub(SAVES_DIR))
        } else {
            Keep::Memory
        };
        Saves {
            version: config.version,
            keep,
            files: files.clone(),
            slots: None,
            dirty: BTreeSet::new(),
            problems: Vec::new(),
        }
    }

    /// True when the slots are kept in the data folder, past the run.
    pub fn persists(&self) -> bool {
        matches!(self.keep, Keep::Folder(_))
    }

    /// `[saves] version`.
    pub fn version(&self) -> u32 {
        self.version
    }

    fn slots(&mut self) -> &mut BTreeMap<String, SaveSlot> {
        let (keep, files, problems) = (&self.keep, &self.files, &mut self.problems);
        self.slots.get_or_insert_with(|| match keep {
            Keep::Folder(dir) => read_folder(dir, problems),
            Keep::Memory => read_seed(files, problems),
        })
    }

    /// Keeps `data` in `slot`, stamped with `[saves] version` and `saved_at`.
    pub fn write(&mut self, slot: &str, saved_at: i64, data: Json) {
        let data_version = self.version;
        self.slots().insert(
            slot.to_string(),
            SaveSlot {
                data_version,
                saved_at,
                data,
            },
        );
        self.dirty.insert(slot.to_string());
    }

    /// The data in `slot`, or `None` for an empty slot.
    pub fn read(&mut self, slot: &str) -> Option<SaveSlot> {
        self.slots().get(slot).cloned()
    }

    /// True if `slot` holds data.
    pub fn exists(&mut self, slot: &str) -> bool {
        self.slots().contains_key(slot)
    }

    /// Every slot that holds data, sorted by name.
    pub fn list(&mut self) -> Vec<SaveInfo> {
        self.slots()
            .iter()
            .map(|(slot, s)| SaveInfo {
                slot: slot.clone(),
                data_version: s.data_version,
                saved_at: s.saved_at,
            })
            .collect()
    }

    /// Empties `slot`.
    pub fn delete(&mut self, slot: &str) {
        if self.slots().remove(slot).is_some() {
            self.dirty.insert(slot.to_string());
        }
    }

    /// EndFrame: writes the slots changed since the last call to the data
    /// folder (once each, outside any script's budget) and returns the
    /// problems: a file that could not be written (`RE0573`, the run goes on
    /// with the slot in memory) and the bad files found since (`RE0574`).
    pub fn end_frame(&mut self) -> Vec<Diagnostic> {
        let mut out = std::mem::take(&mut self.problems);
        let dirty = std::mem::take(&mut self.dirty);
        let Keep::Folder(dir) = &self.keep else {
            return out;
        };
        for slot in dirty {
            let name = format!("{slot}.json");
            let kept = self.slots.as_ref().and_then(|s| s.get(&slot));
            let (verb, result) = match kept {
                Some(s) => ("write", dir.write(&name, &to_file(s))),
                None => ("delete", dir.remove(&name)),
            };
            if let Err(e) = result {
                out.push(Diagnostic::warning(
                    codes::DATA_WRITE_FAILED,
                    format!(
                        "cannot {verb} save slot {slot:?} ({}): {e}; the run goes on with the slot in memory",
                        place(dir, &name)
                    ),
                ));
            }
        }
        out
    }

    /// The saves of a `play.run` copy: the same slots, in memory, so nothing
    /// the copy saves reaches the data folder.
    pub fn fork(&mut self) -> Saves {
        let slots = self.slots().clone();
        Saves {
            version: self.version,
            keep: Keep::Memory,
            files: self.files.clone(),
            slots: Some(slots),
            dirty: BTreeSet::new(),
            problems: Vec::new(),
        }
    }
}

/// The slots in the data folder's `dir`. A file that is not a save is set
/// aside (`RE0574`); one that cannot be read reads as empty (`RE0573`).
fn read_folder(dir: &WritableDir, problems: &mut Vec<Diagnostic>) -> BTreeMap<String, SaveSlot> {
    let mut slots = BTreeMap::new();
    let names = match dir.list("") {
        Ok(names) => names,
        Err(e) => {
            problems.push(Diagnostic::warning(
                codes::DATA_WRITE_FAILED,
                format!(
                    "cannot list the save slots in {}: {e}; the run starts with none",
                    place(dir, "")
                ),
            ));
            return slots;
        }
    };
    for name in names {
        let Some(slot) = slot_of(&name) else {
            continue;
        };
        match dir.read(&name) {
            Ok(Some(bytes)) => match parse(&bytes) {
                Ok(s) => {
                    slots.insert(slot.to_string(), s);
                }
                Err(why) => problems.push(set_aside(dir, &name, &bytes, &why)),
            },
            Ok(None) => {}
            Err(e) => problems.push(Diagnostic::warning(
                codes::DATA_WRITE_FAILED,
                format!(
                    "cannot read save slot {slot:?} ({}): {e}; it reads as empty",
                    place(dir, &name)
                ),
            )),
        }
    }
    slots
}

/// Moves the bad file `name` (holding `bytes`) to the first free
/// `name.bad`, `name.bad.1`, ..., so it is kept and never overwritten: a
/// copy, then a remove, since a data folder has no rename.
fn set_aside(dir: &WritableDir, name: &str, bytes: &[u8], why: &str) -> Diagnostic {
    let moved = (|| -> std::io::Result<String> {
        let mut n = 0;
        loop {
            let bad = if n == 0 {
                format!("{name}.bad")
            } else {
                format!("{name}.bad.{n}")
            };
            if dir.read(&bad)?.is_none() {
                dir.write(&bad, bytes)?;
                dir.remove(name)?;
                return Ok(bad);
            }
            n += 1;
        }
    })();
    let message = match moved {
        Ok(bad) => format!(
            "{} {why}; it was set aside as {} and the slot reads as empty",
            place(dir, name),
            place(dir, &bad)
        ),
        Err(e) => format!(
            "{} {why}, and it could not be set aside ({e}); the slot reads as empty",
            place(dir, name)
        ),
    };
    Diagnostic::warning(codes::DATA_FILE_SET_ASIDE, message)
}

/// The slots of the project's seed, `data/seed/saves/`, where a repeatable
/// run's saves start. A seed that is not a save is skipped (`RE0574`; `real
/// verify` fails on it).
fn read_seed(files: &ProjectFiles, problems: &mut Vec<Diagnostic>) -> BTreeMap<String, SaveSlot> {
    let mut slots = BTreeMap::new();
    for path in files.list(SEED_DIR, ".json") {
        let path = path.as_str();
        let Some(slot) = path
            .strip_prefix(SEED_DIR)
            .and_then(|p| p.strip_prefix('/'))
            .and_then(slot_of)
        else {
            continue;
        };
        let parsed = files
            .read(path)
            .map_err(|e| format!("cannot be read ({e})"))
            .and_then(|d| parse(&d.bytes));
        match parsed {
            Ok(s) => {
                slots.insert(slot.to_string(), s);
            }
            Err(why) => problems.push(
                Diagnostic::warning(
                    codes::DATA_FILE_SET_ASIDE,
                    format!("{path} {why}; the slot starts empty"),
                )
                .at(Location::file(path)),
            ),
        }
    }
    slots
}

#[cfg(test)]
mod tests {
    use super::*;
    use re_core::files::{Form, LayerKind, MemLayer, ProjectPath};
    use serde_json::json;

    fn project(pairs: &[(&str, &str)]) -> ProjectFiles {
        let mut layer = MemLayer::new(LayerKind::Memory);
        for (path, text) in pairs {
            layer.insert(
                ProjectPath::new(path).unwrap(),
                text.as_bytes().to_vec(),
                Form::Source,
            );
        }
        ProjectFiles::for_tests(layer)
    }

    const SEED: &str = r#"{ "$schema": "../../../schemas/save.schema.json", "format": "real-engine/save", "version": 1,
        "data_version": 1, "saved_at": 1767225600, "data": { "coins": 3 } }"#;

    #[test]
    fn files_parse_or_say_why_not() {
        let slot = parse(SEED.as_bytes()).unwrap();
        assert_eq!(slot.data, json!({ "coins": 3 }));
        assert_eq!((slot.data_version, slot.saved_at), (1, 1_767_225_600));
        // What is written reads back.
        assert_eq!(parse(&to_file(&slot)).unwrap(), slot);
        let text = String::from_utf8(to_file(&slot)).unwrap();
        assert!(
            text.starts_with(
                "{\n  \"format\": \"real-engine/save\",\n  \"version\": 1,\n  \"data_version\": 1,"
            ),
            "{text}"
        );
        assert!(text.ends_with("}\n"), "{text}");

        for (text, why) in [
            ("{", "is not JSON"),
            (
                r#"{ "format": "real-engine/scene" }"#,
                r#"has format "real-engine/scene""#,
            ),
            ("[1]", "is not a save file"),
            (
                r#"{ "format": "real-engine/save", "version": 2 }"#,
                "is save file version 2, newer than this build reads (1)",
            ),
            (
                r#"{ "format": "real-engine/save", "version": 1, "data_version": 1, "saved_at": 0 }"#,
                "is not a valid save file (missing field `data`",
            ),
            (
                r#"{ "format": "real-engine/save", "version": 1, "data_version": 0, "saved_at": 0, "data": {} }"#,
                "has data_version 0",
            ),
            (
                r#"{ "format": "real-engine/save", "version": 1, "data_version": 1, "saved_at": 0, "data": 3 }"#,
                "holds data that is not a table",
            ),
        ] {
            let e = parse(text.as_bytes()).unwrap_err();
            assert!(e.starts_with(why), "{text}: {e}");
        }
    }

    #[test]
    fn a_run_that_keeps_nothing_starts_from_the_seed_and_writes_nothing() {
        let files = project(&[
            ("data/seed/saves/slot1.json", SEED),
            ("data/seed/saves/broken.json", "{"),
            ("data/seed/saves/Upper.json", SEED),
            ("data/seed/saves/sub/x.json", SEED),
        ]);
        for config in [
            SavesConfig::default(),
            SavesConfig {
                mode: SaveMode::Session,
                ..SavesConfig::default()
            },
        ] {
            // A persisting run keeps saves only in persist mode.
            let mut saves = Saves::new(&files, &config, config.mode == SaveMode::Session);
            assert!(!saves.persists());
            assert_eq!(
                saves.list(),
                [SaveInfo {
                    slot: "slot1".into(),
                    data_version: 1,
                    saved_at: 1_767_225_600
                }]
            );
            let problems = saves.end_frame();
            assert_eq!(problems.len(), 1, "{problems:#?}");
            assert_eq!(problems[0].code, codes::DATA_FILE_SET_ASIDE);
            assert!(
                problems[0]
                    .message
                    .starts_with("data/seed/saves/broken.json is not JSON"),
                "{}",
                problems[0].message
            );
            saves.write("slot2", 5, json!([1]));
            saves.delete("slot1");
            assert!(saves.end_frame().is_empty());
            assert!(!saves.exists("slot1"));
            assert_eq!(saves.read("slot2").unwrap().data, json!([1]));
            assert!(files.writable(Writable::Data).list("").unwrap().is_empty());
        }
    }

    #[test]
    fn a_persisting_run_writes_its_slots_at_end_frame_and_sets_bad_files_aside() {
        let files = project(&[("data/seed/saves/seeded.json", SEED)]);
        let folder = files.writable(Writable::Data).sub(SAVES_DIR);
        folder.write("old.json", SEED.as_bytes()).unwrap();
        folder.write("bad.json", b"not json").unwrap();
        folder.write("bad.json.bad", b"an older bad file").unwrap();
        folder.write("notes.txt", b"not a slot").unwrap();

        let mut saves = Saves::new(&files, &SavesConfig::default(), true);
        assert!(saves.persists());
        // The folder, not the seed.
        assert!(saves.exists("old") && !saves.exists("seeded") && !saves.exists("bad"));
        saves.write("new", 7, json!({ "a": 1 }));
        saves.write("new", 8, json!({ "a": 2 }));
        saves.delete("old");
        // Nothing reaches the folder before EndFrame.
        assert!(folder.read("new.json").unwrap().is_none());

        let problems = saves.end_frame();
        assert_eq!(problems.len(), 1, "{problems:#?}");
        assert_eq!(problems[0].code, codes::DATA_FILE_SET_ASIDE);
        assert!(
            problems[0].message.contains(
                "is not JSON (expected ident at line 1 column 2); it was set aside as saves/bad.json.bad.1 in the data folder"
            ),
            "{}",
            problems[0].message
        );
        assert_eq!(
            folder.list("").unwrap(),
            ["bad.json.bad", "bad.json.bad.1", "new.json", "notes.txt"]
        );
        let new = parse(&folder.read("new.json").unwrap().unwrap()).unwrap();
        assert_eq!((new.saved_at, new.data), (8, json!({ "a": 2 })));

        // A fork carries the slots in memory and never writes the folder.
        let mut copy = saves.fork();
        assert!(!copy.persists() && copy.exists("new"));
        copy.write("from-copy", 9, json!({}));
        copy.delete("new");
        assert!(copy.end_frame().is_empty());
        assert!(folder.read("from-copy.json").unwrap().is_none());
        assert!(folder.read("new.json").unwrap().is_some());

        // A new run reads what this one wrote.
        let mut next = Saves::new(&files, &SavesConfig::default(), true);
        assert_eq!(next.read("new").unwrap().data, json!({ "a": 2 }));
        assert!(next.end_frame().is_empty());
    }

    #[test]
    fn a_write_the_folder_refuses_is_a_warning_and_the_slot_stays_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let files = ProjectFiles::project(dir.path());
        let mut saves = Saves::new(&files, &SavesConfig::default(), true);
        // A file where the folder should be: nothing can be written in it.
        std::fs::create_dir_all(dir.path().join(".real/data")).unwrap();
        std::fs::write(dir.path().join(".real/data/saves"), b"in the way").unwrap();
        saves.write("slot1", 1, json!({ "coins": 1 }));
        let problems = saves.end_frame();
        // Reading the folder may fail too; the write's failure comes last.
        assert!(problems.iter().all(|d| d.code == codes::DATA_WRITE_FAILED));
        let last = &problems.last().expect("a warning").message;
        assert!(
            last.starts_with("cannot write save slot \"slot1\" (")
                && last.ends_with("; the run goes on with the slot in memory"),
            "{last}"
        );
        assert!(saves.exists("slot1"));
    }
}
