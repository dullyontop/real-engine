//! Offscreen capture: render into a texture, read it back and write a PNG.
//!
//! `copy_texture_to_buffer` needs rows padded to 256 bytes
//! ([`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`]); [`read_texture`] strips the padding, so
//! callers always get tightly packed rows.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use re_core::{Diagnostic, Location};

use crate::codes;

/// How long a readback waits for the GPU before giving up.
const READBACK_TIMEOUT: Duration = Duration::from_secs(60);

/// The padded row size for a texture copy: `width * bytes_per_pixel` rounded up to
/// a multiple of 256.
pub fn padded_bytes_per_row(width: u32, bytes_per_pixel: u32) -> u32 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    (width * bytes_per_pixel).div_ceil(align) * align
}

fn readback_error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error(codes::READBACK_FAILED, message)
}

/// Reads one aspect of mip 0 of a 2D texture back to the CPU as tightly packed rows.
///
/// The texture needs `COPY_SRC` usage. Depth aspects need
/// `DownlevelFlags::DEPTH_TEXTURE_AND_BUFFER_COPIES`, which GL lacks. It blocks until the
/// GPU is done, which a browser never allows: use [`read_texture_async`] there.
pub fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    aspect: wgpu::TextureAspect,
) -> Result<Vec<u8>, Diagnostic> {
    let pending = Readback::start(device, queue, texture, aspect)?;
    // Native wgpu runs map callbacks from poll; the timeout keeps a broken driver
    // from hanging CI.
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(pending.index.clone()),
            timeout: Some(READBACK_TIMEOUT),
        })
        .map_err(|e| readback_error(format!("waiting for the GPU failed: {e}")))?;
    let (lock, ready) = &*pending.state;
    let state = lock.lock().unwrap_or_else(PoisonError::into_inner);
    let (mut state, _) = ready
        .wait_timeout_while(state, READBACK_TIMEOUT, |s| s.result.is_none())
        .unwrap_or_else(PoisonError::into_inner);
    let result = state
        .result
        .take()
        .ok_or_else(|| readback_error("the buffer map callback never ran"))?;
    drop(state);
    pending.finish(result)
}

/// [`read_texture`] without blocking, for the browser (ADR-0049 §3).
///
/// The returned future owns the staging buffer until the map completes: on WebGPU,
/// dropping a buffer destroys it and fails a pending map. In a browser the page's event
/// loop completes the map; on native, someone has to call `device.poll` (for example
/// once a frame) before the future can finish.
pub fn read_texture_async(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    aspect: wgpu::TextureAspect,
) -> impl Future<Output = Result<Vec<u8>, Diagnostic>> + use<> {
    let started = Readback::start(device, queue, texture, aspect);
    async move {
        let pending = started?;
        let result = MapDone(pending.state.clone()).await;
        pending.finish(result)
    }
}

/// What the map callback writes and the reader waits for.
#[derive(Default)]
struct MapState {
    result: Option<Result<(), wgpu::BufferAsyncError>>,
    waker: Option<Waker>,
}

type Shared = Arc<(Mutex<MapState>, Condvar)>;

/// A copy into a mappable buffer, submitted, whose map is pending.
struct Readback {
    buffer: wgpu::Buffer,
    index: wgpu::SubmissionIndex,
    state: Shared,
    width: u32,
    height: u32,
    bpp: u32,
    padded: u32,
}

