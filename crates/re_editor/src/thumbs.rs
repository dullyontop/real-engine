//! The Assets panel's thumbnails (ADR-0064 §7): pictures of textures and
//! materials, at most [`SIZE`] pixels on a side, kept in memory under their
//! own budget and on disk under `.real/cache/thumbs/`.
//!
//! A thumbnail is asked for when its tile is drawn, and the asset index's
//! worker thread ([`crate::assets`]) makes it. It reads the file and keys the
//! thumbnail by the file's bytes, the thumbnail kind's version and the size
//! ([`re_assets::cache_key`]); a material's key also covers the bytes of the
//! textures it names. A thumbnail on disk under that key is read back. Else a
//! texture is decoded and shrunk there, and a material's textures are decoded
//! and shrunk for the preview's renderer, which draws the material on the
//! Material panel's sphere in a fixed light, at most one a frame
//! ([`crate::preview`]); the picture made is written to the disk cache by
//! the worker. Nothing is loaded into the App's asset server and no sidecar
//! is written, so browsing changes no project file.
//!
//! At most [`LIMIT`] thumbnails are kept as egui textures (9 MiB), the one
//! drawn least recently going first; on disk they stay until `.real/cache/`
//! is cleared. A thumbnail that cannot be made is noted once with `RE8853`,
//! and its kind's icon is shown until the file (or a texture a material
//! names) changes. Models, prefabs and graphs show their icons until WP39's
//! mesh rasteriser and graph previews make pictures of them; the other kinds
//! always do.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::Path;
use std::sync::OnceLock;

use re_core::{Diagnostic, Location, ProjectFiles};
use re_render::Image;
use re_types::{MaterialDesc, TextureData, TextureFormat};

use crate::assets::AssetKind;
use crate::codes;

/// A thumbnail's longest side, in pixels.
pub const SIZE: u32 = 96;

/// The most thumbnails kept as egui textures. None is larger than
/// [`SIZE`] × [`SIZE`] RGBA, so they take at most 9 MiB.
pub const LIMIT: usize = 256;

/// The version of texture thumbnails: a new one makes them again.
const TEXTURE_VERSION: u32 = 1;

/// The version of material thumbnails.
const MATERIAL_VERSION: u32 = 1;

/// The longest side a material's textures are shrunk to for its sphere, which
/// shows about half of a texture across its width.
const MATERIAL_TEXTURE: u32 = 256;

/// The side of the capture a material's sphere is drawn in: twice the
/// thumbnail's, shrunk after, which smooths the sphere's edge.
pub(crate) const MATERIAL_DRAWN: u32 = 2 * SIZE;

/// True for the kinds that have thumbnails now.
pub fn has_thumbnail(kind: AssetKind) -> bool {
    matches!(kind, AssetKind::Texture | AssetKind::Material)
}

/// What the worker made for one ask.
pub(crate) struct Made {
    /// The picture, or what the preview's renderer is to draw.
    pub picture: Picture,
    /// The files the thumbnail shows besides its own: a material's textures.
    pub uses: Vec<String>,
}

/// A made picture, or a material for the preview's renderer to draw.
pub(crate) enum Picture {
    /// The thumbnail.
    Image(Image),
    /// A material that is not on disk yet.
    Draw(Box<MaterialJob>),
}

/// A material for the preview's renderer to draw as a thumbnail.
#[derive(Debug)]
pub(crate) struct MaterialJob {
    /// The ask it answers.
    pub ask: u64,
    /// The material file.
    pub path: String,
    /// The thumbnail's disk cache key.
    pub key: String,
    /// The material, without its textures.
    pub desc: MaterialDesc,
    /// Its textures, shrunk, by slot: 0 base colour, 1 metallic-roughness,
    /// 2 normal.
    pub textures: Vec<(usize, TextureData)>,
    /// The textures' files.
    pub uses: Vec<String>,
}

