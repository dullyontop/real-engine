//! A test-only writer for place and model files, binary and XML, from the
//! same format facts the readers follow (ADR-0048 §4). It writes what the
//! tests need, every value type included, so no sample file is committed.

#![allow(dead_code)]

use std::fmt::Write as _;

use base64::Engine as _;
use re_import::place::dom::{CFrame, Font, PlaceValue, id_of_rotation};

/// One instance to write.
#[derive(Clone, Debug)]
pub struct Inst {
    pub class: String,
    pub props: Vec<(String, PlaceValue)>,
    pub parent: Option<usize>,
}

pub fn inst(class: &str, parent: Option<usize>, props: Vec<(&str, PlaceValue)>) -> Inst {
    Inst {
        class: class.to_string(),
        props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        parent,
    }
}

pub fn s(text: &str) -> PlaceValue {
    PlaceValue::String(text.as_bytes().to_vec())
}

/// How chunk bodies are stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    None,
    Lz4,
    Zstd,
}

const MAGIC: &[u8; 14] = b"<roblox!\x89\xff\r\n\x1a\n";

fn interleave<const W: usize>(words: &[[u8; W]]) -> Vec<u8> {
    let n = words.len();
    let mut out = vec![0u8; n * W];
    for (i, w) in words.iter().enumerate() {
        for b in 0..W {
            out[b * n + i] = w[b];
        }
    }
    out
}

fn zz32(v: i32) -> u32 {
    ((v << 1) ^ (v >> 31)) as u32
}

