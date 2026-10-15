//! [`GeomError`], converted to a [`Diagnostic`] at the crate boundary.

use re_core::Diagnostic;

use crate::codes;

/// Why a geometry operation failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum GeomError {
    /// A heightfield's size, samples, spacing or offset are not usable (`RE4500`).
    #[error("invalid heightfield: {0}")]
    Heightfield(String),
    /// A primitive's size or tessellation is out of range (`RE4501`).
    #[error("invalid {shape}: {message}")]
    Primitive {
        /// The primitive (`box`, `sphere`, …).
        shape: &'static str,
        /// What is wrong.
        message: String,
    },
    /// A merged mesh would have too many vertices (`RE4502`).
    #[error("merged mesh would have {0} vertices, more than a u32 index can name")]
    TooLarge(u64),
}

impl GeomError {
    /// The diagnostic code.
    pub fn code(&self) -> re_core::Code {
        match self {
            GeomError::Heightfield(_) => codes::INVALID_HEIGHTFIELD,
            GeomError::Primitive { .. } => codes::INVALID_PRIMITIVE,
            GeomError::TooLarge(_) => codes::MESH_TOO_LARGE,
        }
    }

    /// The error as a diagnostic.
    pub fn to_diagnostic(&self) -> Diagnostic {
        Diagnostic::error(self.code(), self.to_string())
    }
}

impl From<GeomError> for Diagnostic {
    fn from(e: GeomError) -> Diagnostic {
        e.to_diagnostic()
    }
}
