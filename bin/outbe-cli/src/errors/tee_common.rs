//! Typed errors for shared TEE CLI helpers, identity, renewal, and RPC parsing.

use std::path::PathBuf;

/// Errors when the CLI verifies TEE admission history.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeAdmissionHistoryError {
    /// The refreshed height of the finalized block is below the prior height.
    #[error("finalized proof height regressed")]
    FinalizedProofHeightRegressed,
    /// The block number in the history response does not match the requested height.
    #[error("history header height mismatch at {0}")]
    HistoryHeaderHeightMismatch(u64),
    /// Decoding the block consensus artifacts at the given height failed.
    #[error("decode history header {height}: {reason}")]
    DecodeHistoryHeader { height: u64, reason: String },
    /// The requested history height is zero or above the finalized height.
    #[error("invalid admission history height")]
    InvalidAdmissionHistoryHeight,
    /// The finality certificate payload does not match the block digest.
    #[error("finality certificate payload mismatch")]
    FinalityCertificatePayloadMismatch,
    /// The finality proof contains too many ancestor blocks.
    #[error("too many finality ancestors")]
    TooManyFinalityAncestors,
    /// The first block in the finality chain does not match the target height.
    #[error("finality proof height mismatch at {0}")]
    FinalityProofHeightMismatch(u64),
    /// Adjacent blocks in the finality ancestor chain are not contiguous parents.
    #[error("invalid finality ancestor chain")]
    InvalidFinalityAncestorChain,
    /// The distance to the target block exceeds the maximum proof window.
    #[cfg(test)]
    #[error("distance to target block exceeds maximum proof window")]
    ProofWindowExpired,
    /// The proof request was rejected as invalid.
    #[cfg(test)]
    #[error("invalid proof request")]
    InvalidProofRequest,
}

/// Errors for TEE identity, key loading, and `tee pubkey`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeIdentityError {
    /// Connecting to the enclave endpoint failed.
    #[error("connect enclave at {socket}: {source}")]
    ConnectEnclave {
        socket: String,
        source: outbe_tee::TransportError,
    },
    /// The enclave returned an unexpected response to `GetPublicKeys`.
    #[error("expected enclave PublicKeys, got {0}")]
    UnexpectedPublicKeysResponse(String),
    /// The `GetPublicKeys` enclave request failed.
    #[error("enclave GetPublicKeys failed: {0}")]
    GetPublicKeysFailed(outbe_tee::TransportError),
    /// The permanent offer key in the enclave is not ready yet.
    #[error("enclave permanent offer key is not ready; onboarding recipient cannot be compared with chain state")]
    PermanentOfferKeyNotReady,
    /// The offer key in the on-chain `TeeRegistry` is still zero.
    #[error("on-chain tributeOfferPublicKey == 0 - chain is not TEE-bootstrapped yet, nothing to diff against")]
    ChainNotTeeBootstrapped,
    /// The enclave offer key differs from the on-chain `TeeRegistry` key.
    #[error("[FAIL] MISMATCH - enclave offer key 0x{enclave} != on-chain 0x{onchain}")]
    OfferKeyMismatch { enclave: String, onchain: String },
    /// Hashing the validator-to-NodeHost binding failed.
    #[error("hash address-to-NodeHost binding: {0}")]
    HashValidatorNodeBinding(outbe_primitives::tee_attestation_v1::CodecError),
    /// Signing the validator-to-NodeHost binding failed.
    #[error("sign address-to-NodeHost binding: {0}")]
    SignValidatorNodeBinding(Box<Self>),
    /// The parsed 32-byte identifier is all zeros.
    #[error("{0} must be nonzero")]
    ArgumentMustBeNonzero(&'static str),
    /// The decoded byte slice does not match the expected fixed length.
    #[error("{argument} must contain exactly {expected} bytes, got {actual}")]
    InvalidHexArrayLength {
        argument: &'static str,
        expected: usize,
        actual: usize,
    },
    /// The secret file for Reth P2P is missing, empty, or too large.
    #[error("Reth P2P secret {} must be a bounded regular file", .0.display())]
    InvalidRethP2pSecretFile(PathBuf),
    /// The public key for Reth P2P does not use 33-byte compressed SEC1.
    #[error("Reth P2P public key is not compressed SEC1-33")]
    UncompressedRethP2pPublicKey,
    /// The secret key for Reth P2P does not match the committed NodeHost identity.
    #[error("Reth P2P secret does not match the committed NodeHost identity")]
    RethP2pSecretMismatch,
    /// The decoded secret for Reth P2P does not have 32 bytes.
    #[error("Reth P2P secret must contain exactly 32 bytes, got {0}")]
    InvalidRethP2pSecretLength(usize),
    /// The secret bytes for Reth P2P do not form a valid secp256k1 key.
    #[error("invalid Reth P2P secp256k1 secret: {0}")]
    InvalidRethP2pSecret(k256::ecdsa::Error),
    /// Signing a 32-byte hash with the secp256k1 key failed.
    #[error("secp256k1 signing failed: {0}")]
    Secp256k1SigningFailed(k256::ecdsa::Error),
    /// Hashing the development node identity failed.
    #[error("invalid development node identity: {0}")]
    InvalidDevelopmentNodeIdentity(outbe_primitives::tee_attestation_v1::CodecError),
}