/// Makes the thumbnail of `path`, of `kind` (a texture or a material),
/// reading the project through `files` and the disk cache in `dir`. Says
/// why when it cannot.
pub(crate) fn make(
    files: &ProjectFiles,
    dir: &Path,
    path: &str,
    kind: AssetKind,
) -> Result<Made, String> {
    match kind {
        AssetKind::Texture => texture(files, dir, path).map(|image| Made {
            picture: Picture::Image(image),
            uses: Vec::new(),
        }),
        AssetKind::Material => material(files, dir, path),
        other => Err(format!("{} assets have no thumbnails", other.name())),
    }
}

/// The key of a thumbnail made by `kind`, at `version`, from `sources`.
fn key(kind: &str, version: u32, sources: &[&[u8]]) -> String {
    re_assets::cache_key(kind, version, &SIZE.to_le_bytes(), sources)
        .to_hex()
        .to_string()
}

/// The thumbnail on disk under `key`, unless it cannot be read or is not one.
fn cached(dir: &Path, key: &str) -> Option<Image> {
    let image = Image::load_png(&dir.join(format!("{key}.png"))).ok()?;
    let fits = (1..=SIZE).contains(&image.width) && (1..=SIZE).contains(&image.height);
    fits.then_some(image)
}

/// Writes `image` to the disk cache under `key`, whole or not at all (another
/// editor of the project may read it). The cache is a convenience: a
/// thumbnail that cannot be written is made again next time.
pub(crate) fn store(dir: &Path, key: &str, image: &Image) {
    let part = dir.join(format!("{key}.png.part"));
    if image.save_png(&part).is_ok()
        && std::fs::rename(&part, dir.join(format!("{key}.png"))).is_err()
    {
        let _ = std::fs::remove_file(&part);
    }
}

fn texture(files: &ProjectFiles, dir: &Path, path: &str) -> Result<Image, String> {
    let bytes = files.read_source(path).map_err(|e| e.to_string())?;
    let key = key("thumbnail/texture", TEXTURE_VERSION, &[&bytes]);
    if let Some(image) = cached(dir, &key) {
        return Ok(image);
    }
    let data = re_assets::decode_image(&bytes, TextureFormat::Rgba8Srgb, path, path)
        .map_err(|d| d.message)?;
    let image = shrink(
        &Image {
            width: data.width,
            height: data.height,
            rgba: data.pixels,
        },
        SIZE,
        Mode::Colour,
    );
    store(dir, &key, &image);
    Ok(image)
}

fn material(files: &ProjectFiles, dir: &Path, path: &str) -> Result<Made, String> {
    let bytes = files.read_source(path).map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes);
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("invalid material: {e}"))?;
    let file = re_assets::check_material(&value)?;
    let mut named = Vec::new();
    for (slot, texture) in [
        &file.base_color_texture,
        &file.metallic_roughness_texture,
        &file.normal_texture,
    ]
    .into_iter()
    .enumerate()
    {
        let Some(texture) = texture else {
            continue;
        };
        let rel = re_assets::normalize_path(texture).map_err(|d| d.message)?;
        let bytes = files
            .read_source(&rel)
            .map_err(|e| format!("its texture {rel}: {e}"))?;
        named.push((slot, rel, bytes));
    }
    let uses: Vec<String> = named.iter().map(|(_, rel, _)| rel.clone()).collect();
    let mut sources: Vec<&[u8]> = vec![&bytes];
    sources.extend(named.iter().map(|(_, _, b)| &b[..]));
    let key = key("thumbnail/material", MATERIAL_VERSION, &sources);
    if let Some(image) = cached(dir, &key) {
        return Ok(Made {
            picture: Picture::Image(image),
            uses,
        });
    }
    let mut textures = Vec::new();
    for (slot, rel, bytes) in &named {
        // Base colour is sRGB; the other maps are linear data.
        let (format, mode) = if *slot == 0 {
            (TextureFormat::Rgba8Srgb, Mode::Colour)
        } else {
            (TextureFormat::Rgba8Unorm, Mode::Data)
        };
        let data = re_assets::decode_image(bytes, format, rel, rel).map_err(|d| d.message)?;
        let small = shrink(
            &Image {
                width: data.width,
                height: data.height,
                rgba: data.pixels,
            },
            MATERIAL_TEXTURE,
            mode,
        );
        textures.push((
            *slot,
            TextureData {
                width: small.width,
                height: small.height,
                format,
                pixels: small.rgba,
                levels: Vec::new(),
            },
        ));
    }
    // Made as the loader makes it, but with no texture to look up: that
    // would write the textures' sidecars.
    let bare = re_assets::MaterialFile {
        base_color_texture: None,
        metallic_roughness_texture: None,
        normal_texture: None,
        ..file
    };
    let desc = re_assets::material_desc(files, path, &text, &bare).map_err(|d| d.message)?;
    Ok(Made {
        picture: Picture::Draw(Box::new(MaterialJob {
            ask: 0,
            path: path.to_string(),
            key,
            desc,
            textures,
            uses: uses.clone(),
        })),
        uses,
    })
}

