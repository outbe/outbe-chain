use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("missing required option {0}")]
    Missing(&'static str),
    #[error("invalid command-line options: {0}")]
    Options(#[from] lexopt::Error),
    #[error("invalid value for {option}: {value}")]
    InvalidValue { option: &'static str, value: String },
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to load Radicle key: {0}")]
    Key(#[from] radicle::crypto::ssh::keystore::Error),
    #[error("Radicle key is missing at {0}")]
    MissingKey(PathBuf),
    #[error("failed to initialize Heartwood: {0}")]
    Runtime(#[from] radicle_node::runtime::Error),
}

#[derive(Debug, Error)]
pub enum RepositoryAvailabilityError {
    #[error(transparent)]
    Storage(#[from] radicle::storage::Error),
    #[error(transparent)]
    Repository(#[from] radicle::storage::RepositoryError),
    #[error("storage offline")]
    Offline,
    #[error("storage failed")]
    StorageFailed,
}
