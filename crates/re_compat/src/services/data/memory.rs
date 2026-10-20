//! Memory stores (ADR-0048): queues, sorted maps and hash maps whose items
//! expire on the data clock, with the place's memory quota and request units.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as J;

use super::backend::{DataBackend, encode};

/// The largest stored value, in bytes of its JSON text.
pub const MAX_VALUE_BYTES: usize = 32 * 1024;
/// The longest expiration, in seconds (45 days); also the default.
pub const MAX_EXPIRATION_SECS: f64 = 3_888_000.0;
/// The most items one structure holds.
pub const MAX_ITEMS: usize = 1_000_000;
/// The most bytes one structure holds, keys included.
pub const MAX_STRUCTURE_BYTES: usize = 100 * 1024 * 1024;
/// The longest key we accept. The vendor states no limit; this one is ours,
/// so a key cannot grow memory without bound.
pub const MAX_KEY_BYTES: usize = 1024;

/// The `format` of a memory store file.
const FORMAT: &str = "real-engine/memorystore";

/// The memory quota of the place, in bytes, with `players` players.
pub fn quota_bytes(players: u32) -> usize {
    64 * 1024 + (1.2 * 1024.0 * f64::from(players)) as usize
}

/// Request units per minute for the place, with `players` players.
pub fn units_per_minute(players: u32) -> f64 {
    1000.0 + 120.0 * f64::from(players)
}

/// A sort key of a sorted-map item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SortKey {
    /// A number.
    Num(f64),
    /// A string.
    Str(String),
}

/// The order of sorted-map items: items without a sort key first, then
/// number sort keys ascending, then string sort keys; ties by key. (The
/// order across kinds of sort key is ours.)
pub fn item_order(a: (&Option<SortKey>, &str), b: (&Option<SortKey>, &str)) -> Ordering {
    let rank = |s: &Option<SortKey>| match s {
        None => 0,
        Some(SortKey::Num(_)) => 1,
        Some(SortKey::Str(_)) => 2,
    };
    rank(a.0)
        .cmp(&rank(b.0))
        .then_with(|| match (a.0, b.0) {
            (Some(SortKey::Num(x)), Some(SortKey::Num(y))) => x.total_cmp(y),
            (Some(SortKey::Str(x)), Some(SortKey::Str(y))) => x.cmp(y),
            _ => Ordering::Equal,
        })
        .then_with(|| a.1.cmp(b.1))
}

/// A sorted-map or hash-map item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    /// The value.
    pub value: J,
    /// The sort key (sorted maps only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort_key: Option<SortKey>,
    /// When it expires (Unix milliseconds on the data clock).
    pub expires: i64,
}

/// A queue item.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QueueItem {
    /// Insertion number, for the order among equal priorities.
    pub seq: u64,
    /// The value.
    pub value: J,
    /// Higher priorities are read first.
    pub priority: f64,
    /// When it expires.
    pub expires: i64,
    /// While a read holds it: the read's id and when it becomes visible
    /// again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<(String, i64)>,
}

/// The kinds of structure, which are kept apart by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    /// `GetQueue`.
    Queue,
    /// `GetSortedMap`.
    SortedMap,
    /// `GetHashMap`.
    HashMap,
}

impl Kind {
    fn dir(self) -> &'static str {
        match self {
            Kind::Queue => "memory/queues",
            Kind::SortedMap => "memory/sorted",
            Kind::HashMap => "memory/hash",
        }
    }
}

/// One structure.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Structure {
    /// Map items by key (sorted and hash maps).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub items: BTreeMap<String, Item>,
    /// Queue items in insertion order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub queue: Vec<QueueItem>,
    /// The next queue insertion number and read id.
    #[serde(default)]
    pub next: u64,
}

#[derive(Serialize, Deserialize)]
struct File {
    format: String,
    version: u32,
    #[serde(flatten)]
    structure: Structure,
}

impl Structure {
    /// Drops expired items, and makes held queue items visible again once
    /// their hold ends.
    pub fn purge(&mut self, now: i64) -> bool {
        let before = (self.items.len(), self.queue.len());
        self.items.retain(|_, i| i.expires > now);
        self.queue.retain(|i| i.expires > now);
        let mut changed = before != (self.items.len(), self.queue.len());
        for q in &mut self.queue {
            if q.held.as_ref().is_some_and(|(_, until)| *until <= now) {
                q.held = None;
                changed = true;
            }
        }
        changed
    }

    /// The bytes the structure counts against the quota: keys and values.
    pub fn bytes(&self) -> usize {
        self.items
            .iter()
            .map(|(k, i)| k.len() + i.value.to_string().len())
            .sum::<usize>()
            + self
                .queue
                .iter()
                .map(|q| q.value.to_string().len())
                .sum::<usize>()
    }

