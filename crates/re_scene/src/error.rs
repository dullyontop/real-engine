//! Errors of the World facade, scene and prefab files, the hierarchy and
//! procedural sources. The message texts are those of the core spike, kept
//! verbatim; each variant also has a stable code (`RE1200`–`RE1999`, or the
//! wrapped [`ReflectError`]'s code) and converts to a [`Diagnostic`].

use re_core::{Code, Diagnostic, Location};
use re_reflect::ReflectError;

use crate::codes;

/// An error from `re_scene`.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum SceneError {
    /// A registry or value error (codes `RE1000`–`RE1199`).
    #[error(transparent)]
    Reflect(#[from] ReflectError),
    /// No entity has this stable id.
    #[error("no entity with id {0:?}")]
    UnknownEntity(String),
    /// The stable id is taken.
    #[error("an entity with id {0:?} already exists")]
    DuplicateId(String),
    /// The stable id is empty or contains `/`.
    #[error("id {0:?} must be non-empty and must not contain '/'")]
    InvalidId(String),
    /// Parenting `entity` under `new_parent` would create a cycle.
    #[error(
        "cannot parent {entity:?} under {new_parent:?}: {new_parent:?} is {entity:?} or one of its descendants"
    )]
    HierarchyCycle {
        /// The entity being moved.
        entity: String,
        /// The requested parent.
        new_parent: String,
    },
    /// The text is not a valid document (JSON syntax, unknown keys, wrong shapes).
    #[error("parse error: {0}")]
    Parse(String),
    /// An entity of a file is invalid; `at` names it (`entities[0] (id "a")`).
    #[error("parse error: {at}: {error}")]
    ParseAt {
        /// The entity's index and id.
        at: String,
        /// What is wrong with it.
        error: Box<SceneError>,
    },
    /// An entity's parent is not defined before it.
    #[error(
        "parse error: {at}: parent {parent:?} is not defined earlier in the file or in the world"
    )]
    ForwardParent {
        /// The entity's index and id.
        at: String,
        /// The parent id it names.
        parent: String,
    },
    /// Wrong `format` or unsupported `version`.
    #[error("parse error: {0}")]
    UnsupportedFormat(String),
    /// A generator failed; the previous output is kept.
    #[error("{source_id}: generation failed: {message}")]
    GeneratorFailed {
        /// The procedural source's id.
        source_id: String,
        /// What went wrong.
        message: String,
        /// The generator's own diagnostic, if it returned one.
        cause: Option<Box<Diagnostic>>,
    },
    /// A file declares a script component differently from the registered one.
    #[error(
        "script component {name:?} declared in the file does not match the registered component of that name"
    )]
    ScriptComponentMismatch {
        /// The component's name.
        name: String,
    },
    /// The entity is not generated.
    #[error("{0:?} is not a generated entity")]
    NotGenerated(String),
    /// Well-formed but unsupported content.
    #[error("{0}")]
    Invalid(String),
}

impl SceneError {
    /// The stable diagnostic code of this error.
    pub fn code(&self) -> Code {
        match self {
            SceneError::Reflect(e) => e.code(),
            SceneError::UnknownEntity(_) => codes::UNKNOWN_ID,
            SceneError::DuplicateId(_) => codes::DUPLICATE_ID,
            SceneError::InvalidId(_) => codes::INVALID_ID,
            SceneError::HierarchyCycle { .. } => codes::HIERARCHY_CYCLE,
            SceneError::Parse(_) | SceneError::ParseAt { .. } => codes::PARSE_ERROR,
            SceneError::ForwardParent { .. } => codes::FORWARD_PARENT,
            SceneError::UnsupportedFormat(_) => codes::UNSUPPORTED_FORMAT,
            SceneError::GeneratorFailed { .. } => codes::GENERATOR_FAILED,
            SceneError::ScriptComponentMismatch { .. } => codes::SCRIPT_COMPONENT_MISMATCH,
            SceneError::NotGenerated(_) => codes::NOT_GENERATED,
            SceneError::Invalid(_) => codes::INVALID_CONTENT,
        }
    }

    /// This error as an error [`Diagnostic`]. A [`SceneError::ParseAt`] keeps
    /// the inner error's own diagnostic in `related`, so its specific code is
    /// not lost.
    pub fn to_diagnostic(&self) -> Diagnostic {
        match self {
            SceneError::Reflect(e) => e.to_diagnostic(),
            SceneError::ParseAt { error, .. } => {
                Diagnostic::error(self.code(), self.to_string()).with_related(error.to_diagnostic())
            }
            SceneError::UnknownEntity(id) | SceneError::DuplicateId(id) => {
                Diagnostic::error(self.code(), self.to_string()).at(Location::entity(id.clone()))
            }
            SceneError::HierarchyCycle { entity, .. } => {
                Diagnostic::error(self.code(), self.to_string())
                    .at(Location::entity(entity.clone()))
            }
            SceneError::GeneratorFailed {
                source_id, cause, ..
            } => {
                let d = Diagnostic::error(self.code(), self.to_string())
                    .at(Location::entity(source_id.clone()))
                    .with_hint("the previous generated output is kept");
                match cause {
                    Some(c) => d.with_related((**c).clone()),
                    None => d,
                }
            }
            SceneError::InvalidId(_) => Diagnostic::error(self.code(), self.to_string())
                .with_hint("'/' is reserved for generated ids (<source id>/<key>)"),
            _ => Diagnostic::error(self.code(), self.to_string()),
        }
    }
}

impl From<SceneError> for Diagnostic {
    fn from(e: SceneError) -> Diagnostic {
        e.to_diagnostic()
    }
}
