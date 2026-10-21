//! Values across the generator boundary (ADR-0043 §4): fields, points and
//! meshes as `buffer`s, fragments as tables. Everything a script returns is
//! checked; a problem is `RE4103` naming what was wrong.
//!
//! * field: `{ n, spacing, offset: vector, samples: buffer }`, `n * n` f32
//!   samples row by row (`samples[z * n + x]`). `offset` (the world position
//!   of sample (0, 0)) may be left out of an output: the field is centred.
//! * points: `{ count, data: buffer, tags: { string } }`, 28-byte records of
//!   u32 id and f32 position x, y, z and normal x, y, z. `tags` may be left
//!   out of an output (all empty).
//! * mesh: `{ positions, normals, uvs, colors, indices: buffer, material }`:
//!   f32 x 3, f32 x 3, f32 x 2, f32 x 4 per vertex and u32 indices, three
//!   per triangle. An output needs `positions` and `indices`; normals default
//!   to +Y, uvs to 0 and colours to white.
//! * fragment (output only): `{ items = { { key, name?, parent?, mesh?,
//!   components? } } }`. Keys are local to the node (the entity's key is
//!   `<node id>/<key>`); `parent` names an earlier item; `mesh` (a mesh
//!   table) makes the item draw it; `components` is `{ Name = { field =
//!   value } }`, converted as attribute values are.

use std::sync::Arc;

use mlua::{Buffer, Lua, Table, Value, Vector};
use re_proc::{Field, FragItem, Fragment, MeshValue, Output, Point, Points, PortType};
use re_types::glam::Vec3;
use re_types::mesh::{MeshData, Vertex};

const POINT_BYTES: usize = 28;

fn f32s(bytes: &[u8]) -> impl Iterator<Item = f32> + '_ {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(*c))
}

fn bytes_of(values: impl IntoIterator<Item = f32>) -> Vec<u8> {
    values.into_iter().flat_map(f32::to_le_bytes).collect()
}

/// True if every number in `v` is finite.
fn finite_value(v: &re_reflect::Value) -> bool {
    use re_reflect::Value as V;
    match v {
        V::Float(f) => f.is_finite(),
        V::Vec3(x) => x.is_finite(),
        V::Quat(q) => q.is_finite(),
        V::Color(c) => [c.r, c.g, c.b, c.a].iter().all(|x| x.is_finite()),
        V::List(items) | V::Record(_, items) => items.iter().all(finite_value),
        V::Map(m) => m.values().all(finite_value),
        V::Bool(_) | V::Int(_) | V::Str(_) | V::Entity(_) | V::Enum(_) => true,
    }
}

/// An input value as Luau sees it.
pub(crate) fn to_lua(lua: &Lua, output: &Output) -> mlua::Result<Value> {
    let t = lua.create_table()?;
    match output {
        Output::Field(f) => {
            let h = &f.0;
            t.raw_set("n", h.n)?;
            t.raw_set("spacing", h.spacing)?;
            t.raw_set("offset", Vector::new(h.offset.x, h.offset.y, h.offset.z))?;
            t.raw_set(
                "samples",
                lua.create_buffer(bytes_of(h.samples.iter().copied()))?,
            )?;
        }
        Output::Points(p) => {
            let mut data = Vec::with_capacity(p.points.len() * POINT_BYTES);
            let tags = lua.create_table()?;
            for (i, pt) in p.points.iter().enumerate() {
                data.extend_from_slice(&pt.id.to_le_bytes());
                data.extend(bytes_of(pt.position.to_array()));
                data.extend(bytes_of(pt.normal.to_array()));
                tags.raw_set(i + 1, pt.tag.as_str())?;
            }
            t.raw_set("count", p.points.len())?;
            t.raw_set("data", lua.create_buffer(data)?)?;
            t.raw_set("tags", tags)?;
        }
        Output::Mesh(m) => {
            let v = &m.mesh.vertices;
            t.raw_set(
                "positions",
                lua.create_buffer(bytes_of(v.iter().flat_map(|v| v.pos)))?,
            )?;
            t.raw_set(
                "normals",
                lua.create_buffer(bytes_of(v.iter().flat_map(|v| v.normal)))?,
            )?;
            t.raw_set(
                "uvs",
                lua.create_buffer(bytes_of(v.iter().flat_map(|v| v.uv)))?,
            )?;
            t.raw_set(
                "colors",
                lua.create_buffer(bytes_of(v.iter().flat_map(|v| v.color)))?,
            )?;
            let indices: Vec<u8> = m
                .mesh
                .indices
                .iter()
                .flat_map(|i| i.to_le_bytes())
                .collect();
            t.raw_set("indices", lua.create_buffer(indices)?)?;
            t.raw_set("vertices", v.len())?;
            t.raw_set("triangles", m.mesh.triangle_count())?;
            t.raw_set("material", m.material.as_str())?;
        }
        Output::Fragment(_) => {
            return Err(mlua::Error::runtime(
                "fragments are not passed to Luau nodes",
            ));
        }
    }
    Ok(Value::Table(t))
}