/// How [`shrink`] averages pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// sRGB colour with alpha: averaged in linear light, weighted by alpha.
    Colour,
    /// Linear data (normals, roughness): averaged as it is.
    Data,
}

/// `image` made to fit in `max` × `max` pixels, keeping its shape: each
/// pixel is the mean of those it covers. An image that already fits is
/// returned as it is.
pub(crate) fn shrink(image: &Image, max: u32, mode: Mode) -> Image {
    let (w, h) = (image.width, image.height);
    if w <= max && h <= max {
        return image.clone();
    }
    let long = w.max(h) as u64;
    let side = |s: u32| ((s as u64 * max as u64 + long / 2) / long).clamp(1, max as u64) as u32;
    let (ow, oh) = (side(w), side(h));
    // The source pixels `[from, to)` along an axis of `size` that output
    // pixel `i` of `out` covers.
    let span = |i: u32, out: u32, size: u32| {
        let from = (i as u64 * size as u64 / out as u64) as u32;
        let to = (((i + 1) as u64 * size as u64) / out as u64) as u32;
        (from, to.max(from + 1))
    };
    let lut = srgb_lut();
    let mut rgba = vec![0u8; (ow * oh * 4) as usize];
    for oy in 0..oh {
        let (y0, y1) = span(oy, oh, h);
        for ox in 0..ow {
            let (x0, x1) = span(ox, ow, w);
            let mut sum = [0f32; 4];
            for y in y0..y1 {
                let row = &image.rgba[((y * w + x0) * 4) as usize..((y * w + x1) * 4) as usize];
                for p in row.as_chunks::<4>().0 {
                    match mode {
                        Mode::Colour => {
                            let a = f32::from(p[3]) / 255.0;
                            for c in 0..3 {
                                sum[c] += lut[p[c] as usize] * a;
                            }
                            sum[3] += a;
                        }
                        Mode::Data => {
                            for c in 0..4 {
                                sum[c] += f32::from(p[c]);
                            }
                        }
                    }
                }
            }
            let n = ((y1 - y0) * (x1 - x0)) as f32;
            let out = &mut rgba[((oy * ow + ox) * 4) as usize..][..4];
            match mode {
                Mode::Colour => {
                    let a = sum[3];
                    for c in 0..3 {
                        out[c] = if a > 0.0 {
                            re_render::capture::linear_to_srgb_u8(sum[c] / a)
                        } else {
                            0
                        };
                    }
                    out[3] = (a / n * 255.0).round() as u8;
                }
                Mode::Data => {
                    for c in 0..4 {
                        out[c] = (sum[c] / n).round() as u8;
                    }
                }
            }
        }
    }
    Image {
        width: ow,
        height: oh,
        rgba,
    }
}

/// Each sRGB byte's value in linear light.
fn srgb_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        std::array::from_fn(|i| {
            let c = i as f32 / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                libm::powf((c + 0.055) / 1.055, 2.4)
            }
        })
    })
}

/// A thumbnail shown as an egui texture.
struct Shown {
    texture: egui::TextureHandle,
    size: egui::Vec2,
    /// The files it shows besides its own.
    uses: Vec<String>,
    /// The frame it was last drawn in.
    drawn: Cell<u64>,
}

/// An ask sent to the worker: its number and the asset's kind.
#[derive(Clone, Copy, Debug)]
struct Ask {
    number: u64,
    kind: AssetKind,
}

