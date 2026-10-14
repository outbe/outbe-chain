//! Error types for the feeder main loop, Oracle RPC client, and vote decoding.

use serde_json::Value;
use thiserror::Error;

/// Errors that occur in the feeder main loop or state setup.
#[derive(Debug, Error)]
pub(crate) enum FeederError {
    /// The configured state path is not a directory.
    #[error("feeder state path is not a directory")]
    StatePathNotDirectory,
    /// The aggregator produced no price data for the current period.
    #[error("no price data available")]
    NoPriceDataAvailable,
}

/// Errors that occur during Oracle RPC calls, signing, or receipt checks.
#[derive(Debug, Error)]
pub(crate) enum OracleError {
    /// The JSON-RPC server returned an error object.
    #[error("{method}: {error}")]
    RpcFailure {
        /// The JSON-RPC method name.
        method: String,
        /// The JSON-RPC error payload.
        error: Value,
    },
    /// The JSON-RPC response lacks a result field.
    #[error("{method}: missing result")]
    MissingResult {
        /// The JSON-RPC method name.
        method: String,
    },
    /// The block object lacks a block hash string.
    #[error("missing block hash")]
    MissingBlockHash,
    /// The RPC chain ID does not match the configured chain ID.
    #[error("RPC chain id {chain} differs from configured {expected_chain}")]
    ChainIdMismatch {
        /// The chain ID from the RPC endpoint.
        chain: u64,
        /// The chain ID from the feeder configuration.
        expected_chain: u64,
    },
    /// The genesis block object lacks a block hash string.
    #[error("missing genesis hash")]
    MissingGenesisHash,
    /// The chain already consumed the pending nonce.
    #[error("pending nonce already consumed; reconcile before replacement")]
    PendingNonceConsumed,
    /// The mempool holds another transaction behind the feeder nonce.
    #[error("other transactions queued behind feeder nonce")]
    TransactionsQueuedBehindNonce,
    /// The signer has an untracked pending transaction in the mempool.
    #[error("signer has an untracked pending transaction; waiting for nonce {nonce}")]
    UntrackedPendingTransaction {
        /// The canonical nonce of the signer.
        nonce: u64,
    },
    /// The gas price response is not a valid hex string.
    #[error("invalid gas price")]
    InvalidGasPrice,
    /// The pending transaction hash does not match the signed bytes.
    #[error("pending transaction hash does not match signed bytes")]
    PendingHashMismatch,
    /// The broadcast call returned a different transaction hash.
    #[error("broadcast returned an unexpected transaction hash")]
    UnexpectedBroadcastHash,
    /// The receipt transaction hash does not match the pending vote hash.
    #[error("receipt transaction hash mismatch")]
    ReceiptHashMismatch,
    /// The receipt block hash does not match the canonical block hash.
    #[error("receipt is not canonical")]
    ReceiptNotCanonical,
    /// The JSON-RPC quantity is not a string.
    #[error("invalid RPC quantity")]
    InvalidQuantity,
    /// The JSON-RPC quantity string lacks the `0x` prefix.
    #[error("invalid RPC quantity prefix")]
    InvalidQuantityPrefix,
}

/// Errors that occur when the feeder decodes `submitVote` calldata.
#[derive(Debug, Error)]
pub(crate) enum VoteDecodeError {
    /// ABI decoding of `submitVote` calldata failed.
    #[error("decode submitVote: {0}")]
    AbiDecode(#[from] alloy_sol_types::Error),
}