fn table(v: Value, what: &str) -> Result<Table, String> {
    match v {
        Value::Table(t) => Ok(t),
        other => Err(format!(
            "{what}: expected a table, found {}",
            other.type_name()
        )),
    }
}

fn buffer(t: &Table, key: &str, what: &str) -> Result<Option<Vec<u8>>, String> {
    match t.raw_get::<Value>(key).map_err(|e| e.to_string())? {
        Value::Nil => Ok(None),
        Value::Buffer(b) => Ok(Some(Buffer::to_vec(&b))),
        other => Err(format!(
            "{what}.{key}: expected a buffer, found {}",
            other.type_name()
        )),
    }
}

fn need<T>(v: Option<T>, what: &str, key: &str) -> Result<T, String> {
    v.ok_or_else(|| format!("{what}.{key} is missing"))
}

fn number(t: &Table, key: &str, what: &str) -> Result<Option<f64>, String> {
    match t.raw_get::<Value>(key).map_err(|e| e.to_string())? {
        Value::Nil => Ok(None),
        Value::Integer(i) => Ok(Some(crate::lua_int(i) as f64)),
        Value::Number(n) => Ok(Some(n)),
        other => Err(format!(
            "{what}.{key}: expected a number, found {}",
            other.type_name()
        )),
    }
}

fn finite(values: &[f32], what: &str) -> Result<(), String> {
    match values.iter().position(|x| !x.is_finite()) {
        Some(i) => Err(format!("{what}: value {i} is not finite ({})", values[i])),
        None => Ok(()),
    }
}

/// The output a script returned, as `ty`. `key` is the node's key (the
/// derived asset names of its meshes come from it) and `node` its id.
pub(crate) fn from_lua(
    v: Value,
    ty: PortType,
    node: &str,
    key: &[u8; 32],
) -> Result<Output, String> {
    Ok(match ty {
        PortType::Field => Output::Field(Arc::new(field(&table(v, "field")?)?)),
        PortType::Points => Output::Points(Arc::new(points(&table(v, "points")?)?)),
        PortType::Mesh => Output::Mesh(Arc::new(mesh_value(&table(v, "mesh")?, "mesh", *key)?)),
        PortType::Fragment => {
            Output::Fragment(Arc::new(fragment(&table(v, "fragment")?, node, key)?))
        }
    })
}

fn field(t: &Table) -> Result<Field, String> {
    let n = need(number(t, "n", "field")?, "field", "n")?;
    if n.fract() != 0.0 || !(2.0..=16_384.0).contains(&n) {
        return Err(format!(
            "field.n: expected a whole number from 2 to 16384, found {n}"
        ));
    }
    let n = n as u32;
    let spacing = need(number(t, "spacing", "field")?, "field", "spacing")? as f32;
    if !(spacing.is_finite() && spacing > 0.0) {
        return Err(format!(
            "field.spacing: expected a positive number, found {spacing}"
        ));
    }
    let bytes = need(buffer(t, "samples", "field")?, "field", "samples")?;
    let samples: Vec<f32> = f32s(&bytes).collect();
    let want = (n as usize) * (n as usize);
    if bytes.len() != want * 4 {
        return Err(format!(
            "field.samples: {} bytes ({} samples), expected {} samples (n = {n})",
            bytes.len(),
            bytes.len() / 4,
            want
        ));
    }
    finite(&samples, "field.samples")?;
    match t.raw_get::<Value>("offset").map_err(|e| e.to_string())? {
        Value::Nil => Field::new(n, spacing, samples),
        Value::Vector(o) => {
            let offset = Vec3::new(o.x(), o.y(), o.z());
            if !offset.is_finite() {
                return Err("field.offset is not finite".into());
            }
            re_geom::Heightfield::new(n, spacing, offset, samples)
                .map(Field)
                .map_err(|e| e.to_string())
        }
        other => Err(format!(
            "field.offset: expected a vector, found {}",
            other.type_name()
        )),
    }
}

