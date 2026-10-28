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
    /// A coordinate does not fit the 1 mm polygon grid (`RE4503`).
    #[error("out of range: {0}")]
    OutOfRange(String),
    /// A polygon or path is unusable (`RE4504`).
    #[error("invalid polygon or path: {0}")]
    Polygon(String),
    /// A polygon could not be triangulated (`RE4505`).
    #[error("triangulation failed: {0}")]
    Triangulation(String),
    /// An argument to a modelling operation is unusable (`RE4510`).
    #[error("invalid argument: {0}")]
    Argument(String),
    /// A polygon mesh breaks a validity rule (`RE4511`).
    #[error("invalid mesh: {0}")]
    InvalidMesh(String),
    /// A modelling operation's result would be too large (`RE4515`).
    #[error(
        "the result would have {points} points and {corners} corners, more than the {max} allowed",
        max = crate::poly::MAX_ELEMENTS
    )]
    PolyTooLarge {
        /// The points the result would have.
        points: u64,
        /// The corners the result would have.
        corners: u64,
    },
}

impl GeomError {
    /// The diagnostic code.
    pub fn code(&self) -> re_core::Code {
        match self {
            GeomError::Heightfield(_) => codes::INVALID_HEIGHTFIELD,
            GeomError::Primitive { .. } => codes::INVALID_PRIMITIVE,
            GeomError::TooLarge(_) => codes::MESH_TOO_LARGE,
            GeomError::OutOfRange(_) => codes::OUT_OF_RANGE,
            GeomError::Polygon(_) => codes::INVALID_POLYGON,
            GeomError::Triangulation(_) => codes::TRIANGULATION_FAILED,
            GeomError::Argument(_) => codes::INVALID_ARGUMENT,
            GeomError::InvalidMesh(_) => codes::INVALID_POLYMESH,
            GeomError::PolyTooLarge { .. } => codes::POLYMESH_TOO_LARGE,
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
