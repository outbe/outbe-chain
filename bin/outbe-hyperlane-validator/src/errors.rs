//! Typed errors for `outbe-hyperlane-validator`.

/// Errors that the Hyperlane validator daemon returns.
#[derive(Debug, thiserror::Error)]
pub enum ValidatorError {
    /// The RPC provider rejected the transaction.
    #[error("transaction failed: {0}")]
    TransactionFailed(String),
    /// The validator address in the configuration is not a 20-byte hex address.
    #[error("account.validator_address is not a valid 20-byte hex address: {0}")]
    InvalidValidatorAddress(String),
    /// The poll interval in the configuration is zero.
    #[error("hyperlane.poll_interval_secs must be > 0")]
    ZeroPollInterval,
    /// A stored record has the wrong length.
    #[error("corrupt {0} record in the store")]
    CorruptRecord(&'static str),
    /// A new leaf does not continue the stored leaves.
    #[error("leaf gap in domain {domain}: expected index {expected}, got {got}")]
    LeafGap {
        domain: u32,
        expected: u32,
        got: u32,
    },
    /// A signed root has no stored leaf.
    #[error("checkpoint {index} of domain {domain} has no leaf")]
    CheckpointWithoutLeaf { domain: u32, index: u32 },
    /// A different signed root is already stored at this index.
    #[error("checkpoint {index} of domain {domain} is already signed with a different root")]
    CheckpointConflict { domain: u32, index: u32 },
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
    /// The contracts of a domain do not match each other.
    #[error("domain {domain}: {what}")]
    OriginMismatch { domain: u32, what: String },
    /// The chain did not return the finalized block.
    #[error("the chain did not return a finalized block")]
    NoFinalizedBlock,
    /// A log has no block number (a pending log).
    #[error("InsertedIntoTree log without a block number")]
    LogWithoutBlock,
    /// A stored signed root differs from the root that the stored leaves
    /// give at the same index.
    #[error(
        "domain {domain} index {index}: signed root {signed} differs from the leaf root {derived}"
    )]
    RootConflict {
        domain: u32,
        index: u32,
        signed: alloy_primitives::B256,
        derived: alloy_primitives::B256,
    },
    /// The stored history of a domain was signed for another hook.
    #[error(
        "domain {domain}: the local history belongs to hook {stored}, but hookByDomain is {current}. To recover, stop the daemon and delete the database directory: every domain syncs and signs again"
    )]
    HookMismatch {
        domain: u32,
        stored: alloy_primitives::Address,
        current: alloy_primitives::Address,
    },
    /// The domain halted after a reorg. It signs nothing more.
    #[error("domain {0} is halted after a reorg")]
    Halted(u32),
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