fn points(t: &Table) -> Result<Points, String> {
    let bytes = need(buffer(t, "data", "points")?, "points", "data")?;
    if !bytes.len().is_multiple_of(POINT_BYTES) {
        return Err(format!(
            "points.data: {} bytes is not a whole number of {POINT_BYTES}-byte records",
            bytes.len()
        ));
    }
    let count = bytes.len() / POINT_BYTES;
    let tags: Option<Table> = match t.raw_get::<Value>("tags").map_err(|e| e.to_string())? {
        Value::Nil => None,
        Value::Table(tags) => {
            if tags.raw_len() != count {
                return Err(format!(
                    "points.tags: {} tags for {count} points",
                    tags.raw_len()
                ));
            }
            Some(tags)
        }
        other => {
            return Err(format!(
                "points.tags: expected a table of strings, found {}",
                other.type_name()
            ));
        }
    };
    let mut out = Vec::with_capacity(count);
    for (i, rec) in bytes.as_chunks::<POINT_BYTES>().0.iter().enumerate() {
        let id = u32::from_le_bytes([rec[0], rec[1], rec[2], rec[3]]);
        let v: Vec<f32> = f32s(&rec[4..]).collect();
        finite(&v, &format!("points.data record {i}"))?;
        let tag = match &tags {
            None => String::new(),
            Some(tags) => match tags.raw_get::<Value>(i + 1).map_err(|e| e.to_string())? {
                Value::String(s) => s.to_str().map_err(|e| e.to_string())?.to_string(),
                other => {
                    return Err(format!(
                        "points.tags[{}]: expected a string, found {}",
                        i + 1,
                        other.type_name()
                    ));
                }
            },
        };
        out.push(Point {
            id,
            position: Vec3::new(v[0], v[1], v[2]),
            normal: Vec3::new(v[3], v[4], v[5]),
            tag,
        });
    }
    Ok(Points { points: out })
}

fn mesh_value(t: &Table, what: &str, asset: [u8; 32]) -> Result<MeshValue, String> {
    let material = match t.raw_get::<Value>("material").map_err(|e| e.to_string())? {
        Value::Nil => String::new(),
        Value::String(s) => s.to_str().map_err(|e| e.to_string())?.to_string(),
        other => {
            return Err(format!(
                "{what}.material: expected a string, found {}",
                other.type_name()
            ));
        }
    };
    Ok(MeshValue {
        mesh: mesh(t, what)?,
        material,
        asset,
    })
}

fn mesh(t: &Table, what: &str) -> Result<MeshData, String> {
    let pos: Vec<f32> = f32s(&need(buffer(t, "positions", what)?, what, "positions")?).collect();
    if !pos.len().is_multiple_of(3) || pos.is_empty() {
        return Err(format!(
            "{what}.positions: {} floats is not a non-empty whole number of vertices (3 floats each)",
            pos.len()
        ));
    }
    finite(&pos, &format!("{what}.positions"))?;
    let n = pos.len() / 3;
    let per = |key: &str, width: usize| -> Result<Option<Vec<f32>>, String> {
        let Some(bytes) = buffer(t, key, what)? else {
            return Ok(None);
        };
        let v: Vec<f32> = f32s(&bytes).collect();
        if bytes.len() != n * width * 4 {
            return Err(format!(
                "{what}.{key}: {} floats, expected {} ({width} per vertex for {n} vertices)",
                bytes.len() / 4,
                n * width
            ));
        }
        finite(&v, &format!("{what}.{key}"))?;
        Ok(Some(v))
    };
    let normals = per("normals", 3)?;
    let uvs = per("uvs", 2)?;
    let colors = per("colors", 4)?;
    let ibytes = need(buffer(t, "indices", what)?, what, "indices")?;
    if !ibytes.len().is_multiple_of(4) {
        return Err(format!(
            "{what}.indices: {} bytes is not whole u32s",
            ibytes.len()
        ));
    }
    let indices: Vec<u32> = ibytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_le_bytes(*c))
        .collect();
    let vertices = (0..n)
        .map(|i| Vertex {
            pos: [pos[i * 3], pos[i * 3 + 1], pos[i * 3 + 2]],
            normal: normals
                .as_ref()
                .map_or([0.0, 1.0, 0.0], |v| [v[i * 3], v[i * 3 + 1], v[i * 3 + 2]]),
            uv: uvs
                .as_ref()
                .map_or([0.0, 0.0], |v| [v[i * 2], v[i * 2 + 1]]),
            color: colors.as_ref().map_or([1.0; 4], |v| {
                [v[i * 4], v[i * 4 + 1], v[i * 4 + 2], v[i * 4 + 3]]
            }),
        })
        .collect();
    let mesh = MeshData { vertices, indices };
    if mesh.indices.is_empty() || !mesh.is_well_formed() {
        return Err(format!(
            "{what}.indices: {} indices do not make whole triangles of the {n} vertices",
            mesh.indices.len()
        ));
    }
    Ok(mesh)
}

fn string(v: Value, what: &str) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.to_str().map_err(|e| e.to_string())?.to_string()),
        other => Err(format!(
            "{what}: expected a string, found {}",
            other.type_name()
        )),
    }
}

