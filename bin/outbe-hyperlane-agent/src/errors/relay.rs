//! Errors of the relayer role.

/// Errors that the relayer returns for a message that it cannot use.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RelayError {
    /// The `Dispatch` bytes are shorter than the message header.
    #[error("the message has {0} bytes, the header needs 77")]
    ShortMessage(usize),
    /// The origin Mailbox has no `Dispatch` log for a stored leaf.
    #[error("domain {domain} leaf {index}: no Dispatch log with this message id in block {block}")]
    NoDispatch { domain: u32, index: u32, block: u64 },
    /// An announced storage location is not an `s3+http(s)` location that
    /// the relayer can read.
    #[error("storage location {0} is not a readable s3+http(s) location")]
    BadLocation(String),
    /// A checkpoint file is not the wanted checkpoint of the validator.
    #[error("the checkpoint {index} of validator {validator} has other content or another signer")]
    WrongCheckpoint {
        validator: alloy_primitives::Address,
        index: u32,
    },
    /// The destination uses an ISM that the relayer cannot serve.
    #[error("ISM {ism} has module type {module}: only the message-id multisig ISM is supported")]
    UnsupportedIsm {
        ism: alloy_primitives::Address,
        module: u8,
    },
    /// The `process` transaction is in a block but reverted.
    #[error("process transaction {0} reverted")]
    Reverted(alloy_primitives::B256),
    /// The domain of a relayer task is not in the running domains.
    #[error("domain {0} is not a running domain")]
    NoChain(u32),
}
