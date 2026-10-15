//! Typed errors for `outbe-radicle`.

/// Error that the Outbe Radicle sidecar binary returns.
#[derive(Debug, thiserror::Error)]
pub enum RadicleBinaryError {
    /// The signal handler failed to install.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// The sidecar failed to initialize or run.
    #[error(transparent)]
    Sidecar(#[from] outbe_radicle_runtime::Error),
}
