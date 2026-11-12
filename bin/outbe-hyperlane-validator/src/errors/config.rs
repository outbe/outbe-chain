//! Errors of the TOML configuration.

/// Errors that the validator returns for a missing or invalid configuration value.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ConfigError {
    /// The validator address in the configuration is not a 20-byte hex address.
    #[error("account.validator_address is not a valid 20-byte hex address: {0}")]
    InvalidValidatorAddress(String),
    /// The poll interval in the configuration is zero.
    #[error("hyperlane.poll_interval_secs must be > 0")]
    ZeroPollInterval,
    /// Two `[[origin]]` blocks have the same domain.
    #[error("two [[origin]] blocks for domain {0}")]
    DuplicateOrigin(u32),
    /// An `[[origin]]` block has no RPC endpoint.
    #[error("[[origin]] domain {0} has no rpc_urls")]
    NoOriginRpc(u32),
    /// An `[[origin]]` block has a zero poll interval.
    #[error("[[origin]] domain {0}: interval_secs must be > 0")]
    ZeroOriginInterval(u32),
    /// The controller knows a domain that has no `[[origin]]` block.
    #[error("domain {0} has no [[origin]] block in the configuration")]
    MissingOrigin(u32),
    /// The daemon and the `db` commands need `[storage] db_path`.
    #[error("storage.db_path is not set in the configuration")]
    NoDbPath,
    /// The daemon needs `[server] public_url`.
    #[error("the [server] section with public_url is not set in the configuration")]
    NoServer,
    /// `server.public_url` is not `http(s)://host[:port]`.
    #[error("server.public_url must be http(s)://host[:port] without a path: {0}")]
    InvalidPublicUrl(String),
}
