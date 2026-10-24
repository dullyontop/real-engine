//! The look of each `Enum.Material`: roughness, metalness, a tint, and a
//! procedural detail texture that multiplies the part's colour, plus the
//! special cases (`Neon` glows, `ForceField` is a see-through glow, `Glass`
//! is smooth).
//!
//! Textures are greyscale, built once per material from `re_noise`, and tile
//! every [`UV_STUDS`](super::mesh::UV_STUDS) studs. Their mean brightness is
//! close to 1 so a part keeps the colour its script gave it.

use re_noise::Fractal;
use re_types::render::{AlphaMode, MaterialDesc, TextureData, TextureFormat, TextureRef};

/// The side of a material texture, in pixels.
pub const TEXTURE_SIZE: u32 = 128;

/// A detail pattern.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pattern {
    /// No texture.
    Smooth,
    /// Fine noise of the given strength (percent).
    Grain(u8),
    /// Blotchy stone: cells and noise.
    Stone,
    /// Cobbles or pebbles: round cells with dark gaps.
    Cobbles,
    /// Veins.
    Marble,
    /// Wood grain along X.
    Wood,
    /// Planks along X with seams.
    Planks,
    /// Bricks with mortar.
    Brick,
    /// A square grid of tiles.
    Tiles,
    /// Overlapping rows (roofs).
    Rows,
    /// Brushed streaks along X.
    Brushed,
    /// Raised diamonds.
    Diamond,
    /// A fine weave.
    Weave,
    /// Cracks between plates.
    Cracks,
}

/// How one material looks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Look {
    pub roughness: f32,
    pub metallic: f32,
    /// Multiplies the part's colour.
    pub tint: [f32; 3],
    pub pattern: Pattern,
}

const fn look(roughness: f32, metallic: f32, pattern: Pattern) -> Look {
    Look {
        roughness,
        metallic,
        tint: [1.0; 3],
        pattern,
    }
}

/// The look of a material by its item name; unknown names look like
/// `Plastic`.
pub(crate) fn of(material: &str) -> Look {
    use Pattern::*;
    match material {
        "SmoothPlastic" => look(0.35, 0.0, Smooth),
        "Neon" => look(0.5, 0.0, Smooth),
        "Glass" => look(0.06, 0.0, Smooth),
        "ForceField" | "Air" | "Water" => look(0.2, 0.0, Smooth),
        "Ice" | "Glacier" => look(0.15, 0.0, Grain(6)),
        "Foil" => look(0.25, 0.9, Grain(8)),
        "Metal" => look(0.35, 0.85, Brushed),
        "DiamondPlate" => look(0.4, 0.85, Diamond),
        "CorrodedMetal" => look(0.75, 0.45, Stone),
        "Wood" => look(0.75, 0.0, Wood),
        "WoodPlanks" => look(0.75, 0.0, Planks),
        "Marble" => look(0.3, 0.0, Marble),
        "Granite" | "Basalt" | "Rock" | "Limestone" | "Sandstone" | "Slate" => {
            look(0.85, 0.0, Stone)
        }
        "Pebble" | "Cobblestone" => look(0.9, 0.0, Cobbles),
        "CrackedLava" => look(0.9, 0.0, Cracks),
        "Brick" => look(0.9, 0.0, Brick),
        "CeramicTiles" => look(0.3, 0.0, Tiles),
        "ClayRoofTiles" | "RoofShingles" => look(0.85, 0.0, Rows),
        "Fabric" | "Carpet" => look(0.95, 0.0, Weave),
        "Concrete" | "Pavement" | "Asphalt" | "Plaster" => look(0.9, 0.0, Grain(18)),
        "Sand" | "Snow" | "Salt" | "Ground" | "Mud" => look(0.95, 0.0, Grain(22)),
        "Grass" | "LeafyGrass" => look(0.95, 0.0, Grain(30)),
        "Cardboard" | "Leather" | "Rubber" => look(0.85, 0.0, Grain(10)),
        _ => look(0.6, 0.0, Grain(4)),
    }
}

