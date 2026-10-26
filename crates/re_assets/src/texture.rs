//! Texture processing at import (ADR-0047): the `[assets]` settings, box-filtered
//! mip chains and, with the `texture-encode` feature, BC7 and BC5 blocks.
//!
//! An encoded texture carries its whole mip chain, because the renderer cannot
//! filter blocks. Each level is box-filtered from the one above (in linear light
//! for sRGB colour, as the renderer filters RGBA8), padded to whole 4×4 blocks by
//! repeating its last column and row when it is smaller than a block, and
//! encoded one row of blocks per job on the worker pool, so the bytes are the
//! same for any number of threads. A texture whose width or height is not a
//! multiple of 4 stays RGBA8: BC textures need whole blocks at the top level.

#[cfg(feature = "texture-encode")]
use re_core::TaskPool;
use re_types::render::TextureData;
#[cfg(any(feature = "texture-encode", test))]
use re_types::render::TextureFormat;
use serde::{Deserialize, Serialize};

/// True when this build encodes textures (the `texture-encode` feature, which
/// `re_cli`'s `tooling` turns on). Without it textures import as RGBA8.
pub const TEXTURE_ENCODER: bool = cfg!(feature = "texture-encode");

/// `[assets]` in `real.toml`: how imported textures are stored (ADR-0047).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AssetsConfig {
    /// `"bc"` (the default) or `"none"`.
    pub texture_compression: TextureCompression,
    /// `"fast"` (the default) or `"high"`.
    pub texture_quality: TextureQuality,
}

/// How textures are stored.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextureCompression {
    /// BC7 blocks, and BC5 for normal maps, where the encoder is built in.
    #[default]
    Bc,
    /// RGBA8: 4 bytes a pixel, mips made at upload.
    None,
}

/// How hard the encoder tries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextureQuality {
    /// block_compression's `very_fast` presets: about 6 s for a 2048² texture on
    /// four cores.
    #[default]
    Fast,
    /// Its `basic` presets: about 1 dB better and 3 to 4 times slower.
    High,
}

impl AssetsConfig {
    /// True when imported textures are encoded: the encoder is built in and
    /// compression is on.
    pub fn encodes(&self) -> bool {
        TEXTURE_ENCODER && self.texture_compression == TextureCompression::Bc
    }

    /// The settings share of an import's cache key: `sidecar` (the JSON of the
    /// sidecar's `settings`) and what this config stores, the encoded formats
    /// and preset or RGBA8. A build without the encoder stores RGBA8, so its
    /// entries never stand in for encoded ones.
    pub fn cache_settings(&self, sidecar: &[u8]) -> Vec<u8> {
        let stored: &[u8] = match (self.encodes(), self.texture_quality) {
            (false, _) => b"rgba8",
            (true, TextureQuality::Fast) => b"bc7+bc5 very_fast",
            (true, TextureQuality::High) => b"bc7+bc5 basic",
        };
        [sidecar, b"\0", stored].concat()
    }
}

/// What a texture holds, which picks its block format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TextureRole {
    /// sRGB colour or linear RGBA data, as its format says: BC7.
    Rgba,
    /// A tangent-space normal map: BC5, two channels.
    Normal,
}

/// The box-filtered mip chain of RGBA8 `pixels`, from the top level down to
/// 1×1, as the renderer makes it for RGBA8 uploads: each level halves the one
/// above (rounded down, at least 1), averaging colour in linear light when
/// `srgb` is set and alpha as it is.
#[cfg(any(feature = "texture-encode", test))]
pub(crate) fn mip_chain(
    width: u32,
    height: u32,
    pixels: &[u8],
    srgb: bool,
) -> Vec<(u32, u32, Vec<u8>)> {
    let to_linear: Vec<f32> = (0..=255u8)
        .map(|v| {
            let c = f32::from(v) / 255.0;
            if !srgb {
                c
            } else if c <= 0.04045 {
                c / 12.92
            } else {
                libm::powf((c + 0.055) / 1.055, 2.4)
            }
        })
        .collect();
    let encode = |c: f32| -> u8 {
        let c = c.clamp(0.0, 1.0);
        if !srgb {
            return (c * 255.0).round() as u8;
        }
        let s = if c <= 0.003_130_8 {
            12.92 * c
        } else {
            1.055 * libm::powf(c, 1.0 / 2.4) - 0.055
        };
        (s * 255.0).round() as u8
    };
    let mut levels = vec![(width, height, pixels.to_vec())];
    while let Some((w, h, src)) = levels.last().filter(|(w, h, _)| *w > 1 || *h > 1) {
        let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
        let mut out = Vec::with_capacity((nw * nh * 4) as usize);
        for y in 0..nh {
            for x in 0..nw {
                let mut sum = [0f32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let sx = (x * 2 + dx).min(w - 1);
                    let sy = (y * 2 + dy).min(h - 1);
                    let i = ((sy * w + sx) * 4) as usize;
                    for c in 0..3 {
                        sum[c] += to_linear[src[i + c] as usize];
                    }
                    sum[3] += f32::from(src[i + 3]) / 255.0;
                }
                for (c, s) in sum.iter().enumerate() {
                    let avg = s / 4.0;
                    out.push(if c == 3 {
                        (avg * 255.0).round() as u8
                    } else {
                        encode(avg)
                    });
                }
            }
        }
        levels.push((nw, nh, out));
    }
    levels
}

