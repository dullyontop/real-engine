//! Data store files (ADR-0048): one file per store and scope, a map from key
//! to its record.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::Value as J;

use super::backend::{DataBackend, encode};

/// The `format` of a data store file.
const FORMAT: &str = "real-engine/datastore";

/// Standard (`GetDataStore`) or ordered (`GetOrderedDataStore`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StoreKind {
    /// Any storable value per key.
    Standard,
    /// An integer per key, listed in order by `GetSortedAsync`.
    Ordered,
}

impl StoreKind {
    /// The directory of this kind's files under the data directory.
    pub fn dir(self) -> &'static str {
        match self {
            StoreKind::Standard => "datastore",
            StoreKind::Ordered => "ordered",
        }
    }
}

/// One key's current version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Record {
    /// The stored value.
    pub value: J,
    /// Increases by one with each write; `DataStoreKeyInfo.Version` is its
    /// decimal text.
    pub version: u64,
    /// When the key was first written (Unix milliseconds on the data clock).
    pub created: i64,
    /// When the key was last written.
    pub updated: i64,
    /// The user ids given with the last write.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub user_ids: Vec<i64>,
    /// The metadata given with the last write.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub metadata: serde_json::Map<String, J>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    format: String,
    version: u32,
    created: i64,
    updated: i64,
    keys: BTreeMap<String, Record>,
}

/// One store and scope.
#[derive(Clone, Debug, Default)]
pub struct Store {
    /// When the store was first written.
    pub created: i64,
    /// When it was last written.
    pub updated: i64,
    /// Its keys.
    pub keys: BTreeMap<String, Record>,
}

/// Which store: kind, name and scope.
pub type StoreId = (StoreKind, String, String);

/// The relative path of a store's file.
pub fn path(id: &StoreId) -> String {
    format!("{}/{}/{}.json", id.0.dir(), encode(&id.1), encode(&id.2))
}

/// The stores read so far, by id.
#[derive(Default)]
pub struct Stores {
    map: HashMap<StoreId, Store>,
    /// Stores whose file is there but did not read and could not be set
    /// aside; they are never written, so the file is not lost.
    blocked: HashSet<StoreId>,
}

/// A data file that did not read. The store or structure starts empty; the
/// file was set aside, or, when it could not be, it is left alone and the
/// store is not written this run.
#[derive(Debug)]
pub struct Unreadable {
    /// The file's relative path.
    pub path: String,
    /// What was wrong.
    pub reason: String,
    /// True when the file was renamed to `.bad`.
    pub set_aside: bool,
}

/// Reads `path` from `backend`: its bytes, nothing, or why it is unusable.
/// A file that `parse` refuses is set aside when possible.
pub(super) fn read_file<T>(
    backend: &mut dyn DataBackend,
    path: &str,
    parse: impl FnOnce(&[u8]) -> Result<T, String>,
) -> Result<Option<T>, Unreadable> {
    match backend.read(path) {
        Ok(None) => Ok(None),
        Ok(Some(bytes)) => match parse(&bytes) {
            Ok(t) => Ok(Some(t)),
            Err(reason) => {
                let set_aside = backend.set_aside(path).is_ok();
                Err(Unreadable {
                    path: path.to_string(),
                    reason,
                    set_aside,
                })
            }
        },
        Err(e) => Err(Unreadable {
            path: path.to_string(),
            reason: e.to_string(),
            set_aside: false,
        }),
    }
}

/// The error a save of a blocked file gives.
pub(super) fn blocked_error(path: &str) -> std::io::Error {
    std::io::Error::other(format!(
        "{path} did not read and is left as it is, so it is not overwritten"
    ))
}

impl Stores {
    /// The store `id`, read from `backend` the first time.
    pub fn get(
        &mut self,
        backend: &mut dyn DataBackend,
        id: &StoreId,
    ) -> (&mut Store, Option<Unreadable>) {
        let mut problem = None;
        if !self.map.contains_key(id) {
            let store = match read_file(backend, &path(id), parse) {
                Ok(s) => s.unwrap_or_default(),
                Err(u) => {
                    if !u.set_aside {
                        self.blocked.insert(id.clone());
                    }
                    problem = Some(u);
                    Store::default()
                }
            };
            self.map.insert(id.clone(), store);
        }
        (self.map.get_mut(id).expect("inserted"), problem)
    }