    /// How many items it holds.
    pub fn count(&self) -> usize {
        self.items.len() + self.queue.len()
    }

    /// The visible queue items in read order: priority, highest first, then
    /// insertion order.
    pub fn visible(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.queue.len())
            .filter(|&i| self.queue[i].held.is_none())
            .collect();
        idx.sort_by(|&a, &b| {
            let (x, y) = (&self.queue[a], &self.queue[b]);
            y.priority.total_cmp(&x.priority).then(x.seq.cmp(&y.seq))
        });
        idx
    }

    /// Map items in sorted-map order.
    pub fn sorted(&self) -> Vec<(&String, &Item)> {
        let mut v: Vec<(&String, &Item)> = self.items.iter().collect();
        v.sort_by(|a, b| item_order((&a.1.sort_key, a.0), (&b.1.sort_key, b.0)));
        v
    }
}

/// Every structure read so far, the request units and the memory used.
#[derive(Default)]
pub struct Memory {
    map: BTreeMap<(Kind, String), Structure>,
    units: f64,
    units_at: f64,
    units_started: bool,
}

/// Why a memory store request failed, by the vendor's error name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The place's request units for this minute are spent.
    TotalRequestsOverLimit,
    /// The place's memory quota is used up.
    TotalMemoryOverLimit,
    /// The structure is at 100 MB.
    DataStructureMemoryOverLimit,
    /// The structure is at 1,000,000 items.
    DataStructureItemsOverLimit,
    /// The value is over 32 KB.
    ItemValueSizeTooLarge,
}

impl Refusal {
    /// The error name scripts see at the start of the message.
    pub fn name(self) -> &'static str {
        match self {
            Refusal::TotalRequestsOverLimit => "TotalRequestsOverLimit",
            Refusal::TotalMemoryOverLimit => "TotalMemoryOverLimit",
            Refusal::DataStructureMemoryOverLimit => "DataStructureMemoryOverLimit",
            Refusal::DataStructureItemsOverLimit => "DataStructureItemsOverLimit",
            Refusal::ItemValueSizeTooLarge => "ItemValueSizeTooLarge",
        }
    }
}

fn path(kind: Kind, name: &str) -> String {
    format!("{}/{}.json", kind.dir(), encode(name))
}

impl Memory {
    /// Takes `units` request units at simulation time `now`, or refuses
    /// when the minute's units are spent.
    pub fn charge(&mut self, units: f64, now: f64, players: u32) -> Result<(), Refusal> {
        let cap = units_per_minute(players);
        if !self.units_started {
            self.units = cap;
            self.units_at = now;
            self.units_started = true;
        } else if now > self.units_at {
            self.units = (self.units + (now - self.units_at) * cap / 60.0).min(cap);
            self.units_at = now;
        }
        if self.units < units {
            return Err(Refusal::TotalRequestsOverLimit);
        }
        self.units -= units;
        Ok(())
    }

    /// Structure `name` of `kind`, read from `backend` the first time and
    /// purged at `now`. A file that does not read is set aside; the second
    /// value says why.
    pub fn get(
        &mut self,
        backend: &mut dyn DataBackend,
        kind: Kind,
        name: &str,
        now: i64,
    ) -> (&mut Structure, Option<String>) {
        let key = (kind, name.to_string());
        let mut problem = None;
        if !self.map.contains_key(&key) {
            let p = path(kind, name);
            let s = match backend.read(&p) {
                Ok(None) => Structure::default(),
                Ok(Some(bytes)) => match serde_json::from_slice::<File>(&bytes) {
                    Ok(f) if f.format == FORMAT && f.version == 1 => f.structure,
                    Ok(f) => {
                        let _ = backend.set_aside(&p);
                        problem = Some(format!(
                            "{p}: expected format {FORMAT:?} version 1, found {:?} version {}",
                            f.format, f.version
                        ));
                        Structure::default()
                    }
                    Err(e) => {
                        let _ = backend.set_aside(&p);
                        problem = Some(format!("{p}: {e}"));
                        Structure::default()
                    }
                },
                Err(e) => {
                    problem = Some(format!("{p}: {e}"));
                    Structure::default()
                }
            };
            self.map.insert(key.clone(), s);
        }
        let s = self.map.get_mut(&key).expect("inserted");
        s.purge(now);
        (s, problem)
    }

    /// The bytes all structures read so far hold, purged at `now`.
    pub fn used_bytes(&mut self, now: i64) -> usize {
        self.map
            .values_mut()
            .map(|s| {
                s.purge(now);
                s.bytes()
            })
            .sum()
    }

