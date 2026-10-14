//! Image decoding with the `image` crate: PNG and JPEG to RGBA8.

use re_core::{Diagnostic, Location};
use re_types::render::{TextureData, TextureFormat};

use crate::codes;

/// Decodes PNG or JPEG bytes (detected from the content) into a texture.
/// 16-bit and grey images are converted to 8-bit RGBA.
///
/// # Errors
///
/// [`codes::IMAGE_INVALID`] for any other format or a corrupt image. `what`
/// names the image in the message (`assets/rock.png`, `image 0 of tree.glb`).
pub fn decode_image(
    bytes: &[u8],
    format: TextureFormat,
    what: &str,
    file: &str,
) -> Result<TextureData, Diagnostic> {
    let invalid = |why: String| {
        Diagnostic::error(codes::IMAGE_INVALID, format!("cannot decode {what}: {why}"))
            .at(Location::file(file))
            .with_hint("textures must be PNG or JPEG")
    };
    let kind = image::guess_format(bytes).map_err(|e| invalid(e.to_string()))?;
    if !matches!(kind, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Err(invalid(format!("{kind:?} images are not supported")));
    }
    let img = image::load_from_memory_with_format(bytes, kind)
        .map_err(|e| invalid(e.to_string()))?
        .into_rgba8();
    Ok(TextureData {
        width: img.width(),
        height: img.height(),
        format,
        pixels: img.into_raw(),
    })
}
