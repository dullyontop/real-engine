//! The bake format (ADR-0043 §2), frozen with node versions so other work
//! can build on it before `proc.bake` lands: the [`Baked`] provenance
//! component, the id a baked entity takes, where baked assets are written,
//! and the `.heightfield` file layout.

use glam::Vec3;
use re_geom::Heightfield;
use re_reflect::component;
use re_scene::ecs as bevy_ecs;

component! {
    /// Where a baked entity came from: the procedural source and key it was
    /// generated with, and what decided its content. Saved like any
    /// component, so everything baked from a source can be found.
    pub struct Baked {
        /// The procedural source's id.
        pub source: String = String::new(),
        /// The entity's stable key in the source's output.
        pub key: String = String::new(),
        /// The source's generator (`Procedural.generator`).
        pub generator: String = String::new(),
        /// The generator's version when baked.
        pub generator_version: i64 = 1,
        /// The source's version stamp when baked (hex).
        pub stamp: String = String::new(),
        /// The source's seed when baked.
        pub seed: i64 = 0,
        /// The hash of the source's other inputs (its parameters and graph
        /// document), hex.
        pub inputs: String = String::new(),
    }
}

/// The id a baked entity takes: `<source>.<key>` with every `/` of the key
/// replaced by `.` (authored ids may not contain `/`), then `~2`, `~3`, ...
/// until `taken` says the id is free.
pub fn baked_id(source: &str, key: &str, taken: impl Fn(&str) -> bool) -> String {
    let base = format!("{source}.{}", key.replace('/', "."));
    if !taken(&base) {
        return base;
    }
    (2u64..)
        .map(|i| format!("{base}~{i}"))
        .find(|id| !taken(id))
        .expect("some suffix is free")
}

/// The directory baked assets of `source` are written to, relative to the
/// project. The source id is encoded into a name every file system takes:
/// bytes outside `[A-Za-z0-9_-]` and every `.` become `%XX`, so `..` and
/// `a\b` stay one directory, and a Windows device name (`CON`, `aux`, `com1`)
/// gets its first letter encoded.
pub fn baked_dir(source: &str) -> String {
    let mut name = String::new();
    for b in source.bytes() {
        if b.is_ascii_alphanumeric() || b == b'_' || b == b'-' {
            name.push(b as char);
        } else {
            name.push_str(&format!("%{b:02X}"));
        }
    }
    let upper = name.to_ascii_uppercase();
    let device = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit());
    if device {
        name = format!("%{:02X}{}", name.as_bytes()[0], &name[1..]);
    }
    format!("assets/baked/{name}")
}

/// The project path of a baked asset: content-addressed by the first 16 hex
/// digits of its blake3 hash, with `extension` (`glb` or `heightfield`).
pub fn baked_asset_path(source: &str, content: &[u8], extension: &str) -> String {
    let hex = blake3::hash(content).to_hex();
    format!("{}/{}.{extension}", baked_dir(source), &hex[..16])
}

/// The first four bytes of a `.heightfield` file.
pub const HEIGHTFIELD_MAGIC: [u8; 4] = *b"REHF";

/// The `.heightfield` format version this engine writes and reads.
pub const HEIGHTFIELD_VERSION: u32 = 1;