fn zz64(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn i32s(v: &[i32]) -> Vec<u8> {
    interleave(&v.iter().map(|&x| zz32(x).to_be_bytes()).collect::<Vec<_>>())
}

fn u32s(v: &[u32]) -> Vec<u8> {
    interleave(&v.iter().map(|&x| x.to_be_bytes()).collect::<Vec<_>>())
}

fn i64s(v: &[i64]) -> Vec<u8> {
    interleave(&v.iter().map(|&x| zz64(x).to_be_bytes()).collect::<Vec<_>>())
}

fn f32s(v: &[f32]) -> Vec<u8> {
    interleave(
        &v.iter()
            .map(|&x| x.to_bits().rotate_left(1).to_be_bytes())
            .collect::<Vec<_>>(),
    )
}

fn referents(v: &[i32]) -> Vec<u8> {
    let mut prev = 0i32;
    let deltas: Vec<i32> = v
        .iter()
        .map(|&x| {
            let d = x.wrapping_sub(prev);
            prev = x;
            d
        })
        .collect();
    i32s(&deltas)
}

fn string(out: &mut Vec<u8>, b: &[u8]) {
    out.extend((b.len() as u32).to_le_bytes());
    out.extend(b);
}

fn f32le(out: &mut Vec<u8>, x: f32) {
    out.extend(x.to_le_bytes());
}

fn chunk(out: &mut Vec<u8>, name: &[u8], body: &[u8], c: Compression) {
    let mut n = [0u8; 4];
    n[..name.len()].copy_from_slice(name);
    out.extend(n);
    let stored = match c {
        Compression::None => None,
        Compression::Lz4 => Some(lz4_flex::block::compress(body)),
        Compression::Zstd => Some(ruzstd::encoding::compress_to_vec(
            body,
            ruzstd::encoding::CompressionLevel::Fastest,
        )),
    };
    match &stored {
        None => out.extend(0u32.to_le_bytes()),
        Some(s) => out.extend((s.len() as u32).to_le_bytes()),
    }
    out.extend((body.len() as u32).to_le_bytes());
    out.extend([0u8; 4]);
    out.extend(stored.as_deref().unwrap_or(body));
}

fn type_id(v: &PlaceValue) -> u8 {
    use PlaceValue as V;
    match v {
        V::String(_) => 0x01,
        V::Bool(_) => 0x02,
        V::Int32(_) => 0x03,
        V::Float32(_) => 0x04,
        V::Float64(_) => 0x05,
        V::UDim(..) => 0x06,
        V::UDim2(..) => 0x07,
        V::Ray { .. } => 0x08,
        V::Faces(_) => 0x09,
        V::Axes(_) => 0x0A,
        V::BrickColor(_) => 0x0B,
        V::Color3(_) => 0x0C,
        V::Vector2(_) => 0x0D,
        V::Vector3(_) => 0x0E,
        V::CFrame(_) => 0x10,
        V::Enum(_) => 0x12,
        V::Ref(_) => 0x13,
        V::Vector3int16(_) => 0x14,
        V::NumberSequence(_) => 0x15,
        V::ColorSequence(_) => 0x16,
        V::NumberRange(..) => 0x17,
        V::Rect(_) => 0x18,
        V::PhysicalProperties(_) => 0x19,
        V::Color3uint8(_) => 0x1A,
        V::Int64(_) => 0x1B,
        V::SharedString(_) => 0x1C,
        V::Bytecode(_) => 0x1D,
        V::OptionalCFrame(_) => 0x1E,
        V::UniqueId(_) => 0x1F,
        V::Font(_) => 0x20,
        V::SecurityCapabilities(_) => 0x21,
        V::Content(_) => 0x22,
    }
}

fn cframes(out: &mut Vec<u8>, cfs: &[CFrame]) {
    for c in cfs {
        match id_of_rotation(&c.rotation) {
            Some(id) => out.push(id),
            None => {
                out.push(0);
                for r in c.rotation {
                    f32le(out, r);
                }
            }
        }
    }
    for k in 0..3 {
        out.extend(f32s(&cfs.iter().map(|c| c.position[k]).collect::<Vec<_>>()));
    }
}

/// The column of values `vs`, all of one type; `shared` collects shared
/// strings.
fn column(vs: &[&PlaceValue], shared: &mut Vec<Vec<u8>>) -> Vec<u8> {
    use PlaceValue as V;
    let mut out = Vec::new();
    macro_rules! each {
        ($p:pat => $e:expr) => {
            vs.iter()
                .map(|v| match v {
                    $p => $e,
                    _ => panic!("a column mixes types"),
                })
                .collect::<Vec<_>>()
        };
    }
    match vs[0] {
        V::String(_) | V::Bytecode(_) => {
            for v in vs {
                if let V::String(b) | V::Bytecode(b) = v {
                    string(&mut out, b);
                }
            }
        }
        V::Bool(_) => out.extend(each!(V::Bool(b) => *b as u8)),
        V::Int32(_) => out.extend(i32s(&each!(V::Int32(x) => *x))),
        V::Float32(_) => out.extend(f32s(&each!(V::Float32(x) => *x))),
        V::Float64(_) => {
            for x in each!(V::Float64(x) => *x) {
                out.extend(x.to_le_bytes());
            }
        }
        V::UDim(..) => {
            out.extend(f32s(&each!(V::UDim(s, _) => *s)));
            out.extend(i32s(&each!(V::UDim(_, o) => *o)));
        }
        V::UDim2(..) => {
            out.extend(f32s(&each!(V::UDim2(a, ..) => *a)));
            out.extend(f32s(&each!(V::UDim2(_, _, c, _) => *c)));
            out.extend(i32s(&each!(V::UDim2(_, b, ..) => *b)));
            out.extend(i32s(&each!(V::UDim2(.., d) => *d)));
        }
        V::Ray { .. } => {
            for (o, d) in each!(V::Ray { origin, direction } => (*origin, *direction)) {
                for x in o.into_iter().chain(d) {
                    f32le(&mut out, x);
                }
            }
        }
        V::Faces(_) => out.extend(each!(V::Faces(x) => *x)),
        V::Axes(_) => out.extend(each!(V::Axes(x) => *x)),
        V::BrickColor(_) => out.extend(u32s(&each!(V::BrickColor(x) => *x))),
        V::Color3(_) => {
            for k in 0..3 {
                out.extend(f32s(&each!(V::Color3(c) => c[k])));
            }
        }
        V::Vector2(_) => {
            for k in 0..2 {
                out.extend(f32s(&each!(V::Vector2(c) => c[k])));
            }
        }
        V::Vector3(_) => {
            for k in 0..3 {
                out.extend(f32s(&each!(V::Vector3(c) => c[k])));
            }
        }
        V::CFrame(_) => cframes(&mut out, &each!(V::CFrame(c) => *c)),
        V::Enum(_) => out.extend(u32s(&each!(V::Enum(x) => *x))),
        V::Ref(_) => out.extend(referents(&each!(V::Ref(r) => r.map_or(-1, |i| i as i32)))),
        V::Vector3int16(_) => {
            for v in each!(V::Vector3int16(v) => *v) {
                for x in v {
                    out.extend((x as u16).to_le_bytes());
                }
            }
        }
        V::NumberSequence(_) => {
            for keys in each!(V::NumberSequence(k) => k) {
                out.extend((keys.len() as u32).to_le_bytes());
                for k in keys {
                    for x in k {
                        f32le(&mut out, *x);
                    }
                }
            }
        }
        V::ColorSequence(_) => {
            for keys in each!(V::ColorSequence(k) => k) {
                out.extend((keys.len() as u32).to_le_bytes());
                for (t, c) in keys {
                    f32le(&mut out, *t);
                    for x in c {
                        f32le(&mut out, *x);
                    }
                    f32le(&mut out, 0.0);
                }
            }
        }
        V::NumberRange(..) => {
            for (a, b) in each!(V::NumberRange(a, b) => (*a, *b)) {
                f32le(&mut out, a);
                f32le(&mut out, b);
            }
        }
        V::Rect(_) => {
            for k in 0..4 {
                out.extend(f32s(&each!(V::Rect(r) => r[k])));
            }
        }
        V::PhysicalProperties(_) => {
            for p in each!(V::PhysicalProperties(p) => *p) {
                match p {
                    None => out.push(0),
                    Some(p) => {
                        out.push(3);
                        for x in p {
                            f32le(&mut out, x);
                        }
                    }
                }
            }
        }
        V::Color3uint8(_) => {
            for k in 0..3 {
                out.extend(each!(V::Color3uint8(c) => c[k]));
            }
        }
        V::Int64(_) => out.extend(i64s(&each!(V::Int64(x) => *x))),
        V::SecurityCapabilities(_) => {
            out.extend(i64s(&each!(V::SecurityCapabilities(x) => *x as i64)))
        }
        V::SharedString(_) => {
            let idx = each!(V::SharedString(b) => {
                match shared.iter().position(|s| s == b) {
                    Some(i) => i as u32,
                    None => {
                        shared.push(b.clone());
                        (shared.len() - 1) as u32
                    }
                }
            });
            out.extend(u32s(&idx));
        }
        V::OptionalCFrame(_) => {
            out.push(0x10);
            let cfs = each!(V::OptionalCFrame(c) => c.unwrap_or(CFrame::IDENTITY));
            cframes(&mut out, &cfs);
            out.push(0x02);
            out.extend(each!(V::OptionalCFrame(c) => c.is_some() as u8));
        }
        V::UniqueId(_) => out.extend(interleave(&each!(V::UniqueId(u) => *u))),
        V::Font(_) => {
            for f in each!(V::Font(f) => f) {
                string(&mut out, f.family.as_bytes());
                out.extend(f.weight.to_le_bytes());
                out.push((f.style == "Italic") as u8);
                string(&mut out, f.cached_face.as_bytes());
            }
        }
        V::Content(_) => {
            let kinds = each!(V::Content(c) => c.is_some() as i32);
            out.extend(i32s(&kinds));
            let uris: Vec<&String> = vs
                .iter()
                .filter_map(|v| match v {
                    V::Content(Some(u)) => Some(u),
                    _ => None,
                })
                .collect();
            out.extend((uris.len() as u32).to_le_bytes());
            for u in uris {
                string(&mut out, u.as_bytes());
            }
            out.extend(0u32.to_le_bytes()); // objects
            out.extend(0u32.to_le_bytes()); // external
        }
    }
    out
}

/// A binary file of `instances`. Every instance of a class must carry the
/// same properties, in the same order, with values of one type each.
pub fn binary(instances: &[Inst], compression: Compression) -> Vec<u8> {
    let mut classes: Vec<&str> = instances.iter().map(|i| i.class.as_str()).collect();
    classes.sort();
    classes.dedup();
    let mut out = Vec::new();
    out.extend(MAGIC);
    out.extend(0u16.to_le_bytes());
    out.extend((classes.len() as u32).to_le_bytes());
    out.extend((instances.len() as u32).to_le_bytes());
    out.extend([0u8; 8]);

    let mut meta = Vec::new();
    meta.extend(1u32.to_le_bytes());
    string(&mut meta, b"ExplicitAutoJoints");
    string(&mut meta, b"true");
    chunk(&mut out, b"META", &meta, compression);

    let mut shared = Vec::new();
    let mut props = Vec::new();
    let mut insts = Vec::new();
    for (id, class) in classes.iter().enumerate() {
        let members: Vec<usize> = (0..instances.len())
            .filter(|&i| instances[i].class == *class)
            .collect();
        let mut body = Vec::new();
        body.extend((id as u32).to_le_bytes());
        string(&mut body, class.as_bytes());
        body.push(0);
        body.extend((members.len() as u32).to_le_bytes());
        body.extend(referents(
            &members.iter().map(|&i| i as i32).collect::<Vec<_>>(),
        ));
        insts.push(body);
        let first = &instances[members[0]];
        for (k, (name, v0)) in first.props.iter().enumerate() {
            let vs: Vec<&PlaceValue> = members
                .iter()
                .map(|&i| {
                    let (n, v) = &instances[i].props[k];
                    assert_eq!(n, name, "instances of {class} differ in properties");
                    v
                })
                .collect();
            let mut body = Vec::new();
            body.extend((id as u32).to_le_bytes());
            string(&mut body, name.as_bytes());
            body.push(type_id(v0));
            body.extend(column(&vs, &mut shared));
            props.push(body);
        }
    }
    if !shared.is_empty() {
        let mut body = Vec::new();
        body.extend(0u32.to_le_bytes());
        body.extend((shared.len() as u32).to_le_bytes());
        for s in &shared {
            body.extend([0u8; 16]);
            string(&mut body, s);
        }
        chunk(&mut out, b"SSTR", &body, compression);
    }
    for b in insts {
        chunk(&mut out, b"INST", &b, compression);
    }
    for b in props {
        chunk(&mut out, b"PROP", &b, compression);
    }
    let mut prnt = vec![0u8];
    prnt.extend((instances.len() as u32).to_le_bytes());
    prnt.extend(referents(&(0..instances.len() as i32).collect::<Vec<_>>()));
    prnt.extend(referents(
        &instances
            .iter()
            .map(|i| i.parent.map_or(-1, |p| p as i32))
            .collect::<Vec<_>>(),
    ));
    chunk(&mut out, b"PRNT", &prnt, compression);
    chunk(&mut out, b"END", b"</roblox>", Compression::None);
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn xyz(out: &mut String, v: [f32; 3]) {
    write!(out, "<X>{}</X><Y>{}</Y><Z>{}</Z>", v[0], v[1], v[2]).unwrap();
}

fn cframe_xml(out: &mut String, c: &CFrame) {
    xyz(out, c.position);
    for (k, n) in [
        "R00", "R01", "R02", "R10", "R11", "R12", "R20", "R21", "R22",
    ]
    .iter()
    .enumerate()
    {
        write!(out, "<{n}>{}</{n}>", c.rotation[k]).unwrap();
    }
}

fn font_xml(out: &mut String, f: &Font) {
    write!(
        out,
        "<Family><url>{}</url></Family><Weight>{}</Weight><Style>{}</Style><CachedFaceId><url>{}</url></CachedFaceId>",
        esc(&f.family),
        f.weight,
        f.style,
        esc(&f.cached_face)
    )
    .unwrap();
}

fn prop_xml(out: &mut String, name: &str, v: &PlaceValue, shared: &mut Vec<Vec<u8>>) {
    use PlaceValue as V;
    let (tag, body) = match v {
        V::String(b) => match std::str::from_utf8(b) {
            Ok(t) if name == "Source" => ("ProtectedString", format!("<![CDATA[{t}]]>")),
            Ok(t) => ("string", esc(t)),
            Err(_) => ("BinaryString", b64(b)),
        },
        V::Bytecode(b) => ("BinaryString", b64(b)),
        V::Bool(b) => ("bool", b.to_string()),
        V::Int32(x) => ("int", x.to_string()),
        V::Int64(x) => ("int64", x.to_string()),
        V::Float32(x) => ("float", x.to_string()),
        V::Float64(x) => ("double", x.to_string()),
        V::UDim(s, o) => ("UDim", format!("<S>{s}</S><O>{o}</O>")),
        V::UDim2(a, b, c, d) => (
            "UDim2",
            format!("<XS>{a}</XS><XO>{b}</XO><YS>{c}</YS><YO>{d}</YO>"),
        ),
        V::Ray { origin, direction } => {
            let mut b = String::from("<origin>");
            xyz(&mut b, *origin);
            b.push_str("</origin><direction>");
            xyz(&mut b, *direction);
            b.push_str("</direction>");
            ("Ray", b)
        }
        V::Faces(x) => ("Faces", format!("<faces>{x}</faces>")),
        V::Axes(x) => ("Axes", format!("<axes>{x}</axes>")),
        V::BrickColor(x) => ("BrickColor", x.to_string()),
        V::Color3(c) => (
            "Color3",
            format!("<R>{}</R><G>{}</G><B>{}</B>", c[0], c[1], c[2]),
        ),
        V::Color3uint8(c) => (
            "Color3uint8",
            (0xff00_0000u32 | u32::from(c[0]) << 16 | u32::from(c[1]) << 8 | u32::from(c[2]))
                .to_string(),
        ),
        V::Vector2(v) => ("Vector2", format!("<X>{}</X><Y>{}</Y>", v[0], v[1])),
        V::Vector3(v) => {
            let mut b = String::new();
            xyz(&mut b, *v);
            ("Vector3", b)
        }
        V::Vector3int16(v) => (
            "Vector3int16",
            format!("<X>{}</X><Y>{}</Y><Z>{}</Z>", v[0], v[1], v[2]),
        ),
        V::CFrame(c) => {
            let mut b = String::new();
            cframe_xml(&mut b, c);
            ("CoordinateFrame", b)
        }
        V::OptionalCFrame(c) => {
            let mut b = String::new();
            if let Some(c) = c {
                b.push_str("<CFrame>");
                cframe_xml(&mut b, c);
                b.push_str("</CFrame>");
            }
            ("OptionalCoordinateFrame", b)
        }
        V::Enum(x) => ("token", x.to_string()),
        V::Ref(r) => ("Ref", r.map_or("null".into(), |i| format!("RBX{i}"))),
        V::NumberSequence(k) => (
            "NumberSequence",
            k.iter().map(|[a, b, c]| format!("{a} {b} {c} ")).collect(),
        ),
        V::ColorSequence(k) => (
            "ColorSequence",
            k.iter()
                .map(|(t, c)| format!("{t} {} {} {} 0 ", c[0], c[1], c[2]))
                .collect(),
        ),
        V::NumberRange(a, b) => ("NumberRange", format!("{a} {b} ")),
        V::Rect(r) => (
            "Rect2D",
            format!(
                "<min><X>{}</X><Y>{}</Y></min><max><X>{}</X><Y>{}</Y></max>",
                r[0], r[1], r[2], r[3]
            ),
        ),
        V::PhysicalProperties(p) => (
            "PhysicalProperties",
            match p {
                None => "<CustomPhysics>false</CustomPhysics>".into(),
                Some(p) => format!(
                    "<CustomPhysics>true</CustomPhysics><Density>{}</Density><Friction>{}</Friction><Elasticity>{}</Elasticity><FrictionWeight>{}</FrictionWeight><ElasticityWeight>{}</ElasticityWeight><AcousticAbsorption>{}</AcousticAbsorption>",
                    p[0], p[1], p[2], p[3], p[4], p[5]
                ),
            },
        ),
        V::SharedString(b) => {
            let i = match shared.iter().position(|s| s == b) {
                Some(i) => i,
                None => {
                    shared.push(b.clone());
                    shared.len() - 1
                }
            };
            ("SharedString", format!("key{i}"))
        }
        V::UniqueId(u) => ("UniqueId", u.iter().map(|b| format!("{b:02x}")).collect()),
        V::Font(f) => {
            let mut b = String::new();
            font_xml(&mut b, f);
            ("Font", b)
        }
        V::SecurityCapabilities(x) => ("SecurityCapabilities", x.to_string()),
        V::Content(c) => (
            "Content",
            match c {
                None => "<null></null>".into(),
                Some(u) => format!("<url>{}</url>", esc(u)),
            },
        ),
    };
    write!(out, "<{tag} name=\"{}\">{body}</{tag}>", esc(name)).unwrap();
}

/// An XML file of `instances`.
pub fn xml(instances: &[Inst]) -> String {
    let mut shared = Vec::new();
    fn item(out: &mut String, all: &[Inst], i: usize, shared: &mut Vec<Vec<u8>>) {
        write!(
            out,
            "<Item class=\"{}\" referent=\"RBX{i}\"><Properties>",
            all[i].class
        )
        .unwrap();
        for (k, v) in &all[i].props {
            prop_xml(out, k, v, shared);
        }
        out.push_str("</Properties>");
        for (c, inst) in all.iter().enumerate() {
            if inst.parent == Some(i) {
                item(out, all, c, shared);
            }
        }
        out.push_str("</Item>\n");
    }
    let mut body = String::new();
    for (i, inst) in instances.iter().enumerate() {
        if inst.parent.is_none() {
            item(&mut body, instances, i, &mut shared);
        }
    }
    let mut out = String::from(
        "<roblox xmlns:xmime=\"http://www.w3.org/2005/05/xmlmime\" version=\"4\">\n<Meta name=\"ExplicitAutoJoints\">true</Meta>\n<External>null</External>\n",
    );
    out.push_str(&body);
    if !shared.is_empty() {
        out.push_str("<SharedStrings>");
        for (i, s) in shared.iter().enumerate() {
            write!(
                out,
                "<SharedString md5=\"key{i}\">{}</SharedString>",
                b64(s)
            )
            .unwrap();
        }
        out.push_str("</SharedStrings>");
    }
    out.push_str("</roblox>\n");
    out
}

/// An `AttributesSerialize` blob of string, bool, double and Vector3 values,
/// and a Color3 the importer skips.
pub fn attributes() -> Vec<u8> {
    let mut b = Vec::new();
    b.extend(5u32.to_le_bytes());
    string(&mut b, b"Label");
    b.push(0x02);
    string(&mut b, b"gold");
    string(&mut b, b"Open");
    b.push(0x03);
    b.push(1);
    string(&mut b, b"Speed");
    b.push(0x06);
    b.extend(2.5f64.to_le_bytes());
    string(&mut b, b"Spawn");
    b.push(0x11);
    for x in [1.0f32, 2.0, 3.0] {
        f32le(&mut b, x);
    }
    string(&mut b, b"Tint");
    b.push(0x0F);
    for x in [1.0f32, 0.5, 0.0] {
        f32le(&mut b, x);
    }
    b
}
