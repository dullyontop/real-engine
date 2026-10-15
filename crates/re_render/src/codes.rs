//! The diagnostic codes `re_render` owns (range `RE6000`–`RE6999`).
//!
//! Each code has an explain page in `crates/re_render/explain/` and an entry in
//! [`DIAGNOSTICS`](crate::DIAGNOSTICS). A code is never reused for a different
//! meaning (ADR-0004).

use re_core::Code;

/// wgpu reported an error that no error scope caught (validation, out of memory or
/// an internal error). It is logged instead of panicking.
pub const GPU_ERROR: Code = Code::new("RE6001");

/// No GPU adapter matched the requested backends.
pub const NO_ADAPTER: Code = Code::new("RE6002");

/// The adapter refused to create a device.
pub const DEVICE_FAILED: Code = Code::new("RE6003");

/// A window surface could not be created or has no usable configuration.
pub const SURFACE_FAILED: Code = Code::new("RE6004");

/// A render target or capture size is zero, too large, or of a format that cannot be
/// rendered to.
pub const INVALID_TARGET: Code = Code::new("RE6005");

/// `RE_BACKENDS` names an unknown backend.
pub const UNKNOWN_BACKEND: Code = Code::new("RE6006");

/// The snapshot's camera cannot be drawn from (field of view, clip distances or
/// transform out of range).
pub const INVALID_CAMERA: Code = Code::new("RE6007");

/// A mesh handle was released, or never issued by this renderer.
pub const STALE_MESH: Code = Code::new("RE6010");

/// A texture handle was released, or never issued by this renderer.
pub const STALE_TEXTURE: Code = Code::new("RE6011");

/// A material handle was released, or never issued by this renderer.
pub const STALE_MATERIAL: Code = Code::new("RE6012");

/// A material still names a texture asset instead of an uploaded texture handle.
pub const UNRESOLVED_TEXTURE: Code = Code::new("RE6013");

/// Mesh data is malformed (indices out of range, a partial triangle, non-finite
/// positions or too many vertices).
pub const BAD_MESH: Code = Code::new("RE6014");

/// Texture data is malformed (zero or oversized dimensions, or a pixel buffer of
/// the wrong length).
pub const BAD_TEXTURE: Code = Code::new("RE6015");

/// A material has non-finite or out-of-range values.
pub const BAD_MATERIAL: Code = Code::new("RE6016");

/// A frame has more lights than the forward pass draws; the extra lights are ignored.
pub const TOO_MANY_LIGHTS: Code = Code::new("RE6017");

/// A frame has more highlights than the renderer draws; the later ones are left out.
pub const TOO_MANY_HIGHLIGHTS: Code = Code::new("RE6018");

/// Reading a texture back to the CPU failed.
pub const READBACK_FAILED: Code = Code::new("RE6020");

/// An image file could not be written.
pub const IMAGE_WRITE_FAILED: Code = Code::new("RE6021");

/// An image file could not be read or decoded.
pub const IMAGE_READ_FAILED: Code = Code::new("RE6022");

/// Two images that were compared have different sizes.
pub const IMAGE_SIZE_MISMATCH: Code = Code::new("RE6023");

/// A WGSL `#include` names an unknown file or forms a cycle.
pub const SHADER_INCLUDE: Code = Code::new("RE6030");