impl From<TeeIdentityError> for String {
    fn from(error: TeeIdentityError) -> Self {
        error.to_string()
    }
}

/// Errors for `tee renew` and `tee status`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeRenewalError {
    /// The command requires the global `--private-key` flag.
    #[error("tee renew requires the global --private-key EVM signer")]
    PrivateKeyRequired,
    /// Loading the committed NodeHost manifest failed.
    #[error("load committed NodeHost manifest: {0}")]
    LoadCommittedManifest(outbe_tee::TransportError),
    /// The manifest chain identifier differs from the RPC chain identifier.
    #[error("committed NodeHost manifest chain id does not match eth_chainId")]
    ManifestChainIdMismatch,
    /// NodeHost renewal requires the `--reth-p2p-secret-key` flag.
    #[error("NodeHost renewal requires --reth-p2p-secret-key")]
    MissingRethP2pSecretKey,
    /// Connecting or initializing the NodeHost enclave failed.
    #[error("connect NodeHost enclave: {0}")]
    ConnectNodeHostEnclave(outbe_tee::TransportError),
    /// Signing the renewal intent with the node key failed.
    #[error("node authority signing failed: {0}")]
    NodeAuthoritySigningFailed(TeeIdentityError),
    /// Manual renewal did not reach canonical finality before the timeout.
    #[error("tee renew timed out waiting for canonical finality; last outcome: {0}")]
    RenewalTimeout(String),
}

/// Errors when the CLI extracts TEE JSON-RPC fields or updates call context.
#[derive(Debug, thiserror::Error)]
pub(crate) enum TeeRpcError {
    /// The active protocol version exceeds `u32::MAX`.
    #[error("active protocol version overflow")]
    ProtocolVersionOverflow,
    /// The genesis block response has no `hash` field.
    #[error("genesis RPC block has no hash")]
    MissingGenesisHash,
    /// The JSON-RPC object is missing a required string field.
    #[error("RPC response has no {0}")]
    MissingField(String),
    /// The JSON-RPC object is missing a required array field.
    #[error("RPC response has no {0} array")]
    MissingArrayField(String),
    /// The JSON-RPC array contains a non-string element.
    #[error("RPC {0} contains a non-string node")]
    NonStringArrayNode(String),
    /// Parsing a hexadecimal `U256` field from the JSON-RPC object failed.
    #[error("decode RPC {field}: {source}")]
    DecodeU256Field {
        field: String,
        source: alloy_primitives::ruint::ParseError,
    },
}