/// The asset name of a material's texture.
pub(crate) fn texture_name(material: &str) -> String {
    format!("compat:material/{material}")
}

/// The material a part is drawn with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MaterialKey {
    /// The `Enum.Material` item, by name order in [`crate::render::MATERIALS`].
    pub material: u8,
    /// The sRGB colour, or white when the colour is in the mesh's vertices.
    pub color: [u8; 3],
    /// Opacity, 255 for opaque.
    pub alpha: u8,
    /// `Reflectance` times 255.
    pub reflectance: u8,
}

/// The `MaterialDesc` of a key. Its texture, if any, is named by
/// [`texture_name`] as a [`TextureRef::Asset`].
pub(crate) fn desc(key: &MaterialKey, material: &str) -> MaterialDesc {
    let l = of(material);
    let lin = |c: u8| srgb_to_linear(f32::from(c) / 255.0);
    let color = [
        lin(key.color[0]) * l.tint[0],
        lin(key.color[1]) * l.tint[1],
        lin(key.color[2]) * l.tint[2],
    ];
    let mut alpha = f32::from(key.alpha) / 255.0;
    let r = f32::from(key.reflectance) / 255.0;
    let mut d = MaterialDesc {
        base_color: [color[0], color[1], color[2], 1.0],
        roughness: l.roughness + (0.05 - l.roughness) * r,
        metallic: l.metallic.max(r * 0.6),
        ..MaterialDesc::default()
    };
    if l.pattern != Pattern::Smooth {
        d.base_color_texture = Some(TextureRef::Asset(texture_name(material)));
    }
    match material {
        "Neon" => {
            d.emissive = color;
            d.emissive_strength = 1.5;
        }
        "ForceField" => {
            d.emissive = color;
            d.unlit = true;
            alpha *= 0.35;
        }
        _ => {}
    }
    if alpha < 1.0 {
        d.base_color[3] = alpha;
        d.alpha_mode = AlphaMode::Blend;
    }
    d
}

pub(crate) fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        libm::powf((c + 0.055) / 1.055, 2.4)
    }
}

/// The detail texture of a material, `None` for a smooth one.
pub(crate) fn texture(material: &str) -> Option<TextureData> {
    let pattern = of(material).pattern;
    if pattern == Pattern::Smooth {
        return None;
    }
    let n = TEXTURE_SIZE;
    let seed = material.bytes().fold(0x9E37_79B9_7F4A_7C15u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01B3)
    });
    let mut values = Vec::with_capacity((n * n) as usize);
    for y in 0..n {
        for x in 0..n {
            // Coordinates in studs across one tile.
            let u = x as f32 / n as f32 * super::mesh::UV_STUDS;
            let v = y as f32 / n as f32 * super::mesh::UV_STUDS;
            values.push(sample(pattern, seed, u, v));
        }
    }
    // Brightness around 1: the colour the script gave stays the colour seen.
    let mean = values.iter().sum::<f32>() / values.len() as f32;
    let scale = if mean > 0.0 { 0.97 / mean } else { 1.0 };
    let mut pixels = Vec::with_capacity(values.len() * 4);
    for v in values {
        let g = (linear_to_srgb((v * scale).clamp(0.0, 1.0)) * 255.0).round() as u8;
        pixels.extend_from_slice(&[g, g, g, 255]);
    }
    Some(TextureData {
        width: n,
        height: n,
        format: TextureFormat::Rgba8Srgb,
        pixels,
    })
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * libm::powf(c, 1.0 / 2.4) - 0.055
    }
}

/// Tiling noise: `fbm` over a torus so the texture repeats every tile.
/// `f.lacunarity` aside, the noise has `freq` features per stud.
fn tiled(seed: u64, f: &(Fractal, f32), u: f32, v: f32, period: f32) -> f32 {
    let (f, freq) = f;
    let tau = std::f32::consts::TAU;
    let (a, b) = (u / period * tau, v / period * tau);
    let r = period / tau * freq;
    // Sample a 4D torus through two 3D noises.
    let n1 = re_noise::fbm3(
        seed,
        f,
        r * libm::cosf(a),
        r * libm::sinf(a),
        r * libm::cosf(b),
    );
    let n2 = re_noise::fbm3(
        seed ^ 0x5555,
        f,
        r * libm::sinf(b),
        r * libm::cosf(a),
        r * libm::cosf(b),
    );
    (n1 + n2) * 0.5
}

