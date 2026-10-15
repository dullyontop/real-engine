//! [`CompatError`]: what the class layer, the data loaders and the scene
//! conventions report.

use re_core::{Code, Diagnostic};
use re_reflect::ReflectError;

use crate::codes;
use crate::data::Status;

/// An error of the compatibility layer. Each variant has a stable code
/// ([`CompatError::code`]).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CompatError {
    /// A data file is malformed or inconsistent (`RE9003`).
    #[error("invalid compat data: {0}")]
    InvalidData(String),
    /// No class has this name (`RE9001`).
    #[error(
        "unsupported API member: unknown class {name:?}{}",
        did_you_mean(suggestion)
    )]
    UnknownClass {
        /// The name asked for.
        name: String,
        /// The closest class name, if it is plausibly a typo.
        suggestion: Option<String>,
    },
    /// The class has no member of this name (`RE9001`).
    #[error("unsupported API member: {class}.{member}{}", did_you_mean(suggestion))]
    UnknownMember {
        /// The class.
        class: String,
        /// The member asked for.
        member: String,
        /// The closest member name of the class or its superclasses.
        suggestion: Option<String>,
    },
    /// The member is declared but not implemented: its status is `missing` or
    /// `stub-error` (`RE9001`).
    #[error("unsupported API member: {class}.{member} is not implemented yet ({})", status.as_str())]
    NotImplemented {
        /// The class that declares it.
        class: String,
        /// The member.
        member: String,
        /// Its status.
        status: Status,
    },
    /// The member is a method or event, not a property (`RE9001`).
    #[error("unsupported API member: {class}.{member} is a {kind}, not a property")]
    NotAProperty {
        /// The class.
        class: String,
        /// The member.
        member: String,
        /// `method` or `event`.
        kind: &'static str,
    },
    /// The member needs an online service (`RE9002`).
    #[error("{member} is not available offline")]
    NotAvailableOffline {
        /// `Class.Member`.
        member: String,
    },
    /// A script wrote a read-only property (`RE9005`).
    #[error("{class}.{property} is read-only")]
    ReadOnly {
        /// The class that declares it.
        class: String,
        /// The property.
        property: String,
    },
    /// A compat scene breaks the scene conventions, or an entity is not an
    /// instance of the class asked for (`RE9004`).
    #[error("{0}")]
    Scene(String),
    /// An error raised by the layer's behaviour, with its own code (a refused
    /// attribute, a locked parent).
    #[error("{message}")]
    Runtime {
        /// The diagnostic code.
        code: Code,
        /// What went wrong.
        message: String,
    },
    /// A reflection error (unknown component, value of the wrong type, ...).
    #[error(transparent)]
    Reflect(#[from] ReflectError),
}

fn did_you_mean(suggestion: &Option<String>) -> String {
    suggestion
        .as_ref()
        .map(|s| format!(" (did you mean {s:?}?)"))
        .unwrap_or_default()
}

impl CompatError {
    /// The stable diagnostic code of this error.
    pub fn code(&self) -> Code {
        match self {
            CompatError::InvalidData(_) => codes::INVALID_DATA,
            CompatError::UnknownClass { .. }
            | CompatError::UnknownMember { .. }
            | CompatError::NotImplemented { .. }
            | CompatError::NotAProperty { .. } => re_script::codes::UNSUPPORTED_MEMBER,
            CompatError::NotAvailableOffline { .. } => codes::NOT_AVAILABLE_OFFLINE,
            CompatError::ReadOnly { .. } => codes::READ_ONLY,
            CompatError::Scene(_) => codes::SCENE_CONVENTION,
            CompatError::Runtime { code, .. } => *code,
            CompatError::Reflect(e) => e.code(),
        }
    }

    /// This error as an error [`Diagnostic`], with a hint where one helps.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let d = Diagnostic::error(self.code(), self.to_string());
        match self {
            CompatError::UnknownClass {
                suggestion: Some(s),
                ..
            }
            | CompatError::UnknownMember {
                suggestion: Some(s),
                ..
            } => d.with_hint(format!("use \"{s}\"")),
            CompatError::Reflect(e) => e.to_diagnostic(),
            _ => d,
        }
    }
}

impl From<CompatError> for Diagnostic {
    fn from(e: CompatError) -> Diagnostic {
        e.to_diagnostic()
    }
}
