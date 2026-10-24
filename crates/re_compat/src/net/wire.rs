//! The wire forms of what crosses between the server and a client (ADR-0050
//! §3): replication operations in a [`Frame`], and remote messages.
//!
//! Everything is encoded with postcard. Floats travel as stored, bit for
//! bit, so a networked run hashes like an in-process one.
//!
//! * **Instances in replicated values** travel as [`Ref`]s: the `game` root,
//!   or a `u32` handle the server gave the instance on this connection when
//!   it sent its `Create` (or `Bind`). A handle is never reused.
//! * **Names** (component, record and enum variant names) travel once per
//!   direction: a [`Frame`] lists the names it introduces, which take the
//!   next numbers, and everything after refers to them by number.
//! * **Remote messages** are self-contained: an instance argument travels as
//!   its stable id and is found by id where it is delivered, as before the
//!   split, so a message waiting for a listener still finds an instance that
//!   replicates later.

use std::collections::HashMap;

use re_reflect::{Color, RecordId, Value};
use re_scene::Entity;
use serde::{Deserialize, Serialize};

use super::value::{InstanceKey, NetKey, NetValue};
use super::{Msg, PromptAction};
use crate::enums::EnumItem;

/// The version of the wire types. A change to any of them bumps it.
pub const WIRE_VERSION: u32 = 2;

/// The datatypes a [`NetValue::Data`] can name.
const DATATYPES: &[&str] = &[
    "CFrame",
    "Color3",
    "Vector2",
    "UDim",
    "UDim2",
    "Rect",
    "NumberRange",
    "NumberSequence",
    "ColorSequence",
    "NumberSequenceKeypoint",
    "ColorSequenceKeypoint",
    "PhysicalProperties",
    "Font",
];

/// How deep lists, maps, records and tables may nest in what is decoded, so
/// a hostile peer cannot overflow the stack.
pub const MAX_DEPTH: u32 = 128;

thread_local! {
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Decodes one nesting level, refusing past [`MAX_DEPTH`].
fn nested<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Level;
    impl Drop for Level {
        fn drop(&mut self) {
            DEPTH.with(|c| c.set(c.get() - 1));
        }
    }
    let depth = DEPTH.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    let _level = Level;
    if depth > MAX_DEPTH {
        return Err(serde::de::Error::custom("nested too deep"));
    }
    T::deserialize(d)
}

/// An instance referred to inside a replicated value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Ref {
    /// The `game` root.
    Game,
    /// An instance the server sent on this connection.
    Handle(u32),
}