/// Distance to the nearest of a tiling set of jittered cell points, and to
/// the second nearest, in studs.
fn cells(seed: u64, u: f32, v: f32, size: f32, period: f32) -> (f32, f32) {
    let count = (period / size).round().max(1.0) as i32;
    let (cu, cv) = ((u / size).floor() as i32, (v / size).floor() as i32);
    let mut best = (f32::MAX, f32::MAX);
    for dy in -1..=1 {
        for dx in -1..=1 {
            let (gx, gy) = (cu + dx, cv + dy);
            let (wx, wy) = (gx.rem_euclid(count), gy.rem_euclid(count));
            let h =
                (seed ^ (wx as u64) << 20 ^ (wy as u64) << 40).wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let jx = ((h >> 11) & 0xFFFF) as f32 / 65535.0;
            let jy = ((h >> 31) & 0xFFFF) as f32 / 65535.0;
            let px = (gx as f32 + 0.15 + 0.7 * jx) * size;
            let py = (gy as f32 + 0.15 + 0.7 * jy) * size;
            let d = ((u - px).powi(2) + (v - py).powi(2)).sqrt();
            if d < best.0 {
                best = (d, best.0);
            } else if d < best.1 {
                best.1 = d;
            }
        }
    }
    best
}

/// A soft edge: 0 inside a gap of half-width `w` around a line at `d` = 0.
fn edge(d: f32, w: f32) -> f32 {
    ((d.abs() - w) / w).clamp(0.0, 1.0)
}

