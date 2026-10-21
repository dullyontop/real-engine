//! The XML place and model reader (`.rbxlx`, `.rbxmx`), from the format
//! facts ADR-0048 restates: one `roblox` element with `version="4"`; `Item`
//! elements with a `class` and a file-unique `referent` (`null` is kept for
//! an empty reference), nested for children; one `Properties` element per
//! item whose children are typed elements carrying the property in a `name`
//! attribute; an optional `SharedStrings` table keyed by its `md5`
//! attribute. `Meta` is kept; the legacy `External` elements are ignored.
//!
//! Like the binary reader it trusts nothing: nesting deeper than
//! [`MAX_DEPTH`] and more than [`MAX_INSTANCES`](super::binary::MAX_INSTANCES)
//! items are refused, and every problem is `RE9809` with its line.

use std::collections::{BTreeSet, HashMap};

use base64::Engine as _;
use quick_xml::events::Event;
use re_core::{Diagnostic, Location};

use super::binary::{MAX_INSTANCES, check_cycles};
use super::dom::{CFrame, Dom, Font, Instance, PlaceValue};
use crate::codes;

/// The deepest element nesting the reader follows.
pub const MAX_DEPTH: usize = 2048;

/// An element of the document.
#[derive(Debug, Default)]
struct El {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<El>,
    text: String,
    line: u32,
}

impl El {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn child(&self, name: &str) -> Option<&El> {
        self.children.iter().find(|c| c.name == name)
    }
}

type R<T> = Result<T, Diagnostic>;

fn malformed(file: &str, line: u32, msg: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::error(
        codes::PLACE_MALFORMED,
        format!("{file} is not a readable place file: {msg} (line {line})"),
    )
    .at(Location::file_at(file.to_string(), line, 1))
}

/// The line of byte `pos` of `text`.
fn line_of(text: &str, pos: usize) -> u32 {
    Lines::default().at(text, pos)
}

/// Line numbers of increasing byte positions, counted once.
#[derive(Default)]
struct Lines {
    pos: usize,
    line: u32,
}

impl Lines {
    fn at(&mut self, text: &str, pos: usize) -> u32 {
        let end = pos.min(text.len());
        if end < self.pos {
            *self = Lines::default();
        }
        self.line += text.as_bytes()[self.pos..end]
            .iter()
            .filter(|&&b| b == b'\n')
            .count() as u32;
        self.pos = end;
        1 + self.line
    }
}