/// `data` as `config` stores it: block-compressed with its mip chain when
/// the config [`encodes`](AssetsConfig::encodes) and the size is whole
/// blocks, else unchanged.
pub(crate) fn process(data: TextureData, role: TextureRole, config: &AssetsConfig) -> TextureData {
    if !config.encodes()
        || data.format.is_compressed()
        || !data.width.is_multiple_of(4)
        || !data.height.is_multiple_of(4)
        || !data.is_well_formed()
    {
        return data;
    }
    encode(data, role, config.texture_quality)
}

#[cfg(feature = "texture-encode")]
fn encode(data: TextureData, role: TextureRole, quality: TextureQuality) -> TextureData {
    use block_compression::{BC7Settings, CompressionVariant};

    let opaque = data.pixels.as_chunks::<4>().0.iter().all(|p| p[3] == 255);
    let (format, variant) = match role {
        TextureRole::Normal => (TextureFormat::Bc5Unorm, CompressionVariant::BC5),
        TextureRole::Rgba => {
            let settings = match (quality, opaque) {
                (TextureQuality::Fast, true) => BC7Settings::opaque_very_fast(),
                (TextureQuality::Fast, false) => BC7Settings::alpha_very_fast(),
                (TextureQuality::High, true) => BC7Settings::opaque_basic(),
                (TextureQuality::High, false) => BC7Settings::alpha_basic(),
            };
            let format = if data.format.is_srgb() {
                TextureFormat::Bc7Srgb
            } else {
                TextureFormat::Bc7Unorm
            };
            (format, CompressionVariant::BC7(settings))
        }
    };
    let pool = TaskPool::global();
    let mut levels = mip_chain(data.width, data.height, &data.pixels, data.format.is_srgb())
        .into_iter()
        .map(|(w, h, pixels)| encode_level(variant, w, h, &pixels, pool));
    let pixels = levels.next().expect("a mip chain has its top level");
    TextureData {
        width: data.width,
        height: data.height,
        format,
        pixels,
        levels: levels.collect(),
    }
}

#[cfg(not(feature = "texture-encode"))]
fn encode(data: TextureData, _role: TextureRole, _quality: TextureQuality) -> TextureData {
    data
}

/// One level of RGBA8 `pixels` encoded as `variant`, a row of blocks per job.
#[cfg(feature = "texture-encode")]
fn encode_level(
    variant: block_compression::CompressionVariant,
    width: u32,
    height: u32,
    pixels: &[u8],
    pool: &TaskPool,
) -> Vec<u8> {
    let padded = pad_to_blocks(width, height, pixels);
    let (pw, ph) = (width.div_ceil(4) * 4, height.div_ceil(4) * 4);
    let row_bytes = variant.bytes_per_row(pw) as usize;
    let strip = pw as usize * 4 * 4;
    let mut out = vec![0u8; row_bytes * (ph / 4) as usize];
    pool.scope(|s| {
        for (blocks, rows) in out.chunks_mut(row_bytes).zip(padded.chunks(strip)) {
            s.spawn(move |_| {
                block_compression::encode::compress_rgba8(variant, rows, blocks, pw, 4, pw * 4);
            });
        }
    });
    out
}