/// A heightfield as a `.heightfield` file, all little-endian: the magic
/// `REHF`, the u32 format version (1), the u32 samples per side `n`, the f32
/// spacing, the f32 x, y, z of sample (0, 0), then the `n * n` f32 heights
/// row by row (the layout the physics heightfield takes).
pub fn write_heightfield(h: &Heightfield) -> Vec<u8> {
    let mut out = Vec::with_capacity(28 + h.samples.len() * 4);
    out.extend_from_slice(&HEIGHTFIELD_MAGIC);
    out.extend_from_slice(&HEIGHTFIELD_VERSION.to_le_bytes());
    out.extend_from_slice(&h.n.to_le_bytes());
    out.extend_from_slice(&h.spacing.to_le_bytes());
    for x in h.offset.to_array() {
        out.extend_from_slice(&x.to_le_bytes());
    }
    for s in &h.samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Reads a `.heightfield` file written by [`write_heightfield`].
///
/// # Errors
///
/// A wrong magic or version, a short or long file, or a heightfield that
/// fails [`Heightfield::validate`].
pub fn read_heightfield(bytes: &[u8]) -> Result<Heightfield, String> {
    let word = |i: usize| -> Result<[u8; 4], String> {
        bytes
            .get(i * 4..i * 4 + 4)
            .map(|b| b.try_into().expect("four bytes"))
            .ok_or_else(|| "the file is too short".to_string())
    };
    if word(0)? != HEIGHTFIELD_MAGIC {
        return Err("not a heightfield file (no `REHF` magic)".into());
    }
    let version = u32::from_le_bytes(word(1)?);
    if version != HEIGHTFIELD_VERSION {
        return Err(format!(
            "heightfield format version {version} is not supported (this engine reads {HEIGHTFIELD_VERSION})"
        ));
    }
    let n = u32::from_le_bytes(word(2)?);
    let spacing = f32::from_le_bytes(word(3)?);
    let offset = Vec3::new(
        f32::from_le_bytes(word(4)?),
        f32::from_le_bytes(word(5)?),
        f32::from_le_bytes(word(6)?),
    );
    let count = (n as usize)
        .checked_mul(n as usize)
        .ok_or("the sample count overflows")?;
    let expected = count
        .checked_mul(4)
        .and_then(|b| b.checked_add(28))
        .ok_or("the sample count overflows")?;
    if bytes.len() != expected {
        return Err(format!(
            "expected {expected} bytes for {n} x {n} samples, found {}",
            bytes.len()
        ));
    }
    let samples = (0..count)
        .map(|i| word(7 + i).map(f32::from_le_bytes))
        .collect::<Result<Vec<_>, _>>()?;
    Heightfield::new(n, spacing, offset, samples).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baked_ids_replace_slashes_and_take_a_suffix_on_a_clash() {
        assert_eq!(
            baked_id("island", "crates/7/lid", |_| false),
            "island.crates.7.lid"
        );
        let taken = ["island.ground", "island.ground~2"];
        assert_eq!(
            baked_id("island", "ground", |id| taken.contains(&id)),
            "island.ground~3"
        );
    }

    #[test]
    fn baked_dirs_are_one_portable_directory() {
        assert_eq!(baked_dir("island-2_b"), "assets/baked/island-2_b");
        assert_eq!(baked_dir(".."), "assets/baked/%2E%2E");
        assert_eq!(baked_dir("a\\b:c"), "assets/baked/a%5Cb%3Ac");
        assert_eq!(baked_dir("con"), "assets/baked/%63on");
        assert_eq!(baked_dir("LPT1"), "assets/baked/%4CPT1");
        assert_eq!(baked_dir("lpt"), "assets/baked/lpt");
        assert_ne!(baked_dir("a.b"), baked_dir("a%2Eb"));
    }

    #[test]
    fn baked_asset_paths_are_content_addressed() {
        let p = baked_asset_path("island", b"abc", "glb");
        assert!(p.starts_with("assets/baked/island/"), "{p}");
        assert!(p.ends_with(".glb"));
        assert_eq!(p.len(), "assets/baked/island/".len() + 16 + 4);
        assert_eq!(p, baked_asset_path("island", b"abc", "glb"));
        assert_ne!(p, baked_asset_path("island", b"abd", "glb"));
    }

    #[test]
    fn heightfields_round_trip_bit_for_bit() {
        let samples: Vec<f32> = (0..9).map(|i| i as f32 * 0.37 - 1.0).collect();
        let h = Heightfield::new(3, 2.5, Vec3::new(-2.5, 0.25, -2.5), samples).unwrap();
        let bytes = write_heightfield(&h);
        assert_eq!(&bytes[..4], b"REHF");
        assert_eq!(bytes.len(), 28 + 9 * 4);
        let back = read_heightfield(&bytes).unwrap();
        assert_eq!(back.n, h.n);
        assert_eq!(back.spacing.to_bits(), h.spacing.to_bits());
        assert_eq!(back.offset, h.offset);
        let bits = |s: &[f32]| s.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&back.samples), bits(&h.samples));

        assert!(read_heightfield(&bytes[..bytes.len() - 1]).is_err());
        let mut wrong = bytes.clone();
        wrong[4] = 2;
        assert!(read_heightfield(&wrong).unwrap_err().contains("version 2"));
        assert!(read_heightfield(b"nope").is_err());
        let mut huge = bytes[..28].to_vec();
        huge[8..12].copy_from_slice(&0x8000_0000u32.to_le_bytes());
        assert!(read_heightfield(&huge).is_err());
    }

    #[test]
    fn the_baked_component_reflects_its_fields() {
        let names: Vec<_> = <Baked as re_reflect::Reflect>::schema()
            .fields
            .iter()
            .map(|f| f.name.to_string())
            .collect();
        assert_eq!(
            names,
            [
                "source",
                "key",
                "generator",
                "generator_version",
                "stamp",
                "seed",
                "inputs"
            ]
        );
    }
}

