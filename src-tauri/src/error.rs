//! A single error type that crosses the native boundary with a stable,
//! discriminated JSON shape so the frontend can branch on `code` instead of
//! pattern-matching prose.

use std::fmt;

use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// Caller-supplied data is out of bounds or malformed.
    Validation { field: String, message: String },
    /// A referenced entity does not exist (stale or foreign id).
    NotFound { entity: String, id: String },
    /// The request is well-formed but conflicts with current state.
    Conflict { code: String, message: String },
    /// Something failed underneath us; the message is diagnostic, not a promise.
    Internal { message: String },
}

impl AppError {
    pub fn validation(field: impl Into<String>, message: impl Into<String>) -> Self {
        AppError::Validation {
            field: field.into(),
            message: message.into(),
        }
    }

    pub fn not_found(entity: impl Into<String>, id: impl Into<String>) -> Self {
        AppError::NotFound {
            entity: entity.into(),
            id: id.into(),
        }
    }

    pub fn conflict(code: impl Into<String>, message: impl Into<String>) -> Self {
        AppError::Conflict {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        AppError::Internal {
            message: message.into(),
        }
    }

    /// Machine-readable discriminant. Kept in sync with `src/domain/errors.ts`.
    pub fn code(&self) -> &'static str {
        match self {
            AppError::Validation { .. } => "VALIDATION",
            AppError::NotFound { .. } => "NOT_FOUND",
            AppError::Conflict { .. } => "CONFLICT",
            AppError::Internal { .. } => "INTERNAL",
        }
    }

    pub fn message(&self) -> String {
        match self {
            // `field` travels as its own key; keep the message free of it so the
            // UI can place it against the offending control.
            AppError::Validation { message, .. } => message.clone(),
            AppError::NotFound { entity, id } => format!("no {entity} with id {id}"),
            AppError::Conflict { message, .. } => message.clone(),
            AppError::Internal { message } => message.clone(),
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppError::Validation { field, message } => {
                write!(f, "[VALIDATION] {field}: {message}")
            }
            _ => write!(f, "[{}] {}", self.code(), self.message()),
        }
    }
}

impl std::error::Error for AppError {}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Always an object with `code` + `message`; variant-specific fields are
        // additive so the frontend can render them when present.
        let mut s = serializer.serialize_struct("AppError", 4)?;
        s.serialize_field("code", self.code())?;
        s.serialize_field("message", &self.message())?;
        match self {
            AppError::Validation { field, .. } => {
                s.serialize_field("field", field)?;
            }
            AppError::NotFound { entity, id } => {
                s.serialize_field("entity", entity)?;
                s.serialize_field("id", id)?;
            }
            AppError::Conflict { code, .. } => {
                s.serialize_field("conflict", code)?;
            }
            AppError::Internal { .. } => {}
        }
        s.end()
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::internal(format!("sqlite: {e}"))
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::internal(format!("io: {e}"))
    }
}
