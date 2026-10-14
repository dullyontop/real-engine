//! Errors of the registry and of value conversion. Every variant renders a
//! message that says what was wrong and, where possible, what would have been
//! right. The texts are those of the core spike, kept verbatim; each variant
//! also has a stable code (`RE1000`–`RE1199`) and converts to a
//! [`Diagnostic`].

use re_core::{Code, Diagnostic};

use crate::codes;
use crate::value::ValueError;

/// An error from the component registry or from converting a value.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ReflectError {
    /// No component type has this name.
    #[error("unknown component {name:?}{}", did_you_mean(suggestion))]
    UnknownComponent {
        /// The name asked for.
        name: String,
        /// The closest registered name, if it is plausibly a typo.
        suggestion: Option<String>,
    },
    /// The component has no field of this name.
    #[error(
        "component {component} has no field {field:?}{}; fields are: {}",
        did_you_mean(suggestion),
        valid.join(", ")
    )]
    UnknownField {
        /// The component's name.
        component: String,
        /// The field asked for.
        field: String,
        /// The closest field name, if it is plausibly a typo.
        suggestion: Option<String>,
        /// Every field of the component, in declaration order.
        valid: Vec<String>,
    },
    /// A value does not fit the field at `path` (`Component.field`).
    #[error("{path}: {error}")]
    Value {
        /// `Component.field`.
        path: String,
        /// What is wrong with the value.
        error: ValueError,
    },
    /// A component type of this name is already registered.
    #[error("component type {0:?} is already registered")]
    DuplicateComponent(String),
    /// The entity lacks the component.
    #[error("entity has no {component} component")]
    MissingComponent {
        /// The component's name.
        component: String,
    },
    /// A malformed component schema or field list.
    #[error("{0}")]
    InvalidSchema(String),
}

fn did_you_mean(suggestion: &Option<String>) -> String {
    suggestion
        .as_ref()
        .map(|s| format!(" (did you mean {s:?}?)"))
        .unwrap_or_default()
}

impl ReflectError {
    /// The stable diagnostic code of this error.
    pub fn code(&self) -> Code {
        match self {
            ReflectError::UnknownComponent { .. } => codes::UNKNOWN_COMPONENT,
            ReflectError::UnknownField { .. } => codes::UNKNOWN_FIELD,
            ReflectError::Value { error, .. } => error.code(),
            ReflectError::DuplicateComponent(_) => codes::DUPLICATE_COMPONENT,
            ReflectError::MissingComponent { .. } => codes::MISSING_COMPONENT,
            ReflectError::InvalidSchema(_) => codes::INVALID_SCHEMA,
        }
    }

    /// This error as an error [`Diagnostic`], with a hint where one helps.
    pub fn to_diagnostic(&self) -> Diagnostic {
        let d = Diagnostic::error(self.code(), self.to_string());
        match self {
            ReflectError::UnknownComponent {
                suggestion: Some(s),
                ..
            } => d.with_hint(format!("use \"{s}\"")),
            ReflectError::UnknownField {
                suggestion: Some(s),
                ..
            } => d.with_hint(format!("use \"{s}\"")),
            ReflectError::MissingComponent { component } => {
                d.with_hint(format!("add a {component} component first"))
            }
            _ => d,
        }
    }
}

impl From<ReflectError> for Diagnostic {
    fn from(e: ReflectError) -> Diagnostic {
        e.to_diagnostic()
    }
}

/// The closest candidate by edit distance, if it is plausibly a typo
/// (case-insensitive; at most a third of the candidate's length away).
pub fn suggest<'a>(input: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
    let lower = input.to_ascii_lowercase();
    candidates
        .map(|c| (edit_distance(&lower, &c.to_ascii_lowercase()), c))
        .filter(|(d, c)| *d <= (c.len().max(3) / 3).max(1))
        .min_by_key(|(d, c)| (*d, c.to_string()))
        .map(|(_, c)| c.to_string())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != *cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestions_catch_typos_only() {
        let names = ["RigidBody", "Transform", "Collider"];
        assert_eq!(
            suggest("Rigidbody", names.iter().copied()).as_deref(),
            Some("RigidBody")
        );
        assert_eq!(
            suggest("RigidBdy", names.iter().copied()).as_deref(),
            Some("RigidBody")
        );
        assert_eq!(suggest("Health", names.iter().copied()), None);
    }
}