/// A component field value on the wire: a [`Value`] with instances as
/// [`Ref`]s and names by number.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum WireValue {
    /// [`Value::Bool`].
    Bool(bool),
    /// [`Value::Int`].
    Int(i64),
    /// [`Value::Float`], bit for bit.
    Float(f64),
    /// [`Value::Vec3`].
    Vec3([f32; 3]),
    /// [`Value::Quat`], `x, y, z, w`.
    Quat([f32; 4]),
    /// [`Value::Color`], `r, g, b, a`.
    Color([f32; 4]),
    /// [`Value::Str`].
    Str(String),
    /// [`Value::Entity`]: `None` is null, or an instance the receiver does
    /// not have.
    Entity(Option<Ref>),
    /// [`Value::Enum`]: the variant's name number.
    Enum(u32),
    /// [`Value::List`].
    List(#[serde(deserialize_with = "nested")] Vec<WireValue>),
    /// [`Value::Map`], in key order.
    Map(#[serde(deserialize_with = "nested")] Vec<(String, WireValue)>),
    /// [`Value::Record`]: the record type's name number and its fields.
    Record(u32, #[serde(deserialize_with = "nested")] Vec<WireValue>),
}

impl WireValue {
    /// How many instance references it holds.
    pub(crate) fn refs(&self) -> usize {
        match self {
            WireValue::Entity(Some(_)) => 1,
            WireValue::List(items) | WireValue::Record(_, items) => {
                items.iter().map(WireValue::refs).sum()
            }
            WireValue::Map(m) => m.iter().map(|(_, v)| v.refs()).sum(),
            _ => 0,
        }
    }
}

/// Numbers the names one side sends; the names new since the last frame go
/// in the next frame's [`Frame::names`].
#[derive(Clone, Debug, Default)]
pub(crate) struct Names {
    ids: HashMap<String, u32>,
    fresh: Vec<String>,
}

impl Names {
    /// The number of `name`, numbering it if it is new.
    pub(crate) fn id(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.ids.len() as u32;
        self.ids.insert(name.to_string(), id);
        self.fresh.push(name.to_string());
        id
    }

    /// The names numbered since the last call.
    pub(crate) fn take_fresh(&mut self) -> Vec<String> {
        std::mem::take(&mut self.fresh)
    }
}

/// How many names one side may number on a connection.
pub const MAX_NAMES: usize = 1 << 16;

/// The longest name kept in a [`NameTable`].
pub const MAX_NAME_LEN: usize = 1024;

/// The names the other side numbered, in order.
#[derive(Clone, Debug, Default)]
pub(crate) struct NameTable(Vec<String>);

impl NameTable {
    /// Adds the names a frame introduces, up to [`MAX_NAMES`] in all; past
    /// that the rest are dropped, and what refers to them is not understood.
    /// A name longer than [`MAX_NAME_LEN`] bytes keeps its number but is
    /// stored empty (no component, field or key is that long).
    pub(crate) fn extend(&mut self, names: &[String]) {
        let room = MAX_NAMES.saturating_sub(self.0.len());
        self.0.extend(names.iter().take(room).map(|n| {
            if n.len() > MAX_NAME_LEN {
                String::new()
            } else {
                n.clone()
            }
        }));
    }

    /// The name with number `id`.
    pub(crate) fn get(&self, id: u32) -> Option<&str> {
        self.0.get(id as usize).map(String::as_str)
    }
}

/// Encodes `v`, with instances through `refs` (`None`: the other side does not
/// have it, sent as null).
pub(crate) fn to_wire(
    v: &Value,
    names: &mut Names,
    refs: &dyn Fn(Entity) -> Option<Ref>,
) -> WireValue {
    match v {
        Value::Bool(b) => WireValue::Bool(*b),
        Value::Int(i) => WireValue::Int(*i),
        Value::Float(f) => WireValue::Float(*f),
        Value::Vec3(v) => WireValue::Vec3(v.to_array()),
        Value::Quat(q) => WireValue::Quat(q.to_array()),
        Value::Color(c) => WireValue::Color([c.r, c.g, c.b, c.a]),
        Value::Str(s) => WireValue::Str(s.clone()),
        Value::Entity(e) => WireValue::Entity(e.and_then(refs)),
        Value::Enum(name) => WireValue::Enum(names.id(name)),
        Value::List(items) => {
            WireValue::List(items.iter().map(|i| to_wire(i, names, refs)).collect())
        }
        Value::Map(m) => WireValue::Map(
            m.iter()
                .map(|(k, x)| (k.clone(), to_wire(x, names, refs)))
                .collect(),
        ),
        Value::Record(id, fields) => WireValue::Record(
            names.id(id.name()),
            fields.iter().map(|f| to_wire(f, names, refs)).collect(),
        ),
    }
}

/// Decodes `v`, with references through `refs` (`None` reads as null).
/// `None` when it names a name or record type this side does not know.
pub(crate) fn from_wire(
    v: &WireValue,
    names: &NameTable,
    refs: &dyn Fn(Ref) -> Option<Entity>,
) -> Option<Value> {
    Some(match v {
        WireValue::Bool(b) => Value::Bool(*b),
        WireValue::Int(i) => Value::Int(*i),
        WireValue::Float(f) => Value::Float(*f),
        WireValue::Vec3(v) => Value::Vec3(glam::Vec3::from_array(*v)),
        WireValue::Quat(q) => Value::Quat(glam::Quat::from_array(*q)),
        WireValue::Color([r, g, b, a]) => Value::Color(Color {
            r: *r,
            g: *g,
            b: *b,
            a: *a,
        }),
        WireValue::Str(s) => Value::Str(s.clone()),
        WireValue::Entity(r) => Value::Entity(r.and_then(refs)),
        WireValue::Enum(id) => Value::Enum(names.get(*id)?.to_string()),
        WireValue::List(items) => Value::List(
            items
                .iter()
                .map(|i| from_wire(i, names, refs))
                .collect::<Option<_>>()?,
        ),
        WireValue::Map(m) => Value::Map(
            m.iter()
                .map(|(k, x)| Some((k.clone(), from_wire(x, names, refs)?)))
                .collect::<Option<_>>()?,
        ),
        WireValue::Record(id, fields) => Value::Record(
            RecordId::by_name(names.get(*id)?)?,
            fields
                .iter()
                .map(|f| from_wire(f, names, refs))
                .collect::<Option<_>>()?,
        ),
    })
}

/// One replication operation. Server to client: everything but [`Op::Up`];
/// client to server: [`Op::Up`] only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Op {
    /// The client already has the instance with stable id `id` (it was made
    /// from the same scene); `comps` are the values it has, by name number.
    Bind {
        /// The handle the instance takes.
        handle: u32,
        /// Its stable id.
        id: String,
        /// Its components, by name number.
        comps: Vec<(u32, Vec<WireValue>)>,
    },
    /// A new instance, its components to follow.
    Create {
        /// The handle the instance takes.
        handle: u32,
        /// Its parent.
        parent: Ref,
        /// Its stable id.
        id: String,
        /// Its name.
        name: String,
        /// For a service (a child of `game` with a service tag), its class:
        /// a client that made that service itself adopts it.
        service: Option<String>,
    },
    /// A rename.
    Name {
        /// The instance.
        handle: u32,
        /// Its new name.
        name: String,
    },
    /// A move to another parent.
    Parent {
        /// The instance.
        handle: u32,
        /// Its new parent.
        parent: Ref,
    },
    /// A component the sender did not send before, all its fields.
    Insert {
        /// The instance.
        handle: u32,
        /// The component's name number.
        comp: u32,
        /// Its fields.
        values: Vec<WireValue>,
    },
    /// Fields of a component that changed since it was last sent.
    Set {
        /// The instance.
        handle: u32,
        /// The component's name number.
        comp: u32,
        /// The changed fields, by index.
        fields: Vec<(u32, WireValue)>,
    },
    /// A component the instance no longer has.
    Drop {
        /// The instance.
        handle: u32,
        /// The component's name number.
        comp: u32,
    },
    /// The client now simulates the instance (or no longer does): its
    /// upstream components flow to the server.
    Own {
        /// The instance.
        handle: u32,
        /// True when the client owns it.
        owned: bool,
    },
    /// Instances the server removed, or moved where they do not replicate.
    Remove {
        /// Their handles.
        handles: Vec<u32>,
    },
    /// Client to server: an owned instance's component as the client has
    /// it, all its fields.
    Up {
        /// The instance.
        handle: u32,
        /// The component's name number.
        comp: u32,
        /// Its fields.
        values: Vec<WireValue>,
    },
}

/// What one side sends the other in one tick: one reliable message.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    /// The sender's tick.
    pub tick: u64,
    /// Names numbered for the first time in this frame, in order.
    pub names: Vec<String>,
    /// Replication operations, in order.
    pub ops: Vec<Op>,
    /// Remote messages ([`encode_msg`]), after the operations.
    pub msgs: Vec<Vec<u8>>,
}

