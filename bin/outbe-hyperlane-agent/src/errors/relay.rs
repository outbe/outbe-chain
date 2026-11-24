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
}
