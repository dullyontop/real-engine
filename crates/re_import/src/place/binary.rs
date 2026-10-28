//! The binary place and model reader (`.rbxl`, `.rbxm`), from the format
//! facts ADR-0048 restates:
//!
//! * a 32-byte header: `<roblox!`, the bytes `89 ff 0d 0a 1a 0a`, a
//!   little-endian `u16` version 0, the class and instance counts as `i32`,
//!   and 8 reserved bytes;
//! * chunks, each a 16-byte header (a 4-byte name padded with zeros, the
//!   compressed length, the uncompressed length, 4 reserved bytes) and a
//!   body. A compressed length of 0 means the body is stored as is;
//!   otherwise it is a ZSTD frame when it starts with `28 b5 2f fd` and a
//!   bare LZ4 block when it does not;
//! * `META` (string pairs), `SSTR` (shared strings, each after a 16-byte
//!   hash), one `INST` per class (its id, name, a service flag and the
//!   referents of its instances), one `PROP` per class and property (the
//!   class id, the property's serialised name, a type id and one value per
//!   instance of the class), one `PRNT` (children and their parents, `-1`
//!   for none) and `END`.
//!
//! Integer arrays are zigzag-encoded, big-endian and byte-interleaved (all
//! first bytes, then all second bytes, …); float arrays move the sign bit to
//! the lowest bit before the same interleaving; referent arrays are
//! interleaved integers stored as differences, summed while reading.
//!
//! The reader trusts nothing: every length is checked against the bytes
//! left before anything is allocated, a chunk may expand to at most
//! [`MAX_CHUNK`] bytes, a file may hold at most [`MAX_INSTANCES`]
//! instances, and they may nest at most [`MAX_DEPTH`] levels deep. It
//! returns `RE9809` with the chunk and offset, never panics.

use std::collections::HashMap;

use re_core::Diagnostic;

use super::dom::{CFrame, Dom, Font, Instance, PlaceValue, rotation_of_id};
use super::xml::MAX_DEPTH;
use crate::codes;

/// The magic the file starts with.
pub const MAGIC: &[u8; 14] = b"<roblox!\x89\xff\r\n\x1a\n";
/// The largest a chunk body may expand to.
pub const MAX_CHUNK: usize = 256 * 1024 * 1024;
/// The most instances a file may declare.
pub const MAX_INSTANCES: usize = 10_000_000;
/// The first bytes of a ZSTD frame.
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// True if `bytes` start like a binary place file.
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.starts_with(&MAGIC[..8])
}

/// Where a problem is: the chunk and the offset in the file (of the chunk
/// header) or in the chunk's body.
#[derive(Clone, Debug)]
struct At {
    file: String,
    chunk: String,
    offset: usize,
}

impl At {
    fn error(&self, msg: impl std::fmt::Display) -> Diagnostic {
        Diagnostic::error(
            codes::PLACE_MALFORMED,
            format!(
                "{} is not a readable place file: {msg} (chunk {}, at byte {})",
                self.file, self.chunk, self.offset
            ),
        )
        .at(re_core::Location::file(self.file.clone()))
    }
}

/// A cursor over a chunk body that refuses to read past its end.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
    at: &'a At,
}

type R<T> = Result<T, Diagnostic>;

impl<'a> Cursor<'a> {
    fn err(&self, msg: impl std::fmt::Display) -> Diagnostic {
        At {
            offset: self.pos,
            ..self.at.clone()
        }
        .error(msg)
    }