impl Readback {
    fn start(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        texture: &wgpu::Texture,
        aspect: wgpu::TextureAspect,
    ) -> Result<Readback, Diagnostic> {
        let format = texture.format();
        let bpp = format.block_copy_size(Some(aspect)).ok_or_else(|| {
            readback_error(format!(
                "the {aspect:?} aspect of a {format:?} texture cannot be copied"
            ))
        })?;
        let (width, height) = (texture.width(), texture.height());
        let padded = padded_bytes_per_row(width, bpp);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("re_render readback"),
            size: u64::from(padded) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("re_render readback"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let state: Shared = Arc::default();
        let callback_state = state.clone();
        encoder.map_buffer_on_submit(&buffer, wgpu::MapMode::Read, .., move |result| {
            let (lock, ready) = &*callback_state;
            let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
            state.result = Some(result);
            if let Some(waker) = state.waker.take() {
                waker.wake();
            }
            ready.notify_all();
        });
        let index = queue.submit([encoder.finish()]);
        Ok(Readback {
            buffer,
            index,
            state,
            width,
            height,
            bpp,
            padded,
        })
    }

    /// Copies the mapped rows out without their padding.
    fn finish(self, result: Result<(), wgpu::BufferAsyncError>) -> Result<Vec<u8>, Diagnostic> {
        result.map_err(|e| readback_error(format!("mapping the readback buffer failed: {e}")))?;
        let row = (self.width * self.bpp) as usize;
        let mut out = Vec::with_capacity(row * self.height as usize);
        {
            let view = self
                .buffer
                .get_mapped_range(..)
                .map_err(|e| readback_error(format!("reading the mapped buffer failed: {e}")))?;
            for chunk in view.chunks(self.padded as usize).take(self.height as usize) {
                out.extend_from_slice(&chunk[..row]);
            }
        }
        self.buffer.unmap();
        Ok(out)
    }
}

/// Resolves when the map callback has run.
struct MapDone(Shared);

impl Future for MapDone {
    type Output = Result<(), wgpu::BufferAsyncError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.0.0.lock().unwrap_or_else(PoisonError::into_inner);
        match state.result.take() {
            Some(result) => Poll::Ready(result),
            None => {
                state.waker = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

/// An RGBA8 image with tightly packed rows, top row first.
///
/// For images read from an sRGB target, the bytes are sRGB-encoded, which is what
/// PNG expects.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl Image {
    /// The RGBA value at `(x, y)`.
    ///
    /// # Panics
    ///
    /// Panics if the pixel is outside the image.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        assert!(
            x < self.width && y < self.height,
            "pixel ({x}, {y}) is outside the image"
        );
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// Writes the image as an 8-bit RGBA PNG, creating parent directories.
    pub fn save_png(&self, path: &Path) -> Result<(), Diagnostic> {
        write_png_rgba8(path, self.width, self.height, &self.rgba)
    }

    /// Reads an 8-bit RGB or RGBA PNG (RGB gets an opaque alpha).
    pub fn load_png(path: &Path) -> Result<Image, Diagnostic> {
        let fail = |what: String| {
            Diagnostic::error(
                codes::IMAGE_READ_FAILED,
                format!("could not read the PNG: {what}"),
            )
            .at(Location::file(path.display().to_string()))
        };
        let file = std::fs::File::open(path).map_err(|e| fail(e.to_string()))?;
        let mut decoder = png::Decoder::new(std::io::BufReader::new(file));
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().map_err(|e| fail(e.to_string()))?;
        let size = reader
            .output_buffer_size()
            .ok_or_else(|| fail("the image is too large".into()))?;
        let mut buf = vec![0; size];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| fail(e.to_string()))?;
        buf.truncate(info.buffer_size());
        if info.bit_depth != png::BitDepth::Eight {
            return Err(fail(format!(
                "{:?} bits per channel (expected 8)",
                info.bit_depth
            )));
        }
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => buf
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            other => {
                return Err(fail(format!(
                    "colour type {other:?} (expected RGB or RGBA)"
                )));
            }
        };
        Ok(Image {
            width: info.width,
            height: info.height,
            rgba,
        })
    }

    /// Summary statistics, with `background` as the colour to count differences from.
    pub fn stats(&self, background: [u8; 4]) -> ImageStats {
        image_stats(self.width, self.height, &self.rgba, background)
    }
}

/// Writes tightly packed RGBA8 pixels as a PNG, creating parent directories.
pub fn write_png_rgba8(
    path: &Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> Result<(), Diagnostic> {
    let fail = |what: String| {
        Diagnostic::error(
            codes::IMAGE_WRITE_FAILED,
            format!("could not write the PNG: {what}"),
        )
        .at(Location::file(path.display().to_string()))
    };
    if rgba.len() as u64 != u64::from(width) * u64::from(height) * 4 {
        return Err(fail(format!(
            "{} bytes of pixels for a {width}x{height} image",
            rgba.len()
        )));
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| fail(e.to_string()))?;
    }
    let file = std::fs::File::create(path).map_err(|e| fail(e.to_string()))?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| fail(e.to_string()))?;
    writer
        .write_image_data(rgba)
        .map_err(|e| fail(e.to_string()))?;
    writer.finish().map_err(|e| fail(e.to_string()))
}

/// Simple image statistics, for "is the image plausible" checks.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageStats {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The centre pixel.
    pub center: [u8; 4],
    /// The number of distinct RGBA values.
    pub distinct_colors: usize,
    /// Pixels further than 2 levels from the background colour in some channel.
    pub pixels_differing_from_background: usize,
    /// Mean Rec. 709 luma of all pixels, on the 0–255 scale of the stored bytes.
    pub luma_mean: f32,
    /// Standard deviation of the luma of the pixels that differ from the background.
    pub luma_stddev_foreground: f32,
}

/// Rec. 709 luma of an RGBA8 pixel's stored bytes (0–255).
pub fn luma(px: [u8; 4]) -> f32 {
    0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2])
}

/// Statistics over tightly packed RGBA8 pixels.
pub fn image_stats(width: u32, height: u32, rgba: &[u8], background: [u8; 4]) -> ImageStats {
    let pixels = rgba.as_chunks::<4>().0;
    let c = (height / 2 * width + width / 2) as usize;
    let center = pixels.get(c).copied().unwrap_or_default();
    let mut distinct = std::collections::HashSet::new();
    let mut differing = 0usize;
    let (mut total, mut sum, mut sum2) = (0f64, 0f64, 0f64);
    for &px in pixels {
        distinct.insert(px);
        let l = f64::from(luma(px));
        total += l;
        let close = px.iter().zip(background).all(|(a, b)| a.abs_diff(b) <= 2);
        if !close {
            differing += 1;
            sum += l;
            sum2 += l * l;
        }
    }
    let n = differing as f64;
    let stddev = if differing > 1 {
        let mean = sum / n;
        (sum2 / n - mean * mean).max(0.0).sqrt()
    } else {
        0.0
    };
    ImageStats {
        width,
        height,
        center,
        distinct_colors: distinct.len(),
        pixels_differing_from_background: differing,
        luma_mean: if pixels.is_empty() {
            0.0
        } else {
            (total / pixels.len() as f64) as f32
        },
        luma_stddev_foreground: stddev as f32,
    }
}