/// The thumbnails, as the [`AssetIndex`](crate::AssetIndex) keeps them on
/// the UI thread: those shown, those asked for, and those that cannot be
/// made. They are asked for while the panel draws (through a shared
/// reference, so the asks and the marks of what was drawn are cells).
#[derive(Default)]
pub(crate) struct Thumbs {
    shown: HashMap<String, Shown>,
    /// Made, and waiting for an egui context to become textures, with the
    /// files each shows besides its own.
    ready: Vec<(String, Image, Vec<String>)>,
    /// Asked of the worker (or of the preview's renderer) and not made yet.
    asked: HashMap<String, Ask>,
    /// Asked for while drawing, sent with the next poll.
    wanted: RefCell<BTreeMap<String, AssetKind>>,
    /// Those that cannot be made, until their file changes.
    failed: HashMap<String, AssetKind>,
    /// Materials waiting for the preview's renderer.
    to_draw: VecDeque<MaterialJob>,
    /// Notes (`RE8853`) for the console.
    notes: Vec<Diagnostic>,
    /// The last ask's number.
    asks: u64,
    /// The worker's answers still due.
    due: usize,
    /// Counts frames, for the one drawn least recently.
    frame: u64,
    /// How many were drawn this frame.
    drawn: Cell<usize>,
}

impl std::fmt::Debug for Thumbs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Thumbs")
            .field("shown", &self.shown.len())
            .field("asked", &self.asked.len())
            .field("failed", &self.failed.len())
            .field("due", &self.due)
            .finish_non_exhaustive()
    }
}

impl Thumbs {
    /// The thumbnail of the asset at `path`, of `kind`, to draw now: its
    /// texture and its size in pixels. Asks for it when it is not made yet
    /// and there is room for it.
    pub(crate) fn get(&self, path: &str, kind: AssetKind) -> Option<(egui::TextureId, egui::Vec2)> {
        if !has_thumbnail(kind) {
            return None;
        }
        if let Some(s) = self.shown.get(path) {
            if s.drawn.replace(self.frame) != self.frame {
                self.drawn.set(self.drawn.get() + 1);
            }
            return Some((s.texture.id(), s.size));
        }
        if self.asked.contains_key(path) || self.failed.contains_key(path) {
            return None;
        }
        let mut wanted = self.wanted.borrow_mut();
        // Room for it without putting away one drawn this frame.
        if self.drawn.get() + self.asked.len() + wanted.len() < LIMIT {
            wanted.insert(path.to_string(), kind);
        }
        None
    }

    /// True when the thumbnail of `path` is shown.
    pub(crate) fn is_shown(&self, path: &str) -> bool {
        self.shown.contains_key(path)
    }

    /// The worker's answers still due.
    pub(crate) fn due(&self) -> usize {
        self.due
    }

    /// Starts a frame, and takes the asks made while drawing the last one,
    /// numbered.
    pub(crate) fn begin_frame(&mut self) -> Vec<(u64, String, AssetKind)> {
        self.frame += 1;
        self.drawn.set(0);
        let wanted = std::mem::take(self.wanted.get_mut());
        let mut asks = Vec::with_capacity(wanted.len());
        for (path, kind) in wanted {
            self.asks += 1;
            self.asked.insert(
                path.clone(),
                Ask {
                    number: self.asks,
                    kind,
                },
            );
            asks.push((self.asks, path, kind));
        }
        self.due += asks.len();
        asks
    }

    /// Takes in the worker's answer to ask `number`, for `path`. An answer to
    /// an ask made before the file changed is dropped.
    pub(crate) fn answered(&mut self, number: u64, path: String, made: Result<Made, String>) {
        self.due = self.due.saturating_sub(1);
        let Some(ask) = self
            .asked
            .get(&path)
            .copied()
            .filter(|a| a.number == number)
        else {
            return;
        };
        match made {
            Ok(Made {
                picture: Picture::Image(image),
                uses,
            }) => {
                self.asked.remove(&path);
                self.ready.push((path, image, uses));
            }
            Ok(Made {
                picture: Picture::Draw(mut job),
                ..
            }) => {
                job.ask = number;
                self.to_draw.push_back(*job);
            }
            Err(why) => self.cannot(path, ask.kind, &why),
        }
    }