    fn left(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        if n > self.left() {
            return Err(self.err(format!("{n} bytes are needed but {} are left", self.left())));
        }
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> R<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().expect("2")))
    }

    fn u32(&mut self) -> R<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().expect("4")))
    }

    fn f32(&mut self) -> R<f32> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().expect("4")))
    }

    fn f64(&mut self) -> R<f64> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().expect("8")))
    }

    /// A count of items of at least `each` bytes, checked against the bytes
    /// left so nothing large is allocated for a lie.
    fn count(&mut self, each: usize) -> R<usize> {
        let n = self.u32()? as usize;
        self.check(n, each)?;
        Ok(n)
    }

    fn check(&self, n: usize, each: usize) -> R<()> {
        if n.saturating_mul(each.max(1)) > self.left() {
            return Err(self.err(format!(
                "a count of {n} needs at least {} bytes but {} are left",
                n.saturating_mul(each.max(1)),
                self.left()
            )));
        }
        Ok(())
    }

    fn string(&mut self) -> R<Vec<u8>> {
        let n = self.count(1)?;
        Ok(self.take(n)?.to_vec())
    }

    fn text(&mut self) -> R<String> {
        let b = self.string()?;
        String::from_utf8(b).map_err(|_| self.err("a name is not UTF-8"))
    }

    /// `n` interleaved big-endian words of `W` bytes.
    fn interleaved<const W: usize>(&mut self, n: usize) -> R<Vec<[u8; W]>> {
        self.check(n, W)?;
        let raw = self.take(n * W)?;
        Ok((0..n)
            .map(|i| std::array::from_fn(|b| raw[b * n + i]))
            .collect())
    }

    fn i32s(&mut self, n: usize) -> R<Vec<i32>> {
        Ok(self
            .interleaved::<4>(n)?
            .into_iter()
            .map(|w| unzigzag32(u32::from_be_bytes(w)))
            .collect())
    }

    fn u32s(&mut self, n: usize) -> R<Vec<u32>> {
        Ok(self
            .interleaved::<4>(n)?
            .into_iter()
            .map(u32::from_be_bytes)
            .collect())
    }

    fn i64s(&mut self, n: usize) -> R<Vec<i64>> {
        Ok(self
            .interleaved::<8>(n)?
            .into_iter()
            .map(|w| unzigzag64(u64::from_be_bytes(w)))
            .collect())
    }

    fn f32s(&mut self, n: usize) -> R<Vec<f32>> {
        Ok(self
            .interleaved::<4>(n)?
            .into_iter()
            .map(|w| {
                let v = u32::from_be_bytes(w);
                f32::from_bits(v.rotate_right(1))
            })
            .collect())
    }

    /// Referents: interleaved integers stored as running differences.
    fn referents(&mut self, n: usize) -> R<Vec<i32>> {
        let mut acc = 0i32;
        Ok(self
            .i32s(n)?
            .into_iter()
            .map(|d| {
                acc = acc.wrapping_add(d);
                acc
            })
            .collect())
    }
}

fn unzigzag32(v: u32) -> i32 {
    ((v >> 1) as i32) ^ -((v & 1) as i32)
}

