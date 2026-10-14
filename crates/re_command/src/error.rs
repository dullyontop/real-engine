//! Errors of operations and transactions. The message texts are those of the
//! core spike, kept verbatim (`command 0: no entity with id "crates"`); each
//! error also has a stable code and converts to a [`Diagnostic`].

use re_core::{Code, Diagnostic, Location};
use re_reflect::ReflectError;
use re_scene::SceneError;

use crate::codes;

/// Why one operation failed.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum OpError {
    /// A scene, registry or value error (codes `RE1000`–`RE1999`).
    #[error(transparent)]
    Scene(#[from] SceneError),
    /// `add_component` of a component the entity already has.
    #[error("entity already has a {component} component; use set_field to change it")]
    ComponentAlreadyPresent {
        /// The component's name.
        component: String,
    },
    /// A structural edit of a generated entity.
    #[error("{id:?} is generated: {hint}")]
    GeneratedEntity {
        /// The generated entity's id.
        id: String,
        /// What to do instead.
        hint: &'static str,
    },
    /// A malformed operation.
    #[error("{0}")]
    Invalid(String),
    /// Rolling back (or undoing a dry run) failed: an inverse operation did
    /// not apply, so the world may be partly changed.
    #[error("rollback failed, the world may be partly changed: {0}")]
    RollbackFailed(String),
}

impl From<ReflectError> for OpError {
    fn from(e: ReflectError) -> Self {
        OpError::Scene(SceneError::Reflect(e))
    }
}

impl OpError {
    /// The stable diagnostic code of this error.
    pub fn code(&self) -> Code {
        match self {
            OpError::Scene(e) => e.code(),
            OpError::ComponentAlreadyPresent { .. } => codes::COMPONENT_ALREADY_PRESENT,
            OpError::GeneratedEntity { .. } => codes::GENERATED_ENTITY,
            OpError::Invalid(_) => codes::INVALID_ARGUMENTS,
            OpError::RollbackFailed(_) => codes::UNDO_FAILED,
        }
    }

    /// This error as an error [`Diagnostic`].
    pub fn to_diagnostic(&self) -> Diagnostic {
        match self {
            OpError::Scene(e) => e.to_diagnostic(),
            OpError::GeneratedEntity { id, .. } => {
                Diagnostic::error(self.code(), self.to_string()).at(Location::entity(id.clone()))
            }
            _ => Diagnostic::error(self.code(), self.to_string()),
        }
    }
}

/// A rejected transaction: operation `index` (zero-based) failed with `error`,
/// and every operation before it was rolled back.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[error("command {index}: {error}")]
pub struct TxError {
    /// Index of the failing operation within the transaction.
    pub index: usize,
    /// Why it failed.
    pub error: OpError,
}

impl TxError {
    /// `RE2001` with this error's message (`command 2: ...`), and the failing
    /// operation's own diagnostic (with its specific code) in `related`.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let inner = self.error.to_diagnostic();
        let mut d =
            Diagnostic::error(codes::TRANSACTION_REJECTED, self.to_string()).with_hint(format!(
                "operation {} failed, so the transaction changed nothing",
                self.index
            ));
        if let Some(loc) = &inner.location {
            d = d.at(loc.clone());
        }
        d.with_related(inner)
    }
}

impl From<TxError> for Diagnostic {
    fn from(e: TxError) -> Diagnostic {
        e.to_diagnostic()
    }
}

impl From<OpError> for Diagnostic {
    fn from(e: OpError) -> Diagnostic {
        e.to_diagnostic()
    }
}