/// `pixels` (`width` x `height` RGBA8) grown to whole 4×4 blocks by repeating
/// the last column and row.
#[cfg_attr(not(feature = "texture-encode"), allow(dead_code))]
fn pad_to_blocks(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let (pw, ph) = (w.div_ceil(4) * 4, h.div_ceil(4) * 4);
    if (pw, ph) == (w, h) {
        return pixels.to_vec();
    }
    let mut out = Vec::with_capacity(pw * ph * 4);
    for y in 0..ph {
        let row = y.min(h - 1) * w;
        for x in 0..pw {
            let i = (row + x.min(w - 1)) * 4;
            out.extend_from_slice(&pixels[i..i + 4]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> TextureData {
        TextureData {
            width,
            height,
            format: TextureFormat::Rgba8Srgb,
            pixels: (0..height)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .flat_map(|(x, y)| f(x, y))
                .collect(),
            levels: Vec::new(),
        }
    }

    #[test]
    fn mips_halve_to_one_pixel_in_linear_light() {
        let pixels: Vec<u8> = (0..5 * 3)
            .flat_map(|i| [i as u8 * 10, 0, 255, 255])
            .collect();
        let levels = mip_chain(5, 3, &pixels, true);
        let sizes: Vec<(u32, u32)> = levels.iter().map(|(w, h, _)| (*w, *h)).collect();
        assert_eq!(sizes, TextureData::mip_sizes(5, 3));
        // The renderer's RGBA8 mips: white and black average to sRGB 188 in
        // linear light, 128 for linear data.
        let bw = [255, 255, 255, 255, 0, 0, 0, 255];
        assert_eq!(&mip_chain(2, 1, &bw, true)[1].2, &[188, 188, 188, 255]);
        assert_eq!(&mip_chain(2, 1, &bw, false)[1].2, &[128, 128, 128, 255]);
    }

    #[test]
    fn small_levels_pad_by_repeating_the_edge() {
        let px = |v: u8| [v, v, v, 255];
        let pixels: Vec<u8> = [px(1), px(2), px(3), px(4)].concat();
        let padded = pad_to_blocks(2, 2, &pixels);
        assert_eq!(padded.len(), 4 * 4 * 4);
        let row = |y: usize| -> Vec<u8> {
            padded[y * 16..y * 16 + 16]
                .iter()
                .step_by(4)
                .copied()
                .collect()
        };
        assert_eq!(row(0), [1, 2, 2, 2]);
        assert_eq!(row(1), [3, 4, 4, 4]);
        assert_eq!(row(3), [3, 4, 4, 4]);
        // Whole blocks are left as they are.
        let whole = vec![9u8; 4 * 4 * 4];
        assert_eq!(pad_to_blocks(4, 4, &whole), whole);
    }

    #[test]
    fn textures_that_are_not_whole_blocks_or_not_encoded_stay_rgba8() {
        let on = AssetsConfig::default();
        let off = AssetsConfig {
            texture_compression: TextureCompression::None,
            ..AssetsConfig::default()
        };
        let odd = rgba(6, 4, |x, y| [x as u8, y as u8, 0, 255]);
        assert_eq!(process(odd.clone(), TextureRole::Rgba, &on), odd);
        let even = rgba(8, 4, |x, y| [x as u8, y as u8, 0, 255]);
        assert_eq!(process(even.clone(), TextureRole::Rgba, &off), even);
        assert_eq!(off.cache_settings(b"{}"), b"{}\0rgba8");
        assert_eq!(on.encodes(), TEXTURE_ENCODER);
    }

    #[cfg(feature = "texture-encode")]
    #[test]
    fn encoded_textures_carry_every_level_in_their_block_format() {
        let photo = rgba(16, 8, |x, y| [(x * 16) as u8, (y * 32) as u8, 128, 255]);
        let bc7 = process(photo, TextureRole::Rgba, &AssetsConfig::default());
        assert_eq!(bc7.format, TextureFormat::Bc7Srgb);
        assert!(bc7.is_well_formed(), "{} levels", bc7.levels.len());
        assert_eq!(bc7.levels.len(), 4, "8x4, 4x2, 2x1, 1x1 below 16x8");
        let mut normal = rgba(4, 4, |x, y| [(x * 60) as u8, (y * 60) as u8, 255, 255]);
        normal.format = TextureFormat::Rgba8Unorm;
        let bc5 = process(normal, TextureRole::Normal, &AssetsConfig::default());
        assert_eq!(bc5.format, TextureFormat::Bc5Unorm);
        assert!(bc5.is_well_formed());
        let mut data = rgba(4, 4, |_, _| [10, 20, 30, 40]);
        data.format = TextureFormat::Rgba8Unorm;
        assert_eq!(
            process(data, TextureRole::Rgba, &AssetsConfig::default()).format,
            TextureFormat::Bc7Unorm
        );
    }

    #[cfg(feature = "texture-encode")]
    #[test]
    fn encoding_gives_the_same_bytes_for_any_thread_count() {
        let image = rgba(64, 32, |x, y| {
            [
                (x * 4) as u8,
                (y * 8) as u8,
                ((x ^ y) * 7) as u8,
                (x * y) as u8,
            ]
        });
        let variant = block_compression::CompressionVariant::BC7(
            block_compression::BC7Settings::alpha_very_fast(),
        );
        let one = TaskPool::new(1).unwrap();
        let four = TaskPool::new(4).unwrap();
        assert_eq!(
            encode_level(variant, 64, 32, &image.pixels, &one),
            encode_level(variant, 64, 32, &image.pixels, &four)
        );
    }
}