/// Arguments of `proc.bake`.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BakeArgs {
    /// The procedural source's id.
    pub source: String,
    /// Bake only these keys of its output (each with its generated
    /// descendants); the source keeps generating the rest. Default: all.
    #[serde(default)]
    pub keys: Option<Vec<String>>,
    /// Keep the source generating (bake copies and deletion overrides)
    /// even when every key is baked.
    #[serde(default)]
    pub keep_source: bool,
}

fn bake_error(source: &str, message: impl Into<String>) -> re_core::Diagnostic {
    re_core::Diagnostic::error(crate::codes::BAKE_FAILED, message)
        .at(re_core::Location::entity(source.to_string()))
}

/// The baked file for a `derived:` asset, written under `root` when it is
/// not there yet (unless `write` is false); its project path.
fn bake_asset(
    store: Option<&crate::GraphStore>,
    root: Option<&std::path::Path>,
    source: &str,
    name: &str,
    write: bool,
) -> Result<String, String> {
    let store = store.ok_or("the world has no GraphStore holding derived assets")?;
    let (bytes, ext) = if let Some(mesh) = store.mesh(name) {
        (crate::glb::write_glb(&mesh, name)?, "glb")
    } else if let Some(h) = store.heightfield(name) {
        (write_heightfield(&h), "heightfield")
    } else {
        return Err(format!(
            "the derived asset `{name}` is not in the GraphStore"
        ));
    };
    let path = baked_asset_path(source, &bytes, ext);
    let root = root.ok_or("the GraphStore has no project root to write baked assets to")?;
    let full = root.join(&path);
    if write && !full.exists() {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if let Some(dir) = full.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {path}: {e}"))?;
        }
        // A name of its own, so another process baking the same content
        // cannot take the temporary file away; whichever rename lands, the
        // content is the same.
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = full.with_extension(format!("{ext}.{}-{n}.writing", std::process::id()));
        std::fs::write(&tmp, &bytes).map_err(|e| format!("cannot write {path}: {e}"))?;
        if let Err(e) = std::fs::rename(&tmp, &full) {
            let _ = std::fs::remove_file(&tmp);
            if !full.is_file() {
                return Err(format!("cannot write {path}: {e}"));
            }
        }
    }
    Ok(path)
}