    /// The next material for the preview's renderer to draw.
    pub(crate) fn next_material(&mut self) -> Option<MaterialJob> {
        while let Some(job) = self.to_draw.pop_front() {
            if self
                .asked
                .get(&job.path)
                .is_some_and(|a| a.number == job.ask)
            {
                return Some(job);
            }
        }
        None
    }

    /// Takes in the material the preview's renderer drew for `job`: the
    /// picture, to keep and to write to the disk cache under the key
    /// returned, or why it could not be drawn.
    pub(crate) fn material_drawn(
        &mut self,
        job: &MaterialJob,
        drawn: Result<Image, String>,
    ) -> Option<(String, Image)> {
        if !self
            .asked
            .get(&job.path)
            .is_some_and(|a| a.number == job.ask)
        {
            return None;
        }
        match drawn {
            Ok(image) => {
                self.asked.remove(&job.path);
                self.ready
                    .push((job.path.clone(), image.clone(), job.uses.clone()));
                Some((job.key.clone(), image))
            }
            Err(why) => {
                self.cannot(job.path.clone(), AssetKind::Material, &why);
                None
            }
        }
    }

    fn cannot(&mut self, path: String, kind: AssetKind, why: &str) {
        self.asked.remove(&path);
        self.notes.push(
            Diagnostic::warning(codes::THUMBNAIL, format!("no thumbnail for {path}: {why}"))
                .at(Location::file(path.clone()))
                .with_hint(
                    "the Assets panel shows its kind's icon; the thumbnail is made again when \
                 the file changes",
                ),
        );
        self.failed.insert(path, kind);
    }

    /// Makes egui textures of the thumbnails made since the last frame,
    /// putting away those drawn least recently past [`LIMIT`], and returns
    /// the notes of those that could not be made.
    pub(crate) fn upload(&mut self, ctx: &egui::Context) -> Vec<Diagnostic> {
        for (path, image, uses) in std::mem::take(&mut self.ready) {
            if self.shown.len() >= LIMIT && !self.shown.contains_key(&path) {
                let oldest = self
                    .shown
                    .iter()
                    .min_by_key(|(_, s)| s.drawn.get())
                    .map(|(p, _)| p.clone());
                if let Some(p) = oldest {
                    self.shown.remove(&p);
                }
            }
            let size = [image.width as usize, image.height as usize];
            let pixels = egui::ColorImage::from_rgba_unmultiplied(size, &image.rgba);
            // A small texture is drawn larger, without blurring it.
            let options = if image.width.max(image.height) < SIZE {
                egui::TextureOptions::NEAREST
            } else {
                egui::TextureOptions::LINEAR
            };
            let texture = ctx.load_texture(format!("re_editor.thumb {path}"), pixels, options);
            self.shown.insert(
                path,
                Shown {
                    texture,
                    size: egui::vec2(image.width as f32, image.height as f32),
                    uses,
                    drawn: Cell::new(self.frame),
                },
            );
        }
        std::mem::take(&mut self.notes)
    }