fn unzigzag64(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// One class's instances, from its `INST` chunk.
struct Class {
    name: String,
    /// Indices into [`Dom::instances`].
    instances: Vec<usize>,
}

/// Reads a binary file. `file` names it in messages.
pub fn read(bytes: &[u8], file: &str) -> R<Dom> {
    let header = At {
        file: file.to_string(),
        chunk: "header".into(),
        offset: 0,
    };
    let mut c = Cursor {
        bytes,
        pos: 0,
        at: &header,
    };
    if c.take(14).ok() != Some(&MAGIC[..]) {
        return Err(header.error("it does not start with the binary format's magic bytes"));
    }
    let version = c.u16()?;
    if version != 0 {
        return Err(header.error(format!("format version {version} is not 0")));
    }
    let class_count = c.u32()? as usize;
    let instance_count = c.u32()? as usize;
    c.take(8)?;
    if instance_count > MAX_INSTANCES {
        return Err(header.error(format!(
            "it declares {instance_count} instances; the most the importer reads is {MAX_INSTANCES}"
        )));
    }
    // Every class needs an INST chunk of at least 16 + 13 bytes.
    if class_count.saturating_mul(29) > bytes.len() {
        return Err(header.error(format!(
            "it declares {class_count} classes, more than its size allows"
        )));
    }

    let mut dom = Dom::default();
    let mut classes: HashMap<u32, Class> = HashMap::new();
    let mut by_referent: HashMap<i32, usize> = HashMap::new();
    let mut shared: Vec<Vec<u8>> = Vec::new();
    // Referent-valued properties, resolved once every instance is known.
    let mut pending_refs: Vec<(usize, usize, i32)> = Vec::new();
    let mut parents: Option<Vec<Option<usize>>> = None;
    let mut pos = c.pos;
    let mut ended = false;
    while pos < bytes.len() {
        let head_at = At {
            file: file.to_string(),
            chunk: "header of the next chunk".into(),
            offset: pos,
        };
        let mut hc = Cursor {
            bytes,
            pos,
            at: &head_at,
        };
        let name_bytes = hc.take(4)?;
        let name: String = name_bytes
            .iter()
            .take_while(|&&b| b != 0)
            .map(|&b| b as char)
            .collect();
        let compressed = hc.u32()? as usize;
        let size = hc.u32()? as usize;
        hc.take(4)?;
        let at = At {
            file: file.to_string(),
            chunk: name.clone(),
            offset: pos,
        };
        if size > MAX_CHUNK {
            return Err(at.error(format!(
                "the chunk expands to {size} bytes; the most the importer reads is {MAX_CHUNK}"
            )));
        }
        let stored = if compressed == 0 { size } else { compressed };
        let body_start = hc.pos;
        if stored > bytes.len() - body_start {
            return Err(at.error(format!(
                "the chunk says it holds {stored} bytes but {} are left",
                bytes.len() - body_start
            )));
        }
        let raw = &bytes[body_start..body_start + stored];
        pos = body_start + stored;
        let body = if compressed == 0 {
            raw.to_vec()
        } else {
            decompress(raw, size).map_err(|m| at.error(m))?
        };
        let body_at = At {
            offset: 0,
            ..at.clone()
        };
        let mut b = Cursor {
            bytes: &body,
            pos: 0,
            at: &body_at,
        };
        match name.as_str() {
            "META" => {
                let n = b.count(8)?;
                for _ in 0..n {
                    let k = String::from_utf8_lossy(&b.string()?).into_owned();
                    let v = String::from_utf8_lossy(&b.string()?).into_owned();
                    dom.meta.push((k, v));
                }
            }
            "SSTR" => {
                let _version = b.u32()?;
                let n = b.count(20)?;
                shared.reserve(n);
                for _ in 0..n {
                    b.take(16)?; // the hash, which we do not need
                    shared.push(b.string()?);
                }
            }
            "INST" => {
                let id = b.u32()?;
                let class = b.text()?;
                let service = b.u8()? != 0;
                let n = b.count(4)?;
                if dom.instances.len() + n > instance_count {
                    return Err(b.err(format!(
                        "class {class} has {n} more instances than the header declares"
                    )));
                }
                let refs = b.referents(n)?;
                if service {
                    b.take(n)?;
                }
                let mut list = Vec::with_capacity(n);
                for r in refs {
                    let i = dom.instances.len();
                    if by_referent.insert(r, i).is_some() {
                        return Err(b.err(format!("referent {r} is used twice")));
                    }
                    dom.instances.push(Instance {
                        class: class.clone(),
                        ..Instance::default()
                    });
                    list.push(i);
                }
                if classes
                    .insert(
                        id,
                        Class {
                            name: class.clone(),
                            instances: list,
                        },
                    )
                    .is_some()
                {
                    return Err(b.err(format!("class id {id} is declared twice")));
                }
            }
            "PROP" => {
                let id = b.u32()?;
                let prop = b.text()?;
                let ty = b.u8()?;
                let Some(class) = classes.get(&id) else {
                    return Err(b.err(format!(
                        "property {prop} belongs to class id {id}, which no INST chunk declares"
                    )));
                };
                let n = class.instances.len();
                match values(&mut b, ty, n, &shared)? {
                    Some(Values::Plain(vs)) => {
                        for (i, v) in class.instances.iter().zip(vs) {
                            dom.instances[*i].properties.push((prop.clone(), v));
                        }
                    }
                    Some(Values::Refs(rs)) => {
                        for (i, r) in class.instances.iter().zip(rs) {
                            let k = dom.instances[*i].properties.len();
                            dom.instances[*i]
                                .properties
                                .push((prop.clone(), PlaceValue::Ref(None)));
                            pending_refs.push((*i, k, r));
                        }
                    }
                    None => dom.warnings.push(
                        Diagnostic::warning(
                            codes::PLACE_VALUE_TYPE,
                            format!(
                                "{file}: {}.{prop} has value type 0x{ty:02x}, which the importer does not know; the property is skipped",
                                class.name
                            ),
                        )
                        .at(re_core::Location::file(file.to_string())),
                    ),
                }
            }
            "PRNT" => {
                let _version = b.u8()?;
                let n = b.count(8)?;
                let children = b.referents(n)?;
                let of = b.referents(n)?;
                let mut list = vec![None; dom.instances.len()];
                for (ch, p) in children.into_iter().zip(of) {
                    let Some(&ci) = by_referent.get(&ch) else {
                        return Err(
                            b.err(format!("PRNT names referent {ch}, which no instance has"))
                        );
                    };
                    list[ci] = if p < 0 {
                        None
                    } else {
                        match by_referent.get(&p) {
                            Some(&pi) => Some(pi),
                            None => {
                                return Err(b.err(format!(
                                    "the parent of referent {ch} is referent {p}, which no instance has"
                                )));
                            }
                        }
                    };
                }
                parents = Some(list);
            }
            "END" => {
                ended = true;
                break;
            }
            other => dom.warnings.push(
                Diagnostic::warning(
                    codes::PLACE_VALUE_TYPE,
                    format!("{file}: skipped chunk {other:?}, which the importer does not know"),
                )
                .at(re_core::Location::file(file.to_string())),
            ),
        }
    }
    if !ended {
        return Err(At {
            file: file.to_string(),
            chunk: "END".into(),
            offset: bytes.len(),
        }
        .error("the file ends before its END chunk"));
    }
    for (i, k, r) in pending_refs {
        let target = if r < 0 {
            None
        } else {
            by_referent.get(&r).copied()
        };
        dom.instances[i].properties[k].1 = PlaceValue::Ref(target);
    }
    let parents = parents.unwrap_or_else(|| vec![None; dom.instances.len()]);
    check_ancestry(&parents).map_err(|e| {
        let at = At {
            file: file.to_string(),
            chunk: "PRNT".into(),
            offset: 0,
        };
        match e {
            Ancestry::Cycle(i) => at.error(format!(
                "instance {i} ({}) is its own ancestor",
                dom.instances[i].class
            )),
            Ancestry::TooDeep(i) => at.error(format!(
                "instance {i} ({}) nests deeper than {MAX_DEPTH} levels",
                dom.instances[i].class
            )),
        }
    })?;
    dom.link(&parents);
    Ok(dom)
}

/// What is wrong with a file's parent links.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Ancestry {
    /// This instance is its own ancestor.
    Cycle(usize),
    /// This instance lies more than [`MAX_DEPTH`] levels below its root.
    TooDeep(usize),
}