/// Rewrites the entity references (`entity` fields and lists of them) that
/// name a baked entity, and every `derived:` name, in one component's fields.
fn remap_fields(
    fields: &mut indexmap::IndexMap<String, re_reflect::Doc>,
    schema: &re_reflect::ComponentSchema,
    ids: &std::collections::HashMap<String, String>,
    assets: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<(), String> {
    use re_reflect::{Doc, FieldType};
    fn walk(
        doc: &mut Doc,
        entity: bool,
        ids: &std::collections::HashMap<String, String>,
        assets: &mut dyn FnMut(&str) -> Result<String, String>,
    ) -> Result<(), String> {
        match doc {
            Doc::Str(s) if entity => {
                if let Some(id) = ids.get(s.as_str()) {
                    *s = id.clone();
                }
            }
            Doc::Str(s) if s.starts_with("derived:") => *s = assets(s)?,
            Doc::List(items) => {
                for item in items {
                    walk(item, entity, ids, assets)?;
                }
            }
            Doc::Map(m) => {
                for v in m.values_mut() {
                    walk(v, false, ids, assets)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    for (name, value) in fields.iter_mut() {
        let entity = schema
            .fields
            .iter()
            .find(|f| &f.name == name)
            .is_some_and(|f| {
                matches!(&f.ty, FieldType::Entity)
                    || matches!(&f.ty, FieldType::List(inner) if **inner == FieldType::Entity)
            });
        walk(value, entity, ids, assets)?;
    }
    Ok(())
}

/// The hex blake3 of what a source's output is made from besides its seed
/// and versions: its parameter component and, for a graph, the document.
fn inputs_hash(world: &re_scene::World, source: re_scene::Entity, params: &str) -> String {
    let mut h = blake3::Hasher::new();
    if let Some(doc) = world.entity_doc(source, re_scene::SaveOptions::default())
        && let Some(fields) = doc.components.get(params)
    {
        h.update(serde_json::to_string(fields).unwrap_or_default().as_bytes());
    }
    let id = world.id_of(source).unwrap_or_default();
    if let Some(doc) = world
        .ecs
        .get_resource::<crate::GraphStore>()
        .and_then(|s| s.graph(id))
    {
        h.update(crate::canon::doc_hash(doc).as_bytes());
    }
    h.finalize().to_hex().to_string()
}

/// `proc.bake`. ADR-0043 §2 refuses a bake during play; play runs on a copy
/// of the World that the command host does not edit, so there is nothing to
/// check here.
pub(crate) fn bake<C: re_command::EditTarget>(
    target: &mut C,
    a: BakeArgs,
    call: &mut re_command::Call,
) -> Result<serde_json::Value, re_core::Diagnostic> {
    use std::collections::{HashMap, HashSet};

    use re_command::Op;
    use re_scene::{Generated, OverrideDoc, Overrides, Procedural, SaveOptions};

    let world = target.world();
    let src = world.entity(&a.source).map_err(|e| e.to_diagnostic())?;
    let p = world.ecs.get::<Procedural>(src).cloned().ok_or_else(|| {
        bake_error(
            &a.source,
            format!("`{}` is not a procedural source", a.source),
        )
    })?;
    let info = world.generators.get(&p.generator).ok_or_else(|| {
        bake_error(
            &a.source,
            format!(
                "`{}` uses the unknown generator `{}`",
                a.source, p.generator
            ),
        )
    })?;
    let params_component = info.params_component.clone();

    // The source's output, parents first, in output order.
    let output: Vec<(re_scene::Entity, String)> = re_scene::hierarchy::subtree(&world.ecs, src)
        .into_iter()
        .filter_map(|e| {
            let g = world.ecs.get::<Generated>(e)?;
            (g.source == src).then(|| (e, g.key.clone()))
        })
        .collect();
    if output.is_empty() {
        return Err(bake_error(
            &a.source,
            format!("`{}` has no generated output to bake", a.source),
        )
        .with_hint("regenerate it first, or check its generator's report"));
    }
    let known: HashSet<&str> = output.iter().map(|(_, k)| k.as_str()).collect();
    let chosen: Option<Vec<String>> = a.keys.clone();
    if chosen.as_ref().is_some_and(Vec::is_empty) {
        return Err(bake_error(&a.source, "`keys` names no key to bake")
            .with_hint("leave `keys` out to bake the whole source"));
    }
    if let Some(keys) = &chosen {
        for k in keys {
            if !known.contains(k.as_str()) {
                let hint = re_reflect::suggest(k, known.iter().copied())
                    .map(|s| format!("did you mean `{s}`?"))
                    .unwrap_or_else(|| "`query` lists the generated entities".into());
                return Err(bake_error(
                    &a.source,
                    format!("`{}` generates no key `{k}`", a.source),
                )
                .with_hint(hint));
            }
        }
    }
    // A chosen key brings its generated descendants (below).
    let in_bake = |key: &str| {
        chosen
            .as_ref()
            .is_none_or(|keys| keys.iter().any(|k| k == key))
    };
    let parent_key = |e: re_scene::Entity| {
        re_scene::hierarchy::parent(&world.ecs, e)
            .and_then(|p| world.ecs.get::<Generated>(p))
            .filter(|g| g.source == src)
            .map(|g| g.key.clone())
    };
    let mut baked: Vec<(re_scene::Entity, String)> = Vec::new();
    let mut baked_keys: HashSet<String> = HashSet::new();
    for (e, key) in &output {
        let parent = parent_key(*e);
        let bake_it = in_bake(key) || parent.as_ref().is_some_and(|p| baked_keys.contains(p));
        if !bake_it {
            continue;
        }
        if let Some(p) = &parent
            && !baked_keys.contains(p)
        {
            return Err(bake_error(
                &a.source,
                format!("`{key}` is generated under `{p}`, which is not baked"),
            )
            .with_hint(format!("bake `{p}` too, or the whole source")));
        }
        baked_keys.insert(key.clone());
        baked.push((*e, key.clone()));
    }
    let full = chosen.is_none() && !a.keep_source;

    // Ids: the generated id -> the baked id.
    let mut ids: HashMap<String, String> = HashMap::new();
    let mut taken: HashSet<String> = HashSet::new();
    for (e, key) in &baked {
        let id = baked_id(&a.source, key, |id| {
            taken.contains(id) || world.entity(id).is_ok()
        });
        taken.insert(id.clone());
        ids.insert(world.id_of(*e).unwrap_or_default().to_string(), id);
    }

    let stamp = world
        .ecs
        .get_resource::<crate::GraphStore>()
        .and_then(|s| {
            s.graph(&a.source)
                .map(|d| crate::canon::version_stamp(d, s.evaluator().registry()))
        })
        .map(|h| h.to_hex().to_string())
        .unwrap_or_default();
    let inputs = inputs_hash(world, src, &params_component);
    let store = world.ecs.get_resource::<crate::GraphStore>();
    let root = store
        .and_then(|s| s.root())
        .map(std::path::Path::to_path_buf);
    let write = !call.dry_run();
    let mut written: Vec<String> = Vec::new();
    // Each derived asset is encoded once, however many entities name it.
    let mut paths: HashMap<String, String> = HashMap::new();
    let mut asset = |name: &str| -> Result<String, String> {
        if let Some(path) = paths.get(name) {
            return Ok(path.clone());
        }
        let path = bake_asset(store, root.as_deref(), &a.source, name, write)?;
        if !written.contains(&path) {
            written.push(path.clone());
        }
        paths.insert(name.to_string(), path.clone());
        Ok(path)
    };

    let mut ops: Vec<Op> = Vec::new();
    for (e, key) in &baked {
        let doc = world
            .entity_doc(*e, SaveOptions::default())
            .ok_or_else(|| bake_error(&a.source, format!("`{key}` has no stable id")))?;
        let mut components = doc.components;
        for (name, fields) in components.iter_mut() {
            let Some(k) = world.registry.try_key(name) else {
                continue;
            };
            remap_fields(fields, world.registry.schema(k), &ids, &mut asset)
                .map_err(|m| bake_error(&a.source, format!("`{key}`: {m}")))?;
        }
        let mut provenance = indexmap::IndexMap::new();
        provenance.insert("source".to_string(), re_reflect::Doc::Str(a.source.clone()));
        provenance.insert("key".to_string(), re_reflect::Doc::Str(key.clone()));
        provenance.insert(
            "generator".to_string(),
            re_reflect::Doc::Str(p.generator.clone()),
        );
        provenance.insert(
            "generator_version".to_string(),
            re_reflect::Doc::Int(p.version),
        );
        provenance.insert("stamp".to_string(), re_reflect::Doc::Str(stamp.clone()));
        provenance.insert("seed".to_string(), re_reflect::Doc::Int(p.seed));
        provenance.insert("inputs".to_string(), re_reflect::Doc::Str(inputs.clone()));
        components.insert("Baked".to_string(), provenance);
        let parent = doc.parent.map(|p| ids.get(&p).cloned().unwrap_or(p));
        ops.push(Op::Spawn {
            id: Some(ids[world.id_of(*e).unwrap_or_default()].clone()),
            name: doc.name,
            parent,
            index: None,
            components,
        });
    }
    // References from outside the bake follow it to the baked entities, so
    // removing the generated ones below clears nothing.
    let baked_set: HashSet<re_scene::Entity> = baked.iter().map(|(e, _)| *e).collect();
    for (referrer, key, fi) in world.registry.references_to(&world.ecs, &baked_set) {
        if baked_set.contains(&referrer) {
            continue;
        }
        let (Some(referrer_id), Some(re_reflect::Value::Entity(Some(t)))) = (
            world.id_of(referrer),
            world.registry.get_field(&world.ecs, referrer, key, fi),
        ) else {
            continue;
        };
        let Some(to) = world.id_of(t).and_then(|id| ids.get(id)) else {
            continue;
        };
        let schema = world.registry.schema(key);
        ops.push(Op::SetField {
            id: referrer_id.to_string(),
            component: schema.name.clone(),
            field: schema.fields[fi].name.clone(),
            value: re_reflect::Doc::Str(to.clone()),
        });
    }
    // The generated originals go in the same transaction (so nothing in
    // the world still names a source that no longer generates them, and undo
    // restores them exactly): despawning the top baked keys takes their
    // descendants and records their deletion overrides.
    let tops: Vec<(re_scene::Entity, String)> = baked
        .iter()
        .filter(|(e, _)| !parent_key(*e).is_some_and(|p| baked_keys.contains(&p)))
        .cloned()
        .collect();
    for (e, _) in &tops {
        ops.push(Op::Despawn {
            id: world.id_of(*e).unwrap_or_default().to_string(),
        });
    }
    let overrides: Vec<OverrideDoc> = world
        .ecs
        .get::<Overrides>(src)
        .map(|o| o.0.clone())
        .unwrap_or_default();
    if full {
        // The source keeps no overrides: first the deletions just recorded,
        // then the ones it had.
        for (_, key) in tops.iter().rev() {
            ops.push(Op::SetOverride {
                source: a.source.clone(),
                target: key.clone(),
                component: None,
                field: None,
                value: None,
                deleted: Some(false),
                index: None,
            });
        }
        for o in overrides.iter().rev() {
            ops.push(match o {
                OverrideDoc::Field {
                    target,
                    component,
                    field,
                    ..
                } => Op::SetOverride {
                    source: a.source.clone(),
                    target: target.clone(),
                    component: Some(component.clone()),
                    field: Some(field.clone()),
                    value: None,
                    deleted: None,
                    index: None,
                },
                OverrideDoc::Deleted { target } => Op::SetOverride {
                    source: a.source.clone(),
                    target: target.clone(),
                    component: None,
                    field: None,
                    value: None,
                    deleted: Some(false),
                    index: None,
                },
            });
        }
        if world
            .registry
            .try_key(&params_component)
            .is_some_and(|k| world.registry.has(&world.ecs, src, k))
        {
            ops.push(Op::RemoveComponent {
                id: a.source.clone(),
                component: params_component.clone(),
            });
        }
        ops.push(Op::RemoveComponent {
            id: a.source.clone(),
            component: "Procedural".into(),
        });
    }
    let count = baked.len();
    let mut pairs: Vec<(String, String)> = ids.into_iter().collect();
    pairs.sort();
    let ids: serde_json::Map<String, serde_json::Value> = pairs
        .into_iter()
        .map(|(k, v)| (k, serde_json::Value::String(v)))
        .collect();
    written.sort();
    call.transact(target, format!("proc.bake {}", a.source), ops)?;
    Ok(serde_json::json!({
        "source": a.source,
        "baked": count,
        "full": full,
        "ids": ids,
        "assets": written,
        "dry_run": !write,
    }))
}