/// A linear colour channel encoded as an sRGB byte, as an sRGB target stores it.
pub fn linear_to_srgb_u8(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * libm::powf(c, 1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}

/// An offscreen colour target that can be rendered to, shown in egui and read back.
///
/// The default format is [`Capture::FORMAT`] (sRGB): the renderer writes linear
/// colour and the stored bytes are what a PNG expects.
#[derive(Debug)]
pub struct Capture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Capture {
    /// The default capture format.
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    /// A `width` x `height` capture in [`Capture::FORMAT`].
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Result<Capture, Diagnostic> {
        Self::with_format(device, width, height, Self::FORMAT)
    }

    /// A capture in an RGBA8 or BGRA8 format, sRGB or not. A non-sRGB capture holds
    /// gamma-encoded colour (the renderer encodes it), which is what egui expects
    /// of a texture it shows, on every backend.
    pub fn with_format(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Result<Capture, Diagnostic> {
        let max = device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > max || height > max {
            return Err(Diagnostic::error(
                codes::INVALID_TARGET,
                format!("a {width}x{height} capture is not possible: sizes go from 1 to {max}"),
            ));
        }
        if !matches!(
            format.remove_srgb_suffix(),
            wgpu::TextureFormat::Rgba8Unorm | wgpu::TextureFormat::Bgra8Unorm
        ) {
            return Err(Diagnostic::error(
                codes::INVALID_TARGET,
                format!("captures are RGBA8 or BGRA8, not {format:?}"),
            ));
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("re_render capture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Ok(Capture { texture, view })
    }

    /// The view to render into.
    pub fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    /// The texture.
    pub fn texture(&self) -> &wgpu::Texture {
        &self.texture
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.texture.width()
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.texture.height()
    }

    /// Reads the capture back as RGBA8 (BGRA formats are swizzled).
    pub fn read(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<Image, Diagnostic> {
        let mut rgba = read_texture(device, queue, &self.texture, wgpu::TextureAspect::All)?;
        if self.texture.format().remove_srgb_suffix() == wgpu::TextureFormat::Bgra8Unorm {
            swap_red_blue(&mut rgba);
        }
        Ok(Image {
            width: self.width(),
            height: self.height(),
            rgba,
        })
    }

    /// [`Capture::read`] without blocking, for the browser (see [`read_texture_async`]).
    pub fn read_async(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
    ) -> impl Future<Output = Result<Image, Diagnostic>> + use<> {
        let pending = read_texture_async(device, queue, &self.texture, wgpu::TextureAspect::All);
        let (width, height) = (self.width(), self.height());
        let bgra = self.texture.format().remove_srgb_suffix() == wgpu::TextureFormat::Bgra8Unorm;
        async move {
            let mut rgba = pending.await?;
            if bgra {
                swap_red_blue(&mut rgba);
            }
            Ok(Image {
                width,
                height,
                rgba,
            })
        }
    }

    /// Reads the capture back and writes it to `path` as a PNG.
    pub fn save_png(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        path: &Path,
    ) -> Result<Image, Diagnostic> {
        let image = self.read(device, queue)?;
        image.save_png(path)?;
        Ok(image)
    }
}

/// BGRA to RGBA, in place.
fn swap_red_blue(pixels: &mut [u8]) {
    for px in pixels.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_padding_follows_copy_alignment() {
        assert_eq!(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT, 256);
        assert_eq!(padded_bytes_per_row(64, 4), 256);
        assert_eq!(padded_bytes_per_row(150, 4), 768);
        assert_eq!(padded_bytes_per_row(640, 4), 2560);
        assert_eq!(padded_bytes_per_row(1, 4), 256);
    }

    #[test]
    fn png_round_trip() {
        let dir = std::env::temp_dir().join(format!("re_render_png_{}", std::process::id()));
        let path = dir.join("img.png");
        let image = Image {
            width: 3,
            height: 2,
            rgba: (0..24).map(|i| (i * 10) as u8).collect(),
        };
        image.save_png(&path).unwrap();
        assert_eq!(Image::load_png(&path).unwrap(), image);
        let missing = Image::load_png(&dir.join("missing.png")).unwrap_err();
        assert_eq!(missing.code, codes::IMAGE_READ_FAILED);
        let bad = write_png_rgba8(&path, 4, 4, &[0; 3]).unwrap_err();
        assert_eq!(bad.code, codes::IMAGE_WRITE_FAILED);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn srgb_encoding_matches_the_standard_curve() {
        assert_eq!(linear_to_srgb_u8(0.0), 0);
        assert_eq!(linear_to_srgb_u8(1.0), 255);
        assert_eq!(linear_to_srgb_u8(0.5), 188);
        assert_eq!(linear_to_srgb_u8(0.02), 39);
    }
}