/// Checks that the parent links form trees at most [`MAX_DEPTH`] levels deep.
/// The import walks them recursively, so a deeper chain would overflow the
/// stack, an abort with no crash report, instead of failing with `RE9809`.
pub(crate) fn check_ancestry(parents: &[Option<usize>]) -> Result<(), Ancestry> {
    // 0 unvisited, 1 on the current walk, 2 depth known.
    let mut state = vec![0u8; parents.len()];
    let mut depth = vec![0usize; parents.len()];
    for start in 0..parents.len() {
        let mut walk = Vec::new();
        let mut at = Some(start);
        // The depth of the walk's topmost instance: 0 for a root, else one
        // below the first ancestor whose depth is known.
        let mut top = 0;
        while let Some(i) = at {
            match state[i] {
                2 => {
                    top = depth[i] + 1;
                    break;
                }
                1 => return Err(Ancestry::Cycle(i)),
                _ => {}
            }
            state[i] = 1;
            walk.push(i);
            at = parents[i];
        }
        for (k, &i) in walk.iter().rev().enumerate() {
            depth[i] = top + k;
            if depth[i] >= MAX_DEPTH {
                return Err(Ancestry::TooDeep(i));
            }
            state[i] = 2;
        }
    }
    Ok(())
}

fn decompress(raw: &[u8], size: usize) -> Result<Vec<u8>, String> {
    if raw.starts_with(&ZSTD_MAGIC) {
        let mut out = Vec::with_capacity(size.min(MAX_CHUNK));
        let mut decoder = ruzstd::decoding::StreamingDecoder::new(raw)
            .map_err(|e| format!("the ZSTD frame does not read: {e}"))?;
        use std::io::Read;
        (&mut decoder)
            .take(size as u64 + 1)
            .read_to_end(&mut out)
            .map_err(|e| format!("the ZSTD frame does not read: {e}"))?;
        if out.len() != size {
            return Err(format!(
                "the ZSTD frame expands to {} bytes, not the {size} its header says",
                out.len()
            ));
        }
        Ok(out)
    } else {
        let out = lz4_flex::block::decompress(raw, size)
            .map_err(|e| format!("the LZ4 block does not read: {e}"))?;
        if out.len() != size {
            return Err(format!(
                "the LZ4 block expands to {} bytes, not the {size} its header says",
                out.len()
            ));
        }
        Ok(out)
    }
}

