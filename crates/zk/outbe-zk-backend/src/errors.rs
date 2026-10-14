//! Typed errors for the Barretenberg and ACVM proving backend.

use outbe_zk::error::Error;
use thiserror::Error;

/// Errors that CRS setup returns.
#[derive(Clone, Debug, Error)]
pub enum CrsInitError {
    /// Barretenberg SRS setup returned a backend error.
    #[error("{0}")]
    Failed(#[source] Error),
    /// Barretenberg SRS setup panicked across the FFI boundary.
    #[error("Barretenberg SRS initialization panicked")]
    Panicked,
}

impl From<Error> for CrsInitError {
    fn from(error: Error) -> Self {
        Self::Failed(error)
    }
}

impl From<CrsInitError> for Error {
    fn from(error: CrsInitError) -> Self {
        match error {
            CrsInitError::Failed(source) => source,
            CrsInitError::Panicked => outbe_barretenberg::RuntimeError::Setup(
                "SRS initialization panicked; process restart required".into(),
            )
            .into(),
        }
    }
}
