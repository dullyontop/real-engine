//! [`World::state_hash`]: a blake3 hash of everything that defines the scene
//! state, for determinism checks (replays, ADR-0018), checkpoints and tests.

use re_reflect::Value;

use crate::hierarchy;
use crate::procedural::{Generated, Overrides};
use crate::world::World;

struct Writer(blake3::Hasher);

impl Writer {
    fn bytes(&mut self, b: &[u8]) {
        self.0.update(&(b.len() as u64).to_le_bytes());
        self.0.update(b);
    }
    fn str(&mut self, s: &str) {
        self.bytes(s.as_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.update(&v.to_le_bytes());
    }
    fn tag(&mut self, t: u8) {
        self.0.update(&[t]);
    }
    fn f32s(&mut self, v: &[f32]) {
        for x in v {
            self.0.update(&x.to_bits().to_le_bytes());
        }
    }
}

impl World {
    /// A hash of the scene state: every entity with a stable id (generated
    /// ones included), in id order, with its name, parent, position among its
    /// siblings, generated key, registered components and field values (floats
    /// by their exact bits, entity references by id) and overrides, followed
    /// by the session's suppressions. Runtime handles (`Entity` bits), derived
    /// data (`GlobalTransform`) and resources are not included, so two worlds
    /// built the same way, or a world and its [`fork`](World::fork), hash the same.
    pub fn state_hash(&self) -> blake3::Hash {
        let mut w = Writer(blake3::Hasher::new());
        w.str("real-engine/state-hash/1");
        let mut ids: Vec<(&str, _)> = self.ids().iter().collect();
        ids.sort();
        w.u64(ids.len() as u64);
        for (id, e) in ids {
            w.str(id);
            w.str(self.name_of(e));
            match hierarchy::parent(&self.ecs, e) {
                Some(p) => {
                    w.tag(1);
                    w.str(self.id_of(p).unwrap_or(""));
                    w.u64(hierarchy::sibling_index(&self.ecs, e).unwrap_or(0) as u64);
                }
                None => w.tag(0),
            }
            match self.ecs.get::<Generated>(e) {
                Some(g) => {
                    w.tag(1);
                    w.str(&g.key);
                }
                None => w.tag(0),
            }
            let keys = self.registry.components_of(&self.ecs, e);
            w.u64(keys.len() as u64);
            for key in keys {
                w.str(&self.registry.schema(key).name);
                for v in self.registry.get_all(&self.ecs, e, key).unwrap_or_default() {
                    match v {
                        Value::Bool(b) => {
                            w.tag(1);
                            w.tag(u8::from(b));
                        }
                        Value::Int(i) => {
                            w.tag(2);
                            w.u64(i as u64);
                        }
                        Value::Float(f) => {
                            w.tag(3);
                            w.u64(f.to_bits());
                        }
                        Value::Vec3(v) => {
                            w.tag(4);
                            w.f32s(&v.to_array());
                        }
                        Value::Quat(q) => {
                            w.tag(5);
                            w.f32s(&q.to_array());
                        }
                        Value::Color(c) => {
                            w.tag(6);
                            w.f32s(&[c.r, c.g, c.b, c.a]);
                        }
                        Value::Str(s) => {
                            w.tag(7);
                            w.str(&s);
                        }
                        Value::Entity(t) => {
                            w.tag(8);
                            match t.and_then(|t| self.id_of(t)) {
                                Some(id) => {
                                    w.tag(1);
                                    w.str(id);
                                }
                                None => w.tag(0),
                            }
                        }
                        Value::Enum(s) => {
                            w.tag(9);
                            w.str(&s);
                        }
                    }
                }
            }
            match self.ecs.get::<Overrides>(e) {
                Some(o) if !o.0.is_empty() => {
                    w.tag(1);
                    w.str(&serde_json::to_string(&o.0).unwrap_or_default());
                }
                _ => w.tag(0),
            }
        }
        let s = self.suppressions();
        w.u64(s.len() as u64);
        for id in s.iter() {
            w.str(id);
        }
        w.0.finalize()
    }
}
