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
/// project.
pub fn baked_dir(source: &str) -> String {
    format!("assets/baked/{source}")
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