impl Frame {
    /// The frame's bytes.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("a frame always encodes")
    }

    /// A frame from its bytes; `None` when they are not one.
    pub fn decode(bytes: &[u8]) -> Option<Frame> {
        postcard::from_bytes(bytes).ok()
    }
}

/// A remote message on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum WireMsg {
    Event {
        remote: String,
        player: Option<String>,
        args: Vec<WireNet>,
        unreliable: bool,
    },
    Invoke {
        remote: String,
        player: Option<String>,
        call: u64,
        args: Vec<WireNet>,
    },
    Reply {
        call: u64,
        player: Option<String>,
        result: Result<Vec<WireNet>, String>,
    },
    Prompt {
        prompt: String,
        player: String,
        action: PromptAction,
    },
    Chat {
        player: String,
        text: String,
    },
    Kick {
        player: String,
    },
}

/// A [`NetValue`] on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum WireNet {
    Nil,
    Bool(bool),
    Number(f64),
    Str(Vec<u8>),
    Buffer(Vec<u8>),
    Vector([f32; 3]),
    /// An instance by stable id.
    Instance(Option<String>),
    /// An enum item: its enum's and its own name.
    Enum(String, String),
    /// A datatype: its name, the names its value uses, and the value.
    Data(String, Vec<String>, WireValue),
    Table(#[serde(deserialize_with = "nested")] Vec<(Option<Vec<u8>>, WireNet)>),
}