    /// Writes store `id` (which must have been read) to `backend`.
    pub fn save(&self, backend: &mut dyn DataBackend, id: &StoreId) -> std::io::Result<()> {
        if self.blocked.contains(id) {
            return Err(blocked_error(&path(id)));
        }
        let s = &self.map[id];
        let file = File {
            format: FORMAT.into(),
            version: 1,
            created: s.created,
            updated: s.updated,
            keys: s.keys.clone(),
        };
        let mut text = serde_json::to_string_pretty(&file).expect("plain data serializes");
        text.push('\n');
        backend.write(&path(id), text.as_bytes())
    }
}

fn parse(bytes: &[u8]) -> Result<Store, String> {
    let f: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if f.format != FORMAT || f.version != 1 {
        return Err(format!(
            "expected format {FORMAT:?} version 1, found {:?} version {}",
            f.format, f.version
        ));
    }
    Ok(Store {
        created: f.created,
        updated: f.updated,
        keys: f.keys,
    })
}

impl Store {
    /// Writes `value` to `key` at `now`; returns the new record.
    pub fn write(
        &mut self,
        key: &str,
        value: J,
        user_ids: Vec<i64>,
        metadata: serde_json::Map<String, J>,
        now: i64,
    ) -> Record {
        if self.keys.is_empty() && self.created == 0 {
            self.created = now;
        }
        self.updated = now;
        let r = match self.keys.get(key) {
            Some(old) => Record {
                value,
                version: old.version.saturating_add(1),
                created: old.created,
                updated: now,
                user_ids,
                metadata,
            },
            None => Record {
                value,
                version: 1,
                created: now,
                updated: now,
                user_ids,
                metadata,
            },
        };
        self.keys.insert(key.to_string(), r.clone());
        r
    }
}

#[cfg(test)]
mod tests {
    use super::super::backend::MemoryBackend;
    use super::*;

    fn id() -> StoreId {
        (StoreKind::Standard, "Coins".into(), "global".into())
    }

    #[test]
    fn stores_round_trip_through_their_file() {
        let mut b = MemoryBackend::new();
        let mut s = Stores::default();
        let (st, problem) = s.get(&mut b, &id());
        assert!(problem.is_none());
        st.write("k", J::from(5), vec![7], Default::default(), 100);
        let r = st.write("k", J::from(6), vec![], Default::default(), 200);
        assert_eq!((r.version, r.created, r.updated), (2, 100, 200));
        s.save(&mut b, &id()).unwrap();
        let text = String::from_utf8(b.read(&path(&id())).unwrap().unwrap()).unwrap();
        assert!(text.contains("\"real-engine/datastore\""), "{text}");
        let mut fresh = Stores::default();
        let (st, _) = fresh.get(&mut b, &id());
        assert_eq!(st.keys["k"].value, J::from(6));
        assert_eq!((st.created, st.updated), (100, 200));
        assert_eq!(path(&id()), "datastore/_^coins/_global.json");
    }

    #[test]
    fn a_bad_file_is_set_aside_and_the_store_starts_empty() {
        let mut b = MemoryBackend::new();
        b.write(&path(&id()), b"{ not json").unwrap();
        let mut s = Stores::default();
        let (st, problem) = s.get(&mut b, &id());
        assert!(st.keys.is_empty());
        assert!(problem.unwrap().path.ends_with("_global.json"));
        assert_eq!(b.read(&path(&id())).unwrap(), None);
    }

    /// A backend whose files cannot be renamed.
    struct Stuck(MemoryBackend);

    impl DataBackend for Stuck {
        fn read(&self, path: &str) -> std::io::Result<Option<Vec<u8>>> {
            self.0.read(path)
        }
        fn write(&mut self, path: &str, bytes: &[u8]) -> std::io::Result<()> {
            self.0.write(path, bytes)
        }
        fn set_aside(&mut self, _: &str) -> std::io::Result<()> {
            Err(std::io::Error::other("locked"))
        }
        fn list(&self, dir: &str) -> std::io::Result<Vec<String>> {
            self.0.list(dir)
        }
        fn persists(&self) -> bool {
            true
        }
    }

    #[test]
    fn a_bad_file_that_cannot_be_set_aside_is_never_overwritten() {
        let mut b = Stuck(MemoryBackend::new());
        b.write(&path(&id()), b"{ not json").unwrap();
        let mut s = Stores::default();
        let (st, problem) = s.get(&mut b, &id());
        assert!(!problem.unwrap().set_aside);
        st.write("k", J::from(1), vec![], Default::default(), 1);
        assert!(s.save(&mut b, &id()).is_err());
        assert_eq!(b.read(&path(&id())).unwrap().unwrap(), b"{ not json");
    }
}
