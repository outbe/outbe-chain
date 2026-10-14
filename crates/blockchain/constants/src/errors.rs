//! Error types for protocol constants.

use crate::GENESIS_CONFIG_KEY;
use thiserror::Error;

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtocolConstantsError {
    #[error("invalid genesis {GENESIS_CONFIG_KEY}: {0}")]
    InvalidJson(String),
    #[error("unsupported protocol constants schema version {0}")]
    UnsupportedSchemaVersion(u16),
    #[error("protocol constant {field} must be {requirement}, got {actual}")]
    InvalidValue {
        field: &'static str,
        requirement: &'static str,
        actual: u64,
    },
    #[error("config.{GENESIS_CONFIG_KEY} is supported only by test-protocol-overrides builds")]
    UnsupportedInProduction,
    #[error("protocol constants were already initialized")]
    AlreadyInitialized,
}

impl From<serde_json::Error> for ProtocolConstantsError {
    fn from(error: serde_json::Error) -> Self {
        Self::InvalidJson(error.to_string())
    }
}