fn net_to_wire(v: &NetValue) -> WireNet {
    match v {
        NetValue::Nil => WireNet::Nil,
        NetValue::Bool(b) => WireNet::Bool(*b),
        NetValue::Number(n) => WireNet::Number(*n),
        NetValue::Str(s) => WireNet::Str(s.clone()),
        NetValue::Buffer(b) => WireNet::Buffer(b.clone()),
        NetValue::Vector(v) => WireNet::Vector(*v),
        NetValue::Instance(k) => WireNet::Instance(k.id.clone()),
        NetValue::Enum(item) => WireNet::Enum(item.enum_name().into(), item.name().into()),
        NetValue::Data(ty, v) => {
            let mut names = Names::default();
            let w = to_wire(v, &mut names, &|_| None);
            WireNet::Data(ty.to_string(), names.take_fresh(), w)
        }
        NetValue::Table(entries) => WireNet::Table(
            entries
                .iter()
                .map(|(k, v)| {
                    let k = match k {
                        NetKey::Index => None,
                        NetKey::Str(s) => Some(s.clone()),
                    };
                    (k, net_to_wire(v))
                })
                .collect(),
        ),
    }
}

fn net_from_wire(v: WireNet) -> Option<NetValue> {
    Some(match v {
        WireNet::Nil => NetValue::Nil,
        WireNet::Bool(b) => NetValue::Bool(b),
        WireNet::Number(n) => NetValue::Number(n),
        WireNet::Str(s) => NetValue::Str(s),
        WireNet::Buffer(b) => NetValue::Buffer(b),
        WireNet::Vector(v) => NetValue::Vector(v),
        // Found by id where it is delivered; the entity is the sender's and
        // means nothing here.
        WireNet::Instance(id) => NetValue::Instance(InstanceKey {
            entity: Entity::PLACEHOLDER,
            id,
        }),
        WireNet::Enum(ty, item) => NetValue::Enum(EnumItem::lookup(&ty, &item)?),
        WireNet::Data(ty, names, w) => {
            let ty = DATATYPES.iter().copied().find(|d| *d == ty)?;
            let mut table = NameTable::default();
            table.extend(&names);
            NetValue::Data(ty, from_wire(&w, &table, &|_| None)?)
        }
        WireNet::Table(entries) => NetValue::Table(
            entries
                .into_iter()
                .map(|(k, v)| {
                    let k = match k {
                        None => NetKey::Index,
                        Some(s) => NetKey::Str(s),
                    };
                    Some((k, net_from_wire(v)?))
                })
                .collect::<Option<_>>()?,
        ),
    })
}

fn args_to_wire(args: &[NetValue]) -> Vec<WireNet> {
    args.iter().map(net_to_wire).collect()
}

fn args_from_wire(args: Vec<WireNet>) -> Option<Vec<NetValue>> {
    args.into_iter().map(net_from_wire).collect()
}

/// A remote message's bytes.
pub(crate) fn encode_msg(m: &Msg) -> Vec<u8> {
    let w = match m {
        Msg::Event {
            remote,
            player,
            args,
            unreliable,
        } => WireMsg::Event {
            remote: remote.clone(),
            player: player.clone(),
            args: args_to_wire(args),
            unreliable: *unreliable,
        },
        Msg::Invoke {
            remote,
            player,
            call,
            args,
        } => WireMsg::Invoke {
            remote: remote.clone(),
            player: player.clone(),
            call: *call,
            args: args_to_wire(args),
        },
        Msg::Reply {
            call,
            player,
            result,
        } => WireMsg::Reply {
            call: *call,
            player: player.clone(),
            result: result
                .as_ref()
                .map(|a| args_to_wire(a))
                .map_err(Clone::clone),
        },
        Msg::Prompt {
            prompt,
            player,
            action,
        } => WireMsg::Prompt {
            prompt: prompt.clone(),
            player: player.clone(),
            action: *action,
        },
        Msg::Chat { player, text } => WireMsg::Chat {
            player: player.clone(),
            text: text.clone(),
        },
        Msg::Kick { player } => WireMsg::Kick {
            player: player.clone(),
        },
    };
    postcard::to_stdvec(&w).expect("a message always encodes")
}

