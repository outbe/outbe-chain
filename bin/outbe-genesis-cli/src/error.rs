//! Typed error definitions for `outbe-genesis-cli`.

use std::path::PathBuf;

/// Error returned when decoding a network YAML file via `serde_yaml_ng`.
#[derive(Debug, thiserror::Error)]
#[error("{path}: {source}")]
pub struct YamlError {
    /// File path being parsed.
    pub path: PathBuf,
    /// Underlying `serde_yaml_ng` deserialization error.
    #[source]
    pub source: serde_yaml_ng::Error,
}

/// Typed errors returned by `outbe-genesis-cli` stages.
#[derive(Debug, thiserror::Error)]
pub enum GenesisError {
    /// YAML parse failure.
    #[error(transparent)]
    Yaml(#[from] YamlError),

    /// Filesystem I/O failure with path context.
    #[error("I/O error at {path}: {source}")]
    Io {
        /// Path associated with the I/O operation.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// JSON serialization or deserialization failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// TOML deserialization failure.
    #[error("TOML error: {0}")]
    Toml(#[from] toml::de::Error),

    /// Validation or domain error during genesis calculation.
    #[error("{0}")]
    Validation(String),

    /// External subprocess (`outbe-keygen` or `outbe-chain`) failed.
    #[error("command `{program}` failed: {details}")]
    Subprocess {
        /// Executable name or path.
        program: String,
        /// Exit status or stderr details.
        details: String,
    },
}

impl GenesisError {
    /// Construct a validation error with a formatted message.
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }

    /// Construct an I/O error bound to a path.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
