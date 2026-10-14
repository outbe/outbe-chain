//! Typed errors for JSON-RPC transport and transaction signing.

/// Errors returned by the JSON-RPC client.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RpcError {
    /// The transport does not support the upgrade-key RPC method.
    #[error("upgrade-key RPC unsupported")]
    UpgradeKeyUnsupported,
    /// The transport does not support `eth_call` at an explicit block.
    #[error("eth_call at an explicit block is unsupported")]
    EthCallAtUnsupported,
    /// The transport does not support the RPC method for the TEE renewal schedule.
    #[error("TEE renewal schedule RPC is unsupported")]
    TeeRenewalScheduleUnsupported,
    /// The transport does not support the RPC method for finalized blocks.
    #[error("finalized block RPC is unsupported")]
    FinalizedBlockUnsupported,
    /// The transport does not support the RPC method for Outbe finalization.
    #[error("outbe finalization RPC is unsupported")]
    OutbeFinalizationUnsupported,
    /// The transport does not support the `eth_getProof` RPC method.
    #[error("eth_getProof RPC is unsupported")]
    EthGetProofUnsupported,
    /// The JSON-RPC server returned a null result.
    #[error("empty RPC result")]
    EmptyResult,
    /// The JSON-RPC response id does not match the request id.
    #[error("RPC response id mismatch: expected {expected}, got {actual:?}")]
    ResponseIdMismatch {
        expected: u64,
        actual: Option<serde_json::Value>,
    },
    /// The JSON-RPC server returned an error object.
    #[error("RPC error {code}: {message}")]
    Server { code: i64, message: String },
    /// The JSON-RPC result is not a hex string.
    #[error("expected hex string")]
    ExpectedHexString,
    /// The hex string is not a valid 256-bit unsigned integer.
    #[error("failed to parse U256: {0}")]
    ParseU256(alloy_primitives::ruint::ParseError),
    /// The transaction send result is not a hash string.
    #[error("expected tx hash string")]
    ExpectedTxHashString,
    /// The log query result is not an array.
    #[error("expected array of logs")]
    ExpectedLogsArray,
    /// The mock RPC field is not configured.
    #[cfg(test)]
    #[error("not mocked")]
    NotMocked,
    /// The mock `eth_call` handler is not configured.
    #[cfg(test)]
    #[error("eth_call not mocked")]
    EthCallNotMocked,
    /// The mock proof handler is not configured.
    #[cfg(test)]
    #[error("proof not mocked")]
    ProofNotMocked,
    /// Cloned error from a mock RPC response.
    #[cfg(test)]
    #[error("{0}")]
    ClonedMock(String),
}

/// Errors returned during transaction building and signing.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TxError {
    /// The decoded private key does not have 32 bytes.
    #[error("invalid private key length: expected 32 bytes, got {actual}")]
    InvalidPrivateKeyLength { actual: usize },
    /// The 32-byte value is not a valid secp256k1 secret key.
    #[error("invalid private key: {0}")]
    InvalidPrivateKey(k256::ecdsa::Error),
    /// Signing the EIP-7702 transaction digest failed.
    #[error("EIP-7702 transaction signing failed: {0}")]
    Eip7702SigningFailed(k256::ecdsa::Error),
    /// Adding the 20 percent gas buffer overflowed `u64`.
    #[error("gas limit buffer overflow")]
    GasLimitBufferOverflow,
    /// Signing the legacy transaction digest failed.
    #[error("signing failed: {0}")]
    SigningFailed(k256::ecdsa::Error),
}