enum Values {
    Plain(Vec<PlaceValue>),
    /// Referents, resolved once every instance is known.
    Refs(Vec<i32>),
}

/// `n` values of type `ty`, or `None` for a type id the reader does not know.
fn values(b: &mut Cursor<'_>, ty: u8, n: usize, shared: &[Vec<u8>]) -> R<Option<Values>> {
    use PlaceValue as V;
    let plain = |v: Vec<V>| Ok(Some(Values::Plain(v)));
    match ty {
        0x01 => plain(
            (0..n)
                .map(|_| b.string().map(V::String))
                .collect::<R<_>>()?,
        ),
        0x02 => plain(b.take(n)?.iter().map(|&x| V::Bool(x != 0)).collect()),
        0x03 => plain(b.i32s(n)?.into_iter().map(V::Int32).collect()),
        0x04 => plain(b.f32s(n)?.into_iter().map(V::Float32).collect()),
        0x05 => {
            b.check(n, 8)?;
            plain((0..n).map(|_| b.f64().map(V::Float64)).collect::<R<_>>()?)
        }
        0x06 => {
            let s = b.f32s(n)?;
            let o = b.i32s(n)?;
            plain(s.into_iter().zip(o).map(|(s, o)| V::UDim(s, o)).collect())
        }
        0x07 => {
            let (xs, ys) = (b.f32s(n)?, b.f32s(n)?);
            let (xo, yo) = (b.i32s(n)?, b.i32s(n)?);
            plain(
                (0..n)
                    .map(|i| V::UDim2(xs[i], xo[i], ys[i], yo[i]))
                    .collect(),
            )
        }
        0x08 => {
            b.check(n, 24)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let mut f = [0f32; 6];
                for x in &mut f {
                    *x = b.f32()?;
                }
                out.push(V::Ray {
                    origin: [f[0], f[1], f[2]],
                    direction: [f[3], f[4], f[5]],
                });
            }
            plain(out)
        }
        0x09 => plain(b.take(n)?.iter().map(|&x| V::Faces(x)).collect()),
        0x0A => plain(b.take(n)?.iter().map(|&x| V::Axes(x)).collect()),
        0x0B => plain(b.u32s(n)?.into_iter().map(V::BrickColor).collect()),
        0x0C => {
            let (r, g, bl) = (b.f32s(n)?, b.f32s(n)?, b.f32s(n)?);
            plain((0..n).map(|i| V::Color3([r[i], g[i], bl[i]])).collect())
        }
        0x0D => {
            let (x, y) = (b.f32s(n)?, b.f32s(n)?);
            plain((0..n).map(|i| V::Vector2([x[i], y[i]])).collect())
        }
        0x0E => {
            let (x, y, z) = (b.f32s(n)?, b.f32s(n)?, b.f32s(n)?);
            plain((0..n).map(|i| V::Vector3([x[i], y[i], z[i]])).collect())
        }
        0x10 => plain(cframes(b, n)?.into_iter().map(V::CFrame).collect()),
        0x12 => plain(b.u32s(n)?.into_iter().map(V::Enum).collect()),
        0x13 => Ok(Some(Values::Refs(b.referents(n)?))),
        0x14 => {
            b.check(n, 6)?;
            let mut out = Vec::with_capacity(n);
            for _ in 0..n {
                let mut v = [0i16; 3];
                for x in &mut v {
                    *x = b.u16()? as i16;
                }
                out.push(V::Vector3int16(v));
            }
            plain(out)
        }
        0x15 => {
            let mut out = Vec::with_capacity(n.min(b.left()));
            for _ in 0..n {
                let k = b.count(12)?;
                let mut keys = Vec::with_capacity(k);
                for _ in 0..k {
                    keys.push([b.f32()?, b.f32()?, b.f32()?]);
                }
                out.push(V::NumberSequence(keys));
            }
            plain(out)
        }
        0x16 => {
            let mut out = Vec::with_capacity(n.min(b.left()));
            for _ in 0..n {
                let k = b.count(20)?;
                let mut keys = Vec::with_capacity(k);
                for _ in 0..k {
                    let t = b.f32()?;
                    let c = [b.f32()?, b.f32()?, b.f32()?];
                    let _envelope = b.f32()?;
                    keys.push((t, c));
                }
                out.push(V::ColorSequence(keys));
            }
            plain(out)
        }
        0x17 => {
            b.check(n, 8)?;
            plain(
                (0..n)
                    .map(|_| Ok(V::NumberRange(b.f32()?, b.f32()?)))
                    .collect::<R<_>>()?,
            )
        }
        0x18 => {
            let (a, c, d, e) = (b.f32s(n)?, b.f32s(n)?, b.f32s(n)?, b.f32s(n)?);
            plain((0..n).map(|i| V::Rect([a[i], c[i], d[i], e[i]])).collect())
        }
        0x19 => {
            let mut out = Vec::with_capacity(n.min(b.left()));
            for _ in 0..n {
                let flag = b.u8()?;
                out.push(V::PhysicalProperties(match flag {
                    0 => None,
                    _ => {
                        let mut p = [0f32; 6];
                        for x in p.iter_mut().take(5) {
                            *x = b.f32()?;
                        }
                        // A flag with bit 1 set carries acoustic absorption too.
                        p[5] = if flag & 2 != 0 { b.f32()? } else { 1.0 };
                        Some(p)
                    }
                }));
            }
            plain(out)
        }
        0x1A => {
            let r = b.take(n)?.to_vec();
            let g = b.take(n)?.to_vec();
            let bl = b.take(n)?;
            plain(
                (0..n)
                    .map(|i| V::Color3uint8([r[i], g[i], bl[i]]))
                    .collect(),
            )
        }
        0x1B => plain(b.i64s(n)?.into_iter().map(V::Int64).collect()),
        0x1C => {
            let idx = b.u32s(n)?;
            let mut out = Vec::with_capacity(n);
            for i in idx {
                match shared.get(i as usize) {
                    Some(s) => out.push(V::SharedString(s.clone())),
                    None => {
                        return Err(b.err(format!(
                            "shared string {i} does not exist ({} are declared)",
                            shared.len()
                        )));
                    }
                }
            }
            plain(out)
        }
        0x1D => plain(
            (0..n)
                .map(|_| b.string().map(V::Bytecode))
                .collect::<R<_>>()?,
        ),
        0x1E => {
            let t = b.u8()?;
            if t != 0x10 {
                return Err(b.err(format!(
                    "an optional CFrame column starts with type 0x{t:02x}, not 0x10"
                )));
            }
            let cfs = cframes(b, n)?;
            let t = b.u8()?;
            if t != 0x02 {
                return Err(b.err(format!(
                    "an optional CFrame column's flags have type 0x{t:02x}, not 0x02"
                )));
            }
            let flags = b.take(n)?;
            plain(
                cfs.into_iter()
                    .zip(flags)
                    .map(|(c, &f)| V::OptionalCFrame((f != 0).then_some(c)))
                    .collect(),
            )
        }
        0x1F => plain(
            b.interleaved::<16>(n)?
                .into_iter()
                .map(V::UniqueId)
                .collect(),
        ),
        0x20 => {
            let mut out = Vec::with_capacity(n.min(b.left()));
            for _ in 0..n {
                let family = String::from_utf8_lossy(&b.string()?).into_owned();
                let weight = b.u16()?;
                let style = match b.u8()? {
                    1 => "Italic",
                    _ => "Normal",
                }
                .to_string();
                let cached_face = String::from_utf8_lossy(&b.string()?).into_owned();
                out.push(V::Font(Font {
                    family,
                    weight,
                    style,
                    cached_face,
                }));
            }
            plain(out)
        }
        0x21 => plain(
            b.i64s(n)?
                .into_iter()
                .map(|v| V::SecurityCapabilities(v as u64))
                .collect(),
        ),
        0x22 => {
            // A source kind per value (0 none, 1 URI, 2 object, 3 external),
            // then the URIs and the object referents in value order.
            let kinds = b.i32s(n)?;
            let uris = b.count(4)?;
            let mut list = Vec::with_capacity(uris);
            for _ in 0..uris {
                list.push(String::from_utf8_lossy(&b.string()?).into_owned());
            }
            let mut next = list.into_iter();
            let out = kinds
                .into_iter()
                .map(|k| V::Content(if k == 1 { next.next() } else { None }))
                .collect();
            // Object and external sources are not content we can import;
            // the rest of the column is skipped.
            b.pos = b.bytes.len();
            plain(out)
        }
        _ => Ok(None),
    }
}

/// `n` CFrames: a rotation id per value (nine floats follow an id of 0),
/// then all positions as one interleaved `Vector3` column.
fn cframes(b: &mut Cursor<'_>, n: usize) -> R<Vec<CFrame>> {
    b.check(n, 13)?;
    let mut rots = Vec::with_capacity(n);
    for _ in 0..n {
        let id = b.u8()?;
        if id == 0 {
            let mut r = [0f32; 9];
            for x in &mut r {
                *x = b.f32()?;
            }
            rots.push(r);
        } else {
            match rotation_of_id(id) {
                Some(r) => rots.push(r),
                None => return Err(b.err(format!("0x{id:02x} is not a rotation id"))),
            }
        }
    }
    let (x, y, z) = (b.f32s(n)?, b.f32s(n)?, b.f32s(n)?);
    Ok(rots
        .into_iter()
        .enumerate()
        .map(|(i, rotation)| CFrame {
            position: [x[i], y[i], z[i]],
            rotation,
        })
        .collect())
}
