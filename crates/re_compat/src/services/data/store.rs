//! Data store files (ADR-0048): one file per store and scope, a map from key
//! to its record.

use std::collections::{BTreeMap, HashMap};

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
}

/// A store file that did not read; it was set aside and the store is empty.
#[derive(Debug)]
pub struct Unreadable {
    /// The file's relative path.
    pub path: String,
    /// What was wrong.
    pub reason: String,
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
            let p = path(id);
            let store = match backend.read(&p) {
                Ok(None) => Store::default(),
                Ok(Some(bytes)) => match parse(&bytes) {
                    Ok(s) => s,
                    Err(reason) => {
                        let _ = backend.set_aside(&p);
                        problem = Some(Unreadable { path: p, reason });
                        Store::default()
                    }
                },
                Err(e) => {
                    problem = Some(Unreadable {
                        path: p,
                        reason: e.to_string(),
                    });
                    Store::default()
                }
            };
            self.map.insert(id.clone(), store);
        }
        (self.map.get_mut(id).expect("inserted"), problem)
    }

    /// Writes store `id` (which must have been read) to `backend`.
    pub fn save(&self, backend: &mut dyn DataBackend, id: &StoreId) -> std::io::Result<()> {
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
                version: old.version + 1,
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
}
