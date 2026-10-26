//! Typed errors for RPC calls, relay transactions, journal files, and renewal status.

use alloy_primitives::ruint::ParseError as RuintParseError;
use serde_json::Value;
use thiserror::Error;

/// Errors from RPC calls and hex decoding.
#[derive(Debug, Error)]
pub enum RpcError {
    /// The response id does not match the request id.
    #[error("{0} RPC response id mismatch")]
    ResponseIdMismatch(String),

    /// The RPC server returned an error object.
    #[error("{method} RPC error: {error}")]
    RpcResponse { method: String, error: Value },

    /// The RPC response omits the result field.
    #[error("{0} RPC response has no result")]
    MissingResult(String),

    /// The RPC method returned a value that is not a string.
    #[error("{0} returned a non-string")]
    NonStringResult(&'static str),

    /// The parser rejected the hex `U256` result.
    #[error("parse {method} result: {error}")]
    ParseU256Result {
        method: &'static str,
        error: RuintParseError,
    },

    /// The HTTP client builder failed.
    #[error("build RPC client: {0}")]
    ClientBuild(#[source] reqwest::Error),
}

/// Errors from relay keys and legacy transactions.
#[derive(Debug, Error)]
pub enum TxError {
    /// The key file violates owner or mode rules.
    #[error("relay private key must be an owner-bound 0600 regular file")]
    InvalidKeyFilePermissions,

    /// The key path changed when the reader opened it.
    #[error("relay private key path changed while opening")]
    KeyFilePathChanged,

    /// The key file exceeds the size limit.
    #[error("relay private key grew beyond its size bound")]
    KeyFileSizeExceeded,

    /// The decoded key does not have 32 bytes.
    #[error("relay private key must contain exactly 32 bytes")]
    InvalidKeyLength,

    /// The secp256k1 key rejected the secret bytes.
    #[error("invalid relay private key: {0}")]
    InvalidPrivateKey(k256::ecdsa::Error),

    /// The secp256k1 signer failed to sign the digest.
    #[error("relay signing failed: {0}")]
    SigningFailed(k256::ecdsa::Error),

    /// The EIP-155 recovery id overflowed `u64`.
    #[error("EIP-155 recovery id overflow")]
    RecoveryIdOverflow,
}

/// Errors from atomic journal writes.
#[derive(Debug, Error)]
pub enum JournalStorageError {
    /// The encoded journal exceeds the byte limit.
    #[error("{0} journal exceeds its size cap")]
    SizeCapExceeded(&'static str),
}

/// Errors from TEE renewal status checks.
#[derive(Debug, Error)]
pub enum StatusError {
    /// The critical margin exceeds the warning margin.
    #[error("critical renewal margin cannot exceed warning margin")]
    CriticalMarginExceedsWarning,
}