    /// Writes structure `name` of `kind` to `backend`.
    pub fn save(
        &self,
        backend: &mut dyn DataBackend,
        kind: Kind,
        name: &str,
    ) -> std::io::Result<()> {
        let s = &self.map[&(kind, name.to_string())];
        let file = File {
            format: FORMAT.into(),
            version: 1,
            structure: s.clone(),
        };
        let mut text = serde_json::to_string_pretty(&file).expect("plain data serializes");
        text.push('\n');
        backend.write(&path(kind, name), text.as_bytes())
    }

    /// Checks that a change growing structure `name` of `kind` by `grow`
    /// bytes and `items` items fits: the value limit is checked by the
    /// caller, this checks the structure's and the place's limits.
    pub fn fits(
        &mut self,
        kind: Kind,
        name: &str,
        grow: isize,
        items: isize,
        now: i64,
        players: u32,
    ) -> Result<(), Refusal> {
        if grow <= 0 && items <= 0 {
            return Ok(());
        }
        let used = self.used_bytes(now) as isize;
        let s = &self.map[&(kind, name.to_string())];
        if items > 0 && s.count() as isize + items > MAX_ITEMS as isize {
            return Err(Refusal::DataStructureItemsOverLimit);
        }
        if grow > 0 && s.bytes() as isize + grow > MAX_STRUCTURE_BYTES as isize {
            return Err(Refusal::DataStructureMemoryOverLimit);
        }
        if grow > 0 && used + grow > quota_bytes(players) as isize {
            return Err(Refusal::TotalMemoryOverLimit);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::backend::MemoryBackend;
    use super::*;

    #[test]
    fn sorted_items_order_by_sort_key_kind_then_value_then_key() {
        let mut s = Structure::default();
        let item = |k: Option<SortKey>| Item {
            value: J::Null,
            sort_key: k,
            expires: i64::MAX,
        };
        s.items
            .insert("d".into(), item(Some(SortKey::Str("a".into()))));
        s.items.insert("c".into(), item(Some(SortKey::Num(2.0))));
        s.items.insert("b".into(), item(Some(SortKey::Num(-1.0))));
        s.items.insert("z".into(), item(None));
        s.items.insert("a".into(), item(None));
        let keys: Vec<&str> = s.sorted().iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, ["a", "z", "b", "c", "d"]);
    }

    #[test]
    fn expiry_and_holds_follow_the_clock() {
        let mut s = Structure::default();
        s.items.insert(
            "k".into(),
            Item {
                value: J::from(1),
                sort_key: None,
                expires: 100,
            },
        );
        s.queue.push(QueueItem {
            seq: 0,
            value: J::from(1),
            priority: 0.0,
            expires: 1000,
            held: Some(("r".into(), 50)),
        });
        s.queue.push(QueueItem {
            seq: 1,
            value: J::from(2),
            priority: 5.0,
            expires: 1000,
            held: None,
        });
        assert_eq!(s.visible(), vec![1]);
        assert!(s.purge(50));
        assert_eq!(s.visible(), vec![1, 0]);
        assert!(s.purge(100));
        assert!(s.items.is_empty());
        assert!(!s.purge(100));
    }

    #[test]
    fn units_and_quota_refuse_past_their_limits() {
        let mut m = Memory::default();
        m.charge(1000.0, 0.0, 0).unwrap();
        assert_eq!(m.charge(1.0, 0.0, 0), Err(Refusal::TotalRequestsOverLimit));
        m.charge(1.0, 1.0, 0).unwrap();
        let mut b = MemoryBackend::new();
        let (s, problem) = m.get(&mut b, Kind::HashMap, "h", 0);
        assert!(problem.is_none());
        s.items.insert(
            "k".into(),
            Item {
                value: J::String("x".repeat(60 * 1024)),
                sort_key: None,
                expires: i64::MAX,
            },
        );
        assert!(m.fits(Kind::HashMap, "h", 1024, 1, 0, 0).is_ok());
        assert_eq!(
            m.fits(Kind::HashMap, "h", 8 * 1024, 1, 0, 0),
            Err(Refusal::TotalMemoryOverLimit)
        );
        assert!(m.fits(Kind::HashMap, "h", 8 * 1024, 1, 0, 10).is_ok());
        assert!(m.fits(Kind::HashMap, "h", -5, 0, 0, 0).is_ok());
        m.save(&mut b, Kind::HashMap, "h").unwrap();
        let mut fresh = Memory::default();
        let (s, _) = fresh.get(&mut b, Kind::HashMap, "h", 0);
        assert_eq!(s.items.len(), 1);
    }
}