fn sample(p: Pattern, seed: u64, u: f32, v: f32) -> f32 {
    let period = super::mesh::UV_STUDS;
    let fine = (
        Fractal {
            octaves: 4,
            ..Fractal::default()
        },
        2.0,
    );
    let broad = (
        Fractal {
            octaves: 3,
            ..Fractal::default()
        },
        0.5,
    );
    match p {
        Pattern::Smooth => 1.0,
        Pattern::Grain(strength) => {
            1.0 + tiled(seed, &fine, u, v, period) * f32::from(strength) / 100.0 * 2.0
        }
        Pattern::Stone => {
            let (d1, d2) = cells(seed, u, v, 2.0, period);
            0.85 + 0.25 * tiled(seed, &broad, u, v, period) + 0.15 * (d2 - d1).min(1.0)
                - 0.1 * (1.0 - edge(d2 - d1, 0.06))
        }
        Pattern::Cobbles => {
            let (d1, d2) = cells(seed, u, v, 1.0, period);
            let gap = edge(d2 - d1, 0.08);
            (0.55 + 0.45 * gap) * (0.9 + 0.1 * tiled(seed, &fine, u, v, period)) - 0.15 * d1
        }
        Pattern::Marble => {
            let w = tiled(seed, &broad, u, v, period);
            let vein = libm::sinf((u + v) * 0.8 + w * 6.0).abs();
            0.85 + 0.15 * libm::powf(vein, 0.3)
        }
        Pattern::Wood => {
            let w = tiled(seed, &broad, u, v, period);
            let ring = libm::sinf((v * 3.0 + w * 4.0) * std::f32::consts::PI);
            0.85 + 0.1 * ring + 0.05 * tiled(seed ^ 1, &fine, u * 0.25, v * 4.0, period)
        }
        Pattern::Planks => {
            let plank = (v / 1.0).floor();
            let seam = edge(v - (v / 1.0).round(), 0.04);
            let offset = libm::sinf(plank * 2.7) * 2.0;
            let end = edge(((u + offset) / 4.0).fract() - 0.5, 0.48 / 4.0 * 4.0);
            let grain = tiled(seed, &broad, u, v * 6.0, period);
            let tone = 0.9 + 0.08 * libm::sinf(plank * 1.9);
            (tone + 0.08 * grain) * (0.6 + 0.4 * seam) * (0.75 + 0.25 * end.max(seam.min(1.0)))
        }
        Pattern::Brick => {
            let row = (v / 0.5).floor();
            let su = u + if row as i32 % 2 == 0 { 0.0 } else { 0.5 };
            let mortar = edge(su - su.round(), 0.05) * edge(v / 0.5 - (v / 0.5).round(), 0.1);
            let tone = 0.9 + 0.1 * (libm::sinf(row * 3.1 + (su).floor() * 7.3));
            (0.55 + 0.45 * mortar) * (tone + 0.08 * tiled(seed, &fine, u, v, period))
        }
        Pattern::Tiles => {
            let g = edge(u - u.round(), 0.03) * edge(v - v.round(), 0.03);
            0.7 + 0.3 * g
        }
        Pattern::Rows => {
            let f = (v / 0.5).fract();
            let row = (v / 0.5).floor();
            let split = edge(((u + row * 0.37) / 0.75).fract() - 0.5, 0.45);
            (0.65 + 0.35 * f) * (0.85 + 0.15 * split)
        }
        Pattern::Brushed => 1.0 + 0.06 * tiled(seed, &fine, u * 0.1, v * 8.0, period),
        Pattern::Diamond => {
            let a = ((u + v) * 2.0).fract() - 0.5;
            let b = ((u - v) * 2.0).fract() - 0.5;
            let bump = 1.0 - (a.abs() + b.abs()).min(1.0);
            0.85 + 0.25 * (bump * 3.0).min(1.0)
        }
        Pattern::Weave => {
            let x = (u * 8.0).floor() as i32;
            let y = (v * 8.0).floor() as i32;
            let over = (x + y) % 2 == 0;
            let t = if over {
                (u * 8.0).fract()
            } else {
                (v * 8.0).fract()
            };
            0.85 + 0.15 * libm::sinf(t * std::f32::consts::PI)
        }
        Pattern::Cracks => {
            let (d1, d2) = cells(seed, u, v, 1.5, period);
            0.25 + 0.75 * edge(d2 - d1, 0.07)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn textures_average_to_white_and_tile() {
        for m in [
            "Plastic",
            "Wood",
            "WoodPlanks",
            "Brick",
            "Slate",
            "Grass",
            "DiamondPlate",
            "Fabric",
        ] {
            let t = texture(m).unwrap();
            assert!(t.is_well_formed(), "{m}");
            let n = TEXTURE_SIZE as usize;
            let g = |x: usize, y: usize| f32::from(t.pixels[(y * n + x) * 4]);
            let mean = t.pixels.chunks(4).map(|p| f32::from(p[0])).sum::<f32>() / (n * n) as f32;
            assert!(mean > 180.0, "{m}: mean {mean}");
            // Opposite edges are close: the pattern repeats.
            let seam: f32 = (0..n).map(|y| (g(0, y) - g(n - 1, y)).abs()).sum::<f32>() / n as f32;
            assert!(seam < 40.0, "{m}: seam {seam}");
        }
        assert!(texture("SmoothPlastic").is_none());
    }

    #[test]
    fn keys_become_materials() {
        let key = MaterialKey {
            material: 0,
            color: [255, 0, 0],
            alpha: 128,
            reflectance: 255,
        };
        let d = desc(&key, "Plastic");
        assert!(matches!(d.alpha_mode, AlphaMode::Blend));
        assert!((d.base_color[3] - 128.0 / 255.0).abs() < 1e-6);
        assert!(d.roughness < 0.1 && d.metallic > 0.5);
        assert_eq!(
            d.base_color_texture,
            Some(TextureRef::Asset("compat:material/Plastic".into()))
        );
        let neon = desc(&MaterialKey { alpha: 255, ..key }, "Neon");
        assert!(neon.emissive[0] > 0.9 && neon.base_color_texture.is_none());
        assert!(matches!(neon.alpha_mode, AlphaMode::Opaque));
    }
}
