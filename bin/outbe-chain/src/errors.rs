//! Typed error definitions for the node binary.

pub(crate) mod cli;
pub(crate) mod genesis;
pub(crate) mod launch;
pub(crate) mod snapshot;

pub(crate) use cli::CliError;
pub(crate) use genesis::TeeGenesisError;
pub(crate) use launch::LaunchError;
pub(crate) use snapshot::SnapshotError;

/// Formats any displayable error as a string.
pub(crate) fn display_error<E: std::fmt::Display>(error: E) -> String {
    error.to_string()
}

/// Returns `Ok(())` when the condition holds or `Err(error)` otherwise.
pub(crate) fn require<E>(condition: bool, error: E) -> Result<(), E> {
    if condition {
        Ok(())
    } else {
        Err(error)
    }
}