/// Parses the document into elements.
fn parse(text: &str, file: &str) -> R<El> {
    let mut reader = quick_xml::Reader::from_str(text);
    let mut stack: Vec<El> = vec![El::default()];
    let mut items = 0usize;
    let mut lines = Lines::default();
    loop {
        let pos = reader.buffer_position() as usize;
        let line = lines.at(text, pos);
        let ev = reader
            .read_event()
            .map_err(|e| malformed(file, line, format!("the XML does not parse: {e}")))?;
        let bad = |m: String| malformed(file, line, m);
        match ev {
            Event::Start(e) | Event::Empty(e) if stack.len() > MAX_DEPTH => {
                let _ = e;
                return Err(bad(format!("elements nest deeper than {MAX_DEPTH}")));
            }
            Event::Start(e) => {
                let el = element(&e, line).map_err(bad)?;
                if el.name == "Item" {
                    items += 1;
                    if items > MAX_INSTANCES {
                        return Err(bad(format!(
                            "it holds more than {MAX_INSTANCES} items, the most the importer reads"
                        )));
                    }
                }
                stack.push(el);
            }
            Event::Empty(e) => {
                let el = element(&e, line).map_err(bad)?;
                stack.last_mut().expect("the root").children.push(el);
            }
            Event::End(_) => {
                let el = stack.pop().expect("balanced");
                match stack.last_mut() {
                    Some(parent) => parent.children.push(el),
                    None => return Err(bad("an end tag closes nothing".into())),
                }
            }
            Event::Text(t) => {
                let s = t
                    .xml10_content()
                    .map_err(|e| bad(format!("text is not UTF-8: {e}")))?;
                stack.last_mut().expect("the root").text.push_str(&s);
            }
            Event::CData(t) => {
                let s = t
                    .decode()
                    .map_err(|e| bad(format!("CDATA is not UTF-8: {e}")))?;
                stack.last_mut().expect("the root").text.push_str(&s);
            }
            Event::GeneralRef(r) => {
                let c = if r.is_char_ref() {
                    r.resolve_char_ref()
                        .map_err(|e| bad(format!("a character reference does not read: {e}")))?
                } else {
                    let name = r
                        .decode()
                        .map_err(|e| bad(format!("an entity is not UTF-8: {e}")))?;
                    match &*name {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        other => return Err(bad(format!("unknown entity &{other};"))),
                    }
                };
                if let Some(c) = c {
                    stack.last_mut().expect("the root").text.push(c);
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if stack.len() != 1 {
        return Err(malformed(
            file,
            line_of(text, text.len()),
            "the document ends inside an element",
        ));
    }
    let mut doc = stack.pop().expect("the root");
    let roots: Vec<El> = std::mem::take(&mut doc.children);
    let mut roots = roots.into_iter().filter(|e| e.name == "roblox");
    roots
        .next()
        .ok_or_else(|| malformed(file, 1, "there is no roblox element"))
}

fn element(e: &quick_xml::events::BytesStart<'_>, line: u32) -> Result<El, String> {
    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut attrs = Vec::new();
    for a in e.attributes() {
        let a = a.map_err(|err| format!("an attribute of <{name}> does not read: {err}"))?;
        let k = String::from_utf8_lossy(a.key.as_ref()).into_owned();
        let v = a
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .map_err(|err| format!("attribute {k} of <{name}> does not read: {err}"))?
            .into_owned();
        attrs.push((k, v));
    }
    Ok(El {
        name,
        attrs,
        line,
        ..El::default()
    })
}

/// Reads an XML file. `file` names it in messages.
pub fn read(text: &str, file: &str) -> R<Dom> {
    let root = parse(text, file)?;
    match root.attr("version") {
        Some("4") => {}
        other => {
            return Err(malformed(
                file,
                root.line,
                format!("the roblox element has version {other:?}, not \"4\""),
            ));
        }
    }
    let mut r = Reader {
        file,
        dom: Dom::default(),
        parents: Vec::new(),
        referents: HashMap::new(),
        refs: Vec::new(),
        shared: HashMap::new(),
        pending_shared: Vec::new(),
        unknown: BTreeSet::new(),
    };
    for el in &root.children {
        match el.name.as_str() {
            "Meta" => {
                if let Some(k) = el.attr("name") {
                    r.dom.meta.push((k.to_string(), el.text.clone()));
                }
            }
            "SharedStrings" => {
                for s in el.children.iter().filter(|c| c.name == "SharedString") {
                    let key = s.attr("md5").unwrap_or_default().to_string();
                    let bytes = base64(&s.text).map_err(|m| malformed(file, s.line, m))?;
                    r.shared.insert(key, bytes);
                }
            }
            "Item" => r.item(el, None)?,
            _ => {}
        }
    }
    for (i, k, key, line) in std::mem::take(&mut r.pending_shared) {
        match r.shared.get(&key) {
            Some(b) => r.dom.instances[i].properties[k].1 = PlaceValue::SharedString(b.clone()),
            None => {
                return Err(malformed(
                    file,
                    line,
                    format!("shared string {key:?} is not in the SharedStrings table"),
                ));
            }
        }
    }
    for (i, k, referent) in std::mem::take(&mut r.refs) {
        let target = r.referents.get(&referent).copied();
        r.dom.instances[i].properties[k].1 = PlaceValue::Ref(target);
    }
    for (class, prop, ty) in std::mem::take(&mut r.unknown) {
        r.dom.warnings.push(
            Diagnostic::warning(
                codes::PLACE_VALUE_TYPE,
                format!(
                    "{file}: {class}.{prop} has value type {ty}, which the importer does not know; the property is skipped"
                ),
            )
            .at(Location::file(file.to_string())),
        );
    }
    check_cycles(&r.parents)
        .map_err(|i| malformed(file, 1, format!("instance {i} is its own ancestor")))?;
    let parents = std::mem::take(&mut r.parents);
    r.dom.link(&parents);
    Ok(r.dom)
}

struct Reader<'a> {
    file: &'a str,
    dom: Dom,
    parents: Vec<Option<usize>>,
    referents: HashMap<String, usize>,
    /// `(instance, property index, referent)`, resolved at the end.
    refs: Vec<(usize, usize, String)>,
    shared: HashMap<String, Vec<u8>>,
    pending_shared: Vec<(usize, usize, String, u32)>,
    /// `(class, property, type)` of values of unknown types.
    unknown: BTreeSet<(String, String, String)>,
}

fn base64(text: &str) -> Result<Vec<u8>, String> {
    let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    base64::engine::general_purpose::STANDARD
        .decode(clean.as_bytes())
        .map_err(|e| format!("base64 does not decode: {e}"))
}

impl Reader<'_> {
    fn item(&mut self, el: &El, parent: Option<usize>) -> R<()> {
        let file = self.file;
        let class = el
            .attr("class")
            .ok_or_else(|| malformed(file, el.line, "an Item has no class"))?
            .to_string();
        let i = self.dom.instances.len();
        self.dom.instances.push(Instance {
            class: class.clone(),
            ..Instance::default()
        });
        self.parents.push(parent);
        if let Some(r) = el.attr("referent") {
            if r == "null" {
                return Err(malformed(
                    file,
                    el.line,
                    "an Item has the referent \"null\"",
                ));
            }
            if self.referents.insert(r.to_string(), i).is_some() {
                return Err(malformed(
                    file,
                    el.line,
                    format!("referent {r:?} is used twice"),
                ));
            }
        }
        for c in &el.children {
            match c.name.as_str() {
                "Properties" => {
                    for p in &c.children {
                        let Some(name) = p.attr("name") else {
                            continue;
                        };
                        self.property(i, &class, name, p)?;
                    }
                }
                "Item" => self.item(c, Some(i))?,
                _ => {}
            }
        }
        Ok(())
    }

    fn property(&mut self, i: usize, class: &str, name: &str, p: &El) -> R<()> {
        let file = self.file;
        let bad = |m: String| malformed(file, p.line, format!("{class}.{name}: {m}"));
        let k = self.dom.instances[i].properties.len();
        let v = match p.name.as_str() {
            "Ref" => {
                let t = p.text.trim();
                if t != "null" && !t.is_empty() {
                    self.refs.push((i, k, t.to_string()));
                }
                PlaceValue::Ref(None)
            }
            "SharedString" => {
                self.pending_shared
                    .push((i, k, p.text.trim().to_string(), p.line));
                PlaceValue::SharedString(Vec::new())
            }
            ty => match value(ty, p).map_err(bad)? {
                Some(v) => v,
                None => {
                    self.unknown
                        .insert((class.to_string(), name.to_string(), ty.to_string()));
                    return Ok(());
                }
            },
        };
        self.dom.instances[i].properties.push((name.to_string(), v));
        Ok(())
    }
}

fn num<T: std::str::FromStr>(el: Option<&El>, what: &str) -> Result<T, String> {
    let el = el.ok_or_else(|| format!("{what} is missing"))?;
    parse_num(&el.text, what)
}

fn parse_num<T: std::str::FromStr>(text: &str, what: &str) -> Result<T, String> {
    let t = text.trim();
    let t = match t {
        "INF" | "inf" => "inf",
        "-INF" | "-inf" => "-inf",
        "NAN" | "nan" | "-nan(ind)" => "NaN",
        _ => t,
    };
    t.parse()
        .map_err(|_| format!("{what} {t:?} is not a number of the right kind"))
}

fn xyz(el: &El) -> Result<[f32; 3], String> {
    Ok([
        num(el.child("X"), "X")?,
        num(el.child("Y"), "Y")?,
        num(el.child("Z"), "Z")?,
    ])
}

fn cframe(el: &El) -> Result<CFrame, String> {
    let names = [
        "R00", "R01", "R02", "R10", "R11", "R12", "R20", "R21", "R22",
    ];
    let mut rotation = [0f32; 9];
    for (r, n) in rotation.iter_mut().zip(names) {
        *r = num(el.child(n), n)?;
    }
    Ok(CFrame {
        position: xyz(el)?,
        rotation,
    })
}

fn floats(text: &str, what: &str) -> Result<Vec<f32>, String> {
    text.split_whitespace()
        .map(|t| parse_num(t, what))
        .collect()
}

fn url(el: Option<&El>) -> Option<String> {
    let el = el?;
    if el.child("null").is_some() {
        return None;
    }
    match el.child("url") {
        Some(u) => Some(u.text.trim().to_string()),
        None => {
            let t = el.text.trim();
            (!t.is_empty()).then(|| t.to_string())
        }
    }
}

/// The value of typed element `p`, or `None` for a type the reader does not
/// know.
fn value(ty: &str, p: &El) -> Result<Option<PlaceValue>, String> {
    use PlaceValue as V;
    let t = p.text.as_str();
    Ok(Some(match ty {
        "string" | "ProtectedString" => V::String(t.as_bytes().to_vec()),
        "BinaryString" => V::String(base64(t)?),
        "bool" => match t.trim() {
            "true" => V::Bool(true),
            "false" => V::Bool(false),
            other => return Err(format!("{other:?} is not true or false")),
        },
        "int" => V::Int32(parse_num(t, "int")?),
        "int64" => V::Int64(parse_num(t, "int64")?),
        "float" => V::Float32(parse_num(t, "float")?),
        "double" => V::Float64(parse_num(t, "double")?),
        "token" => V::Enum(parse_num(t, "token")?),
        "BrickColor" => V::BrickColor(parse_num(t, "BrickColor")?),
        "SecurityCapabilities" => V::SecurityCapabilities(parse_num(t, "SecurityCapabilities")?),
        "Vector3" => V::Vector3(xyz(p)?),
        "Vector2" => V::Vector2([num(p.child("X"), "X")?, num(p.child("Y"), "Y")?]),
        "Vector3int16" => V::Vector3int16([
            num(p.child("X"), "X")?,
            num(p.child("Y"), "Y")?,
            num(p.child("Z"), "Z")?,
        ]),
        "Color3" => match p.child("R") {
            Some(_) => V::Color3([
                num(p.child("R"), "R")?,
                num(p.child("G"), "G")?,
                num(p.child("B"), "B")?,
            ]),
            None => {
                let packed: u32 = parse_num(t, "Color3")?;
                let c = packed.to_be_bytes();
                V::Color3([
                    c[1] as f32 / 255.0,
                    c[2] as f32 / 255.0,
                    c[3] as f32 / 255.0,
                ])
            }
        },
        "Color3uint8" => {
            let packed: u32 = parse_num(t, "Color3uint8")?;
            let c = packed.to_be_bytes();
            V::Color3uint8([c[1], c[2], c[3]])
        }
        "CoordinateFrame" => V::CFrame(cframe(p)?),
        "OptionalCoordinateFrame" => V::OptionalCFrame(match p.child("CFrame") {
            Some(c) => Some(cframe(c)?),
            None => None,
        }),
        "UDim" => V::UDim(num(p.child("S"), "S")?, num(p.child("O"), "O")?),
        "UDim2" => V::UDim2(
            num(p.child("XS"), "XS")?,
            num(p.child("XO"), "XO")?,
            num(p.child("YS"), "YS")?,
            num(p.child("YO"), "YO")?,
        ),
        "Ray" => V::Ray {
            origin: xyz(p.child("origin").ok_or("origin is missing")?)?,
            direction: xyz(p.child("direction").ok_or("direction is missing")?)?,
        },
        "Faces" => V::Faces(num(p.child("faces"), "faces")?),
        "Axes" => V::Axes(num(p.child("axes"), "axes")?),
        "NumberSequence" => {
            let f = floats(t, "NumberSequence")?;
            if f.len() % 3 != 0 {
                return Err(format!("{} numbers do not make keypoints of 3", f.len()));
            }
            V::NumberSequence(f.chunks(3).map(|c| [c[0], c[1], c[2]]).collect())
        }
        "ColorSequence" => {
            let f = floats(t, "ColorSequence")?;
            if f.len() % 5 != 0 {
                return Err(format!("{} numbers do not make keypoints of 5", f.len()));
            }
            V::ColorSequence(f.chunks(5).map(|c| (c[0], [c[1], c[2], c[3]])).collect())
        }
        "NumberRange" => {
            let f = floats(t, "NumberRange")?;
            match f[..] {
                [a, b, ..] => V::NumberRange(a, b),
                _ => return Err("a NumberRange needs two numbers".into()),
            }
        }
        "Rect2D" => {
            let min = p.child("min").ok_or("min is missing")?;
            let max = p.child("max").ok_or("max is missing")?;
            V::Rect([
                num(min.child("X"), "min X")?,
                num(min.child("Y"), "min Y")?,
                num(max.child("X"), "max X")?,
                num(max.child("Y"), "max Y")?,
            ])
        }
        "PhysicalProperties" => {
            let custom = p
                .child("CustomPhysics")
                .is_some_and(|c| c.text.trim() == "true");
            V::PhysicalProperties(if custom {
                Some([
                    num(p.child("Density"), "Density")?,
                    num(p.child("Friction"), "Friction")?,
                    num(p.child("Elasticity"), "Elasticity")?,
                    num(p.child("FrictionWeight"), "FrictionWeight")?,
                    num(p.child("ElasticityWeight"), "ElasticityWeight")?,
                    match p.child("AcousticAbsorption") {
                        Some(a) => parse_num(&a.text, "AcousticAbsorption")?,
                        None => 1.0,
                    },
                ])
            } else {
                None
            })
        }
        "Content" | "ContentId" => V::Content(url(Some(p))),
        "Font" => V::Font(Font {
            family: url(p.child("Family")).unwrap_or_default(),
            weight: match p.child("Weight") {
                Some(w) => parse_num(&w.text, "Weight")?,
                None => 400,
            },
            style: p
                .child("Style")
                .map(|s| s.text.trim().to_string())
                .unwrap_or_else(|| "Normal".into()),
            cached_face: url(p.child("CachedFaceId")).unwrap_or_default(),
        }),
        "UniqueId" => {
            let h = t.trim();
            let mut id = [0u8; 16];
            if h.len() != 32 || !h.is_ascii() {
                return Err(format!("{h:?} is not 32 hex digits"));
            }
            for (k, b) in id.iter_mut().enumerate() {
                *b = u8::from_str_radix(&h[2 * k..2 * k + 2], 16)
                    .map_err(|_| format!("{h:?} is not 32 hex digits"))?;
            }
            V::UniqueId(id)
        }
        _ => return Ok(None),
    }))
}