/// A remote message from its bytes; `None` when they are not one, or name an
/// enum item or datatype this build does not have.
pub(crate) fn decode_msg(bytes: &[u8]) -> Option<Msg> {
    Some(match postcard::from_bytes::<WireMsg>(bytes).ok()? {
        WireMsg::Event {
            remote,
            player,
            args,
            unreliable,
        } => Msg::Event {
            remote,
            player,
            args: args_from_wire(args)?,
            unreliable,
        },
        WireMsg::Invoke {
            remote,
            player,
            call,
            args,
        } => Msg::Invoke {
            remote,
            player,
            call,
            args: args_from_wire(args)?,
        },
        WireMsg::Reply {
            call,
            player,
            result,
        } => Msg::Reply {
            call,
            player,
            result: match result {
                Ok(a) => Ok(args_from_wire(a)?),
                Err(e) => Err(e),
            },
        },
        WireMsg::Prompt {
            prompt,
            player,
            action,
        } => Msg::Prompt {
            prompt,
            player,
            action,
        },
        WireMsg::Chat { player, text } => Msg::Chat { player, text },
        WireMsg::Kick { player } => Msg::Kick { player },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn values_round_trip_bit_for_bit() {
        let e = Entity::from_raw_u32(7).unwrap();
        let mut map = BTreeMap::new();
        map.insert("a".to_string(), Value::Float(-0.0));
        map.insert("b".to_string(), Value::Entity(Some(e)));
        let v = Value::List(vec![
            Value::Float(f64::from_bits(0x7ff8_0000_0000_0001)),
            Value::Int(-3),
            Value::Vec3(glam::Vec3::new(1.0, -0.0, f32::MIN_POSITIVE)),
            Value::Quat(glam::Quat::from_xyzw(0.1, 0.2, 0.3, 0.9)),
            Value::Color(Color {
                r: 0.5,
                g: 0.25,
                b: 1.0,
                a: 1.0,
            }),
            Value::Enum("Plastic".into()),
            Value::Map(map),
            Value::Entity(None),
        ]);
        let mut names = Names::default();
        let w = to_wire(&v, &mut names, &|x| (x == e).then_some(Ref::Handle(3)));
        let bytes = postcard::to_stdvec(&w).unwrap();
        let w: WireValue = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(w.refs(), 1);
        let mut table = NameTable::default();
        table.extend(&names.take_fresh());
        let back = from_wire(&w, &table, &|r| (r == Ref::Handle(3)).then_some(e)).unwrap();
        // NaN compares unequal: compare the encoded bytes.
        let mut again = Names::default();
        let again = to_wire(&back, &mut again, &|_| Some(Ref::Handle(3)));
        assert_eq!(postcard::to_stdvec(&again).unwrap(), bytes);
        let Value::List(items) = back else { panic!() };
        let Value::Float(nan) = items[0] else {
            panic!()
        };
        assert_eq!(nan.to_bits(), 0x7ff8_0000_0000_0001);
    }

    #[test]
    fn a_name_numbers_once_per_direction() {
        let mut names = Names::default();
        assert_eq!(names.id("Transform"), 0);
        assert_eq!(names.id("Part"), 1);
        assert_eq!(names.id("Transform"), 0);
        assert_eq!(names.take_fresh(), vec!["Transform", "Part"]);
        assert!(names.take_fresh().is_empty());
        let mut table = NameTable::default();
        table.extend(&["Transform".into(), "Part".into()]);
        assert_eq!(table.get(1), Some("Part"));
        assert_eq!(table.get(2), None);
        // An unknown name number does not decode.
        assert_eq!(from_wire(&WireValue::Enum(9), &table, &|_| None), None);
    }

    #[test]
    fn messages_round_trip() {
        let m = Msg::Event {
            remote: "r1".into(),
            player: Some("p1".into()),
            args: vec![
                NetValue::Number(f64::NEG_INFINITY),
                NetValue::Instance(InstanceKey {
                    entity: Entity::PLACEHOLDER,
                    id: Some("part".into()),
                }),
                NetValue::Table(vec![
                    (NetKey::Index, NetValue::Str(b"x".to_vec())),
                    (NetKey::Str(b"k".to_vec()), NetValue::Bool(true)),
                ]),
            ],
            unreliable: true,
        };
        let back = decode_msg(&encode_msg(&m)).unwrap();
        assert_eq!(format!("{back:?}"), format!("{m:?}"));
        assert!(decode_msg(&[0xff, 0xff]).is_none());
    }

    #[test]
    fn deep_nesting_is_refused_not_a_stack_overflow() {
        let empty = postcard::to_stdvec(&WireValue::List(Vec::new())).unwrap();
        let tag = empty[0];
        // A list holding a list holding ... 100,000 deep.
        let deep = |n: usize| {
            let mut b = Vec::new();
            for _ in 0..n {
                b.extend([tag, 1]);
            }
            b.extend([tag, 0]);
            b
        };
        assert!(postcard::from_bytes::<WireValue>(&deep(100_000)).is_err());
        assert!(postcard::from_bytes::<WireValue>(&deep(MAX_DEPTH as usize - 1)).is_ok());
        // The depth count starts over after a refusal.
        assert!(postcard::from_bytes::<WireValue>(&deep(10)).is_ok());
    }
}