    /// Forgets the thumbnails of the files at and under `path` and of the
    /// materials that show one of them, which changed: they are asked for
    /// again when drawn. A changed texture also lets the materials that could
    /// not be made try again.
    pub(crate) fn forget(&mut self, path: &str) {
        let under = format!("{path}/");
        let hit = |p: &str| p == path || p.starts_with(&under);
        let texture = AssetKind::of_path(path) == Some(AssetKind::Texture)
            || AssetKind::of_path(path).is_none();
        self.shown
            .retain(|p, s| !hit(p) && !s.uses.iter().any(|u| hit(u)));
        self.ready
            .retain(|(p, _, uses)| !hit(p) && !uses.iter().any(|u| hit(u)));
        self.asked
            .retain(|p, a| !hit(p) && !(texture && a.kind == AssetKind::Material));
        self.failed
            .retain(|p, kind| !hit(p) && !(texture && *kind == AssetKind::Material));
        self.wanted.get_mut().retain(|p, _| !hit(p));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Image {
        Image {
            width,
            height,
            rgba: rgba.repeat((width * height) as usize),
        }
    }

    #[test]
    fn shrinking_keeps_the_shape_and_averages_in_linear_light() {
        let small = solid(4, 2, [10, 20, 30, 255]);
        assert_eq!(shrink(&small, SIZE, Mode::Colour), small, "it already fits");

        let wide = shrink(&solid(300, 150, [200, 100, 50, 255]), SIZE, Mode::Colour);
        assert_eq!((wide.width, wide.height), (96, 48));
        assert_eq!(wide.pixel(40, 20), [200, 100, 50, 255]);
        let tall = shrink(&solid(10, 1000, [1, 2, 3, 4]), SIZE, Mode::Data);
        assert_eq!((tall.width, tall.height), (1, 96));

        // Black and white columns average to the sRGB of linear 0.5, not 128.
        let mut stripes = solid(192, 2, [0, 0, 0, 255]);
        for px in stripes.rgba.as_chunks_mut::<8>().0 {
            px[4..7].copy_from_slice(&[255, 255, 255]);
        }
        let grey = shrink(&stripes, SIZE, Mode::Colour);
        assert_eq!((grey.width, grey.height), (96, 1));
        assert_eq!(grey.pixel(10, 0), [188, 188, 188, 255]);
        let data = shrink(&stripes, SIZE, Mode::Data);
        assert_eq!(data.pixel(10, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn transparent_pixels_do_not_darken_their_neighbours() {
        let mut half = solid(192, 1, [0, 0, 0, 0]);
        for px in half.rgba.as_chunks_mut::<8>().0 {
            px[..4].copy_from_slice(&[255, 0, 0, 255]);
        }
        let out = shrink(&half, SIZE, Mode::Colour);
        assert_eq!(out.pixel(5, 0), [255, 0, 0, 128]);
    }

    #[test]
    fn asks_are_numbered_and_an_answer_to_an_old_one_is_dropped() {
        let mut thumbs = Thumbs::default();
        assert!(thumbs.get("a.png", AssetKind::Texture).is_none());
        assert!(
            thumbs.get("a.png", AssetKind::Texture).is_none(),
            "asked once"
        );
        assert!(thumbs.get("m.material.json", AssetKind::Material).is_none());
        assert!(
            thumbs.get("s.luau", AssetKind::Script).is_none(),
            "no thumbnail"
        );
        let asks = thumbs.begin_frame();
        assert_eq!(
            asks,
            [
                (1, "a.png".to_string(), AssetKind::Texture),
                (2, "m.material.json".to_string(), AssetKind::Material)
            ]
        );
        assert_eq!(thumbs.due(), 2);
        assert!(thumbs.begin_frame().is_empty(), "not asked again");

        // A thumbnail that cannot be made is noted once and not asked for
        // again.
        thumbs.answered(2, "m.material.json".into(), Err("bad".into()));
        let ctx = egui::Context::default();
        let notes = thumbs.upload(&ctx);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].code, codes::THUMBNAIL);
        assert!(thumbs.get("m.material.json", AssetKind::Material).is_none());
        assert!(thumbs.begin_frame().is_empty(), "not tried again");

        // The texture changed after it was asked for, so the answer is of its
        // old bytes; the material may show it, so it is tried again.
        thumbs.forget("a.png");
        let image = solid(2, 2, [1, 2, 3, 255]);
        let made = |image: &Image| {
            Ok(Made {
                picture: Picture::Image(image.clone()),
                uses: Vec::new(),
            })
        };
        thumbs.answered(1, "a.png".into(), made(&image));
        assert!(thumbs.ready.is_empty());
        assert!(thumbs.get("a.png", AssetKind::Texture).is_none());
        assert!(thumbs.get("m.material.json", AssetKind::Material).is_none());
        assert_eq!(
            thumbs.begin_frame(),
            [
                (3, "a.png".to_string(), AssetKind::Texture),
                (4, "m.material.json".to_string(), AssetKind::Material)
            ]
        );
        thumbs.answered(3, "a.png".into(), made(&image));
        assert!(thumbs.upload(&ctx).is_empty());
        let (_, size) = thumbs.get("a.png", AssetKind::Texture).expect("shown");
        assert_eq!(size, egui::vec2(2.0, 2.0));
        assert_eq!(thumbs.due(), 1, "the material's answer");
    }
}