/// A local key: not empty, no leading `/`, no reserved first character
/// (`+` and `~` are kept for structural overrides, ADR-0043 §3).
fn local_key(key: &str, what: &str) -> Result<(), String> {
    if key.is_empty() || key.starts_with('/') || key.ends_with('/') || key.contains("//") {
        return Err(format!(
            "{what}: `{key}` is not a key (non-empty parts separated by `/`)"
        ));
    }
    if key.starts_with(['+', '~']) {
        return Err(format!(
            "{what}: keys may not start with `+` or `~` (`{key}`)"
        ));
    }
    Ok(())
}

fn fragment(t: &Table, node: &str, key: &[u8; 32]) -> Result<Fragment, String> {
    let items: Table = match t.raw_get::<Value>("items").map_err(|e| e.to_string())? {
        Value::Table(items) => items,
        Value::Nil => return Ok(Fragment::default()),
        other => {
            return Err(format!(
                "fragment.items: expected a table, found {}",
                other.type_name()
            ));
        }
    };
    let mut out = Fragment::default();
    let mut seen = std::collections::HashSet::new();
    for i in 1..=items.raw_len() {
        let what = format!("fragment.items[{i}]");
        let item = table(items.raw_get::<Value>(i).map_err(|e| e.to_string())?, &what)?;
        let local = string(
            item.raw_get::<Value>("key").map_err(|e| e.to_string())?,
            &format!("{what}.key"),
        )?;
        local_key(&local, &format!("{what}.key"))?;
        if !seen.insert(local.clone()) {
            return Err(format!("{what}.key: `{local}` is used twice"));
        }
        let full = format!("{node}/{local}");
        let name = match item.raw_get::<Value>("name").map_err(|e| e.to_string())? {
            Value::Nil => local.rsplit('/').next().unwrap_or(&local).to_string(),
            v => string(v, &format!("{what}.name"))?,
        };
        let parent_key = match item.raw_get::<Value>("parent").map_err(|e| e.to_string())? {
            Value::Nil => None,
            v => {
                let p = string(v, &format!("{what}.parent"))?;
                if !seen.contains(&p) || p == local {
                    return Err(format!(
                        "{what}.parent: `{p}` is not the key of an earlier item"
                    ));
                }
                Some(format!("{node}/{p}"))
            }
        };
        let mut built = match item.raw_get::<Value>("mesh").map_err(|e| e.to_string())? {
            Value::Nil => FragItem {
                key: full.clone(),
                name: name.clone(),
                parent_key: None,
                components: Vec::new(),
                prefab: None,
            },
            v => {
                let mut h = blake3::Hasher::new();
                h.update(b"re_script luau fragment mesh");
                h.update(key);
                h.update(full.as_bytes());
                let mv = mesh_value(
                    &table(v, &format!("{what}.mesh"))?,
                    &format!("{what}.mesh"),
                    *h.finalize().as_bytes(),
                )?;
                let (item, asset) = re_proc::nodes::mesh_item(full.clone(), name.clone(), &mv);
                if !out.assets.iter().any(|(n, _)| n == &asset.0) {
                    out.assets.push(asset);
                }
                item
            }
        };
        built.parent_key = parent_key;
        match item
            .raw_get::<Value>("components")
            .map_err(|e| e.to_string())?
        {
            Value::Nil => {}
            Value::Table(comps) => {
                let mut names = Vec::new();
                for pair in comps.pairs::<Value, Value>() {
                    let (k, v) = pair.map_err(|e| e.to_string())?;
                    let cname = string(k, &format!("{what}.components: a component name"))?;
                    names.push((cname, v));
                }
                names.sort_by(|a, b| a.0.cmp(&b.0));
                for (cname, v) in names {
                    let at = format!("{what}.components.{cname}");
                    let fields = crate::api::value::fields_from_table(&cname, table(v, &at)?)
                        .map_err(|e| format!("{at}: {e}"))?;
                    let mut values = Vec::with_capacity(fields.len());
                    for (fname, raw) in fields {
                        let value = crate::api::value::dynamic_value(raw)
                            .map_err(|m| format!("{at}.{fname}: {m}"))?;
                        if !finite_value(&value) {
                            return Err(format!("{at}.{fname}: a number is not finite"));
                        }
                        values.push((fname, value));
                    }
                    match built.components.iter_mut().find(|(n, _)| *n == cname) {
                        Some((_, existing)) => {
                            for (fname, value) in values {
                                match existing.iter_mut().find(|(f, _)| *f == fname) {
                                    Some((_, slot)) => *slot = value,
                                    None => existing.push((fname, value)),
                                }
                            }
                        }
                        None => built.components.push((cname, values)),
                    }
                }
            }
            other => {
                return Err(format!(
                    "{what}.components: expected a table, found {}",
                    other.type_name()
                ));
            }
        }
        out.items.push(built);
    }
    Ok(out)
}
