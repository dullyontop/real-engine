//! Texture -> buffer -> CPU readback (respecting the 256-byte row alignment) and PNG output.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use crate::{Result, SpikeError};

/// `bytes_per_row` for `copy_texture_to_buffer` must be a multiple of
/// `wgpu::COPY_BYTES_PER_ROW_ALIGNMENT` (256).
pub fn padded_bytes_per_row(width: u32, bytes_per_pixel: u32) -> u32 {
    let unpadded = width * bytes_per_pixel;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    unpadded.div_ceil(align) * align
}

/// Encode the copy of one aspect of mip 0 of a 2D texture into a new MAP_READ buffer.
/// Returns the buffer and its padded row pitch.
pub fn encode_texture_copy(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    aspect: wgpu::TextureAspect,
) -> Result<(wgpu::Buffer, u32)> {
    let size = texture.size();
    let format = texture.format();
    let bpp = format.block_copy_size(Some(aspect)).ok_or_else(|| {
        SpikeError::Other(format!(
            "format {format:?} aspect {aspect:?} has no block copy size"
        ))
    })?;
    let padded = padded_bytes_per_row(size.width, bpp);
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: u64::from(padded) * u64::from(size.height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
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
                rows_per_image: Some(size.height),
            },
        },
        wgpu::Extent3d {
            width: size.width,
            height: size.height,
            depth_or_array_layers: 1,
        },
    );
    Ok((buffer, padded))
}

/// Map a buffer for reading, block until it is mapped, strip the row padding.
pub fn map_and_unpad(
    device: &wgpu::Device,
    buffer: &wgpu::Buffer,
    submission: Option<wgpu::SubmissionIndex>,
    padded_bytes_per_row: u32,
    unpadded_bytes_per_row: u32,
    rows: u32,
) -> Result<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    buffer.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = tx.send(result);
    });
    // Native wgpu only runs map callbacks from poll()/submit(). Wait for the copy's
    // submission (or the latest one) with a timeout so a broken driver cannot hang CI.
    device.poll(wgpu::PollType::Wait {
        submission_index: submission,
        timeout: Some(Duration::from_secs(60)),
    })?;
    rx.recv()
        .map_err(|e| SpikeError::Other(format!("map callback never ran: {e}")))??;

    let mut out = Vec::with_capacity((unpadded_bytes_per_row * rows) as usize);
    {
        let view = buffer.get_mapped_range(..)?; // wgpu 30: returns Result<BufferView, MapRangeError>
        for row in 0..rows as usize {
            let start = row * padded_bytes_per_row as usize;
            out.extend_from_slice(&view[start..start + unpadded_bytes_per_row as usize]);
        }
    } // BufferView must be dropped before unmap()
    buffer.unmap();
    Ok(out)
}

/// Read back one aspect of mip 0 of a 2D texture as tightly packed rows.
pub fn read_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    aspect: wgpu::TextureAspect,
) -> Result<Vec<u8>> {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback-encoder"),
    });
    let (buffer, padded) = encode_texture_copy(device, &mut encoder, texture, aspect)?;
    let index = queue.submit([encoder.finish()]);
    let size = texture.size();
    let bpp = texture.format().block_copy_size(Some(aspect)).unwrap_or(4);
    map_and_unpad(
        device,
        &buffer,
        Some(index),
        padded,
        size.width * bpp,
        size.height,
    )
}

/// Write tightly packed RGBA8 pixels as a PNG (8-bit, sRGB as PNG assumes).
pub fn write_png_rgba8(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::File::create(path)?;
    let w = std::io::BufWriter::new(file);
    let mut enc = png::Encoder::new(w, width, height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(())
}

/// Convert BGRA8 to RGBA8 in place (for Bgra8 surfaces/targets).
pub fn bgra_to_rgba(pixels: &mut [u8]) {
    for px in pixels.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
}

/// Simple image statistics used for "is the image non-trivial" assertions.
#[derive(Debug, Clone)]
pub struct ImageStats {
    pub width: u32,
    pub height: u32,
    pub center: [u8; 4],
    pub distinct_colors: usize,
    pub pixels_differing_from_clear: usize,
    pub luma_min: f32,
    pub luma_max: f32,
    pub luma_stddev_non_clear: f32,
}

pub fn luma(px: &[u8]) -> f32 {
    0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2])
}

pub fn image_stats(width: u32, height: u32, rgba: &[u8], clear_rgba8: [u8; 4]) -> ImageStats {
    let idx = |x: u32, y: u32| ((y * width + x) * 4) as usize;
    let c = idx(width / 2, height / 2);
    let center = [rgba[c], rgba[c + 1], rgba[c + 2], rgba[c + 3]];
    let mut set = std::collections::HashSet::new();
    let mut differing = 0usize;
    let (mut lmin, mut lmax) = (f32::MAX, f32::MIN);
    let (mut sum, mut sum2, mut n) = (0f64, 0f64, 0f64);
    for px in rgba.as_chunks::<4>().0 {
        set.insert([px[0], px[1], px[2], px[3]]);
        let l = luma(px);
        lmin = lmin.min(l);
        lmax = lmax.max(l);
        let close = px
            .iter()
            .zip(clear_rgba8.iter())
            .all(|(a, b)| (i16::from(*a) - i16::from(*b)).abs() <= 2);
        if !close {
            differing += 1;
            sum += f64::from(l);
            sum2 += f64::from(l) * f64::from(l);
            n += 1.0;
        }
    }
    let stddev = if n > 1.0 {
        let mean = sum / n;
        ((sum2 / n - mean * mean).max(0.0)).sqrt() as f32
    } else {
        0.0
    };
    ImageStats {
        width,
        height,
        center,
        distinct_colors: set.len(),
        pixels_differing_from_clear: differing,
        luma_min: lmin,
        luma_max: lmax,
        luma_stddev_non_clear: stddev,
    }
}

/// Linear float color -> sRGB-encoded u8 (what an *Srgb render target stores).
pub fn linear_to_srgb_u8(c: f64) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let s = if c <= 0.003_130_8 {
        12.92 * c
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (s * 255.0).round() as u8
}
